//! Renders a `Screen` offscreen and reads it back. Skips when no GPU
//! adapter is available.

use lookthrough_core::Rect;
use lookthrough_render::{Gpu, Renderer, Screen, View};

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) =
        pollster_block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some(Gpu { device, queue })
}

/// Minimal executor: wgpu's native futures complete without a reactor.
fn pollster_block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

fn render(gpu: &Gpu, format: wgpu::TextureFormat, screen: &Screen, view: View) -> Vec<u8> {
    let (w, h) = view.size;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut renderer = Renderer::new(&gpu.device, format);
    screen.attach(gpu, format);
    renderer.prepare(&gpu.device, &gpu.queue, screen, view);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    {
        let tv = target.create_view(&Default::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tv,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        renderer.draw(&mut pass);
    }
    let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        target.size(),
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range();
    data.chunks(row as usize)
        .flat_map(|r| r[..(w * 4) as usize].to_vec())
        .collect()
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

#[test]
fn draws_framebuffer_one_to_one_and_cursor() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
        let screen = Screen::new();
        screen.resize(4, 2);
        // Uploads before attach are missed.
        screen.upload(Rect { x: 0, y: 0, w: 1, h: 1 }, &[1, 2, 3, 0]);
        assert!(screen.attach(&gpu, format));
        // Row 0: four distinct pixels; row 1: mid grey, X byte garbage.
        let row0 = [10, 20, 30, 0, 200, 100, 50, 7, 1, 2, 3, 99, 255, 254, 253, 0];
        screen.upload(Rect { x: 0, y: 0, w: 4, h: 1 }, &row0);
        screen.upload(Rect { x: 0, y: 1, w: 4, h: 1 }, &[128, 128, 128, 13].repeat(4));
        // 1x1 opaque red cursor with hotspot (0,0); 1x1 transparent would be mask 0.
        screen.set_cursor((0, 0), 1, 1, &[255, 0, 0, 0], &[0x80]);

        // View 6x4: the framebuffer is centred at (1,1).
        let view = View { size: (6, 4), pointer: Some((4.5, 2.2)) };
        let img = render(&gpu, format, &screen, view);
        assert_eq!(px(&img, 6, 0, 0), [0, 0, 0, 255], "{format:?} border");
        assert_eq!(px(&img, 6, 1, 1), [10, 20, 30, 255], "{format:?}");
        assert_eq!(px(&img, 6, 2, 1), [200, 100, 50, 255], "{format:?}");
        assert_eq!(px(&img, 6, 3, 1), [1, 2, 3, 255], "{format:?}");
        assert_eq!(px(&img, 6, 1, 2), [128, 128, 128, 255], "{format:?}");
        // Cursor at floor(4.5, 2.2) = (4, 2), over the framebuffer.
        assert_eq!(px(&img, 6, 4, 2), [255, 0, 0, 255], "{format:?} cursor");
        assert_eq!(px(&img, 6, 4, 1), [255, 254, 253, 255], "{format:?}");
    }
}
