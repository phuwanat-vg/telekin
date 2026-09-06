//! Clipboard sharing with the host's session.
//!
//! Both platforms are served by the same shape: a background thread owning
//! whatever handle the OS needs, a channel to push text *into* the host's
//! clipboard, and a channel reporting when the host's clipboard *changed*.
//!
//! A thread is not optional on X11. The clipboard there is not a buffer you
//! read and write — it is an ownership protocol. Whoever last copied owns the
//! selection and must stay alive to answer `SelectionRequest` events from
//! whoever pastes. Setting the clipboard therefore means becoming its owner
//! and continuing to serve requests, which needs somewhere to run.

use std::sync::mpsc::{Receiver, Sender};

/// A live clipboard bridge. Dropping it stops the background thread and, on
/// X11, gives up ownership of the selection.
pub struct Clipboard {
    /// Text to place on the host's clipboard.
    pub set: Sender<String>,
    /// Text the host's clipboard changed to.
    pub changed: Receiver<String>,
}

/// Start the bridge. Returns `Err` when the platform backend cannot be
/// reached; clipboard sharing is a convenience, so callers should carry on
/// without it rather than failing the session.
pub fn start() -> anyhow::Result<Clipboard> {
    imp::start()
}

/// How often to re-read the host clipboard when we do not own it.
///
/// X11 can notify on change through XFIXES, but polling avoids depending on
/// another extension for a feature that copies a few bytes a second at most.
#[allow(dead_code)]
const POLL: std::time::Duration = std::time::Duration::from_millis(400);

#[cfg(target_os = "linux")]
#[path = "clipboard_linux.rs"]
mod imp;

#[cfg(windows)]
#[path = "clipboard_windows.rs"]
mod imp;

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn start() -> anyhow::Result<super::Clipboard> {
        anyhow::bail!("clipboard sharing is not implemented for this platform")
    }
}
