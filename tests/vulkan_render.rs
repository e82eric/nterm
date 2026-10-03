#![cfg(feature = "vulkan")]

use bytemuck::Zeroable;
use egui_wgpu::wgpu;
use ghostty_egui::gpu::{
    SharedGpu,
    terminal::{Globals, Glyph, TerminalRenderer},
};

fn texture(
    gpu: &SharedGpu,
    size: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Vulkan test texture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn pixels(gpu: &SharedGpu, texture: &wgpu::Texture, size: u32) -> Vec<u8> {
    let stride = (size * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Vulkan readback"),
        size: u64::from(stride * size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(size),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
        .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut result = Vec::new();
    for row in data.chunks(stride as usize) {
        result.extend_from_slice(&row[..size as usize * 4]);
    }
    drop(data);
    buffer.unmap();
    result
}

#[test]
fn native_cell_record_layout() {
    assert_eq!(std::mem::size_of::<Globals>(), 144);
    assert_eq!(std::mem::offset_of!(Globals, padding), 96);
    assert_eq!(std::mem::offset_of!(Globals, background), 128);
    assert_eq!(std::mem::size_of::<Glyph>(), 32);
}

// Mirror the public C dispatch table rather than calling an alternate renderer.
#[repr(C)]
struct NativeDriver {
    version: u32,
    context: *mut std::ffi::c_void,
    call: unsafe extern "C" fn(*mut std::ffi::c_void, u32, *const [usize; 12]) -> usize,
}

#[test]
fn native_dispatch_renders_images_and_background_images() {
    let gpu = pollster::block_on(SharedGpu::new(
        SharedGpu::instance(),
        None,
        wgpu::TextureFormat::Rgba8Unorm,
    ))
    .unwrap();
    let context = ghostty_egui::gpu::bridge::Context::new(&gpu);
    let driver = unsafe { &*context.driver().cast::<NativeDriver>() };
    assert_eq!(driver.version, 1);
    let call = |op, args: &[usize]| {
        let mut a = [0; 12];
        a[..args.len()].copy_from_slice(args);
        let result = unsafe { (driver.call)(driver.context, op, &a) };
        assert_ne!(result, 0, "native operation {op} failed");
        result
    };
    let mut globals = Globals::zeroed();
    globals.screen = [32.0; 2];
    globals.cell = [16.0; 2];
    globals.grid = 2 | 2 << 16;
    globals.projection = [
        1.0 / 16.0,
        0.0,
        0.0,
        0.0,
        0.0,
        -1.0 / 16.0,
        0.0,
        0.0,
        0.0,
        0.0,
        -1.0,
        0.0,
        -1.0,
        1.0,
        0.0,
        1.0,
    ];
    globals.background = 0xff000000;
    let uniform = call(1, &[144, 1]);
    let bytes = bytemuck::bytes_of(&globals);
    call(2, &[uniform, 0, bytes.as_ptr() as usize, bytes.len()]);
    let target = call(3, &[32, 32, 1, 0]);
    // BGRA source also exercises channel conversion in the host bridge.
    let image_data = [0u8, 0, 255, 255].repeat(4);
    let image = call(
        3,
        &[2, 2, 2, 1, image_data.as_ptr() as usize, image_data.len()],
    );
    let background = call(8, &[0]);
    let image_pipeline = call(8, &[3]);
    let bg_pipeline = call(8, &[4]);
    let image_params: [f32; 12] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 2.0, 16.0, 16.0, 0.0, 0.0];
    let data = call(1, &[48, 0]);
    let bytes: &[u8] = bytemuck::cast_slice(&image_params);
    call(2, &[data, 0, bytes.as_ptr() as usize, bytes.len()]);
    let frame = call(5, &[target]);
    call(6, &[frame, target, background, uniform, 0, 0, 0, 0, 3, 1]);
    call(
        6,
        &[
            frame,
            target,
            image_pipeline,
            uniform,
            data,
            0,
            image,
            0,
            4,
            1,
        ],
    );
    let exported = call(7, &[frame, target]);
    let texture = unsafe { context.texture(exported) };
    let first = pixels(&gpu, &texture.texture, 32);
    assert_eq!(
        &first[(8 * 32 + 8) * 4..(8 * 32 + 8) * 4 + 4],
        &[255, 0, 0, 255]
    );
    assert_eq!(
        &first[(24 * 32 + 24) * 4..(24 * 32 + 24) * 4 + 4],
        &[0, 0, 0, 255]
    );
    call(0, &[exported]);
    drop(texture);
    let params = [1.0f32.to_bits(), 2 << 4]; // stretch, opaque
    let bytes: &[u8] = bytemuck::cast_slice(&params);
    call(2, &[data, 0, bytes.as_ptr() as usize, bytes.len()]);
    let frame = call(5, &[target]);
    call(
        6,
        &[frame, target, bg_pipeline, uniform, data, 0, image, 0, 3, 1],
    );
    let exported = call(7, &[frame, target]);
    let texture = unsafe { context.texture(exported) };
    let second = pixels(&gpu, &texture.texture, 32);
    assert!(second.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    for handle in [
        exported,
        bg_pipeline,
        image_pipeline,
        background,
        image,
        target,
        data,
        uniform,
    ] {
        call(0, &[handle]);
    }
}

#[test]
fn vulkan_terminal_and_egui_share_device_and_render_pixels() {
    let mut gpu = pollster::block_on(SharedGpu::new(
        SharedGpu::instance(),
        None,
        wgpu::TextureFormat::Rgba8Unorm,
    ))
    .unwrap();
    println!("Vulkan adapter: {:?}", gpu.adapter.get_info());
    assert_eq!(gpu.adapter.get_info().backend, wgpu::Backend::Vulkan);
    let renderer = TerminalRenderer::new(&gpu);
    let atlas_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let gray = texture(&gpu, 8, wgpu::TextureFormat::R8Unorm, atlas_usage);
    let color = texture(&gpu, 8, wgpu::TextureFormat::Rgba8UnormSrgb, atlas_usage);
    gpu.queue.write_texture(
        gray.as_image_copy(),
        &[255; 64],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(8),
        },
        gray.size(),
    );
    gpu.queue.write_texture(
        color.as_image_copy(),
        &[0, 0, 255, 255].repeat(64),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(32),
            rows_per_image: Some(8),
        },
        color.size(),
    );
    let output = texture(
        &gpu,
        32,
        gpu.format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = output.create_view(&Default::default());
    let mut globals = Globals::zeroed();
    globals.screen = [32.0; 2];
    globals.cell = [16.0; 2];
    globals.grid = 2 | (2 << 16);
    globals.projection = [
        1.0 / 16.0,
        0.0,
        0.0,
        0.0,
        0.0,
        -1.0 / 16.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        -1.0,
        1.0,
        0.0,
        1.0,
    ];
    globals.cursor = u32::MAX;
    globals.background = 0xff000000;
    let glyphs = [
        Glyph([0, 0, 8, 8, 8 << 16, 0, 0xff0000ff, 0]),
        Glyph([0, 0, 8, 8, 8 << 16, 1, 0xffffffff, 1]),
    ];
    let frame = renderer
        .prepare(
            &gpu,
            &globals,
            &[0xff00ff00, 0, 0, 0],
            &glyphs,
            &gray.create_view(&Default::default()),
            &color.create_view(&Default::default()),
        )
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.encode(&mut encoder, &view, &frame);
    gpu.queue.submit([encoder.finish()]);
    let before = pixels(&gpu, &output, 32);
    let pixel =
        |data: &[u8], x: usize, y: usize| data[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].to_vec();
    assert_eq!(
        pixel(&before, 2, 10),
        [255, 0, 0, 255],
        "glyph coverage/color"
    );
    assert_eq!(pixel(&before, 12, 12), [0, 255, 0, 255], "cell background");
    assert_eq!(
        pixel(&before, 18, 10),
        [0, 0, 255, 255],
        "color atlas glyph"
    );
    assert_eq!(pixel(&before, 28, 28), [0, 0, 0, 255], "global background");

    let ui = gpu.context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(32.0, 32.0),
            )),
            ..Default::default()
        },
        |ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(32.0, 32.0)),
                0.0,
                egui::Color32::BLUE,
            );
        },
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut commands = gpu.encode_ui(&mut encoder, &view, ui, [32, 32], None);
    commands.push(encoder.finish());
    gpu.queue.submit(commands);
    let after = pixels(&gpu, &output, 32);
    assert_eq!(pixel(&after, 26, 26), [0, 0, 255, 255], "egui composition");
    assert_eq!(
        pixel(&after, 2, 10),
        [255, 0, 0, 255],
        "terminal preserved under egui"
    );

    // Invalid cell data is rejected before GPU submission.
    assert!(
        renderer
            .prepare(
                &gpu,
                &globals,
                &[0],
                &[],
                &gray.create_view(&Default::default()),
                &color.create_view(&Default::default())
            )
            .is_err()
    );
}
