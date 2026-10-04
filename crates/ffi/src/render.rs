//! The render thread: owns the wgpu device and the surface, draws the
//! session's [`Screen`](lookthrough_render::Screen) when updates arrive, and
//! asks the server to match the surface size.
//!
//! - The device lives as long as the session. Only the surface comes and
//!   goes with the Android `Surface`, so the framebuffer texture survives
//!   the app going to the background (`research.md` §5).
//! - Present mode is Mailbox when available, otherwise Fifo, with a frame
//!   latency of 1 (`HANDOFF.md` rule 8).

use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use lookthrough_core::session::Writer;
use lookthrough_core::stats::Samples;
use lookthrough_render::{Gpu, Renderer, View, desktop_size};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::client::Shared;

/// A platform window to draw into.
pub(crate) struct Window {
    pub display: RawDisplayHandle,
    pub window: RawWindowHandle,
    /// Size in physical pixels.
    pub size: (u32, u32),
    /// Keeps the handles valid (on Android, the `ANativeWindow` reference).
    pub _keep: Box<dyn Send>,
}

// SAFETY: the handles are plain pointers to objects `keep` holds a
// reference to; `ANativeWindow` may be used from any thread.
#[allow(unsafe_code)]
unsafe impl Send for Window {}

pub(crate) enum Msg {
    /// Replace the surface; the sender is signalled once the old one is
    /// gone.
    Surface(Option<Window>, Sender<()>),
    Redraw,
    /// The server sent an ExtendedDesktopSize.
    ServerSize {
        width: u16,
        height: u16,
        status: u16,
    },
    Quit,
}

/// Wait this long after the last surface change before asking the server
/// to resize; each request costs a compositor mode set (`research.md` §6).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// ExtendedDesktopSize status Neat VNC sends when it hands a
/// SetDesktopSize to the compositor (`RFB_RESIZE_STATUS_REQUEST_FORWARDED`).
const RESIZE_FORWARDED: u16 = 4;

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// The running render thread. Dropping it stops the thread.
pub(crate) struct Thread {
    tx: Sender<Msg>,
    handle: Option<JoinHandle<()>>,
}

impl Thread {
    pub(crate) fn spawn(
        tx: Sender<Msg>,
        rx: Receiver<Msg>,
        shared: Arc<Shared>,
        writer: Writer,
        resize: bool,
    ) -> io::Result<Thread> {
        let handle = std::thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                Render {
                    shared,
                    writer,
                    resize_enabled: resize,
                    instance: None,
                    gfx: None,
                    target: None,
                    resize: Resize::default(),
                    present: Samples::default(),
                    since: Instant::now(),
                }
                .run(rx)
            })?;
        Ok(Thread {
            tx,
            handle: Some(handle),
        })
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Gfx {
    adapter: wgpu::Adapter,
    gpu: Gpu,
    renderer: Renderer,
    format: wgpu::TextureFormat,
}

/// Field order matters: the surface drops before the window it was made
/// from.
struct Target {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    _window: Window,
}

#[derive(Default)]
struct Resize {
    /// The size the view wants, and when to ask for it.
    pending: Option<((u16, u16), Instant)>,
    /// The last size asked for.
    requested: Option<(u16, u16)>,
}

struct Render {
    shared: Arc<Shared>,
    writer: Writer,
    resize_enabled: bool,
    instance: Option<wgpu::Instance>,
    gfx: Option<Gfx>,
    target: Option<Target>,
    resize: Resize,
    /// Update applied → frame presented.
    present: Samples,
    since: Instant,
}

impl Render {
    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            let msg = match self.resize.pending {
                Some((_, at)) => rx.recv_deadline(at),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                Ok(Msg::Surface(window, ack)) => {
                    self.set_surface(window);
                    let _ = ack.send(());
                    self.view_changed();
                }
                Ok(Msg::Redraw) => self.draw(),
                Ok(Msg::ServerSize {
                    width,
                    height,
                    status,
                }) => {
                    match status {
                        0 => tracing::info!(width, height, "server size"),
                        // Neat VNC: accepted and passed to the compositor;
                        // the new size follows in another update.
                        RESIZE_FORWARDED => tracing::debug!("resize request forwarded"),
                        _ => tracing::warn!(width, height, status, "server refused resize"),
                    }
                    // The first ExtendedDesktopSize unlocks SetDesktopSize.
                    if self.resize.requested.is_none()
                        && let Some((_, at)) = &mut self.resize.pending
                    {
                        *at = Instant::now();
                    }
                    self.view_changed();
                }
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.try_resize();
        }
        // Drop the surface before the device.
        self.target = None;
    }

    fn set_surface(&mut self, window: Option<Window>) {
        self.target = None;
        let Some(window) = window else {
            tracing::debug!("surface released");
            return;
        };
        match self.create_target(window) {
            Ok(t) => {
                self.target = Some(t);
                self.draw();
            }
            Err(e) => tracing::error!("can't draw to the surface: {e}"),
        }
    }

    fn create_target(&mut self, window: Window) -> Result<Target, String> {
        let instance = self.instance.get_or_insert_with(|| {
            wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::from_env()
                    .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL),
                ..Default::default()
            })
        });
        // SAFETY: `window.keep` keeps the handles valid, and `Target`
        // drops the surface before it.
        #[allow(unsafe_code)]
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display,
                raw_window_handle: window.window,
            })
        }
        .map_err(|e| e.to_string())?;
        if self.gfx.is_none() {
            self.gfx = Some(create_gfx(instance, &surface)?);
        }
        let gfx = self.gfx.as_mut().unwrap();
        let caps = surface.get_capabilities(&gfx.adapter);
        let format = *caps.formats.first().ok_or("surface has no formats")?;
        if format != gfx.format {
            tracing::debug!(?format, "surface format changed");
            gfx.renderer = Renderer::new(&gfx.gpu.device, format);
            gfx.format = format;
        }
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: window.size.0.max(1),
            height: window.size.1.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&gfx.gpu.device, &config);
        tracing::info!(
            ?format,
            ?present_mode,
            w = config.width,
            h = config.height,
            "surface configured"
        );
        if self.shared.screen.attach(&gfx.gpu, format) {
            tracing::debug!("updates arrived before the GPU; requesting a full update");
            let _ = self.writer.refresh();
        }
        Ok(Target {
            surface,
            config,
            _window: window,
        })
    }

    fn draw(&mut self) {
        self.shared.redraw_started();
        let (Some(gfx), Some(t)) = (&mut self.gfx, &self.target) else {
            return;
        };
        let frame = match t.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                tracing::debug!("surface outdated; reconfiguring");
                t.surface.configure(&gfx.gpu.device, &t.config);
                self.shared.redraw();
                return;
            }
            Err(e) => {
                tracing::warn!(%e, "no surface texture");
                return;
            }
        };
        let view = View {
            size: (t.config.width, t.config.height),
            pointer: self.shared.view.lock().unwrap().pointer,
        };
        let Gpu { device, queue } = &gfx.gpu;
        gfx.renderer
            .prepare(device, queue, &self.shared.screen, view);
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lookthrough"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            gfx.renderer.draw(&mut pass);
        }
        queue.submit([encoder.finish()]);
        let suboptimal = frame.suboptimal;
        frame.present();
        if suboptimal {
            t.surface.configure(device, &t.config);
        }
        self.presented();
    }

    fn presented(&mut self) {
        let Some(applied) = self.shared.applied.lock().unwrap().take() else {
            return;
        };
        self.present.record(applied.elapsed());
        if self.since.elapsed() >= REPORT_INTERVAL {
            if let Some(s) = self.present.summary() {
                tracing::info!("update applied→frame presented: {s}");
            }
            self.present.clear();
            self.since = Instant::now();
        }
    }

    /// Schedules a SetDesktopSize for the surface's size.
    fn view_changed(&mut self) {
        if !self.resize_enabled {
            return;
        }
        let v = *self.shared.view.lock().unwrap();
        let Some(size) = v.size else { return };
        let want = desktop_size(size, v.density);
        let current = self.shared.screen.size();
        let target = self.resize.pending.map(|p| p.0).or(self.resize.requested);
        if target == Some(want) || (target.is_none() && current == want) {
            return;
        }
        // Resize at once the first time, so the session starts at the
        // right size; debounce later changes.
        let delay = if self.resize.requested.is_none() {
            Duration::ZERO
        } else {
            RESIZE_DEBOUNCE
        };
        self.resize.pending = Some((want, Instant::now() + delay));
    }

    fn try_resize(&mut self) {
        let Some(((w, h), at)) = self.resize.pending else {
            return;
        };
        if Instant::now() < at {
            return;
        }
        match self.writer.set_desktop_size(w, h) {
            Ok(true) => {
                tracing::info!(w, h, "requested server resize");
                self.resize.requested = Some((w, h));
                self.resize.pending = None;
            }
            // No ExtendedDesktopSize from the server yet; retried when it
            // arrives.
            Ok(false) => self.resize.pending = Some(((w, h), Instant::now() + RESIZE_DEBOUNCE)),
            Err(e) => {
                tracing::warn!(%e, "resize send failed");
                self.resize.pending = None;
            }
        }
    }
}

fn create_gfx(instance: &wgpu::Instance, surface: &wgpu::Surface<'_>) -> Result<Gfx, String> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(surface),
        force_fallback_adapter: false,
    }))
    .map_err(|e| e.to_string())?;
    let info = adapter.get_info();
    tracing::info!(name = info.name, backend = ?info.backend, "GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lookthrough"),
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let format = surface
        .get_capabilities(&adapter)
        .formats
        .first()
        .copied()
        .ok_or("surface has no formats")?;
    Ok(Gfx {
        renderer: Renderer::new(&device, format),
        gpu: Gpu { device, queue },
        adapter,
        format,
    })
}
