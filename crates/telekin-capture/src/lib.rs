//! Screen capture.
//!
//! Platform backends produce tightly packed BGRA frames:
//!
//! * **Windows** — DXGI Desktop Duplication. The GPU hands us the composed
//!   desktop directly; no per-frame GDI blit, and it reports when nothing
//!   changed so idle screens cost nothing.
//! * **Linux** — X11 `GetImage` for now. Works over a plain X session, which
//!   is what most robot dev boxes still run. A PipeWire backend is the
//!   follow-up for Wayland.

use telekin_proto::MonitorInfo;

/// A captured frame: tightly packed BGRA, `width * height * 4` bytes.
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

pub trait ScreenCapture: Send {
    /// Block until the screen changes, or `timeout` elapses.
    ///
    /// This exists so an idle desktop costs nothing: a backend that can be
    /// woken by the display server sleeps in the kernel instead of asking
    /// "anything yet?" on a timer. It also cuts latency, because a change
    /// wakes the pipeline immediately rather than at the next poll.
    ///
    /// The default assumes the backend does its own waiting inside
    /// [`Self::next_frame`], which is how the Windows duplication API works.
    fn wait_for_change(&mut self, _timeout: std::time::Duration) {}

    /// Grab the next frame. Returns `Ok(None)` if the desktop hasn't changed
    /// within the backend's internal wait — the caller should just try again.
    fn next_frame(&mut self) -> anyhow::Result<Option<Frame<'_>>>;

    /// The most recently captured frame, if any. The host re-encodes this on
    /// an idle timer so a viewer joining a static desktop still sees it.
    fn last_frame(&self) -> Option<Frame<'_>>;

    fn dimensions(&self) -> (u32, u32);
}

/// Enumerate monitors available for capture.
pub fn list_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    imp::list_monitors()
}

/// Open a capture session for the given monitor id.
pub fn open(monitor: u32) -> anyhow::Result<Box<dyn ScreenCapture>> {
    imp::open(monitor)
}

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;

    pub fn list_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
        anyhow::bail!("screen capture is not implemented for this platform")
    }

    pub fn open(_monitor: u32) -> anyhow::Result<Box<dyn ScreenCapture>> {
        anyhow::bail!("screen capture is not implemented for this platform")
    }
}
