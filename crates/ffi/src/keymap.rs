//! Keyboard mapping: Android `KeyEvent`s to X keysyms and QEMU scancodes
//! (`research.md` §8).

/// X keysym for an Android key. `unicode` is
/// `KeyEvent.getUnicodeChar(metaState)` with Ctrl and Meta masked out, or 0.
/// Returns 0 when unknown.
pub fn keysym(key_code: i32, unicode: u32) -> u32 {
    let named = named_keysym(key_code, unicode);
    if named != 0 {
        return named;
    }
    char::from_u32(unicode).map_or(0, char_keysym)
}

/// QEMU scancode from `KeyEvent.getScanCode()`, which is the Linux evdev
/// code on HID keyboards. Android calls it unreliable, so 0 (unknown) makes
/// the session fall back to the keysym alone.
pub fn qnum(scan_code: i32) -> u32 {
    u32::try_from(scan_code).map_or(0, crate::qnum::from_evdev)
}

fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    match cp {
        // Latin-1 keysyms equal their code points.
        0x20..=0x7e | 0xa0..=0xff => cp,
        0..=0x1f | 0x7f..=0x9f => 0,
        _ => 0x0100_0000 | cp,
    }
}

/// Keys whose keysym doesn't come from their character. Numpad keys give
/// keypad keysyms: digits while Num Lock produces them, navigation
/// otherwise.
fn named_keysym(key_code: i32, unicode: u32) -> u32 {
    let numlock = unicode != 0;
    match key_code {
        3 => 0xff50,        // HOME (normally taken by the system) → Home
        4 => 0x1008_ff26,   // BACK → XF86Back
        19 => 0xff52,       // DPAD_UP → Up
        20 => 0xff54,       // DPAD_DOWN → Down
        21 => 0xff51,       // DPAD_LEFT → Left
        22 => 0xff53,       // DPAD_RIGHT → Right
        57 => 0xffe9,       // ALT_LEFT → Alt_L
        58 => 0xfe03,       // ALT_RIGHT → ISO_Level3_Shift (AltGr)
        59 => 0xffe1,       // SHIFT_LEFT
        60 => 0xffe2,       // SHIFT_RIGHT
        61 => 0xff09,       // TAB
        66 => 0xff0d,       // ENTER → Return
        67 => 0xff08,       // DEL → BackSpace
        82 => 0xff67,       // MENU
        92 => 0xff55,       // PAGE_UP → Prior
        93 => 0xff56,       // PAGE_DOWN → Next
        111 => 0xff1b,      // ESCAPE
        112 => 0xffff,      // FORWARD_DEL → Delete
        113 => 0xffe3,      // CTRL_LEFT
        114 => 0xffe4,      // CTRL_RIGHT
        115 => 0xffe5,      // CAPS_LOCK
        116 => 0xff14,      // SCROLL_LOCK
        117 => 0xffeb,      // META_LEFT → Super_L
        118 => 0xffec,      // META_RIGHT → Super_R
        120 => 0xff61,      // SYSRQ → Print
        121 => 0xff13,      // BREAK → Pause
        122 => 0xff50,      // MOVE_HOME → Home
        123 => 0xff57,      // MOVE_END → End
        124 => 0xff63,      // INSERT
        125 => 0x1008_ff27, // FORWARD → XF86Forward
        131..=142 => 0xffbe + (key_code - 131) as u32, // F1-F12
        143 => 0xff7f,      // NUM_LOCK
        144..=153 if numlock => 0xffb0 + (key_code - 144) as u32, // KP_0-9
        144 => 0xff9e,      // KP_Insert
        145 => 0xff9c,      // KP_End
        146 => 0xff99,      // KP_Down
        147 => 0xff9b,      // KP_Next
        148 => 0xff96,      // KP_Left
        149 => 0xff9d,      // KP_Begin
        150 => 0xff98,      // KP_Right
        151 => 0xff95,      // KP_Home
        152 => 0xff97,      // KP_Up
        153 => 0xff9a,      // KP_Prior
        154 => 0xffaf,      // NUMPAD_DIVIDE
        155 => 0xffaa,      // NUMPAD_MULTIPLY
        156 => 0xffad,      // NUMPAD_SUBTRACT
        157 => 0xffab,      // NUMPAD_ADD
        158 if numlock => 0xffae, // KP_Decimal
        158 => 0xff9f,      // KP_Delete
        159 => 0xffac,      // NUMPAD_COMMA → KP_Separator
        160 => 0xff8d,      // NUMPAD_ENTER
        161 => 0xffbd,      // NUMPAD_EQUALS
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters() {
        assert_eq!(keysym(29, 'a' as u32), 0x61); // KEYCODE_A
        assert_eq!(keysym(29, 'A' as u32), 0x41);
        assert_eq!(keysym(29, 'é' as u32), 0xe9);
        assert_eq!(keysym(29, '€' as u32), 0x0100_20ac);
        assert_eq!(keysym(29, 0), 0);
    }

    #[test]
    fn named_keys_win_over_control_characters() {
        assert_eq!(keysym(66, '\n' as u32), 0xff0d);
        assert_eq!(keysym(61, '\t' as u32), 0xff09);
        assert_eq!(keysym(62, ' ' as u32), 0x20); // SPACE has no named entry
        assert_eq!(keysym(135, 0), 0xffc2); // F5
    }

    #[test]
    fn numpad_follows_num_lock() {
        assert_eq!(keysym(151, '7' as u32), 0xffb7);
        assert_eq!(keysym(151, 0), 0xff95);
        assert_eq!(keysym(158, '.' as u32), 0xffae);
        assert_eq!(keysym(158, 0), 0xff9f);
    }

    #[test]
    fn scancodes() {
        assert_eq!(qnum(30), 0x1e); // KEY_A
        assert_eq!(qnum(100), 0xb8); // KEY_RIGHTALT
        assert_eq!(qnum(125), 0xdb); // KEY_LEFTMETA
        assert_eq!(qnum(0), 0);
        assert_eq!(qnum(-1), 0);
    }
}
