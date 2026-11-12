//! Renders the app off screen to a PNG, through the same wgpu renderer the
//! window uses: for looking at pages in tests, without a display.

use std::path::Path;

use eframe::egui_wgpu::{wgpu, Renderer, RendererOptions, ScreenDescriptor};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Runs `frames` frames of `ui` at `size` points and `scale` pixels a point,
/// and writes the last as a PNG at `out`.
pub fn render(size: [f32; 2], scale: f32, frames: usize, out: &Path, mut ui: impl FnMut(&mut egui::Ui)) {
    let context = egui::Context::default();
    crate::install_fonts(&context);
    crate::install_style(&context);
    let mut outputs = Vec::new();
    for _ in 0..frames {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]))),
            viewports: std::iter::once((egui::ViewportId::ROOT, egui::ViewportInfo {
                native_pixels_per_point: Some(scale), ..Default::default()
            })).collect(),
            ..Default::default()
        };
        outputs.push(context.run_ui(input, &mut ui));
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .expect("a GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .expect("a GPU device");
    let mut renderer = Renderer::new(&device, FORMAT, RendererOptions::default());
    let (width, height) = ((size[0] * scale).round() as u32, (size[1] * scale).round() as u32);
    let screen = ScreenDescriptor { size_in_pixels: [width, height], pixels_per_point: scale };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Every frame's textures, in order: the fonts' atlas grows over them.
    for output in &outputs {
        for (id, delta) in &output.textures_delta.set {
            renderer.update_texture(&device, &queue, *id, delta);
        }
    }
    let last = outputs.pop().unwrap();
    let jobs = context.tessellate(last.shapes, scale);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    renderer.update_buffers(&device, &queue, &mut encoder, &jobs, &screen);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("snapshot"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
    }
    let row = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("snapshot"),
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |result| result.expect("map the snapshot"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("the GPU finishes");
    let data = buffer.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&data[y * row as usize..][..(width * 4) as usize]);
    }
    let file = std::io::BufWriter::new(std::fs::File::create(out).expect("create the snapshot"));
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut w| w.write_image_data(&pixels)).expect("write the snapshot");
}
