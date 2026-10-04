//! Client-to-server message encoders. Each returns one complete message, so
//! the caller can send it with a single `write` (no split messages under
//! `TCP_NODELAY`).

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
}
