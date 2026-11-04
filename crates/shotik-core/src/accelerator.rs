//! Layout-independent settings hotkey recorder (`src/app/accelerator.js`).

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyEvent<'a> {
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
    pub meta_key: bool,
    pub key: &'a str,
    pub code: &'a str,
}

pub fn to_accelerator(event: &KeyEvent<'_>, platform: &str) -> Option<String> {
    if matches!(event.key, "Control" | "Alt" | "Shift" | "Meta") {
        return None;
    }
    let mut parts = Vec::new();
    if event.meta_key && platform == "darwin" {
        parts.push("Cmd");
    }
    if event.ctrl_key {
        parts.push("Ctrl");
    }
    if event.alt_key {
        parts.push("Alt");
    }
    if event.shift_key {
        parts.push("Shift");
    }
    if event.meta_key && platform != "darwin" {
        parts.push("Super");
    }
    let code = event.code;
    let function_key = code
        .strip_prefix('F')
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    let key = if code.len() == 4
        && code.starts_with("Key")
        && code.as_bytes()[3].is_ascii_uppercase()
    {
        &code[3..]
    } else if code.len() == 6 && code.starts_with("Digit") && code.as_bytes()[5].is_ascii_digit() {
        &code[5..]
    } else if function_key {
        code
    } else {
        match code {
            "PrintScreen" | "Space" | "Home" | "End" | "PageUp" | "PageDown" | "Insert" => code,
            "ArrowUp" => "Up",
            "ArrowDown" => "Down",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "Backquote" => "`",
            "Minus" => "-",
            "Equal" => "=",
            _ => return None,
        }
    };
    if parts.is_empty() && key != "PrintScreen" && !function_key {
        return None;
    }
    parts.push(key);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All vectors in test/unit/accelerator.test.js.
    #[test]
    fn js_vectors() {
        let cases = [
            (
                KeyEvent {
                    ctrl_key: true,
                    shift_key: true,
                    key: "$",
                    code: "Digit4",
                    ..KeyEvent::default()
                },
                "win32",
                Some("Ctrl+Shift+4"),
            ),
            (
                KeyEvent {
                    meta_key: true,
                    shift_key: true,
                    key: "4",
                    code: "Digit4",
                    ..KeyEvent::default()
                },
                "darwin",
                Some("Cmd+Shift+4"),
            ),
            (
                KeyEvent {
                    meta_key: true,
                    key: "s",
                    code: "KeyS",
                    ..KeyEvent::default()
                },
                "win32",
                Some("Super+S"),
            ),
            (
                KeyEvent {
                    ctrl_key: true,
                    key: "Control",
                    code: "ControlLeft",
                    ..KeyEvent::default()
                },
                "win32",
                None,
            ),
            (
                KeyEvent {
                    key: "PrintScreen",
                    code: "PrintScreen",
                    ..KeyEvent::default()
                },
                "win32",
                Some("PrintScreen"),
            ),
            (
                KeyEvent {
                    key: "F6",
                    code: "F6",
                    ..KeyEvent::default()
                },
                "win32",
                Some("F6"),
            ),
            (
                KeyEvent {
                    key: "a",
                    code: "KeyA",
                    ..KeyEvent::default()
                },
                "win32",
                None,
            ),
            (
                KeyEvent {
                    ctrl_key: true,
                    alt_key: true,
                    key: "е",
                    code: "KeyT",
                    ..KeyEvent::default()
                },
                "win32",
                Some("Ctrl+Alt+T"),
            ),
        ];
        for (event, platform, expected) in cases {
            assert_eq!(
                to_accelerator(&event, platform).as_deref(),
                expected,
                "{event:?}"
            );
        }
    }

    #[test]
    fn modifier_order_and_special_keys_match_electron() {
        let event = KeyEvent {
            ctrl_key: true,
            alt_key: true,
            shift_key: true,
            meta_key: true,
            code: "ArrowLeft",
            ..KeyEvent::default()
        };
        assert_eq!(
            to_accelerator(&event, "darwin").as_deref(),
            Some("Cmd+Ctrl+Alt+Shift+Left")
        );
        assert_eq!(
            to_accelerator(&event, "win32").as_deref(),
            Some("Ctrl+Alt+Shift+Super+Left")
        );
        for (code, expected) in [
            ("Space", "Space"),
            ("Home", "Home"),
            ("End", "End"),
            ("PageUp", "PageUp"),
            ("PageDown", "PageDown"),
            ("Insert", "Insert"),
            ("ArrowUp", "Up"),
            ("ArrowDown", "Down"),
            ("ArrowRight", "Right"),
            ("Backquote", "`"),
            ("Minus", "-"),
            ("Equal", "="),
        ] {
            assert_eq!(
                to_accelerator(
                    &KeyEvent {
                        ctrl_key: true,
                        code,
                        ..KeyEvent::default()
                    },
                    "win32"
                ),
                Some(format!("Ctrl+{expected}"))
            );
        }
        for code in [
            "Enter", "Delete", "Escape", "Numpad1", "KeyЯ", "KeyAA", "F", "F1x", "Digit10", "",
        ] {
            assert!(
                to_accelerator(
                    &KeyEvent {
                        ctrl_key: true,
                        code,
                        ..KeyEvent::default()
                    },
                    "win32"
                )
                .is_none(),
                "{code}"
            );
        }
    }
}
