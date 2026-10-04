//! Client-to-server message encoders. Each returns one complete message, so
//! the caller can send it with a single `write` (no split messages under
//! `TCP_NODELAY`).

use crate::Screen;
use crate::pixel_format::PixelFormat;

pub fn set_pixel_format(pf: &PixelFormat) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[0] = 0;
    m[4..20].copy_from_slice(&pf.to_wire());
    m
}

pub fn set_encodings(encodings: &[i32]) -> Vec<u8> {
    let n = u16::try_from(encodings.len()).expect("too many encodings");
    let mut m = Vec::with_capacity(4 + 4 * encodings.len());
    m.extend_from_slice(&[2, 0]);
    m.extend_from_slice(&n.to_be_bytes());
    for e in encodings {
        m.extend_from_slice(&e.to_be_bytes());
    }
    m
}

pub fn framebuffer_update_request(incremental: bool, x: u16, y: u16, w: u16, h: u16) -> [u8; 10] {
    let mut m = [0u8; 10];
    m[0] = 3;
    m[1] = incremental.into();
    m[2..4].copy_from_slice(&x.to_be_bytes());
    m[4..6].copy_from_slice(&y.to_be_bytes());
    m[6..8].copy_from_slice(&w.to_be_bytes());
    m[8..10].copy_from_slice(&h.to_be_bytes());
    m
}

pub fn key_event(down: bool, keysym: u32) -> [u8; 8] {
    let mut m = [0u8; 8];
    m[0] = 4;
    m[1] = down.into();
    m[4..8].copy_from_slice(&keysym.to_be_bytes());
    m
}

pub fn pointer_event(button_mask: u8, x: u16, y: u16) -> [u8; 6] {
    let mut m = [0u8; 6];
    m[0] = 5;
    m[1] = button_mask;
    m[2..4].copy_from_slice(&x.to_be_bytes());
    m[4..6].copy_from_slice(&y.to_be_bytes());
    m
}

/// QEMU extended key event: keysym plus an XT/qnum scancode.
pub fn qemu_key_event(down: bool, keysym: u32, keycode: u32) -> [u8; 12] {
    let mut m = [0u8; 12];
    m[0] = 255;
    m[1] = 0;
    m[2..4].copy_from_slice(&u16::from(down).to_be_bytes());
    m[4..8].copy_from_slice(&keysym.to_be_bytes());
    m[8..12].copy_from_slice(&keycode.to_be_bytes());
    m
}

/// PointerEvent with the extended mouse buttons extension (-316). Buttons
/// 8 and up go in `ext_mask` (bit 0 = button 8). Only send this after the
/// server confirmed the extension; use [`pointer_event`] otherwise.
pub fn ext_pointer_event(button_mask: u8, ext_mask: u8, x: u16, y: u16) -> [u8; 7] {
    let mut m = [0u8; 7];
    m[0] = 5;
    m[1] = button_mask | 0x80;
    m[2..4].copy_from_slice(&x.to_be_bytes());
    m[4..6].copy_from_slice(&y.to_be_bytes());
    m[6] = ext_mask;
    m
}

pub fn enable_continuous_updates(enable: bool, x: u16, y: u16, w: u16, h: u16) -> [u8; 10] {
    let mut m = framebuffer_update_request(enable, x, y, w, h);
    m[0] = 150;
    m
}

/// Fence flags (`rfbproto` §7.5.4).
pub mod fence {
    pub const BLOCK_BEFORE: u32 = 1 << 0;
    pub const BLOCK_AFTER: u32 = 1 << 1;
    pub const SYNC_NEXT: u32 = 1 << 2;
    pub const REQUEST: u32 = 1 << 31;
    /// The flags this client understands.
    pub const SUPPORTED: u32 = BLOCK_BEFORE | BLOCK_AFTER | SYNC_NEXT;
}

/// Fence message. `payload` must be at most 64 bytes.
pub fn fence(flags: u32, payload: &[u8]) -> Vec<u8> {
    let len = u8::try_from(payload.len())
        .ok()
        .filter(|&n| n <= 64)
        .expect("fence payload over 64 bytes");
    let mut m = Vec::with_capacity(9 + payload.len());
    m.extend_from_slice(&[248, 0, 0, 0]);
    m.extend_from_slice(&flags.to_be_bytes());
    m.push(len);
    m.extend_from_slice(payload);
    m
}

/// SetDesktopSize (ExtendedDesktopSize extension).
pub fn set_desktop_size(width: u16, height: u16, screens: &[Screen]) -> Vec<u8> {
    let n = u8::try_from(screens.len()).expect("too many screens");
    let mut m = Vec::with_capacity(8 + 16 * screens.len());
    m.extend_from_slice(&[251, 0]);
    m.extend_from_slice(&width.to_be_bytes());
    m.extend_from_slice(&height.to_be_bytes());
    m.extend_from_slice(&[n, 0]);
    for s in screens {
        m.extend_from_slice(&s.id.to_be_bytes());
        for v in [s.rect.x, s.rect.y, s.rect.w, s.rect.h] {
            m.extend_from_slice(&v.to_be_bytes());
        }
        m.extend_from_slice(&s.flags.to_be_bytes());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_encodings_layout() {
        assert_eq!(
            set_encodings(&[7, -239]),
            [2, 0, 0, 2, 0, 0, 0, 7, 0xff, 0xff, 0xff, 0x11]
        );
    }

    #[test]
    fn update_request_layout() {
        assert_eq!(
            framebuffer_update_request(true, 1, 2, 1920, 1080),
            [3, 1, 0, 1, 0, 2, 0x07, 0x80, 0x04, 0x38]
        );
    }

    #[test]
    fn fence_layout() {
        assert_eq!(
            fence(fence::BLOCK_BEFORE, &[9, 8]),
            [248, 0, 0, 0, 0, 0, 0, 1, 2, 9, 8]
        );
    }

    #[test]
    fn set_desktop_size_layout() {
        let screen = Screen {
            id: 0x01020304,
            rect: crate::Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 480,
            },
            flags: 0,
        };
        let m = set_desktop_size(640, 480, &[screen]);
        assert_eq!(&m[..8], [251, 0, 2, 128, 1, 224, 1, 0]);
        assert_eq!(&m[8..], [1, 2, 3, 4, 0, 0, 0, 0, 2, 128, 1, 224, 0, 0, 0, 0]);
    }

    #[test]
    fn ext_pointer_layout() {
        assert_eq!(ext_pointer_event(1, 2, 3, 4), [5, 0x81, 0, 3, 0, 4, 2]);
    }
}
