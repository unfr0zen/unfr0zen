//! The dimmed, blurred backdrop behind the modals.
//!
//! A wgpu paint callback can't read what is already on the surface, so the blur
//! is of a snapshot of the window: a modal stays off screen for the frames its
//! capture takes, and the GPU then blurs the snapshot, growing the blur as it
//! fades in over the live page and shrinking it as it fades out. Without a GPU
//! the backdrop only dims.

use std::sync::Arc;

use eframe::egui_wgpu::{self, wgpu, CallbackResources, CallbackTrait, ScreenDescriptor};
use egui::layers::PaintList;
use egui::{Color32, ColorImage, Context, Id, LayerId, Modal, Order, PaintCallbackInfo, Rect, UserData, ViewportCommand};

/// Seconds of the fade in and out.
const FADE: f32 = 0.12;
/// How far the backdrop darkens the window.
const DIM: u8 = 110;
/// A capture asked for in one frame is of the next one, so both stay clean.
const CAPTURE_FRAMES: u8 = 2;
/// The blur's gaussian sigma, in points. Its render targets have a texel a
/// point, which bounds its taps at any display scale.
const SIGMA: f32 = 2.5;

const SHADER: &str = r#"
struct Params { step: vec2f, sigma: f32, opacity: f32, uv_min: vec2f, uv_size: vec2f };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var linear: sampler;

struct Varying { @builtin(position) position: vec4f, @location(0) uv: vec2f };

@vertex fn vs(@builtin(vertex_index) index: u32) -> Varying {
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    let uv = params.uv_min + corner * params.uv_size;
    return Varying(vec4f(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0), uv);
}

@fragment fn blur(in: Varying) -> @location(0) vec4f {
    if params.sigma < 0.01 {
        return textureSampleLevel(source, linear, in.uv, 0.0);
    }
    let radius = i32(ceil(params.sigma * 3.0));
    var sum = vec4f(0.0);
    var total = 0.0;
    for (var tap = -radius; tap <= radius; tap++) {
        let offset = f32(tap);
        let weight = exp(-offset * offset / (2.0 * params.sigma * params.sigma));
        sum += textureSampleLevel(source, linear, in.uv + params.step * offset, 0.0) * weight;
        total += weight;
    }
    return sum / total;
}

@fragment fn show(in: Varying) -> @location(0) vec4f {
    return vec4f(textureSampleLevel(source, linear, in.uv, 0.0).rgb * params.opacity, params.opacity);
}
"#;

/// The bytes of the shader's `Params`.
fn params(step: [f32; 2], sigma: f32, opacity: f32, uv_min: [f32; 2], uv_size: [f32; 2]) -> [u8; 32] {
    let mut bytes = [0; 32];
    let fields = [step[0], step[1], sigma, opacity, uv_min[0], uv_min[1], uv_size[0], uv_size[1]];
    for (chunk, field) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
        *chunk = field.to_ne_bytes();
    }
    bytes
}

/// The blur's pipelines, in the renderer's callback resources.
struct Gpu {
    layout: wgpu::BindGroupLayout,
    blur: wgpu::RenderPipeline,
    show: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    /// The horizontal pass's, the vertical pass's and the final draw's.
    uniforms: [wgpu::Buffer; 3],
    targets: Option<Targets>,
    /// The capture in `targets`, and the sigma they are blurred by.
    generation: u64,
    sigma: f32,
}

/// The snapshot, and the textures its two blur passes render to.
struct Targets {
    size: [usize; 2],
    texel: f32,
    source: wgpu::Texture,
    across: wgpu::TextureView,
    down: wgpu::TextureView,
    /// The bind groups of the horizontal pass, the vertical pass and the final draw.
    groups: [wgpu::BindGroup; 3],
}

impl Gpu {
    fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, format, blend| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = || device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("backdrop"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            blur: pipeline("blur", TARGET_FORMAT, None),
            show: pipeline("show", target, Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("backdrop"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            uniforms: [uniform(), uniform(), uniform()],
            targets: None,
            layout,
            generation: 0,
            sigma: -1.0,
        }
    }

    fn targets(&self, device: &wgpu::Device, size: [usize; 2], texel: f32) -> Targets {
        let texture = |width: u32, height: u32, usage| device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage,
            view_formats: &[],
        });
        let (width, height) = (size[0] as u32, size[1] as u32);
        let target = |side: u32| (side as f32 / texel).ceil().max(1.0) as u32;
        let source = texture(width, height, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let across = texture(target(width), target(height), usage).create_view(&Default::default());
        let down = texture(target(width), target(height), usage).create_view(&Default::default());
        let group = |uniform: &wgpu::Buffer, view: &wgpu::TextureView| device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let groups = [
            group(&self.uniforms[0], &source.create_view(&Default::default())),
            group(&self.uniforms[1], &across),
            group(&self.uniforms[2], &down),
        ];
        Targets { size, texel, source, across, down, groups }
    }
}

/// The format of the snapshot and of the blur's passes: as the capture's bytes
/// are, like the gamma-space colours the rest of the UI is drawn in.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One frame's draw of the blurred snapshot.
struct Paint {
    image: Arc<ColorImage>,
    generation: u64,
    /// Physical pixels a blur texel covers.
    texel: f32,
    sigma: f32,
    opacity: f32,
    uv_min: [f32; 2],
    uv_size: [f32; 2],
}

impl CallbackTrait for Paint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<Gpu>() else { return Vec::new() };
        let new = gpu.generation != self.generation;
        if new {
            let [width, height] = self.image.size;
            let reuse = gpu.targets.as_ref().is_some_and(|t| t.size == self.image.size && t.texel == self.texel);
            if !reuse {
                gpu.targets = Some(gpu.targets(device, self.image.size, self.texel));
            }
            let targets = gpu.targets.as_ref().expect("just made");
            queue.write_texture(
                targets.source.as_image_copy(),
                self.image.as_raw(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width as u32 * 4),
                    rows_per_image: Some(height as u32),
                },
                wgpu::Extent3d { width: width as u32, height: height as u32, depth_or_array_layers: 1 },
            );
            gpu.generation = self.generation;
        }
        let Some(targets) = &gpu.targets else { return Vec::new() };
        queue.write_buffer(&gpu.uniforms[2], 0, &params([0.0; 2], 0.0, self.opacity, self.uv_min, self.uv_size));
        if new || gpu.sigma != self.sigma {
            let width = (self.image.size[0] as f32 / self.texel).ceil().max(1.0);
            let height = (self.image.size[1] as f32 / self.texel).ceil().max(1.0);
            let whole = ([0.0, 0.0], [1.0, 1.0]);
            queue.write_buffer(&gpu.uniforms[0], 0, &params([1.0 / width, 0.0], self.sigma, 1.0, whole.0, whole.1));
            queue.write_buffer(&gpu.uniforms[1], 0, &params([0.0, 1.0 / height], self.sigma, 1.0, whole.0, whole.1));
            for (view, group) in [(&targets.across, &targets.groups[0]), (&targets.down, &targets.groups[1])] {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("backdrop"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&gpu.blur);
                pass.set_bind_group(0, group, &[]);
                pass.draw(0..3, 0..1);
            }
            gpu.sigma = self.sigma;
        }
        Vec::new()
    }

    fn paint(&self, _info: PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(gpu) = resources.get::<Gpu>() else { return };
        let Some(targets) = &gpu.targets else { return };
        pass.set_pipeline(&gpu.show);
        pass.set_bind_group(0, &targets.groups[2], &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Where the frame's modals note their layers for `Backdrop::show`.
fn layers_id() -> Id {
    Id::new("backdrop_modal_layers")
}

/// A modal that leaves its backdrop to `Backdrop`.
pub fn modal(ctx: &Context, name: &str) -> Modal {
    let modal = Modal::new(Id::new(name)).backdrop_color(Color32::TRANSPARENT);
    let layer = modal.area.layer();
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Vec<LayerId>>(layers_id()).push(layer));
    modal
}

#[derive(Default)]
pub struct Backdrop {
    /// A GPU is there to do the blur.
    blur: bool,
    snapshot: Option<Arc<ColorImage>>,
    generation: u64,
    was_open: bool,
    /// Frames left that the modals stay off screen for the capture.
    capturing: u8,
}

impl Backdrop {
    /// A backdrop for the app's renderer, which it readies for its paint
    /// callbacks unless that renderer runs on the CPU.
    pub fn new(render_state: Option<&egui_wgpu::RenderState>) -> Self {
        let Some(render_state) = render_state else { return Self::default() };
        if render_state.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Self::default();
        }
        let gpu = Gpu::new(&render_state.device, render_state.target_format);
        render_state.renderer.write().callback_resources.insert(gpu);
        Self { blur: true, ..Self::default() }
    }

    /// Paints the backdrop of the modals shown this frame, called after them:
    /// the whole window dimmed, and `blurred`, a part of it, blurred if it can be.
    pub fn show(&mut self, ctx: &Context, blurred: Rect) {
        let layers = ctx.data_mut(|data| data.remove_temp::<Vec<LayerId>>(layers_id())).unwrap_or_default();
        let open = !layers.is_empty();

        let captured = ctx.input(|input| input.events.iter().rev().find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        }));
        if let Some(image) = captured {
            self.snapshot = Some(image);
            self.generation += 1;
        }

        if self.blur && open && !self.was_open && self.snapshot.is_none() {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::default()));
            self.capturing = CAPTURE_FRAMES;
        }
        self.was_open = open;

        if self.capturing > 0 {
            self.capturing -= 1;
            ctx.graphics_mut(|graphics| {
                for layer in layers {
                    *graphics.entry(layer) = PaintList::default();
                }
            });
            ctx.request_repaint();
            return;
        }

        let dim = ctx.animate_bool_with_time(Id::new("backdrop_dim"), open, FADE);
        // Its own fade: the capture lands some frames after the dimming starts.
        let blur = ctx.animate_bool_with_time(Id::new("backdrop_blur"), open && self.snapshot.is_some(), FADE);
        if dim == 0.0 && blur == 0.0 {
            self.snapshot = None;
            return;
        }
        // Above the panels and below the modals, which are foreground areas.
        let painter = ctx.layer_painter(LayerId::new(Order::Middle, Id::new("backdrop")));
        let window = ctx.content_rect();
        if let Some(image) = self.snapshot.as_ref().filter(|_| blur > 0.0) {
            painter.add(egui_wgpu::Callback::new_paint_callback(blurred, Paint {
                image: image.clone(),
                generation: self.generation,
                texel: ctx.pixels_per_point().round().max(1.0),
                sigma: SIGMA * blur,
                opacity: blur,
                uv_min: [(blurred.min.x - window.min.x) / window.width(), (blurred.min.y - window.min.y) / window.height()],
                uv_size: [blurred.width() / window.width(), blurred.height() / window.height()],
            }));
        }
        painter.rect_filled(window, 0.0, Color32::from_black_alpha(DIM).gamma_multiply(dim));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, ViewportId};

    struct Frame {
        modal_painted: bool,
        capture_asked: bool,
        blur: Option<Rect>,
    }

    /// Frames with a modal from the second on, the capture arriving in the fourth.
    fn frames(blur: bool, blurred: Rect) -> Vec<Frame> {
        let context = Context::default();
        let mut backdrop = Backdrop { blur, ..Backdrop::default() };
        (0..6).map(|frame| {
            let events = if blur && frame == 3 {
                vec![egui::Event::Screenshot {
                    viewport_id: ViewportId::ROOT,
                    user_data: UserData::default(),
                    image: Arc::new(ColorImage::new([1, 1], vec![Color32::BLACK])),
                }]
            } else {
                Vec::new()
            };
            let output = context.run_ui(egui::RawInput { time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                if frame >= 1 {
                    modal(ui.ctx(), "test").show(ui.ctx(), |ui| ui.label("modal"));
                }
                backdrop.show(ui.ctx(), blurred);
            });
            Frame {
                modal_painted: output.shapes.iter().any(|shape| matches!(shape.shape, egui::Shape::Text(_))),
                capture_asked: output.viewport_output.values()
                    .any(|viewport| viewport.commands.iter().any(|command| matches!(command, ViewportCommand::Screenshot(_)))),
                blur: output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Callback(callback) => Some(callback.rect),
                    _ => None,
                }),
            }
        }).collect()
    }

    #[test]
    fn a_new_modal_asks_for_one_capture_and_stays_off_screen_while_it_is_taken() {
        let frames = frames(true, Rect::from_min_max(Pos2::new(0.0, 36.0), Pos2::new(100.0, 76.0)));
        let asked: Vec<bool> = frames.iter().map(|frame| frame.capture_asked).collect();
        assert_eq!(asked, [false, true, false, false, false, false]);
        let painted: Vec<bool> = frames.iter().map(|frame| frame.modal_painted).collect();
        assert_eq!(painted[..3], [false, false, false]);
        assert!(painted[3..].iter().all(|painted| *painted), "{painted:?}");
    }

    #[test]
    fn the_blur_draws_only_its_part_of_the_window_once_captured() {
        let part = Rect::from_min_max(Pos2::new(0.0, 36.0), Pos2::new(100.0, 76.0));
        let frames = frames(true, part);
        assert!(frames[..3].iter().all(|frame| frame.blur.is_none()));
        assert_eq!(frames[4].blur, Some(part));
    }

    #[test]
    fn without_a_gpu_a_modal_shows_at_once_with_no_capture_or_blur() {
        let frames = frames(false, Rect::from_min_max(Pos2::new(0.0, 36.0), Pos2::new(100.0, 76.0)));
        assert!(frames.iter().all(|frame| !frame.capture_asked && frame.blur.is_none()));
        // An area needs a frame to be placed by its size.
        assert!(frames[2..].iter().all(|frame| frame.modal_painted));
    }
}
