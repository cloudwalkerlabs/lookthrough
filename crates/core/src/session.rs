//! A live session over TCP. This is the IO shell around [`Connection`] and
//! [`Pipeline`]; the desktop app and the Android bindings share it.
//!
//! - One blocking reader thread reads the socket, parses, and feeds the
//!   pipeline (`research.md` §7).
//! - Protocol housekeeping runs in the pipeline's ordered apply step, so it
//!   sees events in wire order: Fence replies (sent only after the preceding
//!   updates are applied), ContinuousUpdates, and update requests when the
//!   server lacks ContinuousUpdates.
//! - [`Writer`] sends input from any thread, straight to the socket, with one
//!   `write` per message.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;

use crate::client_msg::{self, fence};
use crate::pipeline::{self, Output, Pipeline, Sink};
use crate::stats::Samples;
use crate::{Connection, Event, PixelFormat, ProtocolError, Screen, encoding};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("network: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// JPEG quality 0-9; `None` for lossless tiles.
    pub quality: Option<u8>,
    pub pipeline: pipeline::Options,
}

/// Receives a session's output.
pub trait Handler: Send + Sync + 'static {
    /// Decoded rects and events, in wire order. Called from the reader
    /// thread or a decode worker, never concurrently.
    fn output(&self, out: Output);
    /// The session ended. Called once, from the reader thread, after all
    /// output was delivered. `Ok` means the server closed the connection or
    /// the [`Session`] was dropped.
    fn closed(&self, result: Result<(), SessionError>);
}

impl<H: Handler> Handler for Arc<H> {
    fn output(&self, out: Output) {
        (**self).output(out)
    }
    fn closed(&self, result: Result<(), SessionError>) {
        (**self).closed(result)
    }
}

/// How often the update latency summary is logged.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

pub struct Session {
    writer: Writer,
    reader: Option<JoinHandle<()>>,
}

impl Session {
    pub fn connect(
        addr: impl ToSocketAddrs,
        opts: Options,
        handler: impl Handler,
    ) -> Result<Session, SessionError> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let read = stream.try_clone()?;
        let writer = Writer {
            shared: Arc::new(Shared {
                stream: Mutex::new(stream),
                state: Mutex::default(),
                qemu_keys: AtomicBool::new(false),
                ext_buttons: AtomicBool::new(false),
                buttons: AtomicU16::new(0),
                closing: AtomicBool::new(false),
                input_mark: Mutex::new(None),
                update_ends: Mutex::default(),
            }),
        };
        let shared = writer.shared.clone();
        let reader = std::thread::Builder::new()
            .name("rfb-reader".into())
            .spawn(move || {
                let handler = Arc::new(handler);
                let control = Control {
                    shared: shared.clone(),
                    handler: handler.clone(),
                    stats: Mutex::default(),
                };
                let pipeline = Pipeline::new(control, opts.pipeline.clone());
                let result = read_loop(read, &shared, pipeline, opts.quality);
                let result = match result {
                    Err(SessionError::Io(_)) if shared.closing.load(Ordering::Acquire) => Ok(()),
                    r => r,
                };
                handler.closed(result);
            })?;
        Ok(Session {
            writer,
            reader: Some(reader),
        })
    }

    pub fn writer(&self) -> &Writer {
        &self.writer
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.writer.shared.closing.store(true, Ordering::Release);
        let _ = self
            .writer
            .shared
            .stream
            .lock()
            .unwrap()
            .shutdown(Shutdown::Both);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

struct Shared {
    stream: Mutex<TcpStream>,
    state: Mutex<State>,
    /// The server confirmed QEMU extended key events.
    qemu_keys: AtomicBool,
    /// The server confirmed extended mouse buttons.
    ext_buttons: AtomicBool,
    /// Last button mask sent, to spot presses.
    buttons: AtomicU16,
    closing: AtomicBool,
    /// When the oldest input not yet followed by an update was sent.
    input_mark: Mutex<Option<Instant>>,
    /// Parse times of UpdateEnds not yet applied, oldest first.
    update_ends: Mutex<VecDeque<Instant>>,
}

#[derive(Default)]
struct State {
    width: u16,
    height: u16,
    /// The server's screen, from its first ExtendedDesktopSize.
    screen: Option<Screen>,
    /// The server announced ContinuousUpdates support.
    continuous: bool,
}

impl Shared {
    fn send(&self, msg: &[u8]) -> io::Result<()> {
        self.stream.lock().unwrap().write_all(msg)
    }

    fn mark_input(&self) {
        self.input_mark
            .lock()
            .unwrap()
            .get_or_insert_with(Instant::now);
    }
}

/// Sends client messages. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Writer {
    shared: Arc<Shared>,
}

impl Writer {
    /// Sends one complete client message.
    pub fn send(&self, msg: &[u8]) -> io::Result<()> {
        self.shared.send(msg)
    }

    /// A key press or release. `qnum` is the QEMU (XT-based) scancode, or 0
    /// when unknown; it is used when the server supports QEMU key events.
    /// Events with neither a keysym nor a usable scancode are dropped.
    pub fn key(&self, down: bool, keysym: u32, qnum: u32) -> io::Result<()> {
        if down {
            self.shared.mark_input();
        }
        if qnum != 0 && self.shared.qemu_keys.load(Ordering::Relaxed) {
            self.send(&client_msg::qemu_key_event(down, keysym, qnum))
        } else if keysym != 0 {
            self.send(&client_msg::key_event(down, keysym))
        } else {
            Ok(())
        }
    }

    /// Pointer position in framebuffer pixels, with buttons as a mask: bit
    /// `n` is button `n + 1` (0 left, 1 middle, 2 right, 3-6 wheel, 7 back,
    /// 8 forward). Buttons past 7 are dropped unless the server supports
    /// extended mouse buttons.
    pub fn pointer(&self, buttons: u16, x: u16, y: u16) -> io::Result<()> {
        let prev = self.shared.buttons.swap(buttons, Ordering::Relaxed);
        if buttons & !prev != 0 {
            self.shared.mark_input();
        }
        if self.shared.ext_buttons.load(Ordering::Relaxed) {
            let lo = (buttons & 0x7f) as u8;
            let hi = (buttons >> 7) as u8;
            self.send(&client_msg::ext_pointer_event(lo, hi, x, y))
        } else {
            self.send(&client_msg::pointer_event(buttons as u8, x, y))
        }
    }

    /// Asks the server to resize its screen. Returns `Ok(false)` without
    /// sending if the server hasn't sent its ExtendedDesktopSize yet; the
    /// caller should retry when it arrives.
    pub fn set_desktop_size(&self, width: u16, height: u16) -> io::Result<bool> {
        let Some(mut screen) = self.shared.state.lock().unwrap().screen else {
            return Ok(false);
        };
        screen.rect = crate::Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        self.send(&client_msg::set_desktop_size(width, height, &[screen]))?;
        Ok(true)
    }

    /// Requests a full (non-incremental) update.
    pub fn refresh(&self) -> io::Result<()> {
        let (w, h) = {
            let s = self.shared.state.lock().unwrap();
            (s.width, s.height)
        };
        self.send(&client_msg::framebuffer_update_request(false, 0, 0, w, h))
    }
}

fn read_loop(
    mut read: TcpStream,
    shared: &Shared,
    mut pipeline: Pipeline,
    quality: Option<u8>,
) -> Result<(), SessionError> {
    let mut conn = Connection::new();
    let mut buf = BytesMut::with_capacity(1 << 20);
    let mut chunk = vec![0u8; 256 << 10];
    loop {
        let n = read.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Some(event) = conn.poll(&mut buf)? {
            if let Some(out) = conn.take_transmit() {
                shared.send(&out)?;
            }
            match &event {
                Event::ServerInit(init) => {
                    tracing::info!(init.width, init.height, name = init.name, "connected");
                    {
                        let mut s = shared.state.lock().unwrap();
                        s.width = init.width;
                        s.height = init.height;
                    }
                    shared.send(&conn.set_pixel_format(PixelFormat::RGBX8888))?;
                    shared.send(&client_msg::set_encodings(&encodings(quality)))?;
                    shared.send(&client_msg::framebuffer_update_request(
                        false,
                        0,
                        0,
                        init.width,
                        init.height,
                    ))?;
                }
                Event::UpdateEnd => shared.update_ends.lock().unwrap().push_back(Instant::now()),
                _ => {}
            }
            pipeline.submit(event);
        }
        if let Some(out) = conn.take_transmit() {
            shared.send(&out)?;
        }
    }
}

/// SetEncodings list. Order matters: Neat VNC uses the first of
/// Raw/Tight/ZRLE it finds, so Tight goes first. ZRLE is left out until it
/// decodes.
fn encodings(quality: Option<u8>) -> Vec<i32> {
    let mut e = vec![
        encoding::TIGHT,
        encoding::RAW,
        encoding::CURSOR,
        encoding::EXTENDED_DESKTOP_SIZE,
        encoding::DESKTOP_SIZE,
        encoding::DESKTOP_NAME,
        encoding::LAST_RECT,
        encoding::CONTINUOUS_UPDATES,
        encoding::FENCE,
        encoding::QEMU_EXT_KEY_EVENT,
        encoding::EXT_MOUSE_BUTTONS,
    ];
    if let Some(q) = quality {
        e.push(encoding::jpeg_quality(q));
    }
    e
}

/// The pipeline sink: protocol housekeeping in wire order, then the
/// handler.
struct Control<H> {
    shared: Arc<Shared>,
    handler: Arc<H>,
    stats: Mutex<Stats>,
}

struct Stats {
    update_begin: Option<Instant>,
    pixel_rects: u32,
    /// UpdateBegin applied to UpdateEnd applied.
    update: Samples,
    /// UpdateEnd parsed to UpdateEnd applied: the decode work left once the
    /// last byte arrived.
    tail: Samples,
    /// Input (key or button press) sent to the next update applied.
    input: Samples,
    last_report: Instant,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            update_begin: None,
            pixel_rects: 0,
            update: Samples::default(),
            tail: Samples::default(),
            input: Samples::default(),
            last_report: Instant::now(),
        }
    }
}

impl<H: Handler> Sink for Control<H> {
    fn apply(&self, out: Output) {
        if let Err(e) = self.housekeeping(&out) {
            // The reader sees the same broken socket and ends the session.
            tracing::debug!(%e, "send failed");
        }
        if matches!(out, Output::Event(Event::Fence { flags, .. }) if flags & fence::REQUEST != 0) {
            return;
        }
        self.handler.output(out);
    }
}

impl<H: Handler> Control<H> {
    fn housekeeping(&self, out: &Output) -> io::Result<()> {
        let event = match out {
            Output::Rect { .. } => {
                self.stats.lock().unwrap().pixel_rects += 1;
                return Ok(());
            }
            Output::Error(_) => return Ok(()),
            Output::Event(e) => e,
        };
        let shared = &*self.shared;
        match event {
            Event::Fence { flags, payload } if flags & fence::REQUEST != 0 => {
                // Every update before this fence has been applied, which
                // satisfies BlockBefore. Neat VNC uses these replies to
                // measure bandwidth and stops sending while they're
                // outstanding.
                shared.send(&client_msg::fence(flags & fence::SUPPORTED, payload))?;
            }
            Event::EndOfContinuousUpdates => {
                let mut s = shared.state.lock().unwrap();
                if !s.continuous {
                    s.continuous = true;
                    tracing::debug!("enabling continuous updates");
                    shared.send(&client_msg::enable_continuous_updates(
                        true, 0, 0, s.width, s.height,
                    ))?;
                }
            }
            Event::UpdateBegin { .. } => {
                let mut st = self.stats.lock().unwrap();
                st.update_begin = Some(Instant::now());
                st.pixel_rects = 0;
            }
            Event::UpdateEnd => {
                self.update_end();
                let s = shared.state.lock().unwrap();
                if !s.continuous {
                    shared.send(&client_msg::framebuffer_update_request(
                        true, 0, 0, s.width, s.height,
                    ))?;
                }
            }
            Event::DesktopSize { width, height } => self.resized(*width, *height, None)?,
            Event::ExtendedDesktopSize {
                status: 0,
                width,
                height,
                screens,
                ..
            } => self.resized(*width, *height, screens.first().copied())?,
            Event::Supports(encoding::QEMU_EXT_KEY_EVENT) => {
                shared.qemu_keys.store(true, Ordering::Relaxed);
            }
            Event::Supports(encoding::EXT_MOUSE_BUTTONS) => {
                shared.ext_buttons.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
        Ok(())
    }

    fn resized(&self, width: u16, height: u16, screen: Option<Screen>) -> io::Result<()> {
        let mut s = self.shared.state.lock().unwrap();
        if screen.is_some() {
            s.screen = screen;
        }
        if (s.width, s.height) == (width, height) {
            return Ok(());
        }
        s.width = width;
        s.height = height;
        if s.continuous {
            // The continuous region is fixed when enabled; widen it.
            self.shared
                .send(&client_msg::enable_continuous_updates(true, 0, 0, width, height))?;
        }
        Ok(())
    }

    fn update_end(&self) {
        let now = Instant::now();
        let parsed = self.shared.update_ends.lock().unwrap().pop_front();
        let mut st = self.stats.lock().unwrap();
        let Some(begin) = st.update_begin.take() else {
            return;
        };
        if st.pixel_rects == 0 {
            return;
        }
        let update = now - begin;
        st.update.record(update);
        if let Some(p) = parsed {
            st.tail.record(now - p);
        }
        if let Some(input) = self.shared.input_mark.lock().unwrap().take() {
            st.input.record(now - input);
        }
        tracing::trace!(
            rects = st.pixel_rects,
            update_ms = update.as_secs_f64() * 1e3,
            "update applied"
        );
        if now - st.last_report >= REPORT_INTERVAL {
            st.last_report = now;
            if let Some(s) = st.update.summary() {
                tracing::info!("update begin→applied: {s}");
            }
            if let Some(s) = st.tail.summary() {
                tracing::info!("update last byte→applied: {s}");
            }
            if let Some(s) = st.input.summary() {
                tracing::info!("input→update applied: {s}");
            }
            st.update.clear();
            st.tail.clear();
            st.input.clear();
        }
    }
}
