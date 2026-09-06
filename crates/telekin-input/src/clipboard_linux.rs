//! X11 clipboard bridge.
//!
//! Owns a hidden window on its own connection. It answers `SelectionRequest`
//! for text we were told to publish, and otherwise polls the CLIPBOARD
//! selection so a copy made inside the session reaches the viewer.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Instant;

use anyhow::Context;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use super::{Clipboard, POLL};

/// Atoms the selection protocol needs, resolved once.
struct Atoms {
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
    /// Where a conversion result is delivered on our own window.
    dest: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> anyhow::Result<Self> {
        let get = |name: &str| -> anyhow::Result<Atom> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        Ok(Self {
            clipboard: get("CLIPBOARD")?,
            utf8: get("UTF8_STRING")?,
            targets: get("TARGETS")?,
            dest: get("TELEKIN_CLIPBOARD")?,
        })
    }
}

pub fn start() -> anyhow::Result<Clipboard> {
    let (set_tx, set_rx) = mpsc::channel::<String>();
    let (changed_tx, changed_rx) = mpsc::channel::<String>();

    // Connect on this thread so a failure is reported to the caller rather
    // than disappearing into a background thread.
    let (conn, screen_num) =
        x11rb::connect(None).context("clipboard: cannot connect to the X display")?;
    let atoms = Atoms::intern(&conn)?;
    let root = conn.setup().roots[screen_num].root;

    let window = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?
    .check()
    .context("clipboard: could not create the helper window")?;

    std::thread::spawn(move || {
        if let Err(e) = run(conn, window, atoms, set_rx, changed_tx) {
            tracing::warn!("clipboard bridge stopped: {e:#}");
        }
    });

    Ok(Clipboard { set: set_tx, changed: changed_rx })
}

fn run(
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    set_rx: Receiver<String>,
    changed_tx: Sender<String>,
) -> anyhow::Result<()> {
    // What we publish when we own the selection, and the last text we saw on
    // the clipboard from any source. They are compared to avoid echoing a
    // paste straight back to the viewer that sent it.
    let mut owned: Option<String> = None;
    let mut last_seen = String::new();
    let mut next_poll = Instant::now();

    loop {
        match set_rx.try_recv() {
            Ok(text) => {
                // Take ownership so other applications ask us for the text.
                conn.set_selection_owner(window, atoms.clipboard, x11rb::CURRENT_TIME)?;
                conn.flush()?;
                last_seen = text.clone();
                owned = Some(text);
            }
            Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => {}
        }

        while let Some(event) = conn.poll_for_event()? {
            match event {
                Event::SelectionRequest(req) => {
                    serve(&conn, &atoms, &req, owned.as_deref())?;
                }
                Event::SelectionClear(_) => {
                    // Something else copied; stop claiming to hold the text.
                    owned = None;
                }
                _ => {}
            }
        }

        if owned.is_none() && Instant::now() >= next_poll {
            next_poll = Instant::now() + POLL;
            if let Some(text) = read_clipboard(&conn, window, &atoms)? {
                if text != last_seen {
                    last_seen = text.clone();
                    if changed_tx.send(text).is_err() {
                        return Ok(());
                    }
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Answer another application's request for our clipboard text.
fn serve(
    conn: &RustConnection,
    atoms: &Atoms,
    req: &SelectionRequestEvent,
    owned: Option<&str>,
) -> anyhow::Result<()> {
    // `property == NONE` marks a refusal, which is the correct answer for a
    // target we cannot supply.
    let mut property = x11rb::NONE;

    if let Some(text) = owned {
        if req.target == atoms.targets {
            // Advertise only what we can actually convert to.
            let targets = [atoms.targets, atoms.utf8, u32::from(AtomEnum::STRING)];
            conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                req.property,
                AtomEnum::ATOM,
                &targets,
            )?;
            property = req.property;
        } else if req.target == atoms.utf8 || req.target == u32::from(AtomEnum::STRING) {
            conn.change_property8(
                PropMode::REPLACE,
                req.requestor,
                req.property,
                req.target,
                text.as_bytes(),
            )?;
            property = req.property;
        }
    }

    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property,
    };
    conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
    conn.flush()?;
    Ok(())
}

/// Ask the current owner to convert the clipboard to UTF-8 and read it back.
fn read_clipboard(
    conn: &RustConnection,
    window: Window,
    atoms: &Atoms,
) -> anyhow::Result<Option<String>> {
    conn.convert_selection(
        window,
        atoms.clipboard,
        atoms.utf8,
        atoms.dest,
        x11rb::CURRENT_TIME,
    )?;
    conn.flush()?;

    // The owner replies asynchronously, and may not reply at all — an empty
    // clipboard has no owner. Give it a bounded window rather than blocking
    // the bridge.
    let deadline = Instant::now() + std::time::Duration::from_millis(150);
    while Instant::now() < deadline {
        while let Some(event) = conn.poll_for_event()? {
            match event {
                Event::SelectionNotify(n) if n.property != x11rb::NONE => {
                    let reply = conn
                        .get_property(
                            true,
                            window,
                            atoms.dest,
                            AtomEnum::ANY,
                            0,
                            u32::MAX / 4,
                        )?
                        .reply()?;
                    let text = String::from_utf8_lossy(&reply.value).into_owned();
                    return Ok(Some(text));
                }
                // The selection is empty or the owner refused.
                Event::SelectionNotify(_) => return Ok(None),
                // Requests can arrive while we wait; answering them is the
                // caller's job on the next pass.
                _ => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(None)
}
