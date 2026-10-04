//! Keyboard mapping: iced key events to X keysyms and QEMU scancodes.
//!
//! The scancode table is generated from Neat VNC's `qnum-to-evdev.c`
//! (keymap-gen), inverted through the Linux key names that winit maps each
//! physical key code to. Neat VNC turns the qnum back into evdev.

use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{Key, Location};

/// QEMU (XT-based) scancode for a physical key, or 0 when unknown.
pub fn qnum(key: &Physical) -> u32 {
    use Code as C;
    let Physical::Code(code) = key else { return 0 };
    match code {
        C::Backquote => 0x29,
        C::Backslash => 0x2b,
        C::BracketLeft => 0x1a,
        C::BracketRight => 0x1b,
        C::Comma => 0x33,
        C::Digit0 => 0x0b,
        C::Digit1 => 0x02,
        C::Digit2 => 0x03,
        C::Digit3 => 0x04,
        C::Digit4 => 0x05,
        C::Digit5 => 0x06,
        C::Digit6 => 0x07,
        C::Digit7 => 0x08,
        C::Digit8 => 0x09,
        C::Digit9 => 0x0a,
        C::Equal => 0x0d,
        C::IntlBackslash => 0x56,
        C::IntlRo => 0x73,
        C::IntlYen => 0x7d,
        C::KeyA => 0x1e,
        C::KeyB => 0x30,
        C::KeyC => 0x2e,
        C::KeyD => 0x20,
        C::KeyE => 0x12,
        C::KeyF => 0x21,
        C::KeyG => 0x22,
        C::KeyH => 0x23,
        C::KeyI => 0x17,
        C::KeyJ => 0x24,
        C::KeyK => 0x25,
        C::KeyL => 0x26,
        C::KeyM => 0x32,
        C::KeyN => 0x31,
        C::KeyO => 0x18,
        C::KeyP => 0x19,
        C::KeyQ => 0x10,
        C::KeyR => 0x13,
        C::KeyS => 0x1f,
        C::KeyT => 0x14,
        C::KeyU => 0x16,
        C::KeyV => 0x2f,
        C::KeyW => 0x11,
        C::KeyX => 0x2d,
        C::KeyY => 0x15,
        C::KeyZ => 0x2c,
        C::Minus => 0x0c,
        C::Period => 0x34,
        C::Quote => 0x28,
        C::Semicolon => 0x27,
        C::Slash => 0x35,
        C::AltLeft => 0x38,
        C::AltRight => 0xb8,
        C::Backspace => 0x0e,
        C::CapsLock => 0x3a,
        C::ContextMenu => 0xdd,
        C::ControlLeft => 0x1d,
        C::ControlRight => 0x9d,
        C::Enter => 0x1c,
        C::SuperLeft => 0xdb,
        C::SuperRight => 0xdc,
        C::ShiftLeft => 0x2a,
        C::ShiftRight => 0x36,
        C::Space => 0x39,
        C::Tab => 0x0f,
        C::Convert => 0x79,
        C::KanaMode => 0x70,
        C::Lang1 => 0x72,
        C::Lang2 => 0x71,
        C::Lang3 => 0x78,
        C::Lang4 => 0x77,
        C::Lang5 => 0x76,
        C::NonConvert => 0x7b,
        C::Delete => 0xd3,
        C::End => 0xcf,
        C::Help => 0xf5,
        C::Home => 0xc7,
        C::Insert => 0xd2,
        C::PageDown => 0xd1,
        C::PageUp => 0xc9,
        C::ArrowDown => 0xd0,
        C::ArrowLeft => 0xcb,
        C::ArrowRight => 0xcd,
        C::ArrowUp => 0xc8,
        C::NumLock => 0x45,
        C::Numpad0 => 0x52,
        C::Numpad1 => 0x4f,
        C::Numpad2 => 0x50,
        C::Numpad3 => 0x51,
        C::Numpad4 => 0x4b,
        C::Numpad5 => 0x4c,
        C::Numpad6 => 0x4d,
        C::Numpad7 => 0x47,
        C::Numpad8 => 0x48,
        C::Numpad9 => 0x49,
        C::NumpadAdd => 0x4e,
        C::NumpadComma => 0x7e,
        C::NumpadDecimal => 0x53,
        C::NumpadDivide => 0xb5,
        C::NumpadEnter => 0x9c,
        C::NumpadEqual => 0x59,
        C::NumpadMultiply => 0x37,
        C::NumpadParenLeft => 0xf6,
        C::NumpadParenRight => 0xfb,
        C::NumpadSubtract => 0x4a,
        C::Escape => 0x01,
        C::PrintScreen => 0x54,
        C::ScrollLock => 0x46,
        C::Pause => 0xc6,
        C::BrowserBack => 0xea,
        C::BrowserFavorites => 0xe6,
        C::BrowserForward => 0xe9,
        C::BrowserHome => 0xb2,
        C::BrowserRefresh => 0xe7,
        C::BrowserSearch => 0xe5,
        C::BrowserStop => 0xe8,
        C::Eject => 0x6c,
        C::LaunchApp1 => 0xeb,
        C::LaunchApp2 => 0xa1,
        C::LaunchMail => 0xec,
        C::MediaPlayPause => 0xa2,
        C::MediaSelect => 0xed,
        C::MediaStop => 0xa4,
        C::MediaTrackNext => 0x99,
        C::MediaTrackPrevious => 0x90,
        C::Power => 0xde,
        C::Sleep => 0xdf,
        C::AudioVolumeDown => 0xae,
        C::AudioVolumeMute => 0xa0,
        C::AudioVolumeUp => 0xb0,
        C::WakeUp => 0xe3,
        C::Again => 0x85,
        C::Copy => 0xf8,
        C::Cut => 0xbc,
        C::Find => 0xc1,
        C::Open => 0x64,
        C::Paste => 0x65,
        C::Props => 0x86,
        C::Undo => 0x87,
        C::F1 => 0x3b,
        C::F2 => 0x3c,
        C::F3 => 0x3d,
        C::F4 => 0x3e,
        C::F5 => 0x3f,
        C::F6 => 0x40,
        C::F7 => 0x41,
        C::F8 => 0x42,
        C::F9 => 0x43,
        C::F10 => 0x44,
        C::F11 => 0x57,
        C::F12 => 0x58,
        C::F13 => 0x5d,
        C::F14 => 0x5e,
        C::F15 => 0x5f,
        C::F16 => 0x55,
        C::F17 => 0x83,
        C::F18 => 0xf7,
        C::F19 => 0x84,
        C::F20 => 0x5a,
        C::F21 => 0x74,
        C::F22 => 0xf9,
        C::F23 => 0x6d,
        C::F24 => 0x6f,
        _ => 0,
    }
}

/// X keysym for a logical key, or 0 when unknown.
pub fn keysym(key: &Key, location: Location) -> u32 {
    match key {
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => char_keysym(c, location),
                _ => 0,
            }
        }
        Key::Named(n) => named_keysym(*n, location),
        Key::Unidentified => 0,
    }
}

fn char_keysym(c: char, location: Location) -> u32 {
    if location == Location::Numpad {
        let kp = match c {
            '0'..='9' => 0xffb0 + (c as u32 - '0' as u32),
            '*' => 0xffaa,
            '+' => 0xffab,
            ',' => 0xffac,
            '-' => 0xffad,
            '.' => 0xffae,
            '/' => 0xffaf,
            '=' => 0xffbd,
            _ => 0,
        };
        if kp != 0 {
            return kp;
        }
    }
    let cp = c as u32;
    match cp {
        // Latin-1 keysyms equal their code points.
        0x20..=0x7e | 0xa0..=0xff => cp,
        0..=0x1f | 0x7f..=0x9f => 0,
        _ => 0x0100_0000 | cp,
    }
}

fn named_keysym(n: Named, location: Location) -> u32 {
    let right = location == Location::Right;
    let pick = |l: u32, r: u32| if right { r } else { l };
    match n {
        Named::Alt => pick(0xffe9, 0xffea),
        Named::AltGraph => 0xfe03,
        Named::CapsLock => 0xffe5,
        Named::Control => pick(0xffe3, 0xffe4),
        Named::Meta => pick(0xffe7, 0xffe8),
        Named::NumLock => 0xff7f,
        Named::ScrollLock => 0xff14,
        Named::Shift => pick(0xffe1, 0xffe2),
        Named::Super => pick(0xffeb, 0xffec),
        Named::Hyper => pick(0xffed, 0xffee),
        Named::Enter if location == Location::Numpad => 0xff8d,
        Named::Enter => 0xff0d,
        Named::Tab => 0xff09,
        Named::Space => 0x20,
        Named::ArrowDown => 0xff54,
        Named::ArrowLeft => 0xff51,
        Named::ArrowRight => 0xff53,
        Named::ArrowUp => 0xff52,
        Named::End => 0xff57,
        Named::Home => 0xff50,
        Named::PageDown => 0xff56,
        Named::PageUp => 0xff55,
        Named::Backspace => 0xff08,
        Named::Clear => 0xff0b,
        Named::Delete => 0xffff,
        Named::Insert => 0xff63,
        Named::Redo => 0xff66,
        Named::Undo => 0xff65,
        Named::ContextMenu => 0xff67,
        Named::Escape => 0xff1b,
        Named::Find => 0xff68,
        Named::Help => 0xff6a,
        Named::Pause => 0xff13,
        Named::PrintScreen => 0xff61,
        Named::Cancel => 0xff69,
        Named::Execute => 0xff62,
        Named::Select => 0xff60,
        Named::F1 => 0xffbe,
        Named::F2 => 0xffbf,
        Named::F3 => 0xffc0,
        Named::F4 => 0xffc1,
        Named::F5 => 0xffc2,
        Named::F6 => 0xffc3,
        Named::F7 => 0xffc4,
        Named::F8 => 0xffc5,
        Named::F9 => 0xffc6,
        Named::F10 => 0xffc7,
        Named::F11 => 0xffc8,
        Named::F12 => 0xffc9,
        Named::F13 => 0xffca,
        Named::F14 => 0xffcb,
        Named::F15 => 0xffcc,
        Named::F16 => 0xffcd,
        Named::F17 => 0xffce,
        Named::F18 => 0xffcf,
        Named::F19 => 0xffd0,
        Named::F20 => 0xffd1,
        Named::F21 => 0xffd2,
        Named::F22 => 0xffd3,
        Named::F23 => 0xffd4,
        Named::F24 => 0xffd5,
        Named::AudioVolumeDown => 0x1008ff11,
        Named::AudioVolumeMute => 0x1008ff12,
        Named::AudioVolumeUp => 0x1008ff13,
        Named::MediaPlayPause => 0x1008ff14,
        Named::MediaStop => 0x1008ff15,
        Named::MediaTrackPrevious => 0x1008ff16,
        Named::MediaTrackNext => 0x1008ff17,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes() {
        assert_eq!(qnum(&Physical::Code(Code::KeyA)), 0x1e);
        assert_eq!(qnum(&Physical::Code(Code::Home)), 0xc7);
        assert_eq!(qnum(&Physical::Code(Code::SuperLeft)), 0xdb);
        assert_eq!(qnum(&Physical::Code(Code::NumpadEnter)), 0x9c);
    }

    #[test]
    fn keysyms() {
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(keysym(&ch("a"), Location::Standard), 0x61);
        assert_eq!(keysym(&ch("é"), Location::Standard), 0xe9);
        assert_eq!(keysym(&ch("€"), Location::Standard), 0x0100_20ac);
        assert_eq!(keysym(&ch("7"), Location::Numpad), 0xffb7);
        assert_eq!(keysym(&Key::Named(Named::Shift), Location::Right), 0xffe2);
        assert_eq!(keysym(&Key::Named(Named::F5), Location::Standard), 0xffc2);
    }
}
