//! Sans-IO RFB client: bytes in, events out.
//!
//! The caller owns the socket. It appends received bytes to a [`BytesMut`]
//! and calls [`Connection::poll`] until it returns `Ok(None)`. Handshake
//! replies are queued and taken with [`Connection::take_transmit`].
//! Rectangle payloads are zero-copy slices of the receive buffer.

use bytes::{Buf, Bytes, BytesMut};

use crate::Rect;
use crate::client_msg;
use crate::encoding;
use crate::error::{ProtocolError, ProtocolErrorKind};
use crate::pixel_format::PixelFormat;
use crate::tight::{self, Tight};
use crate::wire::{Parse, Reader, Stop};

const MAX_TEXT_LEN: u32 = 64 << 20;
const SECURITY_NONE: u8 = 1;

#[derive(Debug, Clone)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub pixel_format: PixelFormat,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub id: u32,
    pub rect: Rect,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub enum RectData {
    Raw(Bytes),
    Tight(Tight),
    /// ZRLE zlib data; not decoded yet.
    Zrle(Bytes),
}

#[derive(Debug, Clone)]
pub enum Event {
    ServerInit(ServerInit),
    /// A FramebufferUpdate starts. `rects` is `None` when it is terminated
    /// by LastRect instead of a count.
    UpdateBegin {
        rects: Option<u16>,
    },
    Rect {
        rect: Rect,
        data: RectData,
    },
    UpdateEnd,
    Cursor {
        hotspot_x: u16,
        hotspot_y: u16,
        width: u16,
        height: u16,
        /// `width * height` pixels in the client pixel format.
        pixels: Bytes,
        /// 1 bit per pixel, rows padded to a byte, MSB first.
        mask: Bytes,
    },
    DesktopSize {
        width: u16,
        height: u16,
    },
    ExtendedDesktopSize {
        /// 0 = server, 1 = this client, 2 = another client.
        reason: u16,
        /// 0 = success; otherwise the reason SetDesktopSize failed.
        status: u16,
        width: u16,
        height: u16,
        screens: Vec<Screen>,
    },
    DesktopName(String),
    /// The server confirms a pseudo-encoding that has no payload (for
    /// example QEMU extended key events, extended mouse buttons).
    Supports(i32),
    LedState(u32),
    Bell,
    /// ServerCutText. `extended` means the ExtendedClipboard format.
    CutText {
        extended: bool,
        data: Bytes,
    },
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: Bytes,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Version,
    SecurityTypes,
    SecurityResult,
    ServerInit,
    Messages,
    /// Inside a FramebufferUpdate. `None` means until LastRect.
    Update(Option<u16>),
    UpdateDone,
}

pub struct Connection {
    state: State,
    pixel_format: PixelFormat,
    /// Bytes consumed from the server stream so far.
    offset: u64,
    transmit: Vec<u8>,
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

/// What a parse step produced, with payload ranges still relative to the
/// receive buffer.
enum Parsed {
    Event(Event),
    Rect(Rect, RectRanges),
    Cursor {
        hotspot: (u16, u16),
        size: (u16, u16),
        pixels: std::ops::Range<usize>,
        mask: std::ops::Range<usize>,
    },
    CutText {
        extended: bool,
        data: std::ops::Range<usize>,
    },
    Fence {
        flags: u32,
        payload: std::ops::Range<usize>,
    },
    /// Consumed with nothing to report.
    Skip,
}

enum RectRanges {
    Raw(std::ops::Range<usize>),
    Tight(tight::Ranges),
    Zrle(std::ops::Range<usize>),
}

impl Connection {
    pub fn new() -> Self {
        Connection {
            state: State::Version,
            pixel_format: PixelFormat::RGBX8888,
            offset: 0,
            transmit: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !matches!(
            self.state,
            State::Version | State::SecurityTypes | State::SecurityResult | State::ServerInit
        )
    }

    pub fn pixel_format(&self) -> &PixelFormat {
        &self.pixel_format
    }

    /// Bytes the protocol needs sent to the server (handshake replies).
    pub fn take_transmit(&mut self) -> Option<Vec<u8>> {
        (!self.transmit.is_empty()).then(|| std::mem::take(&mut self.transmit))
    }

    /// Builds SetPixelFormat and switches the parser to `pf`. Send it before
    /// any FramebufferUpdateRequest, so no update arrives in the old format.
    pub fn set_pixel_format(&mut self, pf: PixelFormat) -> [u8; 20] {
        self.pixel_format = pf;
        client_msg::set_pixel_format(&pf)
    }

    /// Parses the next event from `buf`, consuming its bytes. Returns
    /// `Ok(None)` when `buf` doesn't hold a complete event yet.
    pub fn poll(&mut self, buf: &mut BytesMut) -> Result<Option<Event>, ProtocolError> {
        loop {
            if self.state == State::UpdateDone {
                self.state = State::Messages;
                return Ok(Some(Event::UpdateEnd));
            }
            let mut r = Reader::new(buf);
            let parsed = match self.step(&mut r) {
                Ok(p) => p,
                Err(Stop::Incomplete) => return Ok(None),
                Err(Stop::Invalid(kind)) => {
                    return Err(ProtocolError {
                        context: self.context(buf),
                        offset: self.offset + r.pos() as u64,
                        kind,
                    });
                }
            };
            let len = r.pos();
            self.offset += len as u64;
            let event = match parsed {
                Parsed::Skip => {
                    buf.advance(len);
                    continue;
                }
                Parsed::Event(e) => {
                    buf.advance(len);
                    e
                }
                other => {
                    let msg = buf.split_to(len).freeze();
                    resolve(other, &msg)
                }
            };
            return Ok(Some(event));
        }
    }

    fn context(&self, buf: &[u8]) -> &'static str {
        match self.state {
            State::Version => "ProtocolVersion",
            State::SecurityTypes => "security types",
            State::SecurityResult => "SecurityResult",
            State::ServerInit => "ServerInit",
            State::Update(_) | State::UpdateDone => {
                let enc = buf
                    .get(8..12)
                    .map(|b| i32::from_be_bytes(b.try_into().unwrap()));
                match enc {
                    Some(encoding::TIGHT) => "FramebufferUpdate/Tight",
                    Some(encoding::RAW) => "FramebufferUpdate/Raw",
                    Some(encoding::ZRLE) => "FramebufferUpdate/ZRLE",
                    Some(encoding::CURSOR) => "FramebufferUpdate/Cursor",
                    Some(encoding::EXTENDED_DESKTOP_SIZE) => {
                        "FramebufferUpdate/ExtendedDesktopSize"
                    }
                    _ => "FramebufferUpdate",
                }
            }
            State::Messages => match buf.first() {
                Some(0) => "FramebufferUpdate",
                Some(1) => "SetColourMapEntries",
                Some(3) => "ServerCutText",
                Some(248) => "Fence",
                _ => "server message",
            },
        }
    }

    fn step(&mut self, r: &mut Reader) -> Parse<Parsed> {
        match self.state {
            State::Version => {
                let v = r.array::<12>()?;
                let (major, minor) = parse_version(&v).ok_or_else(|| {
                    ProtocolErrorKind::UnsupportedVersion(String::from_utf8_lossy(&v).into_owned())
                })?;
                if major != 3 || minor < 8 {
                    return Err(
                        ProtocolErrorKind::UnsupportedVersion(format!("{major}.{minor}")).into(),
                    );
                }
                self.transmit.extend_from_slice(b"RFB 003.008\n");
                self.state = State::SecurityTypes;
                Ok(Parsed::Skip)
            }
            State::SecurityTypes => {
                let n = r.u8()?;
                if n == 0 {
                    let reason = reason_string(r)?;
                    return Err(ProtocolErrorKind::ConnectionRefused(reason).into());
                }
                let types = r.bytes(usize::from(n))?;
                if !types.contains(&SECURITY_NONE) {
                    return Err(ProtocolErrorKind::NoSupportedSecurityType(types.to_vec()).into());
                }
                self.transmit.push(SECURITY_NONE);
                self.state = State::SecurityResult;
                Ok(Parsed::Skip)
            }
            State::SecurityResult => {
                if r.u32()? != 0 {
                    let reason = reason_string(r)?;
                    return Err(ProtocolErrorKind::SecurityFailed(reason).into());
                }
                self.transmit.push(1); // ClientInit: shared
                self.state = State::ServerInit;
                Ok(Parsed::Skip)
            }
            State::ServerInit => {
                let width = r.u16()?;
                let height = r.u16()?;
                let pixel_format = PixelFormat::from_wire(&r.array()?);
                let name = text(r)?;
                self.state = State::Messages;
                Ok(Parsed::Event(Event::ServerInit(ServerInit {
                    width,
                    height,
                    pixel_format,
                    name,
                })))
            }
            State::Messages => self.message(r),
            State::Update(remaining) => self.rect(r, remaining),
            State::UpdateDone => unreachable!(),
        }
    }

    fn message(&mut self, r: &mut Reader) -> Parse<Parsed> {
        match r.u8()? {
            0 => {
                r.u8()?;
                let n = r.u16()?;
                let rects = (n != u16::MAX).then_some(n);
                self.state = if rects == Some(0) {
                    State::UpdateDone
                } else {
                    State::Update(rects)
                };
                Ok(Parsed::Event(Event::UpdateBegin { rects }))
            }
            1 => {
                r.u8()?;
                r.u16()?;
                let n = r.u16()?;
                r.take(usize::from(n) * 6)?;
                tracing::debug!("ignoring SetColourMapEntries");
                Ok(Parsed::Skip)
            }
            2 => Ok(Parsed::Event(Event::Bell)),
            3 => {
                r.take(3)?;
                let len = r.i32()?;
                let extended = len < 0;
                let len = len.unsigned_abs();
                if len > MAX_TEXT_LEN {
                    return Err(ProtocolErrorKind::TooLarge(len.into()).into());
                }
                let data = r.take(len as usize)?;
                Ok(Parsed::CutText { extended, data })
            }
            150 => Ok(Parsed::Event(Event::EndOfContinuousUpdates)),
            248 => {
                r.take(3)?;
                let flags = r.u32()?;
                let len = r.u8()?;
                let payload = r.take(usize::from(len))?;
                Ok(Parsed::Fence { flags, payload })
            }
            t => Err(ProtocolErrorKind::UnknownMessage(t).into()),
        }
    }

    fn rect(&mut self, r: &mut Reader, remaining: Option<u16>) -> Parse<Parsed> {
        let rect = Rect {
            x: r.u16()?,
            y: r.u16()?,
            w: r.u16()?,
            h: r.u16()?,
        };
        let enc = r.i32()?;
        let (w, h) = (usize::from(rect.w), usize::from(rect.h));
        let bpp = self.pixel_format.bytes_per_pixel();

        let mut last = false;
        let parsed = match enc {
            encoding::RAW | encoding::TIGHT | encoding::ZRLE => {
                if rect.x.checked_add(rect.w).is_none() || rect.y.checked_add(rect.h).is_none() {
                    return Err(ProtocolErrorKind::BadRect(rect).into());
                }
                let data = match enc {
                    encoding::RAW => RectRanges::Raw(r.take(w * h * bpp)?),
                    encoding::TIGHT => RectRanges::Tight(tight::parse(
                        r,
                        rect,
                        self.pixel_format.tight_pixel_size(),
                    )?),
                    _ => {
                        let len = r.u32()?;
                        RectRanges::Zrle(r.take(len as usize)?)
                    }
                };
                Parsed::Rect(rect, data)
            }
            encoding::CURSOR => {
                let pixels = r.take(w * h * bpp)?;
                let mask = r.take(w.div_ceil(8) * h)?;
                Parsed::Cursor {
                    hotspot: (rect.x, rect.y),
                    size: (rect.w, rect.h),
                    pixels,
                    mask,
                }
            }
            encoding::DESKTOP_SIZE => Parsed::Event(Event::DesktopSize {
                width: rect.w,
                height: rect.h,
            }),
            encoding::EXTENDED_DESKTOP_SIZE => {
                let n = r.u8()?;
                r.take(3)?;
                let mut screens = Vec::with_capacity(n.into());
                for _ in 0..n {
                    screens.push(Screen {
                        id: r.u32()?,
                        rect: Rect {
                            x: r.u16()?,
                            y: r.u16()?,
                            w: r.u16()?,
                            h: r.u16()?,
                        },
                        flags: r.u32()?,
                    });
                }
                Parsed::Event(Event::ExtendedDesktopSize {
                    reason: rect.x,
                    status: rect.y,
                    width: rect.w,
                    height: rect.h,
                    screens,
                })
            }
            encoding::DESKTOP_NAME => Parsed::Event(Event::DesktopName(text(r)?)),
            encoding::LAST_RECT => {
                last = true;
                Parsed::Skip
            }
            encoding::QEMU_LED_STATE => Parsed::Event(Event::LedState(r.u8()?.into())),
            encoding::VMWARE_LED_STATE => Parsed::Event(Event::LedState(r.u32()?)),
            encoding::QEMU_EXT_KEY_EVENT | encoding::EXT_MOUSE_BUTTONS => {
                Parsed::Event(Event::Supports(enc))
            }
            _ => return Err(ProtocolErrorKind::UnsupportedEncoding(enc).into()),
        };

        // Only advance the state once the whole rect has parsed.
        self.state = match remaining {
            _ if last => State::UpdateDone,
            Some(1) => State::UpdateDone,
            Some(n) => State::Update(Some(n - 1)),
            None => State::Update(None),
        };
        Ok(parsed)
    }
}

fn resolve(p: Parsed, msg: &Bytes) -> Event {
    match p {
        Parsed::Rect(rect, data) => Event::Rect {
            rect,
            data: match data {
                RectRanges::Raw(r) => RectData::Raw(msg.slice(r)),
                RectRanges::Tight(t) => RectData::Tight(t.resolve(msg)),
                RectRanges::Zrle(r) => RectData::Zrle(msg.slice(r)),
            },
        },
        Parsed::Cursor {
            hotspot,
            size,
            pixels,
            mask,
        } => Event::Cursor {
            hotspot_x: hotspot.0,
            hotspot_y: hotspot.1,
            width: size.0,
            height: size.1,
            pixels: msg.slice(pixels),
            mask: msg.slice(mask),
        },
        Parsed::CutText { extended, data } => Event::CutText {
            extended,
            data: msg.slice(data),
        },
        Parsed::Fence { flags, payload } => Event::Fence {
            flags,
            payload: msg.slice(payload),
        },
        Parsed::Event(_) | Parsed::Skip => unreachable!(),
    }
}

fn parse_version(v: &[u8; 12]) -> Option<(u32, u32)> {
    let s = std::str::from_utf8(v).ok()?;
    let rest = s.strip_prefix("RFB ")?.strip_suffix('\n')?;
    let (major, minor) = rest.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn text(r: &mut Reader) -> Parse<String> {
    let len = r.u32()?;
    if len > MAX_TEXT_LEN {
        return Err(ProtocolErrorKind::TooLarge(len.into()).into());
    }
    Ok(String::from_utf8_lossy(r.bytes(len as usize)?).into_owned())
}

fn reason_string(r: &mut Reader) -> Parse<String> {
    text(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_INIT: &[u8] = &[
        0x07, 0x80, 0x04, 0x38, // 1920x1080
        32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0, // pixel format
        0, 0, 0, 4, b'h', b'e', b'a', b'd',
    ];

    fn handshake_bytes() -> Vec<u8> {
        let mut s = b"RFB 003.008\n".to_vec();
        s.extend([1, 1]); // one security type: None
        s.extend([0, 0, 0, 0]); // SecurityResult OK
        s.extend(SERVER_INIT);
        s
    }

    /// Feeds `data` one byte at a time, collecting events.
    fn feed_bytewise(c: &mut Connection, data: &[u8]) -> Vec<Event> {
        let mut buf = BytesMut::new();
        let mut events = Vec::new();
        for &b in data {
            buf.extend_from_slice(&[b]);
            while let Some(e) = c.poll(&mut buf).unwrap() {
                events.push(e);
            }
        }
        assert!(buf.is_empty());
        events
    }

    #[test]
    fn handshake() {
        let mut c = Connection::new();
        let events = feed_bytewise(&mut c, &handshake_bytes());
        assert_eq!(c.take_transmit().unwrap(), b"RFB 003.008\n\x01\x01");
        let [Event::ServerInit(init)] = &events[..] else {
            panic!("{events:?}")
        };
        assert_eq!(
            (init.width, init.height, init.name.as_str()),
            (1920, 1080, "head")
        );
        assert_eq!(init.pixel_format.red_shift, 16);
        assert!(c.is_ready());
    }

    #[test]
    fn refuses_without_security_none() {
        let mut c = Connection::new();
        let mut buf = BytesMut::from(&b"RFB 003.008\n\x01\x02"[..]);
        let e = c.poll(&mut buf).unwrap_err();
        assert!(matches!(e.kind, ProtocolErrorKind::NoSupportedSecurityType(ref t) if t == &[2]));
        assert_eq!(e.offset, 14);
    }

    #[test]
    fn update_with_raw_cursor_and_desktop_size() {
        let mut c = Connection::new();
        feed_bytewise(&mut c, &handshake_bytes());
        c.set_pixel_format(PixelFormat::RGBX8888);

        let mut s = vec![0, 0, 0, 4];
        // Raw 2x1
        s.extend([0, 1, 0, 2, 0, 2, 0, 1, 0, 0, 0, 0]);
        s.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        // Cursor 1x1, hotspot (0,0)
        s.extend([0, 0, 0, 0, 0, 1, 0, 1, 0xff, 0xff, 0xff, 0x11]);
        s.extend([9, 9, 9, 0, 0x80]);
        // ExtendedDesktopSize, one screen
        s.extend([0, 1, 0, 0, 0x07, 0x80, 0x04, 0x38, 0xff, 0xff, 0xfe, 0xcc]);
        s.extend([1, 0, 0, 0]);
        s.extend([0, 0, 0, 42, 0, 0, 0, 0, 0x07, 0x80, 0x04, 0x38, 0, 0, 0, 0]);
        // DesktopSize
        s.extend([0, 0, 0, 0, 0, 10, 0, 20, 0xff, 0xff, 0xff, 0x21]);
        s.push(2); // Bell

        let events = feed_bytewise(&mut c, &s);
        assert!(matches!(events[0], Event::UpdateBegin { rects: Some(4) }));
        let Event::Rect {
            rect,
            data: RectData::Raw(px),
        } = &events[1]
        else {
            panic!()
        };
        assert_eq!(
            *rect,
            Rect {
                x: 1,
                y: 2,
                w: 2,
                h: 1
            }
        );
        assert_eq!(&px[..], [1, 2, 3, 0, 4, 5, 6, 0]);
        assert!(
            matches!(&events[2], Event::Cursor { width: 1, height: 1, mask, .. } if mask[..] == [0x80])
        );
        let Event::ExtendedDesktopSize {
            reason, screens, ..
        } = &events[3]
        else {
            panic!()
        };
        assert_eq!((*reason, screens[0].id), (1, 42));
        assert!(matches!(
            events[4],
            Event::DesktopSize {
                width: 10,
                height: 20
            }
        ));
        assert!(matches!(events[5], Event::UpdateEnd));
        assert!(matches!(events[6], Event::Bell));
        assert_eq!(events.len(), 7);
    }

    #[test]
    fn last_rect_terminates_update() {
        let mut c = Connection::new();
        feed_bytewise(&mut c, &handshake_bytes());
        let mut s = vec![0, 0, 0xff, 0xff];
        s.extend([0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x20]); // LastRect
        let events = feed_bytewise(&mut c, &s);
        assert!(matches!(
            events[..],
            [Event::UpdateBegin { rects: None }, Event::UpdateEnd]
        ));
    }

    #[test]
    fn unknown_encoding_reports_offset_and_context() {
        let mut c = Connection::new();
        let hs = handshake_bytes();
        feed_bytewise(&mut c, &hs);
        let mut buf = BytesMut::from(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 5][..]);
        c.poll(&mut buf).unwrap();
        let e = c.poll(&mut buf).unwrap_err();
        assert!(matches!(e.kind, ProtocolErrorKind::UnsupportedEncoding(5)));
        assert_eq!(e.context, "FramebufferUpdate");
        assert_eq!(e.offset, hs.len() as u64 + 4 + 12);
    }
}
