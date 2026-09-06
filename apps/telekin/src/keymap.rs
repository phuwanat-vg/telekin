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

        _ => return None,
    })
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
