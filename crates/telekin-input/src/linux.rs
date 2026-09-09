//! Input injection via the XTEST extension.
//!
//! XTEST works with a normal user session and needs no root, unlike
//! `/dev/uinput`. It talks in X keycodes, so we translate our portable
//! [`KeyCode`] set through the server's keyboard mapping.

use std::collections::HashSet;

use anyhow::Context;
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::xproto::{ConnectionExt as _, Keycode, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::InputInjector;
use crate::xkeys::{char_keysym, Layout, ScratchPool};
use telekin_proto::{InputEvent, KeyCode, MouseButton};

/// Spare keycodes borrowed for characters the layout lacks. Each stays
/// mapped until the pool comes round to it again, so a client that reads the
/// map late still finds the keysym it was sent.
const SCRATCH_KEYS: usize = 8;


/// Connect to the X server, turning the usual failures into an error that
/// says what to do about it. These are the two things that actually stop
/// people on a robot: a Wayland session (this backend is X11-only) and a
/// headless machine with no X server at all.
fn connect_x11() -> anyhow::Result<(RustConnection, usize)> {
    x11rb::connect(None).map_err(|e| {
        // Order matters: a Wayland desktop usually still has DISPLAY set via
        // XWayland, so an empty DISPLAY is the stronger signal and is checked
        // first. WAYLAND_DISPLAY alone is not conclusive.
        let display = std::env::var("DISPLAY").unwrap_or_default();
        let wayland = std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
        let hint = if display.is_empty() {
            "DISPLAY is not set. On a headless robot, start a virtual display first (`Xvfb :99 -screen 0 1920x1080x24 &`), then pass --display :99"
        } else if wayland {
            "this is a Wayland session and Telekin captures X11 only. Either log in to an Xorg session, or run the desktop under a virtual X display"
        } else {
            "check that an X server is running on this DISPLAY and that XAUTHORITY is readable by this user"
        };
        anyhow::anyhow!("cannot connect to the X display ({e}): {hint}")
    })
}

pub fn open(monitor: u32, width: u32, height: u32) -> anyhow::Result<Box<dyn InputInjector>> {
    Ok(Box::new(XTestInput::new(monitor, width, height)?))
}

struct XTestInput {
    conn: RustConnection,
    root: Window,
    width: u32,
    height: u32,
    /// Our [`KeyCode`] -> X keycode, resolved from the live keyboard mapping.
    keymap: std::collections::HashMap<KeyCode, Keycode>,
    /// What the server's layout can type, for characters that arrive as text.
    layout: Layout,
    /// Spare keycodes for characters the layout cannot type.
    scratch: ScratchPool,
    /// X keycodes of the Shift keys, so typing can tell whether Shift is held.
    shift_keys: Vec<Keycode>,
    held_keys: HashSet<Keycode>,
    held_buttons: HashSet<u8>,
}

impl XTestInput {
    fn new(monitor: u32, width: u32, height: u32) -> anyhow::Result<Self> {
        let (conn, _) = connect_x11()?;
        conn.extension_information(x11rb::protocol::xtest::X11_EXTENSION_NAME)?
            .context("X server has no XTEST extension; cannot inject input")?;
        let root = conn
            .setup()
            .roots
            .get(monitor as usize)
            .with_context(|| format!("X11 screen {monitor} not found"))?
            .root;

        let mut me = Self {
            conn,
            root,
            width,
            height,
            keymap: Default::default(),
            layout: Layout::from_mapping(0, 0, &[]),
            scratch: ScratchPool::new(&[], 0),
            shift_keys: Vec::new(),
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
        };
        me.build_keymap()?;
        Ok(me)
    }

    /// Build the [`KeyCode`] -> X keycode table by reading the server's
    /// mapping and matching keysyms. Doing it at runtime (rather than
    /// hardcoding the usual evdev offsets) keeps non-standard layouts working.
    fn build_keymap(&mut self) -> anyhow::Result<()> {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let count = max - min + 1;
        let mapping = self
            .conn
            .get_keyboard_mapping(min, count)?
            .reply()
            .context("GetKeyboardMapping failed")?;
        let per = mapping.keysyms_per_keycode as usize;
        self.layout = Layout::from_mapping(min, per, &mapping.keysyms);

        for key in ALL_KEYS {
            if let Some((kc, _)) = self.layout.lookup(keysym(*key)) {
                self.keymap.insert(*key, kc);
            }
        }
        self.shift_keys = [KeyCode::ShiftLeft, KeyCode::ShiftRight]
            .iter()
            .filter_map(|k| self.keymap.get(k).copied())
            .collect();
        self.scratch = ScratchPool::new(self.layout.free_keycodes(), SCRATCH_KEYS);
        if self.scratch.is_empty() {
            tracing::warn!(
                "the X keyboard mapping has no free keycode; characters outside the robot's layout cannot be typed"
            );
        }
        Ok(())
    }

    fn fake_key(&mut self, keycode: Keycode, down: bool) -> anyhow::Result<()> {
        let ty = if down { 2 } else { 3 }; // KeyPress / KeyRelease
        self.conn.xtest_fake_input(ty, keycode, 0, self.root, 0, 0, 0)?;
        self.conn.flush()?;
        if down {
            self.held_keys.insert(keycode);
        } else {
            self.held_keys.remove(&keycode);
        }
        Ok(())
    }

    fn fake_button(&mut self, button: u8, down: bool) -> anyhow::Result<()> {
        let ty = if down { 4 } else { 5 }; // ButtonPress / ButtonRelease
        self.conn.xtest_fake_input(ty, button, 0, self.root, 0, 0, 0)?;
        self.conn.flush()?;
        if down {
            self.held_buttons.insert(button);
        } else {
            self.held_buttons.remove(&button);
        }
        Ok(())
    }

    fn click_button(&mut self, button: u8) -> anyhow::Result<()> {
        self.fake_button(button, true)?;
        self.fake_button(button, false)
    }

    /// Type one character.
    ///
    /// A character the layout already has is typed on its own key, with
    /// Shift pressed or lifted around it as the level demands — the keystroke
    /// a person at the robot would make, which every client reads alike.
    /// Anything else borrows a keycode from the scratch pool. The old way,
    /// one scratch keycode remapped and reset around every press, lost
    /// characters: a client that fetched the map after the reset found no
    /// keysym and typed nothing, and on a slow board that was the usual
    /// order of events.
    fn type_char(&mut self, ch: char) -> anyhow::Result<()> {
        let sym = char_keysym(ch);
        if let Some((kc, shifted)) = self.layout.lookup(sym) {
            return self.tap_with_shift(kc, shifted);
        }
        let Some((kc, remap)) = self.scratch.slot_for(sym) else {
            tracing::warn!("no free keycode for text injection; dropping {ch:?}");
            return Ok(());
        };
        if remap {
            self.conn.change_keyboard_mapping(1, kc, 1, &[sym])?;
            self.conn.flush()?;
            // The server must have applied the mapping before the press.
            self.conn.sync()?;
        }
        // One keysym per keycode: with Shift the server would give its
        // uppercase form, so type it plain.
        self.tap_with_shift(kc, false)
    }

    /// Press and release `kc` with Shift in the wanted state, then put Shift
    /// back the way the operator has it. The temporary Shift changes bypass
    /// `held_keys` on purpose: that set mirrors the operator's fingers.
    fn tap_with_shift(&mut self, kc: Keycode, shifted: bool) -> anyhow::Result<()> {
        let held: Vec<Keycode> = self
            .shift_keys
            .iter()
            .copied()
            .filter(|s| self.held_keys.contains(s))
            .collect();
        let lift = !shifted && !held.is_empty();
        let add = shifted && held.is_empty();
        let shift = self.shift_keys.first().copied();
        if lift {
            for s in &held {
                self.raw_key(*s, false)?;
            }
        }
        if add {
            if let Some(s) = shift {
                self.raw_key(s, true)?;
            }
        }
        self.raw_key(kc, true)?;
        self.raw_key(kc, false)?;
        if add {
            if let Some(s) = shift {
                self.raw_key(s, false)?;
            }
        }
        if lift {
            for s in &held {
                self.raw_key(*s, true)?;
            }
        }
        self.conn.flush()?;
        Ok(())
    }

    /// A key event with no bookkeeping.
    fn raw_key(&mut self, kc: Keycode, down: bool) -> anyhow::Result<()> {
        let ty = if down { 2 } else { 3 }; // KeyPress / KeyRelease
        self.conn.xtest_fake_input(ty, kc, 0, self.root, 0, 0, 0)?;
        Ok(())
    }
}

impl Drop for XTestInput {
    fn drop(&mut self) {
        // Give the borrowed keycodes back.
        for kc in self.scratch.mapped().collect::<Vec<_>>() {
            let _ = self.conn.change_keyboard_mapping(1, kc, 1, &[0]);
        }
        let _ = self.conn.flush();
    }
}

impl InputInjector for XTestInput {
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::MouseMove { x, y } => {
                let px = (x.clamp(0.0, 1.0) * self.width as f32).round() as i16;
                let py = (y.clamp(0.0, 1.0) * self.height as f32).round() as i16;
                // MotionNotify with is_absolute (detail = 0).
                self.conn
                    .xtest_fake_input(6, 0, 0, self.root, px, py, 0)?;
                self.conn.flush()?;
                Ok(())
            }
            InputEvent::MouseButton { button, down } => {
                let b = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                    MouseButton::Back => 8,
                    MouseButton::Forward => 9,
                };
                self.fake_button(b, *down)
            }
            InputEvent::MouseWheel { dx, dy } => {
                // X11 models scroll as button clicks: 4 up, 5 down, 6 left, 7 right.
                for _ in 0..wheel_clicks(*dy) {
                    self.click_button(if *dy > 0.0 { 4 } else { 5 })?;
                }
                for _ in 0..wheel_clicks(*dx) {
                    self.click_button(if *dx > 0.0 { 7 } else { 6 })?;
                }
                Ok(())
            }
            InputEvent::Key { key, down } => {
                let Some(kc) = self.keymap.get(key).copied() else {
                    tracing::debug!("key {key:?} not present in host layout; ignoring");
                    return Ok(());
                };
                self.fake_key(kc, *down)
            }
            InputEvent::Text { text } => {
                for ch in text.chars() {
                    self.type_char(ch)?;
                }
                Ok(())
            }
        }
    }

    fn release_all(&mut self) -> anyhow::Result<()> {
        for kc in self.held_keys.drain().collect::<Vec<_>>() {
            self.conn.xtest_fake_input(3, kc, 0, self.root, 0, 0, 0)?;
        }
        for b in self.held_buttons.drain().collect::<Vec<_>>() {
            self.conn.xtest_fake_input(5, b, 0, self.root, 0, 0, 0)?;
        }
        self.conn.flush()?;
        Ok(())
    }
}

const ALL_KEYS: &[KeyCode] = {
    use KeyCode::*;
    &[
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
        ShiftLeft, ShiftRight, ControlLeft, ControlRight, AltLeft, AltRight, MetaLeft, MetaRight,
        Escape, Tab, CapsLock, Space, Enter, Backspace, Delete, Insert,
        Home, End, PageUp, PageDown, ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
        Minus, Equal, BracketLeft, BracketRight, Backslash,
        Semicolon, Quote, Backquote, Comma, Period, Slash,
    ]
};

/// X keysym for each portable key.
fn keysym(key: KeyCode) -> u32 {
    use KeyCode::*;
    match key {
        A => 0x061, B => 0x062, C => 0x063, D => 0x064, E => 0x065, F => 0x066,
        G => 0x067, H => 0x068, I => 0x069, J => 0x06a, K => 0x06b, L => 0x06c,
        M => 0x06d, N => 0x06e, O => 0x06f, P => 0x070, Q => 0x071, R => 0x072,
        S => 0x073, T => 0x074, U => 0x075, V => 0x076, W => 0x077, X => 0x078,
        Y => 0x079, Z => 0x07a,
        Digit0 => 0x030, Digit1 => 0x031, Digit2 => 0x032, Digit3 => 0x033,
        Digit4 => 0x034, Digit5 => 0x035, Digit6 => 0x036, Digit7 => 0x037,
        Digit8 => 0x038, Digit9 => 0x039,
        F1 => 0xffbe, F2 => 0xffbf, F3 => 0xffc0, F4 => 0xffc1, F5 => 0xffc2,
        F6 => 0xffc3, F7 => 0xffc4, F8 => 0xffc5, F9 => 0xffc6, F10 => 0xffc7,
        F11 => 0xffc8, F12 => 0xffc9,
        ShiftLeft => 0xffe1, ShiftRight => 0xffe2,
        ControlLeft => 0xffe3, ControlRight => 0xffe4,
        AltLeft => 0xffe9, AltRight => 0xffea,
        MetaLeft => 0xffeb, MetaRight => 0xffec,
        Escape => 0xff1b, Tab => 0xff09, CapsLock => 0xffe5, Space => 0x020,
        Enter => 0xff0d, Backspace => 0xff08, Delete => 0xffff, Insert => 0xff63,
        Home => 0xff50, End => 0xff57, PageUp => 0xff55, PageDown => 0xff56,
        ArrowUp => 0xff52, ArrowDown => 0xff54, ArrowLeft => 0xff51, ArrowRight => 0xff53,
        Minus => 0x02d, Equal => 0x03d, BracketLeft => 0x05b, BracketRight => 0x05d,
        Backslash => 0x05c, Semicolon => 0x03b, Quote => 0x027, Backquote => 0x060,
        Comma => 0x02c, Period => 0x02e, Slash => 0x02f,
    }
}

/// Button clicks for one scroll event.
///
/// A scroll that arrived should scroll: any non-zero delta is worth at least
/// one click, because rounding a small one to zero is indistinguishable from
/// a dead wheel. The ceiling stops a bad delta from injecting thousands.
fn wheel_clicks(delta: f32) -> i32 {
    const MAX: i32 = 20;
    let magnitude = delta.abs();
    if magnitude <= f32::EPSILON {
        return 0;
    }
    (magnitude.round() as i32).clamp(1, MAX)
}

#[cfg(test)]
mod tests {
    use super::wheel_clicks;

    #[test]
    fn a_whole_line_is_one_click() {
        assert_eq!(wheel_clicks(1.0), 1);
        assert_eq!(wheel_clicks(-1.0), 1);
    }

    #[test]
    fn a_small_delta_still_scrolls() {
        // The bug this guards: 0.025 used to round to zero clicks.
        assert_eq!(wheel_clicks(0.025), 1);
    }

    #[test]
    fn nothing_means_nothing() {
        assert_eq!(wheel_clicks(0.0), 0);
    }

    #[test]
    fn a_runaway_delta_is_capped() {
        assert_eq!(wheel_clicks(10_000.0), 20);
    }
}

/// Needs a live X server with XTEST (WSLg, Xvfb, a desktop). Types through
/// the injector into a window of our own and reads back what the server
/// delivered, interpreting each keycode with the mapping *as it stands when
/// the event is read* — the way a real client does, and what the old
/// one-scratch-keycode approach lost characters to.
#[cfg(test)]
mod live {
    use super::*;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;

    fn keysym_char(sym: u32) -> Option<char> {
        if sym == 0 {
            return None;
        }
        if sym < 0x100 {
            char::from_u32(sym)
        } else if sym & 0xff00_0000 == 0x0100_0000 {
            char::from_u32(sym & 0x00ff_ffff)
        } else {
            None
        }
    }

    #[test]
    #[ignore = "needs an X display with XTEST"]
    fn typed_text_arrives_as_the_same_characters() {
        let (conn, screen_num) = x11rb::connect(None).expect("X display");
        let screen = &conn.setup().roots[screen_num];
        let win = conn.generate_id().unwrap();
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            screen.root,
            0,
            0,
            200,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME).unwrap();
        conn.sync().unwrap();
        while conn.poll_for_event().unwrap().is_some() {}

        let mut inj = XTestInput::new(0, screen.width_in_pixels.into(), screen.height_in_pixels.into())
            .expect("injector");
        // Plain, shifted, uppercase, space, then Thai (not in a Latin
        // layout, so through the scratch pool), then a repeat of the Thai to
        // exercise "already mapped".
        let text = "-:*aA \u{e01}\u{e02}\u{e01}";
        inj.inject(&InputEvent::Text { text: text.into() }).unwrap();
        // Now with Shift held by the "operator": ':' must still come out as
        // ':' and '-' as '-', whatever Shift would have made of the key.
        inj.inject(&InputEvent::Key { key: KeyCode::ShiftLeft, down: true }).unwrap();
        inj.inject(&InputEvent::Text { text: ":-".into() }).unwrap();
        inj.inject(&InputEvent::Key { key: KeyCode::ShiftLeft, down: false }).unwrap();
        let expected = format!("{text}:-");

        let mut got = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while got.chars().count() < expected.chars().count() && std::time::Instant::now() < deadline {
            match conn.poll_for_event().unwrap() {
                Some(Event::KeyPress(e)) => {
                    let m = conn.get_keyboard_mapping(e.detail, 1).unwrap().reply().unwrap();
                    let per = m.keysyms_per_keycode as usize;
                    let shift = e.state.contains(KeyButMask::SHIFT);
                    let sym = if shift && per > 1 && m.keysyms[1] != 0 { m.keysyms[1] } else { m.keysyms[0] };
                    if let Some(c) = keysym_char(sym) {
                        got.push(c);
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        assert_eq!(got, expected);
    }
}
