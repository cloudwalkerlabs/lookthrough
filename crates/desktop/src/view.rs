//! The session view: an iced shader widget that draws the framebuffer
//! texture with `lookthrough-render`, and turns mouse and keyboard events
//! into RFB input written straight to the socket from the UI thread
//! (`HANDOFF.md` rule 7).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc::UnboundedSender;
use iced::keyboard::{self, key::Physical};
use iced::mouse::{self, ScrollDelta};
use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Event, Point, Rectangle, window};
use lookthrough_core::pipeline::Output;
use lookthrough_core::session::{self, SessionError, Writer};
use lookthrough_core::stats::Samples;
use lookthrough_core::Event as Rfb;
use lookthrough_render::{Gpu, Placement, Renderer, Screen, View};

use crate::keymap;

/// Session events the app reacts to.
#[derive(Debug, Clone)]
pub enum Notify {
    /// New pixels or cursor; redraw.
    Redraw,
    /// The view's physical size or scale factor changed.
    ViewChanged,
    /// The server sent an ExtendedDesktopSize.
    ServerSize { width: u16, height: u16, status: u16 },
    DesktopName(String),
    Closed(Option<String>),
}

/// The view's size, as last seen by the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub physical: (u32, u32),
    pub scale: f32,
}

/// State shared by the session handler, the app and the widget.
pub struct Shared {
    pub screen: Screen,
    pub metrics: Mutex<Option<Metrics>>,
    notify: UnboundedSender<Notify>,
    redraw_pending: AtomicBool,
    /// When the oldest update not yet drawn was applied.
    applied: Mutex<Option<Instant>>,
    present: Mutex<PresentStats>,
}

struct PresentStats {
    samples: Samples,
    since: Instant,
}

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

impl Shared {
    pub fn new(notify: UnboundedSender<Notify>) -> Self {
        Shared {
            screen: Screen::new(),
            metrics: Mutex::new(None),
            notify,
            redraw_pending: AtomicBool::new(false),
            applied: Mutex::new(None),
            present: Mutex::new(PresentStats {
                samples: Samples::default(),
                since: Instant::now(),
            }),
        }
    }

    fn send(&self, n: Notify) {
        let _ = self.notify.unbounded_send(n);
    }

    fn redraw(&self) {
        if !self.redraw_pending.swap(true, Ordering::AcqRel) {
            self.send(Notify::Redraw);
        }
    }

    /// Called when a frame is prepared: samples update-applied → drawn.
    fn presented(&self) {
        self.redraw_pending.store(false, Ordering::Release);
        let Some(applied) = self.applied.lock().unwrap().take() else {
            return;
        };
        let mut p = self.present.lock().unwrap();
        p.samples.record(applied.elapsed());
        if p.since.elapsed() >= REPORT_INTERVAL {
            if let Some(s) = p.samples.summary() {
                tracing::info!("update applied→frame prepared: {s}");
            }
            p.samples.clear();
            p.since = Instant::now();
        }
    }
}

impl session::Handler for Shared {
    fn output(&self, out: Output) {
        match out {
            Output::Rect { rect, pixels } => self.screen.upload(rect, &pixels),
            Output::Event(e) => match e {
                Rfb::ServerInit(init) => self.screen.resize(init.width, init.height),
                Rfb::UpdateEnd => {
                    self.applied.lock().unwrap().get_or_insert_with(Instant::now);
                    self.redraw();
                }
                Rfb::Cursor {
                    hotspot_x,
                    hotspot_y,
                    width,
                    height,
                    pixels,
                    mask,
                } => {
                    self.screen
                        .set_cursor((hotspot_x, hotspot_y), width, height, &pixels, &mask);
                    self.redraw();
                }
                Rfb::DesktopSize { width, height } => self.screen.resize(width, height),
                Rfb::ExtendedDesktopSize {
                    status,
                    width,
                    height,
                    ..
                } => {
                    if status == 0 {
                        self.screen.resize(width, height);
                    }
                    self.send(Notify::ServerSize {
                        width,
                        height,
                        status,
                    });
                }
                Rfb::DesktopName(name) => self.send(Notify::DesktopName(name)),
                _ => {}
            },
            Output::Error(e) => tracing::warn!(%e, "decode error"),
        }
    }

    fn closed(&self, result: Result<(), SessionError>) {
        self.send(Notify::Closed(result.err().map(|e| e.to_string())));
    }
}

/// The shader widget program.
pub struct SessionView {
    pub shared: Arc<Shared>,
    pub writer: Writer,
}

#[derive(Default)]
pub struct InputState {
    buttons: u16,
    /// Scroll in pixels not yet sent as wheel clicks.
    scroll: (f32, f32),
    /// Keysym sent for each held key, so release matches press.
    keys: HashMap<Physical, u32>,
}

/// Pixels of smooth scrolling per wheel click.
const SCROLL_STEP: f32 = 40.0;

const LEFT: u16 = 1 << 0;
const MIDDLE: u16 = 1 << 1;
const RIGHT: u16 = 1 << 2;
const WHEEL_UP: u16 = 1 << 3;
const WHEEL_DOWN: u16 = 1 << 4;
const WHEEL_LEFT: u16 = 1 << 5;
const WHEEL_RIGHT: u16 = 1 << 6;
const BACK: u16 = 1 << 7;
const FORWARD: u16 = 1 << 8;

impl SessionView {
    /// Maps a logical position in the widget to a framebuffer pixel.
    fn to_fb(&self, p: Point) -> Option<(u16, u16)> {
        let m = (*self.shared.metrics.lock().unwrap())?;
        let fb = self.shared.screen.size();
        let fb = (u32::from(fb.0), u32::from(fb.1));
        let place = Placement::new(fb, m.physical);
        Some(place.to_fb((p.x * m.scale, p.y * m.scale), fb))
    }

    fn pointer(&self, state: &InputState, p: Point) {
        if let Some((x, y)) = self.to_fb(p)
            && let Err(e) = self.writer.pointer(state.buttons, x, y)
        {
            tracing::debug!(%e, "pointer send failed");
        }
    }

    /// Wheel clicks: press and release of a wheel button each.
    fn wheel(&self, buttons: u16, p: Point, wheel: u16, clicks: u32) {
        let Some((x, y)) = self.to_fb(p) else { return };
        for _ in 0..clicks {
            let _ = self.writer.pointer(buttons | wheel, x, y);
            let _ = self.writer.pointer(buttons, x, y);
        }
    }

    fn key(&self, state: &mut InputState, event: &keyboard::Event) {
        let r = match event {
            keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                location,
                repeat,
                ..
            } => {
                // The remote compositor runs its own key repeat.
                if *repeat {
                    return;
                }
                let mut sym = keymap::keysym(modified_key, *location);
                if sym == 0 {
                    sym = keymap::keysym(key, *location);
                }
                state.keys.insert(*physical_key, sym);
                self.writer.key(true, sym, keymap::qnum(physical_key))
            }
            keyboard::Event::KeyReleased {
                key,
                physical_key,
                location,
                ..
            } => {
                let sym = state
                    .keys
                    .remove(physical_key)
                    .unwrap_or_else(|| keymap::keysym(key, *location));
                self.writer.key(false, sym, keymap::qnum(physical_key))
            }
            keyboard::Event::ModifiersChanged(_) => return,
        };
        if let Err(e) = r {
            tracing::debug!(%e, "key send failed");
        }
    }

    /// Releases held keys and buttons, so nothing sticks when focus leaves.
    fn release_all(&self, state: &mut InputState) {
        for (physical, sym) in state.keys.drain() {
            let _ = self.writer.key(false, sym, keymap::qnum(&physical));
        }
        if state.buttons != 0 {
            state.buttons = 0;
            let _ = self.writer.pointer(0, 0, 0);
        }
    }
}

impl<Message> shader::Program<Message> for SessionView {
    type State = InputState;
    type Primitive = Primitive;

    fn update(
        &self,
        state: &mut InputState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<shader::Action<Message>> {
        // While a button is held, keep tracking outside the widget.
        let pos = if state.buttons & (LEFT | MIDDLE | RIGHT | BACK | FORWARD) != 0 {
            cursor.position_from(bounds.position())
        } else {
            cursor.position_in(bounds)
        };
        match event {
            Event::Mouse(e) => {
                let Some(p) = pos else {
                    // Left the view: redraw to hide the local cursor.
                    return matches!(e, mouse::Event::CursorMoved { .. })
                        .then(shader::Action::request_redraw);
                };
                match e {
                    mouse::Event::CursorMoved { .. } => self.pointer(state, p),
                    mouse::Event::ButtonPressed(b) | mouse::Event::ButtonReleased(b) => {
                        let bit = match b {
                            mouse::Button::Left => LEFT,
                            mouse::Button::Middle => MIDDLE,
                            mouse::Button::Right => RIGHT,
                            mouse::Button::Back => BACK,
                            mouse::Button::Forward => FORWARD,
                            mouse::Button::Other(_) => return None,
                        };
                        if matches!(e, mouse::Event::ButtonPressed(_)) {
                            state.buttons |= bit;
                        } else {
                            state.buttons &= !bit;
                        }
                        self.pointer(state, p);
                    }
                    mouse::Event::WheelScrolled { delta } => {
                        let (dx, dy) = match *delta {
                            ScrollDelta::Lines { x, y } => (x * SCROLL_STEP, y * SCROLL_STEP),
                            ScrollDelta::Pixels { x, y } => (x, y),
                        };
                        state.scroll.0 += dx;
                        state.scroll.1 += dy;
                        // Positive deltas move content right/down, which is
                        // wheel left/up.
                        let buttons = state.buttons;
                        let flush = |acc: &mut f32, pos: u16, neg: u16| {
                            let clicks = (acc.abs() / SCROLL_STEP) as u32;
                            if clicks > 0 {
                                let b = if *acc > 0.0 { pos } else { neg };
                                self.wheel(buttons, p, b, clicks);
                                *acc -= acc.signum() * clicks as f32 * SCROLL_STEP;
                            }
                        };
                        let (mut sx, mut sy) = state.scroll;
                        flush(&mut sy, WHEEL_UP, WHEEL_DOWN);
                        flush(&mut sx, WHEEL_LEFT, WHEEL_RIGHT);
                        state.scroll = (sx, sy);
                    }
                    _ => return None,
                }
                Some(shader::Action::request_redraw().and_capture())
            }
            Event::Keyboard(e) => {
                self.key(state, e);
                Some(shader::Action::capture())
            }
            Event::Window(window::Event::Unfocused) => {
                self.release_all(state);
                None
            }
            _ => None,
        }
    }

    fn draw(&self, _state: &InputState, cursor: mouse::Cursor, bounds: Rectangle) -> Primitive {
        Primitive {
            shared: self.shared.clone(),
            writer: self.writer.clone(),
            pointer: cursor.position_in(bounds),
        }
    }

    fn mouse_interaction(
        &self,
        _state: &InputState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Hidden
        } else {
            mouse::Interaction::default()
        }
    }
}

pub struct Primitive {
    shared: Arc<Shared>,
    writer: Writer,
    /// Local pointer, logical, relative to the widget.
    pointer: Option<Point>,
}

impl std::fmt::Debug for Primitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Primitive")
            .field("pointer", &self.pointer)
            .finish_non_exhaustive()
    }
}

pub struct Pipeline {
    renderer: Renderer,
    gpu: Gpu,
    format: wgpu::TextureFormat,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        tracing::debug!(?format, "creating render pipeline");
        Pipeline {
            renderer: Renderer::new(device, format),
            gpu: Gpu {
                device: device.clone(),
                queue: queue.clone(),
            },
            format,
        }
    }
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let shared = &*self.shared;
        let scale = viewport.scale_factor();
        let m = Metrics {
            physical: (
                (bounds.width * scale).round() as u32,
                (bounds.height * scale).round() as u32,
            ),
            scale,
        };
        let changed = {
            let mut cur = shared.metrics.lock().unwrap();
            let changed = *cur != Some(m);
            *cur = Some(m);
            changed
        };
        if changed {
            shared.send(Notify::ViewChanged);
        }
        if shared.screen.attach(&pipeline.gpu, pipeline.format) {
            tracing::debug!("updates arrived before the GPU; requesting a full update");
            let _ = self.writer.refresh();
        }
        shared.presented();
        let view = View {
            size: m.physical,
            pointer: self.pointer.map(|p| (p.x * scale, p.y * scale)),
        };
        pipeline.renderer.prepare(device, queue, &shared.screen, view);
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        pipeline.renderer.draw(pass);
        true
    }
}
