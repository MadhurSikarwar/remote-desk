//! Physical key names → PC keyboard scan codes.
//!
//! The viewer reports which physical key was used (`KeyboardEvent.code`,
//! for example `"KeyA"` for the key right of Caps Lock). Sending that as a
//! scan code lets the host's own layout turn it into a character, so a
//! viewer and a host with different layouts still behave as if the viewer
//! were typing on the host's keyboard.

/// `(scan code, extended)` for a `KeyboardEvent.code` value, or `None` for
/// keys that are not forwarded.
pub fn scancode_for(code: &str) -> Option<(u16, bool)> {
    let plain = |scancode| Some((scancode, false));
    let extended = |scancode| Some((scancode, true));
    match code {
        "Escape" => plain(0x01),
        "Digit1" => plain(0x02),
        "Digit2" => plain(0x03),
        "Digit3" => plain(0x04),
        "Digit4" => plain(0x05),
        "Digit5" => plain(0x06),
        "Digit6" => plain(0x07),
        "Digit7" => plain(0x08),
        "Digit8" => plain(0x09),
        "Digit9" => plain(0x0A),
        "Digit0" => plain(0x0B),
        "Minus" => plain(0x0C),
        "Equal" => plain(0x0D),
        "Backspace" => plain(0x0E),
        "Tab" => plain(0x0F),
        "KeyQ" => plain(0x10),
        "KeyW" => plain(0x11),
        "KeyE" => plain(0x12),
        "KeyR" => plain(0x13),
        "KeyT" => plain(0x14),
        "KeyY" => plain(0x15),
        "KeyU" => plain(0x16),
        "KeyI" => plain(0x17),
        "KeyO" => plain(0x18),
        "KeyP" => plain(0x19),
        "BracketLeft" => plain(0x1A),
        "BracketRight" => plain(0x1B),
        "Enter" => plain(0x1C),
        "ControlLeft" => plain(0x1D),
        "KeyA" => plain(0x1E),
        "KeyS" => plain(0x1F),
        "KeyD" => plain(0x20),
        "KeyF" => plain(0x21),
        "KeyG" => plain(0x22),
        "KeyH" => plain(0x23),
        "KeyJ" => plain(0x24),
        "KeyK" => plain(0x25),
        "KeyL" => plain(0x26),
        "Semicolon" => plain(0x27),
        "Quote" => plain(0x28),
        "Backquote" => plain(0x29),
        "ShiftLeft" => plain(0x2A),
        "Backslash" => plain(0x2B),
        "KeyZ" => plain(0x2C),
        "KeyX" => plain(0x2D),
        "KeyC" => plain(0x2E),
        "KeyV" => plain(0x2F),
        "KeyB" => plain(0x30),
        "KeyN" => plain(0x31),
        "KeyM" => plain(0x32),
        "Comma" => plain(0x33),
        "Period" => plain(0x34),
        "Slash" => plain(0x35),
        "ShiftRight" => plain(0x36),
        "NumpadMultiply" => plain(0x37),
        "AltLeft" => plain(0x38),
        "Space" => plain(0x39),
        "CapsLock" => plain(0x3A),
        "F1" => plain(0x3B),
        "F2" => plain(0x3C),
        "F3" => plain(0x3D),
        "F4" => plain(0x3E),
        "F5" => plain(0x3F),
        "F6" => plain(0x40),
        "F7" => plain(0x41),
        "F8" => plain(0x42),
        "F9" => plain(0x43),
        "F10" => plain(0x44),
        "NumLock" => plain(0x45),
        "ScrollLock" => plain(0x46),
        "Numpad7" => plain(0x47),
        "Numpad8" => plain(0x48),
        "Numpad9" => plain(0x49),
        "NumpadSubtract" => plain(0x4A),
        "Numpad4" => plain(0x4B),
        "Numpad5" => plain(0x4C),
        "Numpad6" => plain(0x4D),
        "NumpadAdd" => plain(0x4E),
        "Numpad1" => plain(0x4F),
        "Numpad2" => plain(0x50),
        "Numpad3" => plain(0x51),
        "Numpad0" => plain(0x52),
        "NumpadDecimal" => plain(0x53),
        "IntlBackslash" => plain(0x56),
        "F11" => plain(0x57),
        "F12" => plain(0x58),

        "NumpadEnter" => extended(0x1C),
        "ControlRight" => extended(0x1D),
        "NumpadDivide" => extended(0x35),
        "PrintScreen" => extended(0x37),
        "AltRight" => extended(0x38),
        "Home" => extended(0x47),
        "ArrowUp" => extended(0x48),
        "PageUp" => extended(0x49),
        "ArrowLeft" => extended(0x4B),
        "ArrowRight" => extended(0x4D),
        "End" => extended(0x4F),
        "ArrowDown" => extended(0x50),
        "PageDown" => extended(0x51),
        "Insert" => extended(0x52),
        "Delete" => extended(0x53),
        "MetaLeft" => extended(0x5B),
        "MetaRight" => extended(0x5C),
        "ContextMenu" => extended(0x5D),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const ALL: &[&str] = &[
        "Escape",
        "Digit1",
        "Digit2",
        "Digit3",
        "Digit4",
        "Digit5",
        "Digit6",
        "Digit7",
        "Digit8",
        "Digit9",
        "Digit0",
        "Minus",
        "Equal",
        "Backspace",
        "Tab",
        "KeyQ",
        "KeyW",
        "KeyE",
        "KeyR",
        "KeyT",
        "KeyY",
        "KeyU",
        "KeyI",
        "KeyO",
        "KeyP",
        "BracketLeft",
        "BracketRight",
        "Enter",
        "ControlLeft",
        "KeyA",
        "KeyS",
        "KeyD",
        "KeyF",
        "KeyG",
        "KeyH",
        "KeyJ",
        "KeyK",
        "KeyL",
        "Semicolon",
        "Quote",
        "Backquote",
        "ShiftLeft",
        "Backslash",
        "KeyZ",
        "KeyX",
        "KeyC",
        "KeyV",
        "KeyB",
        "KeyN",
        "KeyM",
        "Comma",
        "Period",
        "Slash",
        "ShiftRight",
        "NumpadMultiply",
        "AltLeft",
        "Space",
        "CapsLock",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "NumLock",
        "ScrollLock",
        "Numpad7",
        "Numpad8",
        "Numpad9",
        "NumpadSubtract",
        "Numpad4",
        "Numpad5",
        "Numpad6",
        "NumpadAdd",
        "Numpad1",
        "Numpad2",
        "Numpad3",
        "Numpad0",
        "NumpadDecimal",
        "IntlBackslash",
        "F11",
        "F12",
        "NumpadEnter",
        "ControlRight",
        "NumpadDivide",
        "PrintScreen",
        "AltRight",
        "Home",
        "ArrowUp",
        "PageUp",
        "ArrowLeft",
        "ArrowRight",
        "End",
        "ArrowDown",
        "PageDown",
        "Insert",
        "Delete",
        "MetaLeft",
        "MetaRight",
        "ContextMenu",
    ];

    #[test]
    fn every_listed_key_maps_to_a_distinct_code() {
        let mut seen = HashSet::new();
        for code in ALL {
            let mapped = scancode_for(code).unwrap_or_else(|| panic!("{code} is not mapped"));
            assert!(mapped.0 > 0 && mapped.0 < 0x80, "{code}: {mapped:?}");
            assert!(seen.insert(mapped), "{code} duplicates {mapped:?}");
        }
        assert_eq!(seen.len(), 104);
    }

    #[test]
    fn well_known_keys_have_their_standard_codes() {
        assert_eq!(scancode_for("KeyA"), Some((0x1E, false)));
        assert_eq!(scancode_for("Space"), Some((0x39, false)));
        assert_eq!(scancode_for("Enter"), Some((0x1C, false)));
        assert_eq!(scancode_for("ShiftLeft"), Some((0x2A, false)));
        // Extended keys share a base code with a numpad or left-hand key.
        assert_eq!(scancode_for("NumpadEnter"), Some((0x1C, true)));
        assert_eq!(scancode_for("ControlRight"), Some((0x1D, true)));
        assert_eq!(scancode_for("ArrowLeft"), Some((0x4B, true)));
        assert_eq!(scancode_for("Numpad4"), Some((0x4B, false)));
        assert_eq!(scancode_for("Delete"), Some((0x53, true)));
    }

    #[test]
    fn unknown_names_are_not_forwarded() {
        for code in ["", "keya", "Unidentified", "Pause", "F13", "KeyA "] {
            assert_eq!(scancode_for(code), None, "{code:?}");
        }
    }
}
