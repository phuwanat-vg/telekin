//! Input injection via `SendInput`.

use std::collections::HashSet;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    WHEEL_DELTA, XBUTTON1, XBUTTON2,
};

use super::InputInjector;
use telekin_proto::{InputEvent, KeyCode, MouseButton};

pub fn open(monitor: u32, width: u32, height: u32) -> anyhow::Result<Box<dyn InputInjector>> {
    Ok(Box::new(WinInput {
        origin: monitor_origin(monitor)?,
        width,
        height,
        held_keys: HashSet::new(),
        held_buttons: HashSet::new(),
    }))
}

/// Desktop-coordinate origin of the streamed monitor, so normalized viewer
/// coordinates land on the right screen in a multi-monitor setup.
fn monitor_origin(monitor: u32) -> anyhow::Result<(i32, i32)> {
    // Capture enumerates monitors in DXGI order; reuse that list so ids agree.
    let monitors = capture_origins()?;
    monitors
        .get(monitor as usize)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("monitor {monitor} not found"))
}

/// Minimal DXGI output enumeration for desktop origins. Kept here rather than
/// depending on `telekin-capture` so input stays usable without a capture session.
fn capture_origins() -> anyhow::Result<Vec<(i32, i32)>> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut out = Vec::new();
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                let desc = output.GetDesc()?;
                if desc.AttachedToDesktop.as_bool() {
                    out.push((desc.DesktopCoordinates.left, desc.DesktopCoordinates.top));
                }
                oi += 1;
            }
            ai += 1;
        }
        Ok(out)
    }
}

struct WinInput {
    origin: (i32, i32),
    width: u32,
    height: u32,
    held_keys: HashSet<KeyCode>,
    held_buttons: HashSet<MouseButton>,
}

impl WinInput {
    fn send(&self, inputs: &[INPUT]) -> anyhow::Result<()> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        anyhow::ensure!(
            sent as usize == inputs.len(),
            "SendInput injected {sent}/{} events (blocked by UIPI? \
             the host may need to run elevated)",
            inputs.len()
        );
        Ok(())
    }

    fn mouse(&self, dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key_input(&self, scan: u16, extended: bool, down: bool) -> INPUT {
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYBD_EVENT_FLAGS(0x0001); // KEYEVENTF_EXTENDEDKEY
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// Map normalized monitor coordinates onto the 0..65535 virtual-desktop
    /// space that `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` expects.
    fn absolute_point(&self, x: f32, y: f32) -> (i32, i32) {
        let px = self.origin.0 as f32 + x.clamp(0.0, 1.0) * self.width as f32;
        let py = self.origin.1 as f32 + y.clamp(0.0, 1.0) * self.height as f32;
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN) as f32;
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN) as f32;
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as f32;
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as f32;
            let nx = ((px - vx) / vw * 65535.0).round() as i32;
            let ny = ((py - vy) / vh * 65535.0).round() as i32;
            (nx.clamp(0, 65535), ny.clamp(0, 65535))
        }
    }

    fn button_flags(button: MouseButton, down: bool) -> (MOUSE_EVENT_FLAGS, i32) {
        match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
        }
    }
}

impl InputInjector for WinInput {
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::MouseMove { x, y } => {
                let (nx, ny) = self.absolute_point(*x, *y);
                let input = self.mouse(
                    nx,
                    ny,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                );
                self.send(&[input])
            }
            InputEvent::MouseButton { button, down } => {
                let (flags, data) = Self::button_flags(*button, *down);
                let input = self.mouse(0, 0, data, flags);
                self.send(&[input])?;
                if *down {
                    self.held_buttons.insert(*button);
                } else {
                    self.held_buttons.remove(button);
                }
                Ok(())
            }
            InputEvent::MouseWheel { dx: _, dy } => {
                let clicks = (dy * WHEEL_DELTA as f32).round() as i32;
                if clicks == 0 {
                    return Ok(());
                }
                let input = self.mouse(0, 0, clicks, MOUSEEVENTF_WHEEL);
                self.send(&[input])
            }
            InputEvent::Key { key, down } => {
                let (scan, extended) = scancode(*key);
                let input = self.key_input(scan, extended, *down);
                self.send(&[input])?;
                if *down {
                    self.held_keys.insert(*key);
                } else {
                    self.held_keys.remove(key);
                }
                Ok(())
            }
            InputEvent::Text { text } => {
                let mut inputs = Vec::new();
                for unit in text.encode_utf16() {
                    for up in [false, true] {
                        let mut flags = KEYEVENTF_UNICODE;
                        if up {
                            flags |= KEYEVENTF_KEYUP;
                        }
                        inputs.push(INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 {
                                ki: KEYBDINPUT {
                                    wVk: VIRTUAL_KEY(0),
                                    wScan: unit,
                                    dwFlags: flags,
                                    time: 0,
                                    dwExtraInfo: 0,
                                },
                            },
                        });
                    }
                }
                if inputs.is_empty() {
                    return Ok(());
                }
                self.send(&inputs)
            }
        }
    }

    fn release_all(&mut self) -> anyhow::Result<()> {
        let keys: Vec<_> = self.held_keys.drain().collect();
        let buttons: Vec<_> = self.held_buttons.drain().collect();
        let mut inputs = Vec::new();
        for key in keys {
            let (scan, extended) = scancode(key);
            inputs.push(self.key_input(scan, extended, false));
        }
        for button in buttons {
            let (flags, data) = Self::button_flags(button, false);
            inputs.push(self.mouse(0, 0, data, flags));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        self.send(&inputs)
    }
}

/// PS/2 set-1 scan code for a key, plus whether it needs the extended prefix.
/// Scan codes (rather than virtual keys) keep the host's keyboard layout out
/// of the picture: the viewer sends physical keys, the host applies its own
/// layout — which is what you want when W/A/S/D drives a robot.
fn scancode(key: KeyCode) -> (u16, bool) {
    use KeyCode::*;
    match key {
        Escape => (0x01, false),
        Digit1 => (0x02, false),
        Digit2 => (0x03, false),
        Digit3 => (0x04, false),
        Digit4 => (0x05, false),
        Digit5 => (0x06, false),
        Digit6 => (0x07, false),
        Digit7 => (0x08, false),
        Digit8 => (0x09, false),
        Digit9 => (0x0a, false),
        Digit0 => (0x0b, false),
        Minus => (0x0c, false),
        Equal => (0x0d, false),
        Backspace => (0x0e, false),
        Tab => (0x0f, false),
        Q => (0x10, false),
        W => (0x11, false),
        E => (0x12, false),
        R => (0x13, false),
        T => (0x14, false),
        Y => (0x15, false),
        U => (0x16, false),
        I => (0x17, false),
        O => (0x18, false),
        P => (0x19, false),
        BracketLeft => (0x1a, false),
        BracketRight => (0x1b, false),
        Enter => (0x1c, false),
        ControlLeft => (0x1d, false),
        A => (0x1e, false),
        S => (0x1f, false),
        D => (0x20, false),
        F => (0x21, false),
        G => (0x22, false),
        H => (0x23, false),
        J => (0x24, false),
        K => (0x25, false),
        L => (0x26, false),
        Semicolon => (0x27, false),
        Quote => (0x28, false),
        Backquote => (0x29, false),
        ShiftLeft => (0x2a, false),
        Backslash => (0x2b, false),
        Z => (0x2c, false),
        X => (0x2d, false),
        C => (0x2e, false),
        V => (0x2f, false),
        B => (0x30, false),
        N => (0x31, false),
        M => (0x32, false),
        Comma => (0x33, false),
        Period => (0x34, false),
        Slash => (0x35, false),
        ShiftRight => (0x36, false),
        AltLeft => (0x38, false),
        Space => (0x39, false),
        CapsLock => (0x3a, false),
        F1 => (0x3b, false),
        F2 => (0x3c, false),
        F3 => (0x3d, false),
        F4 => (0x3e, false),
        F5 => (0x3f, false),
        F6 => (0x40, false),
        F7 => (0x41, false),
        F8 => (0x42, false),
        F9 => (0x43, false),
        F10 => (0x44, false),
        F11 => (0x57, false),
        F12 => (0x58, false),
        // Extended (E0-prefixed) keys.
        ControlRight => (0x1d, true),
        AltRight => (0x38, true),
        Home => (0x47, true),
        ArrowUp => (0x48, true),
        PageUp => (0x49, true),
        ArrowLeft => (0x4b, true),
        ArrowRight => (0x4d, true),
        End => (0x4f, true),
        ArrowDown => (0x50, true),
        PageDown => (0x51, true),
        Insert => (0x52, true),
        Delete => (0x53, true),
        MetaLeft => (0x5b, true),
        MetaRight => (0x5c, true),
    }
}
