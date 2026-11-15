//! Textured shapes in batches: the cached images most of a frame is made of
//! (layers, symbols, runs of text), drawn with one small program and one
//! GL draw for each run of the same texture and tint. femtovg draws each of
//! its shapes on its own, with a deal of state for each, which on the
//! unit's driver costs more than the pixels. The tint is a uniform, not a
//! varying: each input a pixel interpolates costs the GPU, and nest-qemu's
//! model of it most.

use glow::HasContext;

/// A vertex: its place in panel pixels and its texture coordinates (u·q,
/// v·q, q: q is 1 but in perspective).
const FLOATS: usize = 5;

const VERTEX: &str = "
attribute vec2 a_pos;
attribute vec3 a_uv;
uniform vec2 u_view;
varying vec3 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(2.0 * a_pos.x / u_view.x - 1.0, 1.0 - 2.0 * a_pos.y / u_view.y, 0.0, 1.0);
}";

const FRAGMENT: &str = "
precision mediump float;
uniform sampler2D u_tex;
uniform vec4 u_color;
varying vec3 v_uv;
void main() {
    gl_FragColor = texture2DProj(u_tex, v_uv) * u_color;
}";

/// A run of vertices drawn with one texture and tint: whether it blends
/// (an opaque image need not), its first vertex and count.
struct Batch {
    texture: glow::Texture,
    rgba: [f32; 4],
    blend: bool,
    projective: bool,
    first: usize,
    count: usize,
}

#[derive(Clone, PartialEq)]
struct CompositeKey {
    base: glow::Texture, overlay: glow::Texture, base_color: [f32; 4], overlay_color: [f32; 4], width: u32, height: u32,
}

struct Composite {
    program: Program,
    base: Option<glow::UniformLocation>, overlay: Option<glow::UniformLocation>,
    base_color: Option<glow::UniformLocation>, overlay_color: Option<glow::UniformLocation>,
    mapping: Option<glow::UniformLocation>,
    target: Option<(glow::Texture, glow::Framebuffer)>,
    key: Option<CompositeKey>,
    size: (u32, u32),
}

struct Program {
    id: glow::Program,
    view: Option<glow::UniformLocation>,
    texture: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
}

pub struct Quads {
    gl: glow::Context,
    projective: Program,
    affine: Program,
    composite: Composite,
    buffer: glow::Buffer,
    vertices: Vec<f32>,
    batches: Vec<Batch>,
    /// The texture last cleared by clear_texture, and the framebuffer
    /// it is cleared through.
    cleared: Option<(glow::Texture, glow::Framebuffer)>,
}

impl Quads {
    /// # Safety
    /// The GL context `gl` loads from must be current, now and when drawing.
    pub unsafe fn new(gl: glow::Context) -> Result<Quads, String> {
        let compile = |kind: u32, source: &str| -> Result<glow::Shader, String> {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(gl.get_shader_info_log(shader));
            }
            Ok(shader)
        };
        let link = |vertex: &str, fragment: &str| -> Result<Program, String> {
            let (vs, fs) = (compile(glow::VERTEX_SHADER, vertex)?, compile(glow::FRAGMENT_SHADER, fragment)?);
            let id = gl.create_program()?;
            gl.attach_shader(id, vs); gl.attach_shader(id, fs);
            for (i, name) in ["a_pos", "a_uv"].iter().enumerate() { gl.bind_attrib_location(id, i as u32, name); }
            gl.link_program(id);
            if !gl.get_program_link_status(id) { return Err(gl.get_program_info_log(id)); }
            gl.delete_shader(vs); gl.delete_shader(fs);
            Ok(Program { id, view: gl.get_uniform_location(id, "u_view"), texture: gl.get_uniform_location(id, "u_tex"), color: gl.get_uniform_location(id, "u_color") })
        };
        let projective = link(VERTEX, FRAGMENT)?;
        let affine = link(&VERTEX.replace("varying vec3 v_uv;", "varying vec2 v_uv;").replace("v_uv = a_uv;", "v_uv = a_uv.xy;"),
                          &FRAGMENT.replace("varying vec3 v_uv;", "varying vec2 v_uv;").replace("texture2DProj", "texture2D"))?;
        let composite = link(&VERTEX.replace("varying vec3 v_uv;", "varying vec3 v_uv; varying vec2 v_base; uniform mat3 u_mapping;")
                .replace("v_uv = a_uv;", "v_uv = a_uv; v_base = (u_mapping * vec3(a_pos, 1.0)).xy;"),
            "precision mediump float; uniform sampler2D u_tex; uniform sampler2D u_base; uniform sampler2D u_overlay;
             uniform vec4 u_color; uniform vec4 u_base_color; uniform vec4 u_overlay_color;
             varying vec3 v_uv; varying vec2 v_base;
             void main() { vec4 b = texture2D(u_base, v_base) * u_base_color;
                 vec4 m = texture2DProj(u_tex, v_uv) * u_color;
                 vec4 o = texture2D(u_overlay, v_base) * u_overlay_color;
                 gl_FragColor = o + (m + b * (1.0 - m.a)) * (1.0 - o.a); }")?;
        let composite = Composite {
            base: gl.get_uniform_location(composite.id, "u_base"), overlay: gl.get_uniform_location(composite.id, "u_overlay"),
            base_color: gl.get_uniform_location(composite.id, "u_base_color"), overlay_color: gl.get_uniform_location(composite.id, "u_overlay_color"),
            mapping: gl.get_uniform_location(composite.id, "u_mapping"), program: composite, target: None, key: None, size: (0, 0),
        };
        let buffer = gl.create_buffer()?;
        Ok(Quads { gl, projective, affine, composite, buffer, vertices: Vec::new(), batches: Vec::new(), cleared: None })
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Clears the bound framebuffer's colour, all of it: a clear the GPU
    /// can make by marking its tiles, where one with a scissor or the
    /// stencil writes every pixel.
    ///
    /// # Safety
    /// The context must be current.
    pub unsafe fn clear(&self, rgba: [f32; 4]) {
        self.clear_bound(rgba);
    }

    /// Clears all of `texture` the same way, through a framebuffer of
    /// its own kept for the next time, and leaves it bound.
    ///
    /// # Safety
    /// The context must be current and `texture` alive.
    pub unsafe fn clear_texture(&mut self, texture: glow::Texture, rgba: [f32; 4]) {
        let fbo = match self.cleared {
            Some((t, fbo)) if t == texture => fbo,
            old => {
                if let Some((_, fbo)) = old { self.gl.delete_framebuffer(fbo); }
                let Ok(fbo) = self.gl.create_framebuffer() else { return };
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                self.gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
                self.cleared = Some((texture, fbo));
                fbo
            }
        };
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        self.clear_bound(rgba);
    }

    unsafe fn clear_bound(&self, rgba: [f32; 4]) {
        self.gl.disable(glow::SCISSOR_TEST);
        self.gl.clear_color(rgba[0], rgba[1], rgba[2], rgba[3]);
        self.gl.clear(glow::COLOR_BUFFER_BIT);
    }

    /// Draws all of `texture`, upside down as femtovg draws into its
    /// images, over the panel, `width` by `height` pixels, opaque. All
    /// of the square, corners too: the unit's driver writes into the page
    /// on show when a frame leaves any of the window unwritten.
    ///
    /// # Safety
    /// The context must be current and `texture` alive.
    pub unsafe fn present(&mut self, texture: glow::Texture, width: f32, height: f32) {
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        self.push(texture, &[[0.0, 0.0, 0.0, 1.0], [width, 0.0, 1.0, 1.0], [width, height, 1.0, 0.0], [0.0, height, 0.0, 0.0]],
                  [1.0, 1.0, 1.0, 1.0], false);
        self.draw(width as u32, height as u32);
    }

    /// A convex polygon of `texture`, its corners as (x, y, u, v) in panel
    /// pixels and texture coordinates, multiplied by the premultiplied
    /// colour `rgba`; `blend` is false where the result is opaque.
    pub fn push(&mut self, texture: glow::Texture, corners: &[[f32; 4]], rgba: [f32; 4], blend: bool) {
        let mut points = [[0f32; 5]; 16];
        let n = corners.len().min(points.len());
        for (point, &[x, y, u, v]) in points.iter_mut().zip(corners) {
            *point = [x, y, u, v, 1.0];
        }
        self.push_projective(texture, &points[..n], rgba, blend);
    }

    /// As `push`, its corners (x, y, u·q, v·q, q): sampled in perspective.
    pub fn push_projective(&mut self, texture: glow::Texture, corners: &[[f32; 5]], rgba: [f32; 4], blend: bool) {
        if corners.len() < 3 {
            return;
        }
        let first = self.vertices.len() / FLOATS;
        for i in 1..corners.len() - 1 {
            for c in [corners[0], corners[i], corners[i + 1]] {
                self.vertices.extend_from_slice(&c);
            }
        }
        let count = 3 * (corners.len() - 2);
        let projective = corners.iter().any(|c| c[4] != 1.0);
        match self.batches.last_mut() {
            Some(b) if b.texture == texture && b.rgba == rgba && b.blend == blend && b.projective == projective => b.count += count,
            _ => self.batches.push(Batch { texture, rgba, blend, projective, first, count }),
        }
    }

    /// A layer repainted in place invalidates its retained composition.
    pub fn invalidate_composite(&mut self) { self.composite.key = None; }

    // An opaque full-screen base, moving geometry, and a full-screen foreground
    // can retain their static composition. Only the moving geometry then samples
    // all three layers, with the same premultiplied source-over order.
    fn composition(&self, width: u32, height: u32) -> Option<([f32; 9], CompositeKey)> {
        if self.batches.len() != 3 { return None; }
        let (base, top) = (&self.batches[0], &self.batches[2]);
        if base.blend || base.projective || top.projective || base.count != 6 || top.count != 6 { return None; }
        let b = &self.vertices[base.first * FLOATS..(base.first + 6) * FLOATS];
        let t = &self.vertices[top.first * FLOATS..(top.first + 6) * FLOATS];
        if b != t { return None; }
        let middle = &self.batches[1];
        if !disjoint_triangles(&self.vertices[middle.first * FLOATS..(middle.first + middle.count) * FLOATS]) { return None; }
        let mapping = full_screen_mapping(b, width as f32, height as f32)?;
        Some((mapping, CompositeKey { base: base.texture, overlay: top.texture, base_color: base.rgba, overlay_color: top.rgba, width, height }))
    }

    unsafe fn compose(&mut self, key: &CompositeKey) -> Result<glow::Texture, String> {
        let gl = &self.gl;
        if self.composite.key.as_ref() == Some(key) { return Ok(self.composite.target.unwrap().0); }
        if self.composite.size != (key.width, key.height) {
            if let Some((texture, framebuffer)) = self.composite.target.take() { gl.delete_texture(texture); gl.delete_framebuffer(framebuffer); }
        }
        let previous = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
        let (texture, framebuffer) = match self.composite.target {
            Some(target) => target,
            None => {
                let texture = gl.create_texture()?;
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, key.width as i32, key.height as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                let framebuffer = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
                self.composite.target = Some((texture, framebuffer));
                self.composite.size = (key.width, key.height);
                (texture, framebuffer)
            }
        };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        let (w, h) = (key.width as f32, key.height as f32);
        let quad: [f32; 30] = [0.,0.,0.,1.,1., w,0.,1.,1.,1., w,h,1.,0.,1., 0.,0.,0.,1.,1., w,h,1.,0.,1., 0.,h,0.,0.,1.];
        let bytes = std::slice::from_raw_parts(quad.as_ptr().cast::<u8>(), quad.len()*4);
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
        gl.use_program(Some(self.affine.id));
        gl.uniform_2_f32(self.affine.view.as_ref(), w, h);
        gl.uniform_1_i32(self.affine.texture.as_ref(), 0);
        gl.active_texture(glow::TEXTURE0);
        gl.disable(glow::BLEND);
        for (source, color) in [(key.base, key.base_color), (key.overlay, key.overlay_color)] {
            gl.bind_texture(glow::TEXTURE_2D, Some(source));
            gl.uniform_4_f32_slice(self.affine.color.as_ref(), &color);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
            gl.enable(glow::BLEND);
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, std::num::NonZeroU32::new(previous as u32).map(glow::NativeFramebuffer));
        self.composite.key = Some(key.clone());
        Ok(texture)
    }

    /// Draws what was pushed onto the bound framebuffer, `width` by
    /// `height` pixels, and forgets it.
    ///
    /// # Safety
    /// The context must be current, and every texture pushed still alive.
    pub unsafe fn draw(&mut self, width: u32, height: u32) {
        if self.batches.is_empty() {
            return;
        }
        let gl = &self.gl;
        gl.viewport(0, 0, width as i32, height as i32);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.disable(glow::STENCIL_TEST);
        gl.disable(glow::SCISSOR_TEST);
        gl.blend_equation(glow::FUNC_ADD);
        gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.buffer));
        let stride = (FLOATS * 4) as i32;
        for (index, (size, offset)) in [(2, 0), (3, 2)].into_iter().enumerate() {
            gl.enable_vertex_attrib_array(index as u32);
            gl.vertex_attrib_pointer_f32(index as u32, size, glow::FLOAT, false, stride, offset * 4);
        }
        let composition = self.composition(width, height).and_then(|(mapping, key)| self.compose(&key).ok().map(|texture| (mapping, key, texture)));
        let gl = &self.gl;
        let bytes = std::slice::from_raw_parts(self.vertices.as_ptr().cast::<u8>(), self.vertices.len() * 4);
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
        let mut blending = None;
        let mut active = None;
        for (index, b) in self.batches.iter().enumerate() {
            if composition.is_some() && index == 2 { break; }
            let combined = composition.is_some() && index == 1;
            let program = if combined { &self.composite.program } else if b.projective { &self.projective } else { &self.affine };
            if active != Some(program.id) {
                gl.use_program(Some(program.id));
                gl.uniform_2_f32(program.view.as_ref(), width as f32, height as f32);
                gl.uniform_1_i32(program.texture.as_ref(), 0);
                active = Some(program.id);
            }
            let blend = b.blend && !combined;
            if blending != Some(blend) {
                if blend { gl.enable(glow::BLEND) } else { gl.disable(glow::BLEND) }
                blending = Some(blend);
            }
            gl.uniform_4_f32_slice(program.color.as_ref(), if index == 0 && composition.is_some() { &[1.0; 4] } else { &b.rgba });
            if let Some((mapping, key, _)) = composition.as_ref().filter(|_| combined) {
                gl.uniform_matrix_3_f32_slice(self.composite.mapping.as_ref(), false, mapping);
                gl.uniform_4_f32_slice(self.composite.base_color.as_ref(), &key.base_color);
                gl.uniform_4_f32_slice(self.composite.overlay_color.as_ref(), &key.overlay_color);
                gl.uniform_1_i32(self.composite.base.as_ref(), 1); gl.uniform_1_i32(self.composite.overlay.as_ref(), 2);
                gl.active_texture(glow::TEXTURE1); gl.bind_texture(glow::TEXTURE_2D, Some(key.base));
                gl.active_texture(glow::TEXTURE2); gl.bind_texture(glow::TEXTURE_2D, Some(key.overlay));
                gl.active_texture(glow::TEXTURE0);
            }
            let texture = if index == 0 { composition.as_ref().map_or(b.texture, |(_, _, texture)| *texture) } else { b.texture };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.draw_arrays(glow::TRIANGLES, b.first as i32, b.count as i32);
        }
        gl.enable(glow::BLEND);
        for index in 0..2 {
            gl.disable_vertex_attrib_array(index);
        }
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.use_program(None);
        self.vertices.clear();
        self.batches.clear();
    }
}

// Reconstructing the base under a moving fragment is valid only when no other
// moving fragment has already contributed there. Shared triangle edges are fine.
fn disjoint_triangles(vertices: &[f32]) -> bool {
    let triangles = vertices.as_chunks::<15>().0;
    for (i, a) in triangles.iter().enumerate() {
        for b in &triangles[i+1..] {
            let separated = [a, b].iter().any(|triangle| (0..3).any(|edge| {
                let (p, q) = (edge * 5, ((edge + 1) % 3) * 5);
                let (nx, ny) = (triangle[q+1]-triangle[p+1], triangle[p]-triangle[q]);
                let interval = |v: &[f32; 15]| {
                    let values = [nx*v[0]+ny*v[1], nx*v[5]+ny*v[6], nx*v[10]+ny*v[11]];
                    (values.into_iter().fold(f32::INFINITY, f32::min), values.into_iter().fold(f32::NEG_INFINITY, f32::max))
                };
                let (aa, bb) = (interval(a), interval(b));
                let tolerance = (nx.abs()+ny.abs()) * 0.0001;
                aa.1 <= bb.0+tolerance || bb.1 <= aa.0+tolerance
            }));
            if !separated { return false; }
        }
    }
    true
}

// The two triangles must cover the screen once, with a one-to-one affine UV map.
fn full_screen_mapping(v: &[f32], width: f32, height: f32) -> Option<[f32; 9]> {
    if v.len() != 30 || width <= 0.0 || height <= 0.0 { return None; }
    let points: [_; 6] = std::array::from_fn(|i| { let p = &v[i*5..]; (p[0], p[1], p[2], p[3], p[4]) });
    if points[0] != points[3] || points[2] != points[4] || points.iter().any(|&(x,y,u,w,q)|
        (x.abs() > 0.001 && (x-width).abs() > 0.001) || (y.abs() > 0.001 && (y-height).abs() > 0.001) || (u != 0.0 && u != 1.0) || (w != 0.0 && w != 1.0) || q != 1.0) { return None; }
    let (a,b,c) = (points[0],points[1],points[2]);
    let (dx1,dy1,dx2,dy2)=(b.0-a.0,b.1-a.1,c.0-a.0,c.1-a.1);
    let det=dx1*dy2-dx2*dy1;
    if (det.abs() - width*height).abs() > 1.0 { return None; }
    if (points[5].0 - (a.0+c.0-b.0)).abs() > 0.001 || (points[5].1 - (a.1+c.1-b.1)).abs() > 0.001 { return None; }
    let map=|u0:f32,u1:f32,u2:f32| { let x=((u1-u0)*dy2-(u2-u0)*dy1)/det; let y=(dx1*(u2-u0)-dx2*(u1-u0))/det; (x,y,u0-x*a.0-y*a.1) };
    let (ux,uy,ut)=map(a.2,b.2,c.2);let (vx,vy,vt)=map(a.3,b.3,c.3);
    if (ux*points[5].0+uy*points[5].1+ut-points[5].2).abs()>1e-5 || (vx*points[5].0+vy*points[5].1+vt-points[5].3).abs()>1e-5 { return None; }
    Some([ux,vx,0.0,uy,vy,0.0,ut,vt,1.0])
}

#[cfg(test)]
mod tests {
    use super::{full_screen_mapping, disjoint_triangles};

    #[test]
    fn overlapping_moving_fragments_are_not_composed() {
        let a = [0.,0.,0.,0.,1., 2.,0.,0.,0.,1., 0.,2.,0.,0.,1.];
        let b = [2.,0.,0.,0.,1., 2.,2.,0.,0.,1., 0.,2.,0.,0.,1.];
        assert!(disjoint_triangles(&[a,b].concat()));
        assert!(!disjoint_triangles(&[a,a].concat()));
    }

    #[test]
    fn rotated_screen_maps_layers_to_the_same_texels() {
        let vertices = [0.,480.,0.,1.,1., 0.,0.,1.,1.,1., 480.,0.,1.,0.,1.,
                        0.,480.,0.,1.,1., 480.,0.,1.,0.,1., 480.,480.,0.,0.,1.];
        let m = full_screen_mapping(&vertices, 480., 480.).unwrap();
        for (x,y,u,v) in [(0.,480.,0.,1.), (480.,0.,1.,0.), (120.,360.,0.25,0.75)] {
            assert!((m[0]*x+m[3]*y+m[6]-u).abs() < 1e-5);
            assert!((m[1]*x+m[4]*y+m[7]-v).abs() < 1e-5);
        }
    }

    #[test]
    fn partial_or_non_affine_layers_are_not_composed() {
        let mut vertices = [0.,0.,0.,1.,1., 480.,0.,1.,1.,1., 480.,480.,1.,0.,1.,
                            0.,0.,0.,1.,1., 480.,480.,1.,0.,1., 0.,480.,0.,0.,1.];
        assert!(full_screen_mapping(&vertices,480.,480.).is_some());
        vertices[25] = 20.;
        assert!(full_screen_mapping(&vertices,480.,480.).is_none());
        vertices[25] = 0.; vertices[27] = 1.;
        assert!(full_screen_mapping(&vertices,480.,480.).is_none());
    }
}
