//! Windows clipboard bridge.
//!
//! Far simpler than X11: the clipboard really is a buffer, so this is a poll
//! loop around Open/Get/SetClipboardData. The sequence number lets us skip the
//! read entirely when nothing has changed.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use anyhow::Context;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use super::{Clipboard, POLL};

pub fn start() -> anyhow::Result<Clipboard> {
    let (set_tx, set_rx) = mpsc::channel::<String>();
    let (changed_tx, changed_rx) = mpsc::channel::<String>();

    std::thread::spawn(move || {
        if let Err(e) = run(set_rx, changed_tx) {
            tracing::warn!("clipboard bridge stopped: {e:#}");
        }
    });

    Ok(Clipboard { set: set_tx, changed: changed_rx })
}

fn run(set_rx: Receiver<String>, changed_tx: Sender<String>) -> anyhow::Result<()> {
    let mut last_sequence = 0u32;
    let mut last_seen = String::new();

    loop {
        match set_rx.try_recv() {
            Ok(text) => {
                if let Err(e) = write_clipboard(&text) {
                    tracing::debug!("could not set the clipboard: {e:#}");
                } else {
                    last_seen = text;
                    // Our own write bumps the sequence; adopt it so the change
                    // is not reported straight back to the viewer.
                    last_sequence = unsafe { GetClipboardSequenceNumber() };
                }
            }
            Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => {}
        }

        let sequence = unsafe { GetClipboardSequenceNumber() };
        if sequence != last_sequence {
            last_sequence = sequence;
            match read_clipboard() {
                Ok(Some(text)) if text != last_seen => {
                    last_seen = text.clone();
                    if changed_tx.send(text).is_err() {
                        return Ok(());
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("could not read the clipboard: {e:#}"),
            }
        }

        std::thread::sleep(POLL);
    }
}

/// Holds the clipboard open for exactly as long as needed. Leaving it open
/// blocks every other application on the desktop, so the close is tied to
/// scope rather than to remembering it on each path.
struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> anyhow::Result<Self> {
        unsafe { OpenClipboard(HWND::default()) }.context("OpenClipboard failed")?;
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn read_clipboard() -> anyhow::Result<Option<String>> {
    let _guard = ClipboardGuard::open()?;
    let handle = match unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) } {
        Ok(h) if !h.is_invalid() => h,
        // Not text; nothing to mirror.
        _ => return Ok(None),
    };

    unsafe {
        let global = HGLOBAL(handle.0);
        let ptr = GlobalLock(global) as *const u16;
        if ptr.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(global);
        Ok(Some(text))
    }
}

fn write_clipboard(text: &str) -> anyhow::Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);

    let _guard = ClipboardGuard::open()?;
    unsafe {
        EmptyClipboard().context("EmptyClipboard failed")?;

        let bytes = utf16.len() * std::mem::size_of::<u16>();
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes).context("GlobalAlloc failed")?;
        let ptr = GlobalLock(global) as *mut u16;
        if ptr.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
        let _ = GlobalUnlock(global);

        // The clipboard owns the block once this succeeds, so it must not be
        // freed here.
        SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(global.0))
            .context("SetClipboardData failed")?;
    }
    Ok(())
}
