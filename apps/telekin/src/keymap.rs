//! egui input -> portable [`telekin_proto`] input.

use telekin_proto::{KeyCode, MouseButton};

pub fn mouse_button(button: egui::PointerButton) -> Option<MouseButton> {
    use egui::PointerButton as P;
    Some(match button {
        P::Primary => MouseButton::Left,
        P::Secondary => MouseButton::Right,
        P::Middle => MouseButton::Middle,
        P::Extra1 => MouseButton::Back,
        P::Extra2 => MouseButton::Forward,
    })
}

/// Map an egui key. Keys outside the portable set return `None`; the caller
/// falls back to sending text, which keeps non-Latin layouts working.
pub fn key(key: egui::Key) -> Option<KeyCode> {
    use egui::Key as K;
    Some(match key {
        K::A => KeyCode::A,
        K::B => KeyCode::B,
        K::C => KeyCode::C,
        K::D => KeyCode::D,
        K::E => KeyCode::E,
        K::F => KeyCode::F,
        K::G => KeyCode::G,
        K::H => KeyCode::H,
        K::I => KeyCode::I,
        K::J => KeyCode::J,
        K::K => KeyCode::K,
        K::L => KeyCode::L,
        K::M => KeyCode::M,
        K::N => KeyCode::N,
        K::O => KeyCode::O,
        K::P => KeyCode::P,
        K::Q => KeyCode::Q,
        K::R => KeyCode::R,
        K::S => KeyCode::S,
        K::T => KeyCode::T,
        K::U => KeyCode::U,
        K::V => KeyCode::V,
        K::W => KeyCode::W,
        K::X => KeyCode::X,
        K::Y => KeyCode::Y,
        K::Z => KeyCode::Z,

        K::Num0 => KeyCode::Digit0,
        K::Num1 => KeyCode::Digit1,
        K::Num2 => KeyCode::Digit2,
        K::Num3 => KeyCode::Digit3,
        K::Num4 => KeyCode::Digit4,
        K::Num5 => KeyCode::Digit5,
        K::Num6 => KeyCode::Digit6,
        K::Num7 => KeyCode::Digit7,
        K::Num8 => KeyCode::Digit8,
        K::Num9 => KeyCode::Digit9,

        K::F1 => KeyCode::F1,
        K::F2 => KeyCode::F2,
        K::F3 => KeyCode::F3,
        K::F4 => KeyCode::F4,
        K::F5 => KeyCode::F5,
        K::F6 => KeyCode::F6,
        K::F7 => KeyCode::F7,
        K::F8 => KeyCode::F8,
        K::F9 => KeyCode::F9,
        K::F10 => KeyCode::F10,
        K::F11 => KeyCode::F11,
        K::F12 => KeyCode::F12,

        K::Escape => KeyCode::Escape,
        K::Tab => KeyCode::Tab,
        K::Space => KeyCode::Space,
        K::Enter => KeyCode::Enter,
        K::Backspace => KeyCode::Backspace,
        K::Delete => KeyCode::Delete,
        K::Insert => KeyCode::Insert,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::ArrowUp => KeyCode::ArrowUp,
        K::ArrowDown => KeyCode::ArrowDown,
        K::ArrowLeft => KeyCode::ArrowLeft,
        K::ArrowRight => KeyCode::ArrowRight,

        K::Minus => KeyCode::Minus,
        K::Equals => KeyCode::Equal,
        K::OpenBracket => KeyCode::BracketLeft,
        K::CloseBracket => KeyCode::BracketRight,
        K::Backslash => KeyCode::Backslash,
        K::Semicolon => KeyCode::Semicolon,
        K::Quote => KeyCode::Quote,
        K::Backtick => KeyCode::Backquote,
        K::Comma => KeyCode::Comma,
        K::Period => KeyCode::Period,
        K::Slash => KeyCode::Slash,

        // egui names the shifted symbol rather than the key under it, so
        // Ctrl+: has to reach the host as the semicolon key with Shift held
        // (Shift is mirrored separately).
        K::Colon => KeyCode::Semicolon,
        K::Plus => KeyCode::Equal,
        K::Pipe => KeyCode::Backslash,
        K::Questionmark => KeyCode::Slash,
        K::Exclamationmark => KeyCode::Digit1,
        K::OpenCurlyBracket => KeyCode::BracketLeft,
        K::CloseCurlyBracket => KeyCode::BracketRight,

        _ => return None,
    })
}

/// Whether a key press should be left to the text event that follows it.
///
/// Letters, digits and shortcuts travel as keys so the robot sees real key
/// presses (Ctrl+C, arrow keys, games). Punctuation typed plainly is
/// different: what `Shift+8` produces depends on the robot's keyboard layout,
/// and egui reports `Shift+;` as a `Colon` key that no physical code covers,
/// so `:` used to vanish. The text event carries the exact character, and the
/// host types that character whatever its layout. With Ctrl, Alt or Cmd held
/// egui produces no text event, so the key itself is what a shortcut needs.
pub fn typed_as_text(key: egui::Key, m: egui::Modifiers) -> bool {
    if m.ctrl || m.alt || m.command {
        return false;
    }
    use egui::Key as K;
    match key {
        K::Minus
        | K::Equals
        | K::Plus
        | K::OpenBracket
        | K::CloseBracket
        | K::OpenCurlyBracket
        | K::CloseCurlyBracket
        | K::Backslash
        | K::Pipe
        | K::Semicolon
        | K::Colon
        | K::Quote
        | K::Backtick
        | K::Comma
        | K::Period
        | K::Slash
        | K::Questionmark
        | K::Exclamationmark => true,
        // The digit itself goes as a key; the symbol above it goes as text.
        K::Num0
        | K::Num1
        | K::Num2
        | K::Num3
        | K::Num4
        | K::Num5
        | K::Num6
        | K::Num7
        | K::Num8
        | K::Num9 => m.shift,
        _ => false,
    }
}

/// Whether a text event carries something the key path did not: anything
/// outside ASCII (Thai, accents, IME), or ASCII punctuation, which
/// [`typed_as_text`] withheld from the key path. Letters, digits and spaces
/// already arrived as keys and would otherwise be typed twice.
pub fn send_as_text(text: &str) -> bool {
    text.chars().any(|c| !c.is_ascii() || c.is_ascii_punctuation())
}

/// Modifier state has to be mirrored explicitly: egui reports modifiers as
/// flags on each event rather than as key presses of their own, so the host
/// would otherwise never see Ctrl or Shift go down.
pub fn modifier_keys(m: egui::Modifiers) -> [(KeyCode, bool); 4] {
    [
        (KeyCode::ShiftLeft, m.shift),
        (KeyCode::ControlLeft, m.ctrl),
        (KeyCode::AltLeft, m.alt),
        (KeyCode::MetaLeft, m.command && !m.ctrl),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key as K, Modifiers as M};

    fn shift() -> M {
        M { shift: true, ..M::NONE }
    }
    fn ctrl() -> M {
        M { ctrl: true, ..M::NONE }
    }

    #[test]
    fn punctuation_and_shifted_digits_travel_as_text() {
        // The two keys that started this: `:` and `*`.
        assert!(typed_as_text(K::Colon, shift()));
        assert!(typed_as_text(K::Num8, shift()));
        assert!(send_as_text(":"));
        assert!(send_as_text("*"));
        // Plain punctuation too, so `;` is not typed twice.
        assert!(typed_as_text(K::Semicolon, M::NONE));
        assert!(send_as_text(";"));
    }

    #[test]
    fn letters_digits_and_shortcuts_stay_keys() {
        assert!(!typed_as_text(K::A, M::NONE));
        assert!(!typed_as_text(K::A, shift()));
        assert!(!typed_as_text(K::Num8, M::NONE));
        assert!(!typed_as_text(K::Semicolon, ctrl()));
        assert!(!typed_as_text(K::Minus, ctrl()), "Ctrl+- is a shortcut, not text");
        assert!(!send_as_text("a"));
        assert!(!send_as_text("8"));
        assert!(!send_as_text(" "));
    }

    #[test]
    fn non_ascii_text_is_always_sent() {
        assert!(send_as_text("ก"));
        assert!(send_as_text("é"));
    }

    #[test]
    fn shifted_symbol_keys_map_to_the_key_under_them() {
        assert_eq!(key(K::Colon), Some(KeyCode::Semicolon));
        assert_eq!(key(K::Plus), Some(KeyCode::Equal));
        assert_eq!(key(K::Pipe), Some(KeyCode::Backslash));
        assert_eq!(key(K::Questionmark), Some(KeyCode::Slash));
        assert_eq!(key(K::OpenCurlyBracket), Some(KeyCode::BracketLeft));
    }
}
