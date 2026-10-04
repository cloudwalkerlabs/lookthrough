//! lookthrough desktop: an iced window showing one live session.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use iced::futures::channel::mpsc::{self, UnboundedReceiver};
use iced::futures::stream::{self, StreamExt};
use iced::widget::{center, shader, stack, text};
use iced::{Element, Fill, Subscription, Task};
use lookthrough_core::session::{self, Session};
use lookthrough_render::desktop_size;

mod keymap;
mod view;

use view::{Notify, SessionView, Shared};

#[derive(Parser, Debug, Clone)]
#[command(about)]
struct Args {
    #[arg(default_value = "127.0.0.1:5901")]
    addr: String,
    /// JPEG quality 0-9; omit for lossless (basic zlib) tiles.
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(0..=9))]
    quality: Option<u8>,
    /// Don't resize the server to the window.
    #[arg(long)]
    no_resize: bool,
}

/// Wait this long after the last window resize before asking the server to
/// resize; each request costs a compositor mode set (`research.md` §6).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// ExtendedDesktopSize status Neat VNC sends when it hands a
/// SetDesktopSize to the compositor (`RFB_RESIZE_STATUS_REQUEST_FORWARDED`).
const RESIZE_FORWARDED: u16 = 4;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()),
        )
        .init();
    let args = Args::parse();
    iced::application(move || App::new(args.clone()), App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .window_size((1280.0, 800.0))
        .run()
}

#[derive(Debug, Clone)]
enum Message {
    Notify(Notify),
    ResizeTick,
}

struct App {
    args: Args,
    shared: Arc<Shared>,
    session: Option<Session>,
    wake: Wake,
    name: String,
    status: Status,
    resize: Resize,
}

enum Status {
    Connected,
    Failed(String),
    Closed(Option<String>),
}

#[derive(Default)]
struct Resize {
    /// The size the window wants, and when to ask for it.
    pending: Option<((u16, u16), Instant)>,
    /// The last size asked for.
    requested: Option<(u16, u16)>,
}

impl App {
    fn new(args: Args) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let shared = Arc::new(Shared::new(tx));
        let opts = session::Options {
            quality: args.quality,
            ..Default::default()
        };
        let (session, status) = match Session::connect(&args.addr, opts, shared.clone()) {
            Ok(s) => (Some(s), Status::Connected),
            Err(e) => (None, Status::Failed(e.to_string())),
        };
        App {
            name: args.addr.clone(),
            args,
            shared,
            session,
            wake: Wake(Arc::new(Mutex::new(Some(rx)))),
            status,
            resize: Resize::default(),
        }
    }

    fn title(&self) -> String {
        format!("{} — lookthrough", self.name)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Notify(n) => match n {
                Notify::Redraw => {}
                Notify::ViewChanged => self.view_changed(),
                Notify::ServerSize {
                    width,
                    height,
                    status,
                } => {
                    match status {
                        0 => tracing::info!(width, height, "server size"),
                        // Neat VNC: accepted and passed to the compositor;
                        // the new size follows in another update.
                        RESIZE_FORWARDED => tracing::debug!("resize request forwarded"),
                        _ => tracing::warn!(width, height, status, "server refused resize"),
                    }
                    // The first ExtendedDesktopSize unlocks SetDesktopSize.
                    self.view_changed();
                    self.try_resize();
                }
                Notify::DesktopName(name) => self.name = name,
                Notify::Closed(err) => {
                    if let Some(e) = &err {
                        tracing::error!("session ended: {e}");
                    } else {
                        tracing::info!("session ended");
                    }
                    self.session = None;
                    self.status = Status::Closed(err);
                }
            },
            Message::ResizeTick => self.try_resize(),
        }
        Task::none()
    }

    /// Schedules a SetDesktopSize for the view's physical size.
    fn view_changed(&mut self) {
        if self.args.no_resize {
            return;
        }
        let Some(m) = *self.shared.metrics.lock().unwrap() else {
            return;
        };
        let want = desktop_size(m.physical, m.scale);
        let current = self.shared.screen.size();
        let target = self.resize.pending.map(|p| p.0).or(self.resize.requested);
        if target == Some(want) || (target.is_none() && current == want) {
            return;
        }
        // Resize at once the first time, so the session starts at the
        // right size; debounce later window drags.
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
        let Some(session) = &self.session else { return };
        if Instant::now() < at {
            return;
        }
        match session.writer().set_desktop_size(w, h) {
            Ok(true) => {
                tracing::info!(w, h, "requested server resize");
                self.resize.requested = Some((w, h));
                self.resize.pending = None;
            }
            // No ExtendedDesktopSize from the server yet; retried when it
            // arrives.
            Ok(false) => {}
            Err(e) => tracing::warn!(%e, "resize send failed"),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let mut layers = stack![];
        if let Some(s) = &self.session {
            layers = layers.push(
                shader(SessionView {
                    shared: self.shared.clone(),
                    writer: s.writer().clone(),
                })
                .width(Fill)
                .height(Fill),
            );
        }
        let message = match &self.status {
            Status::Connected => None,
            Status::Failed(e) => Some(format!("Couldn't connect to {}: {e}", self.args.addr)),
            Status::Closed(None) => Some("The server closed the session.".to_owned()),
            Status::Closed(Some(e)) => Some(format!("Session ended: {e}")),
        };
        if let Some(m) = message {
            layers = layers.push(center(text(m)));
        }
        layers.width(Fill).height(Fill).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let wake = Subscription::run_with(self.wake.clone(), |w| {
            let rx = w.0.lock().unwrap().take();
            stream::iter(rx).flatten().map(Message::Notify)
        });
        let tick = if self.resize.pending.is_some() {
            iced::time::every(Duration::from_millis(50)).map(|_| Message::ResizeTick)
        } else {
            Subscription::none()
        };
        Subscription::batch([wake, tick])
    }
}

/// The session's notification channel, as subscription data. Hashes by
/// identity, so the subscription is created once.
#[derive(Clone)]
struct Wake(Arc<Mutex<Option<UnboundedReceiver<Notify>>>>);

impl Hash for Wake {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}
