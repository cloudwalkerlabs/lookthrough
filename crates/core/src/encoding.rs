//! Encoding and pseudo-encoding numbers used in SetEncodings and rectangle
//! headers.

pub const RAW: i32 = 0;
pub const TIGHT: i32 = 7;
pub const ZRLE: i32 = 16;

pub const CURSOR: i32 = -239;
pub const DESKTOP_SIZE: i32 = -223;
pub const LAST_RECT: i32 = -224;
pub const QEMU_EXT_KEY_EVENT: i32 = -258;
pub const QEMU_LED_STATE: i32 = -261;
pub const DESKTOP_NAME: i32 = -307;
pub const EXTENDED_DESKTOP_SIZE: i32 = -308;
pub const FENCE: i32 = -312;
pub const CONTINUOUS_UPDATES: i32 = -313;
pub const EXT_MOUSE_BUTTONS: i32 = -316;
pub const VMWARE_LED_STATE: i32 = 0x574d_5668;

/// JPEG quality pseudo-encoding for quality `q` in `0..=9`. Neat VNC sends
/// JPEG tiles only when one of these is present.
pub const fn jpeg_quality(q: u8) -> i32 {
    assert!(q <= 9);
    -32 + q as i32
}
