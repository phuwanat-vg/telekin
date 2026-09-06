//! Input injection into the host desktop.
//!
//! Coordinates arriving from the viewer are normalized to the streamed
//! monitor (`0.0..=1.0`); the backend maps them onto that monitor's pixels.

pub mod clipboard;

use telekin_proto::InputEvent;

pub trait InputInjector: Send {
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()>;
    /// Release every key/button currently held. Called when a session ends so
    /// a dropped connection cannot leave a robot driving on a stuck key.
    fn release_all(&mut self) -> anyhow::Result<()>;
}

/// Open an injector targeting the given monitor, whose size is `width` x `height`.
pub fn open(monitor: u32, width: u32, height: u32) -> anyhow::Result<Box<dyn InputInjector>> {
    imp::open(monitor, width, height)
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

    pub fn open(_m: u32, _w: u32, _h: u32) -> anyhow::Result<Box<dyn InputInjector>> {
        anyhow::bail!("input injection is not implemented for this platform")
    }
}
