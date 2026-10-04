//! lookthrough core: the RFB protocol state machine and rectangle decoders.
//! No UI, no GPU. [`Connection`] is sans-IO; [`session`] is the thin
//! blocking-TCP shell around it.

pub mod client_msg;
pub mod connection;
pub mod encoding;
pub mod error;
pub mod pipeline;
pub mod pixel_format;
pub mod session;
pub mod stats;
pub mod tight;
mod wire;

pub use connection::{Connection, Event, RectData, Screen, ServerInit};
pub use error::{DecodeError, Error, ProtocolError};
pub use pixel_format::PixelFormat;

/// A rectangle in framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}
