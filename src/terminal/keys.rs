//! Translates keystrokes into the byte sequences a terminal program expects.
//!
//! Plain printable keys return `None`: they arrive through the platform text
//! input path instead, so dead keys and IME composition keep working.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

pub fn to_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    if mods.platform {
        // macOS line-editing conventions.
        return match key {
            "backspace" => Some(vec![0x15]),
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            _ => None,
        };
    }

    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let modifier = 1 + mods.shift as u8 + 2 * mods.alt as u8 + 4 * mods.control as u8;
    let modified = modifier > 1;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let csi_letter = |letter: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let csi_tilde = |number: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[{number};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };
    let ss3 = |letter: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else {
            format!("\x1bO{letter}").into_bytes()
        }
    };

    let bytes = match key {
        // ESC CR is what agent CLIs read as "insert newline".
        "enter" if mods.shift || mods.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "tab" if mods.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" if mods.alt => b"\x1b\x7f".to_vec(),
        "backspace" if mods.control => vec![0x08],
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "space" if mods.control => vec![0x00],
        "space" if mods.alt => b"\x1b ".to_vec(),
        // Option-arrow moves by word, as in most macOS terminals.
        "left" if mods.alt && !mods.shift && !mods.control => b"\x1bb".to_vec(),
        "right" if mods.alt && !mods.shift && !mods.control => b"\x1bf".to_vec(),
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => csi_tilde(2),
        "delete" => csi_tilde(3),
        "pageup" => csi_tilde(5),
        "pagedown" => csi_tilde(6),
        "f1" => ss3('P'),
        "f2" => ss3('Q'),
        "f3" => ss3('R'),
        "f4" => ss3('S'),
        "f5" => csi_tilde(15),
        "f6" => csi_tilde(17),
        "f7" => csi_tilde(18),
        "f8" => csi_tilde(19),
        "f9" => csi_tilde(20),
        "f10" => csi_tilde(21),
        "f11" => csi_tilde(23),
        "f12" => csi_tilde(24),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if mods.control {
                let byte = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    '@' | '2' | ' ' => 0x00,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '/' | '7' => 0x1f,
                    '?' | '8' => 0x7f,
                    _ => return None,
                };
                if mods.alt {
                    vec![0x1b, byte]
                } else {
                    vec![byte]
                }
            } else if mods.alt {
                // Option acts as Meta.
                let c = if mods.shift {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                let mut out = vec![0x1b];
                out.extend(c.to_string().as_bytes());
                out
            } else {
                return None;
            }
        }
    };
    Some(bytes)
}

/// Encodes a mouse button event for programs that enabled mouse reporting.
/// `column` and `row` are zero-based viewport cells.
pub fn mouse_report(
    button: u8,
    pressed: bool,
    column: usize,
    row: usize,
    shift: bool,
    alt: bool,
    control: bool,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let modifiers = 4 * shift as u8 + 8 * alt as u8 + 16 * control as u8;
    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if pressed { 'M' } else { 'm' };
        Some(
            format!(
                "\x1b[<{};{};{}{}",
                button + modifiers,
                column + 1,
                row + 1,
                suffix
            )
            .into_bytes(),
        )
    } else {
        // Legacy X10 encoding cannot address cells past 223.
        if column >= 223 || row >= 223 {
            return None;
        }
        let code = if pressed { button } else { 3 } + modifiers;
        Some(vec![
            0x1b,
            b'[',
            b'M',
            32 + code,
            32 + 1 + column as u8,
            32 + 1 + row as u8,
        ])
    }
}

/// Wraps pasted text so shells and editors can tell it from typed input.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // Strip ESC so pasted text cannot terminate the bracket early.
        let clean: String = text.chars().filter(|&c| c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    fn bytes(source: &str, mode: TermMode) -> Option<Vec<u8>> {
        to_bytes(&key(source), mode)
    }

    #[test]
    fn printable_keys_use_text_input() {
        assert_eq!(bytes("a", TermMode::NONE), None);
        assert_eq!(bytes("shift-a", TermMode::NONE), None);
        assert_eq!(bytes("space", TermMode::NONE), None);
    }

    #[test]
    fn control_and_meta() {
        assert_eq!(bytes("ctrl-c", TermMode::NONE), Some(vec![0x03]));
        assert_eq!(bytes("ctrl-[", TermMode::NONE), Some(vec![0x1b]));
        assert_eq!(bytes("alt-b", TermMode::NONE), Some(b"\x1bb".to_vec()));
        assert_eq!(bytes("ctrl-alt-c", TermMode::NONE), Some(vec![0x1b, 0x03]));
    }

    #[test]
    fn enter_variants() {
        assert_eq!(bytes("enter", TermMode::NONE), Some(b"\r".to_vec()));
        assert_eq!(
            bytes("shift-enter", TermMode::NONE),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(bytes("shift-tab", TermMode::NONE), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        assert_eq!(bytes("up", TermMode::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(bytes("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(
            bytes("shift-up", TermMode::NONE),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(bytes("alt-left", TermMode::NONE), Some(b"\x1bb".to_vec()));
        assert_eq!(
            bytes("ctrl-right", TermMode::NONE),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn command_keys_are_left_for_shortcuts() {
        assert_eq!(bytes("cmd-t", TermMode::NONE), None);
        assert_eq!(bytes("cmd-backspace", TermMode::NONE), Some(vec![0x15]));
    }

    #[test]
    fn mouse_encodings() {
        assert_eq!(
            mouse_report(0, true, 4, 2, false, false, false, TermMode::SGR_MOUSE),
            Some(b"\x1b[<0;5;3M".to_vec())
        );
        assert_eq!(
            mouse_report(0, false, 4, 2, false, false, false, TermMode::SGR_MOUSE),
            Some(b"\x1b[<0;5;3m".to_vec())
        );
        assert_eq!(
            mouse_report(0, true, 0, 0, false, false, false, TermMode::NONE),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse_report(0, true, 300, 0, false, false, false, TermMode::NONE),
            None
        );
    }

    #[test]
    fn paste_modes() {
        assert_eq!(paste_bytes("a\nb", TermMode::NONE), b"a\rb".to_vec());
        assert_eq!(
            paste_bytes("a\x1b[201~b", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a[201~b\x1b[201~".to_vec()
        );
    }
}
