use crate::screen::Snapshot;
use crate::{Placement, Screen};

/// The target a frame is drawn into.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// Size of the render pass viewport, in physical pixels.
    pub size: (u32, u32),
    /// The local pointer in view pixels, or `None` to hide the cursor.
    pub pointer: Option<(f32, f32)>,
}

/// Draws a [`Screen`]: the framebuffer, 1:1 with nearest sampling when it
/// fits, then the cursor on top.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    fb_quad: wgpu::Buffer,
    cursor_quad: wgpu::Buffer,
    bind: Option<Bound>,
    draw_fb: bool,
    draw_cursor: bool,
}

/// Bind groups for one generation of the screen's textures.
struct Bound {
    generation: u64,
    fb: Option<wgpu::BindGroup>,
    cursor: Option<wgpu::BindGroup>,
}

/// Uniform `Quad` in `shader.wgsl`.
fn quad_bytes(rect: [f32; 4], opaque: bool) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, v) in rect.into_iter().chain([f32::from(u8::from(opaque))]).enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}

impl Renderer {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lookthrough quad"),
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
            label: Some("lookthrough"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lookthrough quad"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lookthrough nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let quad = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Renderer {
            pipeline,
            layout,
            sampler,
            fb_quad: quad("framebuffer quad"),
            cursor_quad: quad("cursor quad"),
            bind: None,
            draw_fb: false,
            draw_cursor: false,
        }
    }

    /// Updates uniforms and bind groups for the next [`Renderer::draw`].
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, screen: &Screen, view: View) {
        let snap = screen.snapshot();
        if self.bind.as_ref().is_none_or(|b| b.generation != snap.generation) {
            self.bind = Some(self.bind_groups(device, &snap));
        }
        let (vw, vh) = (view.size.0.max(1) as f32, view.size.1.max(1) as f32);
        // View pixels (y down) to clip space (y up).
        let clip = |x0: f32, y0: f32, x1: f32, y1: f32| {
            [x0 / vw * 2.0 - 1.0, 1.0 - y0 / vh * 2.0, x1 / vw * 2.0 - 1.0, 1.0 - y1 / vh * 2.0]
        };

        let place = Placement::new(snap.fb_size, view.size);
        self.draw_fb = snap.fb.is_some();
        if self.draw_fb {
            let (x1, y1) = place.to_view((snap.fb_size.0 as f32, snap.fb_size.1 as f32));
            queue.write_buffer(&self.fb_quad, 0, &quad_bytes(clip(place.x, place.y, x1, y1), true));
        }

        self.draw_cursor = false;
        if let (Some((tex, hotspot)), Some(p)) = (&snap.cursor, view.pointer) {
            // The cursor is in framebuffer pixels: scale it with the
            // framebuffer, and snap its origin to whole pixels at 1:1.
            let s = place.scale;
            let mut x0 = p.0 - f32::from(hotspot.0) * s;
            let mut y0 = p.1 - f32::from(hotspot.1) * s;
            if s == 1.0 {
                (x0, y0) = (x0.floor(), y0.floor());
            }
            let (x1, y1) = (x0 + tex.width() as f32 * s, y0 + tex.height() as f32 * s);
            queue.write_buffer(&self.cursor_quad, 0, &quad_bytes(clip(x0, y0, x1, y1), false));
            self.draw_cursor = true;
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(bind) = &self.bind else { return };
        pass.set_pipeline(&self.pipeline);
        if let (true, Some(bg)) = (self.draw_fb, &bind.fb) {
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..4, 0..1);
        }
        if let (true, Some(bg)) = (self.draw_cursor, &bind.cursor) {
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..4, 0..1);
        }
    }

    fn bind_groups(&self, device: &wgpu::Device, snap: &Snapshot) -> Bound {
        let group = |tex: &wgpu::Texture, quad: &wgpu::Buffer| {
            let view = tex.create_view(&Default::default());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: quad.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };
        Bound {
            generation: snap.generation,
            fb: snap.fb.as_ref().map(|t| group(t, &self.fb_quad)),
            cursor: snap.cursor.as_ref().map(|(t, _)| group(t, &self.cursor_quad)),
        }
    }
}
