//! One session as the Android shell drives it: the core [`Session`], the
//! render thread, and input from the UI thread.
//!
//! Input calls arrive over JNI on the Android UI thread and are written
//! straight to the socket on that thread, with no hop (`research.md` §8).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::Sender;
use lookthrough_core::Event as Rfb;
use lookthrough_core::pipeline::Output;
use lookthrough_core::session::{self, Session, SessionError, Writer};
use lookthrough_render::{Placement, Screen};

use crate::render::{self, Msg};
use crate::{SessionListener, keymap};

/// State shared by the session handler, the input calls and the render
/// thread.
pub(crate) struct Shared {
    pub screen: Screen,
    pub view: Mutex<ViewState>,
    listener: Arc<dyn SessionListener>,
    render: Sender<Msg>,
    redraw_pending: AtomicBool,
    /// When the oldest update not yet presented was applied.
    pub applied: Mutex<Option<Instant>>,
}

/// The session view, as the render thread and input see it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewState {
    /// Surface size in physical pixels; `None` without a surface.
    pub size: Option<(u32, u32)>,
    /// Android display density (physical pixels per dp).
    pub density: f32,
    /// The local pointer in surface pixels, or `None` when it left the view.
    pub pointer: Option<(f32, f32)>,
}

impl Shared {
    pub(crate) fn redraw(&self) {
        if !self.redraw_pending.swap(true, Ordering::AcqRel) {
            let _ = self.render.send(Msg::Redraw);
        }
    }

    /// Called by the render thread before it draws.
    pub(crate) fn redraw_started(&self) {
        self.redraw_pending.store(false, Ordering::Release);
    }
}

impl session::Handler for Shared {
    fn output(&self, out: Output) {
        match out {
            Output::Rect { rect, pixels } => self.screen.upload(rect, &pixels),
            Output::Event(e) => match e {
                Rfb::ServerInit(init) => {
                    self.screen.resize(init.width, init.height);
                    self.listener.on_desktop_name(init.name);
                }
                Rfb::UpdateEnd => {
                    self.applied
                        .lock()
                        .unwrap()
                        .get_or_insert_with(Instant::now);
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
                    let _ = self.render.send(Msg::ServerSize {
                        width,
                        height,
                        status,
                    });
                }
                Rfb::DesktopName(name) => self.listener.on_desktop_name(name),
                _ => {}
            },
            Output::Error(e) => tracing::warn!(%e, "decode error"),
        }
    }

    fn closed(&self, result: Result<(), SessionError>) {
        let error = result.err().map(|e| e.to_string());
        match &error {
            Some(e) => tracing::error!("session ended: {e}"),
            None => tracing::info!("session ended"),
        }
        self.listener.on_closed(error);
    }
}

pub(crate) struct Client {
    pub shared: Arc<Shared>,
    writer: Writer,
    session: Mutex<Option<Session>>,
    render: Mutex<Option<render::Thread>>,
    input: Mutex<Input>,
}

#[derive(Default)]
struct Input {
    buttons: u16,
    /// Keysym and scancode sent for each held key code, so release matches
    /// press.
    keys: HashMap<i32, (u32, u32)>,
}

const LEFT: u16 = 1 << 0;
const MIDDLE: u16 = 1 << 1;
const RIGHT: u16 = 1 << 2;
const WHEEL_UP: u16 = 1 << 3;
const WHEEL_DOWN: u16 = 1 << 4;
const WHEEL_LEFT: u16 = 1 << 5;
const WHEEL_RIGHT: u16 = 1 << 6;
const BACK: u16 = 1 << 7;
const FORWARD: u16 = 1 << 8;

/// RFB button mask from `MotionEvent.getButtonState()`.
fn buttons(android: i32) -> u16 {
    [
        (1, LEFT),     // BUTTON_PRIMARY
        (2, RIGHT),    // BUTTON_SECONDARY
        (4, MIDDLE),   // BUTTON_TERTIARY
        (8, BACK),     // BUTTON_BACK
        (16, FORWARD), // BUTTON_FORWARD
    ]
    .into_iter()
    .filter(|(a, _)| android & a != 0)
    .fold(0, |m, (_, b)| m | b)
}

impl Client {
    pub(crate) fn connect(
        addr: &str,
        opts: session::Options,
        resize: bool,
        listener: Arc<dyn SessionListener>,
    ) -> Result<Client, SessionError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            screen: Screen::new(),
            view: Mutex::default(),
            listener,
            render: tx.clone(),
            redraw_pending: AtomicBool::new(false),
            applied: Mutex::new(None),
        });
        let session = Session::connect(addr, opts, shared.clone())?;
        let writer = session.writer().clone();
        let render = render::Thread::spawn(tx, rx, shared.clone(), writer.clone(), resize)?;
        Ok(Client {
            shared,
            writer,
            session: Mutex::new(Some(session)),
            render: Mutex::new(Some(render)),
            input: Mutex::default(),
        })
    }

    /// Ends the session and stops rendering. Idempotent.
    pub(crate) fn close(&self) {
        // Dropping the session joins its reader, which reports `closed`.
        drop(self.session.lock().unwrap().take());
        drop(self.render.lock().unwrap().take());
    }

    /// Sets or clears the surface. Returns once the render thread stopped
    /// using the previous one, as `surfaceDestroyed` requires.
    pub(crate) fn set_surface(&self, window: Option<render::Window>, density: f32) {
        {
            let mut v = self.shared.view.lock().unwrap();
            v.size = window.as_ref().map(|w| w.size);
            v.density = density;
        }
        let (ack, done) = crossbeam_channel::bounded(1);
        if self.shared.render.send(Msg::Surface(window, ack)).is_ok() {
            // Errors when the render thread has quit, which also means it
            // let go of the surface.
            let _ = done.recv();
        }
    }

    /// Maps a surface position to a framebuffer pixel.
    fn to_fb(&self, x: f32, y: f32) -> Option<(u16, u16)> {
        let size = self.shared.view.lock().unwrap().size?;
        let fb = self.shared.screen.size();
        let fb = (u32::from(fb.0), u32::from(fb.1));
        Some(Placement::new(fb, size).to_fb((x, y), fb))
    }

    /// Pointer motion or a button change; `x`, `y` in surface pixels.
    pub(crate) fn pointer(&self, android_buttons: i32, x: f32, y: f32) {
        let b = buttons(android_buttons);
        self.input.lock().unwrap().buttons = b;
        if let Some((fx, fy)) = self.to_fb(x, y)
            && let Err(e) = self.writer.pointer(b, fx, fy)
        {
            tracing::debug!(%e, "pointer send failed");
        }
        self.shared.view.lock().unwrap().pointer = Some((x, y));
        self.shared.redraw();
    }

    /// The pointer left the view: hide the local cursor.
    pub(crate) fn pointer_left(&self) {
        self.shared.view.lock().unwrap().pointer = None;
        self.shared.redraw();
    }

    /// Wheel clicks: positive `dy` scrolls up, positive `dx` right, as
    /// Android's AXIS_VSCROLL and AXIS_HSCROLL.
    pub(crate) fn wheel(&self, x: f32, y: f32, dx: i32, dy: i32) {
        let Some((fx, fy)) = self.to_fb(x, y) else {
            return;
        };
        let held = self.input.lock().unwrap().buttons;
        let click = |wheel: u16, n: i32| {
            for _ in 0..n.unsigned_abs() {
                let _ = self.writer.pointer(held | wheel, fx, fy);
                let _ = self.writer.pointer(held, fx, fy);
            }
        };
        click(if dy > 0 { WHEEL_UP } else { WHEEL_DOWN }, dy);
        click(if dx > 0 { WHEEL_RIGHT } else { WHEEL_LEFT }, dx);
    }

    /// A key press or release. Key repeats should be filtered out by the
    /// caller; the remote compositor runs its own repeat.
    pub(crate) fn key(&self, down: bool, key_code: i32, unicode: u32, scan_code: i32) {
        let (sym, qnum) = {
            let mut input = self.input.lock().unwrap();
            if down {
                let k = (keymap::keysym(key_code, unicode), keymap::qnum(scan_code));
                input.keys.insert(key_code, k);
                k
            } else {
                input
                    .keys
                    .remove(&key_code)
                    .unwrap_or_else(|| (keymap::keysym(key_code, unicode), keymap::qnum(scan_code)))
            }
        };
        if let Err(e) = self.writer.key(down, sym, qnum) {
            tracing::debug!(%e, "key send failed");
        }
    }

    /// Releases held keys and buttons, so nothing sticks when focus leaves.
    pub(crate) fn release_all(&self) {
        let mut input = self.input.lock().unwrap();
        for (_, (sym, qnum)) in input.keys.drain() {
            let _ = self.writer.key(false, sym, qnum);
        }
        if input.buttons != 0 {
            input.buttons = 0;
            let _ = self.writer.pointer(0, 0, 0);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_buttons() {
        assert_eq!(buttons(0), 0);
        assert_eq!(buttons(1), LEFT);
        assert_eq!(buttons(2), RIGHT);
        assert_eq!(buttons(4), MIDDLE);
        assert_eq!(buttons(1 | 2 | 16), LEFT | RIGHT | FORWARD);
        // BUTTON_STYLUS_PRIMARY and others are ignored.
        assert_eq!(buttons(32 | 64), 0);
    }
}
