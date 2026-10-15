//! xterm input encoding: keys, mouse reports, paste and focus events → bytes for the remote.

use eframe::egui::Key;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    /// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: cursor keys send SS3 sequences.
    pub app_cursor: bool,
    /// Option sends ESC-prefixed keys (Meta) instead of composing characters.
    pub option_as_meta: bool,
}

const ESC: u8 = 0x1b;

/// Encodes a key press. Returns `None` for keys that should be handled as text input instead
/// (plain printable characters arrive through text events, which respect the keyboard layout).
pub fn encode_key(key: Key, mods: Mods, modes: KeyModes) -> Option<Vec<u8>> {
    let csi_mod = |final_byte: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{}{}", mods.param(), final_byte as char).into_bytes()
        } else if modes.app_cursor && matches!(final_byte, b'A'..=b'D' | b'H' | b'F') {
            vec![ESC, b'O', final_byte]
        } else {
            vec![ESC, b'[', final_byte]
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[{code};{}~", mods.param()).into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let ss3_fn = |final_byte: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{}{}", mods.param(), final_byte as char).into_bytes()
        } else {
            vec![ESC, b'O', final_byte]
        }
    };
    let alt_prefix = |mut bytes: Vec<u8>| -> Vec<u8> {
        if mods.alt {
            bytes.insert(0, ESC);
        }
        bytes
    };

    let out = match key {
        Key::ArrowUp => csi_mod(b'A'),
        Key::ArrowDown => csi_mod(b'B'),
        Key::ArrowRight => csi_mod(b'C'),
        Key::ArrowLeft => csi_mod(b'D'),
        Key::Home => csi_mod(b'H'),
        Key::End => csi_mod(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3_fn(b'P'),
        Key::F2 => ss3_fn(b'Q'),
        Key::F3 => ss3_fn(b'R'),
        Key::F4 => ss3_fn(b'S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Backspace => alt_prefix(vec![if mods.ctrl { 0x08 } else { 0x7f }]),
        Key::Enter => alt_prefix(vec![b'\r']),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => alt_prefix(vec![b'\t']),
        Key::Escape => alt_prefix(vec![ESC]),
        Key::Space if mods.ctrl => alt_prefix(vec![0]),
        _ => {
            if mods.ctrl {
                return ctrl_byte(key).map(|b| alt_prefix(vec![b]));
            }
            if mods.alt && modes.option_as_meta {
                return meta_char(key, mods.shift).map(|c| vec![ESC, c]);
            }
            return None;
        }
    };
    Some(out)
}

/// Ctrl+key control bytes (Ctrl+A = 0x01 … Ctrl+Z = 0x1a, plus the punctuation classics).
fn ctrl_byte(key: Key) -> Option<u8> {
    if let Some(c) = letter(key) {
        return Some(c - b'a' + 1);
    }
    Some(match key {
        Key::Num2 | Key::Space => 0x00,
        Key::OpenBracket | Key::Num3 => 0x1b,
        Key::Backslash | Key::Num4 => 0x1c,
        Key::CloseBracket | Key::Num5 => 0x1d,
        Key::Num6 => 0x1e,
        Key::Minus | Key::Slash | Key::Num7 => 0x1f,
        Key::Num8 => 0x7f,
        _ => return None,
    })
}

/// Character for Meta+key with a US layout (used only when Option acts as Meta).
fn meta_char(key: Key, shift: bool) -> Option<u8> {
    if let Some(c) = letter(key) {
        return Some(if shift { c.to_ascii_uppercase() } else { c });
    }
    let (plain, shifted) = match key {
        Key::Num0 => (b'0', b')'),
        Key::Num1 => (b'1', b'!'),
        Key::Num2 => (b'2', b'@'),
        Key::Num3 => (b'3', b'#'),
        Key::Num4 => (b'4', b'$'),
        Key::Num5 => (b'5', b'%'),
        Key::Num6 => (b'6', b'^'),
        Key::Num7 => (b'7', b'&'),
        Key::Num8 => (b'8', b'*'),
        Key::Num9 => (b'9', b'('),
        Key::Minus => (b'-', b'_'),
        Key::Equals => (b'=', b'+'),
        Key::OpenBracket => (b'[', b'{'),
        Key::CloseBracket => (b']', b'}'),
        Key::Backslash => (b'\\', b'|'),
        Key::Semicolon => (b';', b':'),
        Key::Quote => (b'\'', b'"'),
        Key::Comma => (b',', b'<'),
        Key::Period => (b'.', b'>'),
        Key::Slash => (b'/', b'?'),
        Key::Backtick => (b'`', b'~'),
        Key::Space => (b' ', b' '),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

fn letter(key: Key) -> Option<u8> {
    use Key::*;
    const LETTERS: [Key; 26] =
        [A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z];
    LETTERS.iter().position(|k| *k == key).map(|i| b'a' + i as u8)
}

/// Whether a key event with these modifiers produces bytes itself, so the text event that
/// usually follows it must be ignored (e.g. Ctrl+C, or Option+B in Meta mode).
pub fn key_consumes_text(key: Key, mods: Mods, modes: KeyModes) -> bool {
    (mods.ctrl && ctrl_byte(key).is_some())
        || (mods.alt && modes.option_as_meta && meta_char(key, mods.shift).is_some())
}

/// Wraps pasted text for the remote: normalizes newlines to CR and adds bracketed-paste markers
/// when the application asked for them. In bracketed mode ESC and ^C are removed (as alacritty
/// does), so pasted text can neither end bracketed mode early nor interrupt the program.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let clean: String = normalized.chars().filter(|c| *c != '\x1b' && *c != '\x03').collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Encodes a mouse report for a cell (0-based col/row). SGR (1006) encoding when `sgr`,
/// otherwise the legacy X10 form (limited to 223 columns/rows).
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    mods: Mods,
    col: usize,
    row: usize,
    sgr: bool,
) -> Option<Vec<u8>> {
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    if sgr {
        let suffix = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    if action == MouseAction::Release {
        code = (code & !3) | 3; // X10 can't say which button was released
    }
    if col > 222 || row > 222 {
        return None;
    }
    Some(vec![ESC, b'[', b'M', 32 + code as u8, 33 + col as u8, 33 + row as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { shift: false, alt: false, ctrl: false };
    const CTRL: Mods = Mods { shift: false, alt: false, ctrl: true };

    fn k(key: Key, mods: Mods) -> Vec<u8> {
        encode_key(key, mods, KeyModes::default()).unwrap()
    }

    #[test]
    fn cursor_keys_and_modes() {
        assert_eq!(k(Key::ArrowUp, NONE), b"\x1b[A");
        let app = KeyModes { app_cursor: true, ..Default::default() };
        assert_eq!(encode_key(Key::ArrowUp, NONE, app).unwrap(), b"\x1bOA");
        let shift = Mods { shift: true, ..NONE };
        assert_eq!(encode_key(Key::ArrowUp, shift, app).unwrap(), b"\x1b[1;2A");
        assert_eq!(k(Key::ArrowLeft, CTRL), b"\x1b[1;5D");
        assert_eq!(k(Key::Home, NONE), b"\x1b[H");
        assert_eq!(k(Key::Delete, NONE), b"\x1b[3~");
        assert_eq!(k(Key::PageUp, Mods { alt: true, ..NONE }), b"\x1b[5;3~");
        assert_eq!(k(Key::F1, NONE), b"\x1bOP");
        assert_eq!(k(Key::F5, NONE), b"\x1b[15~");
        assert_eq!(k(Key::F12, shift), b"\x1b[24;2~");
    }

    #[test]
    fn control_and_meta() {
        assert_eq!(k(Key::C, CTRL), [0x03]);
        assert_eq!(k(Key::OpenBracket, CTRL), [0x1b]);
        assert_eq!(k(Key::Space, CTRL), [0x00]);
        assert_eq!(k(Key::Backspace, NONE), [0x7f]);
        assert_eq!(k(Key::Tab, Mods { shift: true, ..NONE }), b"\x1b[Z");
        assert_eq!(k(Key::Enter, NONE), b"\r");
        assert!(encode_key(Key::A, NONE, KeyModes::default()).is_none(), "plain text goes via text events");
        let meta = KeyModes { option_as_meta: true, ..Default::default() };
        let alt = Mods { alt: true, ..NONE };
        assert_eq!(encode_key(Key::B, alt, meta).unwrap(), b"\x1bb");
        assert!(encode_key(Key::B, alt, KeyModes::default()).is_none());
        assert!(key_consumes_text(Key::C, CTRL, KeyModes::default()));
        assert!(!key_consumes_text(Key::C, NONE, KeyModes::default()));
    }

    #[test]
    fn paste_and_mouse() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("x\x1b[201~y\x03", true), b"\x1b[200~x[201~y\x1b[201~");
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Press, NONE, 0, 0, true).unwrap(),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Release, NONE, 9, 4, true).unwrap(),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            encode_mouse(MouseButton::WheelDown, MouseAction::Press, NONE, 1, 1, false).unwrap(),
            [0x1b, b'[', b'M', 32 + 65, 34, 34]
        );
        assert!(encode_mouse(MouseButton::Left, MouseAction::Press, NONE, 300, 1, false).is_none());
    }
}
