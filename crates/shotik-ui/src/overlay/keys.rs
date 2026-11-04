//! Keyboard by physical key, the way the JS reads `e.code`: a Russian layout must work. Windows reports
//! the hardware scan code in every key message; Slint's key events only carry layout-dependent text.

use serde::Deserialize;

/// The keys the overlay reacts to, named as `KeyboardEvent.code` names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Code {
    KeyA,
    KeyC,
    KeyF,
    KeyP,
    KeyR,
    KeyS,
    KeyT,
    KeyV,
    KeyY,
    KeyZ,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    BracketLeft,
    BracketRight,
    Escape,
    Enter,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Alt,
    Shift,
}

const VK_SHIFT: u16 = 0x10;
const VK_MENU: u16 = 0x12;
const VK_ESCAPE: u16 = 0x1B;
const VK_RETURN: u16 = 0x0D;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;

/// Maps a key message. The JS reads named keys through `e.key`, which a numpad arrow with NumLock off also
/// produces, and those virtual keys do not depend on the layout. Everything else goes by scan code.
pub fn code_of(virtual_key: u16, scan_code: u8) -> Option<Code> {
    Some(match virtual_key {
        VK_ESCAPE => Code::Escape,
        VK_RETURN => Code::Enter,
        VK_LEFT => Code::ArrowLeft,
        VK_RIGHT => Code::ArrowRight,
        VK_UP => Code::ArrowUp,
        VK_DOWN => Code::ArrowDown,
        VK_MENU => Code::Alt,
        VK_SHIFT => Code::Shift,
        _ => return code_of_scan(scan_code),
    })
}

/// Scan code set 1, the row of the physical key on a standard keyboard.
fn code_of_scan(scan_code: u8) -> Option<Code> {
    Some(match scan_code {
        0x02 => Code::Digit1,
        0x03 => Code::Digit2,
        0x04 => Code::Digit3,
        0x05 => Code::Digit4,
        0x06 => Code::Digit5,
        0x07 => Code::Digit6,
        0x08 => Code::Digit7,
        0x09 => Code::Digit8,
        0x0A => Code::Digit9,
        0x0B => Code::Digit0,
        0x13 => Code::KeyR,
        0x14 => Code::KeyT,
        0x15 => Code::KeyY,
        0x19 => Code::KeyP,
        0x1A => Code::BracketLeft,
        0x1B => Code::BracketRight,
        0x1E => Code::KeyA,
        0x1F => Code::KeyS,
        0x21 => Code::KeyF,
        0x2C => Code::KeyZ,
        0x2E => Code::KeyC,
        0x2F => Code::KeyV,
        _ => return None,
    })
}

/// The scan code in the `lParam` of `WM_KEYDOWN`, `WM_KEYUP` and their `SYS` forms.
pub fn scan_code_of(lparam: isize) -> u8 {
    ((lparam >> 16) & 0xFF) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a Russian layout sends for the physical `C` key: its own virtual key can differ on other
    /// layouts, so only the scan code may decide.
    #[test]
    fn letters_go_by_the_physical_key_whatever_the_layout_reports() {
        let lparam_of_physical_c: isize = 0x002E_0001;
        assert_eq!(scan_code_of(lparam_of_physical_c), 0x2E);
        for virtual_key in [0x43_u16, 0xDD, 0xBC, 0] {
            assert_eq!(code_of(virtual_key, 0x2E), Some(Code::KeyC), "virtual key {virtual_key:#x}");
        }
    }

    #[test]
    fn every_shortcut_key_has_its_row_on_the_keyboard() {
        let table = [
            (0x1E, Code::KeyA),
            (0x2E, Code::KeyC),
            (0x21, Code::KeyF),
            (0x19, Code::KeyP),
            (0x13, Code::KeyR),
            (0x1F, Code::KeyS),
            (0x14, Code::KeyT),
            (0x2F, Code::KeyV),
            (0x15, Code::KeyY),
            (0x2C, Code::KeyZ),
            (0x0B, Code::Digit0),
            (0x02, Code::Digit1),
            (0x0A, Code::Digit9),
            (0x1A, Code::BracketLeft),
            (0x1B, Code::BracketRight),
        ];
        for (scan, code) in table {
            assert_eq!(code_of(0, scan), Some(code));
        }
    }

    #[test]
    fn named_keys_go_by_virtual_key_so_numpad_arrows_count() {
        // Numpad 4 with NumLock off: VK_LEFT with the scan code of the numpad key, as `e.key` reports it.
        assert_eq!(code_of(VK_LEFT, 0x4B), Some(Code::ArrowLeft));
        assert_eq!(code_of(VK_RETURN, 0x1C), Some(Code::Enter));
        assert_eq!(code_of(VK_ESCAPE, 0x01), Some(Code::Escape));
        assert_eq!(code_of(VK_MENU, 0x38), Some(Code::Alt));
        assert_eq!(code_of(VK_SHIFT, 0x2A), Some(Code::Shift));
    }

    #[test]
    fn a_key_the_overlay_does_not_use_maps_to_nothing() {
        assert_eq!(code_of(0x61, 0x4F), None); // numpad 1: `e.code` is Numpad1, not Digit1
        assert_eq!(code_of(0x20, 0x39), None); // space
    }
}
