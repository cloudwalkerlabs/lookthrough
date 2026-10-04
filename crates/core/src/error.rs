use crate::Rect;

/// A violation of the protocol by the server, or a server feature we don't
/// support. The connection can't continue after one of these.
#[derive(Debug, thiserror::Error)]
#[error("{context} at stream offset {offset}: {kind}")]
pub struct ProtocolError {
    /// The message (and encoding, for rectangles) being parsed.
    pub context: &'static str,
    /// Byte offset in the server-to-client stream where parsing failed.
    pub offset: u64,
    pub kind: ProtocolErrorKind,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolErrorKind {
    #[error("unsupported protocol version {0:?}")]
    UnsupportedVersion(String),
    #[error("server refused connection: {0}")]
    ConnectionRefused(String),
    #[error("server doesn't offer security type None; it offers {0:?}")]
    NoSupportedSecurityType(Vec<u8>),
    #[error("security handshake failed: {0}")]
    SecurityFailed(String),
    #[error("unknown server message type {0}")]
    UnknownMessage(u8),
    #[error("unsupported encoding {0}")]
    UnsupportedEncoding(i32),
    #[error("invalid Tight control byte {0:#04x}")]
    InvalidTightControl(u8),
    #[error("invalid Tight filter {0}")]
    InvalidTightFilter(u8),
    #[error("length {0} exceeds limit")]
    TooLarge(u64),
    #[error("rectangle {0:?} overflows the framebuffer coordinate space")]
    BadRect(Rect),
}

/// A rectangle whose payload parsed but couldn't be decoded.
#[derive(Debug, thiserror::Error)]
#[error("decoding {encoding} rect {rect:?}: {kind}")]
pub struct DecodeError {
    pub encoding: &'static str,
    pub rect: Rect,
    pub kind: DecodeErrorKind,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeErrorKind {
    #[error("zlib: {0}")]
    Zlib(#[from] flate2::DecompressError),
    #[error("zlib stream produced {got} bytes, expected {expected}")]
    ZlibShort { got: usize, expected: usize },
    #[error("jpeg: {0}")]
    Jpeg(String),
    #[error("jpeg is {got:?}, expected {expected:?}")]
    JpegSize {
        got: (usize, usize),
        expected: (usize, usize),
    },
    #[error("palette index {0} out of range")]
    PaletteIndex(u8),
    #[error("{0} is not supported")]
    Unsupported(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
}
