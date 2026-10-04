//! Android bindings for lookthrough.
//!
//! - The control API (connect, close, session callbacks) is exported with
//!   uniffi; Kotlin calls it off the UI thread.
//! - The surface and input go through hand-written JNI functions
//!   (`android.rs`). Input is written to the socket on the calling UI
//!   thread with no hop (`research.md` §8), and a JNI call is cheaper than
//!   uniffi's JNA path. These functions find the session by [`Session::id`].

// Off Android only the uniffi half is built, for tests and binding
// generation; the JNI half is what uses the rest.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use lookthrough_core::session;

#[cfg(target_os = "android")]
mod android;
mod client;
mod keymap;
mod qnum;
mod render;

use client::Client;

uniffi::setup_scaffolding!();

/// Session callbacks. They arrive on Rust threads; post to the main thread
/// before touching UI, and don't block.
#[uniffi::export(with_foreign)]
pub trait SessionListener: Send + Sync {
    fn on_desktop_name(&self, name: String);
    /// The session ended; `error` is `None` when the server closed it or
    /// it was closed locally.
    fn on_closed(&self, error: Option<String>);
}

#[derive(Debug, uniffi::Record)]
pub struct SessionOptions {
    /// JPEG quality 0-9; `None` for lossless tiles.
    pub quality: Option<u8>,
    /// Resize the server's screen to the view (`research.md` §6).
    pub resize: bool,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ConnectError {
    #[error("{0}")]
    Failed(String),
}

/// A live session.
#[derive(uniffi::Object)]
pub struct Session {
    id: u64,
    client: Arc<Client>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static CLIENTS: LazyLock<Mutex<HashMap<u64, Weak<Client>>>> = LazyLock::new(Mutex::default);

/// The session with this id, if it is still open.
pub(crate) fn client(id: u64) -> Option<Arc<Client>> {
    CLIENTS.lock().unwrap().get(&id)?.upgrade()
}

#[uniffi::export]
impl Session {
    /// Connects and starts the session. Blocks while connecting, so call it
    /// off the UI thread.
    #[uniffi::constructor]
    pub fn connect(
        address: String,
        options: SessionOptions,
        listener: Arc<dyn SessionListener>,
    ) -> Result<Arc<Session>, ConnectError> {
        tracing::info!(address, ?options, "connecting");
        let opts = session::Options {
            quality: options.quality,
            ..Default::default()
        };
        let client = Client::connect(&address, opts, options.resize, listener)
            .map_err(|e| ConnectError::Failed(e.to_string()))?;
        let client = Arc::new(client);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        CLIENTS.lock().unwrap().insert(id, Arc::downgrade(&client));
        Ok(Arc::new(Session { id, client }))
    }

    /// The handle the JNI surface and input functions take.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Ends the session. Blocks until its threads stop; `on_closed` is
    /// called before this returns.
    pub fn close(&self) {
        CLIENTS.lock().unwrap().remove(&self.id);
        self.client.close();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        CLIENTS.lock().unwrap().remove(&self.id);
    }
}

/// Sets up logging, once. `filter` is a `tracing` env-filter directive
/// such as `"info,wgpu_core=warn"`.
#[uniffi::export]
pub fn init_logging(filter: String) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter = tracing_subscriber::EnvFilter::new(filter);
    let registry = tracing_subscriber::registry().with(filter);
    #[cfg(target_os = "android")]
    let registry = registry.with(paranoid_android::layer("lookthrough"));
    #[cfg(not(target_os = "android"))]
    let registry = registry.with(tracing_subscriber::fmt::layer());
    // Fails if already set, e.g. when the activity is recreated.
    let _ = registry.try_init();
}
