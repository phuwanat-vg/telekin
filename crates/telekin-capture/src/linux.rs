//! X11 capture backend.
//!
//! Uses the MIT-SHM extension when available (the image lands in shared
//! memory, so there is no round-trip copy through the X socket) and falls
//! back to a plain `GetImage` otherwise — which is what you get over an
//! `ssh -X` style connection.
//!
//! Change detection uses the XDAMAGE extension: the server reports which
//! rectangles were repainted, so an idle desktop costs no framebuffer grab at
//! all, and a small edit grabs only its own rectangle instead of 8 MB. When
//! the extension is missing we fall back to grabbing everything and comparing
//! against the previous frame, which is correct but far more expensive.
//!
//! Wayland needs a PipeWire/xdg-desktop-portal backend instead; that is a
//! separate module, not a variation of this one.

use std::sync::{Arc, Condvar, Mutex};

use anyhow::Context;
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Rectangle, Screen};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::{Frame, ScreenCapture};
use telekin_proto::MonitorInfo;


/// Connect to the X server, turning the usual failures into an error that
/// says what to do about it. These are the two things that actually stop
/// people on a robot: a Wayland session (this backend is X11-only) and a
/// headless machine with no X server at all.
/// Connect to the display this host should capture.
///
/// When `DISPLAY` is set — a desktop launcher, a systemd user unit, or an
/// explicit `--display` — that is the answer and a failure is reported as-is.
/// When it is not, the host was almost certainly started over SSH, where the
/// environment carries no session at all. Rather than declare the robot
/// screenless, look for one.
fn connect_x11() -> anyhow::Result<(RustConnection, usize)> {
    if let Ok(display) = std::env::var("DISPLAY") {
        if !display.is_empty() {
            return x11rb::connect(None).map_err(|e| explain(&display, e));
        }
    }

    // Serialised because the only way to hand x11rb a cookie is through the
    // environment, which is process-wide. Sessions are rare events, so a lock
    // held for the length of a connect costs nothing.
    static PROBE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = PROBE.lock().unwrap_or_else(|e| e.into_inner());

    let displays = local_displays();
    let cookies = readable_cookies();
    let mut last: Option<anyhow::Error> = None;

    // Named `dpy`, not `display`: `tracing`'s macros bring a function called
    // `display` into scope, and an inline `{display}` capture resolves to that
    // instead of to the local.
    for dpy in &displays {
        for cookie in &cookies {
            match cookie {
                Some(path) => std::env::set_var("XAUTHORITY", path),
                None => std::env::remove_var("XAUTHORITY"),
            }
            match x11rb::connect(Some(dpy)) {
                Ok(conn) => {
                    // Kept for every later connection in this process, so a
                    // capture thread does not have to repeat the search.
                    std::env::set_var("DISPLAY", dpy);
                    let using = match cookie {
                        Some(path) => format!(" using {}", path.display()),
                        None => String::new(),
                    };
                    tracing::info!("found a desktop on {dpy}{using}");
                    return Ok(conn);
                }
                Err(e) => last = Some(anyhow::anyhow!("{e}")),
            }
        }
    }

    let tried = if displays.is_empty() {
        "no X server is running on this machine".to_string()
    } else {
        format!("tried {}", displays.join(", "))
    };
    Err(anyhow::anyhow!(
        "no desktop session this account can reach ({tried}). Sign in on the robot,          or start a virtual display (`Xvfb :99 -screen 0 1920x1080x24 &`) and pass          --display :99. Last error: {}",
        last.map(|e| e.to_string()).unwrap_or_else(|| "none".into())
    ))
}

/// Display numbers with a socket on this machine, lowest first.
///
/// The login-screen server is usually a high number (`:1001` on GDM) and a
/// real session a low one, so ordering makes the first hit the useful one.
fn local_displays() -> Vec<String> {
    let mut found: Vec<u32> = std::fs::read_dir("/tmp/.X11-unix")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_prefix('X'))
                .and_then(|n| n.parse::<u32>().ok())
        })
        .collect();
    found.sort_unstable();
    found.iter().map(|n| format!(":{n}")).collect()
}

/// Cookie files worth trying, most likely first.
///
/// `/run/user/<uid>/gdm/Xauthority` is the one that matters on Ubuntu: it
/// belongs to the signed-in user and is the only readable route to their
/// session. `~/.Xauthority` is often present but empty. `None` means "try
/// with no cookie at all", which is what an `xhost +` setup needs.
fn readable_cookies() -> Vec<Option<std::path::PathBuf>> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("XAUTHORITY").map(std::path::PathBuf::from) {
        out.push(Some(path));
    }
    let uid = unsafe { libc_getuid() };
    let run = std::path::PathBuf::from(format!("/run/user/{uid}"));
    out.push(Some(run.join("gdm/Xauthority")));
    if let Some(home) = std::env::var_os("HOME") {
        out.push(Some(std::path::PathBuf::from(home).join(".Xauthority")));
    }
    // Xwayland writes its cookie here when a session is Wayland with XWayland.
    if let Ok(entries) = std::fs::read_dir(&run) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".mutter-Xwaylandauth") {
                out.push(Some(entry.path()));
            }
        }
    }
    out.push(None);
    out.retain(|c| c.as_ref().is_none_or(|p| p.exists()));
    out
}

/// This process's real user id.
///
/// Reading `/proc/self/status` rather than taking a dependency on `libc` for
/// one number, since this crate otherwise has none on Linux.
unsafe fn libc_getuid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|l| l.split_whitespace().next().map(str::to_owned))
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or(1000)
}

fn explain(display: &str, e: impl std::fmt::Display) -> anyhow::Error {
    // A Wayland desktop usually still has DISPLAY set via XWayland, so an
    // empty DISPLAY is the stronger signal; WAYLAND_DISPLAY alone is not.
    let wayland = std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
    let hint = if wayland {
        "this is a Wayland session and Telekin captures X11 only. Either log in to an Xorg session, or run the desktop under a virtual X display"
    } else {
        "check that an X server is running on this DISPLAY and that XAUTHORITY is readable by this user"
    };
    anyhow::anyhow!("cannot connect to the X display {display} ({e}): {hint}")
}

pub fn list_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    let (conn, _) = connect_x11()?;
    Ok(conn
        .setup()
        .roots
        .iter()
        .enumerate()
        .map(|(i, screen)| MonitorInfo {
            id: i as u32,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
            name: format!("X11 screen {i}"),
        })
        .collect())
}

pub fn open(monitor: u32) -> anyhow::Result<Box<dyn ScreenCapture>> {
    Ok(Box::new(X11Capture::new(monitor)?))
}

struct X11Capture {
    conn: RustConnection,
    screen: Screen,
    width: u32,
    height: u32,
    /// Tightly packed BGRA output — the most recently captured frame.
    buffer: Vec<u8>,
    /// The frame before it, kept so an unchanged grab can be detected and
    /// skipped. X11 has no damage events, so without this the pipeline
    /// re-encodes a static desktop every frame — pure waste on an SBC.
    prev: Vec<u8>,
    have_frame: bool,
    shm: Option<ShmBuffer>,
    /// Blocking damage watcher, when the server has the extension.
    damage: Option<DamageWatcher>,
    /// Scratch for the pixels of one damaged rectangle.
    tile: Vec<u8>,
    /// Rectangles reported by the last damage poll.
    rects: Vec<Rectangle>,
    /// Full re-grab deadline. Damage can be missed (a compositor that paints
    /// without reporting, or an event lost while we were busy), so the whole
    /// screen is re-read on a slow timer and compared as a safety net.
    last_full_grab: std::time::Instant,
}

/// How often to re-read the whole screen even when damage reports nothing.
const FULL_RESYNC: std::time::Duration = std::time::Duration::from_secs(2);

/// Damage rectangles collected by the watcher thread, and the signal that
/// some arrived.
#[derive(Default)]
struct DamageInbox {
    rects: Vec<Rectangle>,
    /// Set when more rectangles arrived than are worth tracking, or when the
    /// watcher decided the screen is wholly dirty.
    overflow: bool,
    /// Bumped on every batch, so a waiter can tell "nothing yet" from "a
    /// batch arrived and was already taken".
    generation: u64,
}

/// Blocks on the X connection so an idle desktop costs no CPU.
///
/// The alternative — asking for damage on a timer — burns a round trip per
/// frame forever and still reports a change up to one frame late. Here the
/// server wakes us the moment something is repainted.
struct DamageWatcher {
    inbox: Arc<(Mutex<DamageInbox>, Condvar)>,
    /// Generation already consumed by the capture side.
    seen: u64,
}

impl DamageWatcher {
    fn start(display_hint: Option<&str>, width: u32, height: u32) -> anyhow::Result<Self> {
        let (conn, screen_num) = match display_hint {
            Some(d) => x11rb::connect(Some(d))?,
            None => x11rb::connect(None)?,
        };
        if conn.extension_information(damage::X11_EXTENSION_NAME)?.is_none() {
            anyhow::bail!("server has no DAMAGE extension");
        }
        conn.damage_query_version(1, 1)?.reply()?;

        let root = conn.setup().roots[screen_num].root;
        let id = conn.generate_id()?;
        conn.damage_create(id, root, damage::ReportLevel::DELTA_RECTANGLES)?
            .check()
            .context("damage_create rejected")?;

        let inbox = Arc::new((Mutex::new(DamageInbox::default()), Condvar::new()));
        let thread_inbox = inbox.clone();

        std::thread::spawn(move || {
            loop {
                // Blocks in the kernel until the server has something to say.
                let first = match conn.wait_for_event() {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::debug!("damage watcher stopped: {e}");
                        return;
                    }
                };

                let mut batch: Vec<Rectangle> = Vec::new();
                let mut overflow = false;
                let consider = |event: Event, batch: &mut Vec<Rectangle>| {
                    if let Event::DamageNotify(n) = event {
                        if batch.len() >= MAX_DAMAGE_EVENTS {
                            return true;
                        }
                        let r = clamp_rect(n.area, width, height);
                        if r.width > 0 && r.height > 0 {
                            batch.push(r);
                        }
                    }
                    false
                };
                overflow |= consider(first, &mut batch);

                // Drain whatever else arrived in the same wake-up.
                loop {
                    match conn.poll_for_event() {
                        Ok(Some(e)) => overflow |= consider(e, &mut batch),
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!("damage watcher stopped: {e}");
                            return;
                        }
                    }
                }

                // Acknowledge, or the server stops reporting.
                if conn.damage_subtract(id, x11rb::NONE, x11rb::NONE).is_err() {
                    return;
                }
                if conn.flush().is_err() {
                    return;
                }

                let (lock, cv) = &*thread_inbox;
                let mut inbox = lock.lock().expect("damage inbox poisoned");
                if overflow || inbox.rects.len() + batch.len() > MAX_DAMAGE_EVENTS {
                    inbox.overflow = true;
                    inbox.rects.clear();
                } else {
                    inbox.rects.append(&mut batch);
                }
                inbox.generation += 1;
                cv.notify_all();
            }
        });

        Ok(Self { inbox, seen: 0 })
    }

    /// Block until a batch arrives or `timeout` elapses.
    fn wait(&mut self, timeout: std::time::Duration) {
        let (lock, cv) = &*self.inbox;
        let guard = lock.lock().expect("damage inbox poisoned");
        let _unused = cv
            .wait_timeout_while(guard, timeout, |inbox| inbox.generation == self.seen)
            .expect("damage inbox poisoned");
    }

    /// Take everything reported since the last call. `None` means the screen
    /// should be treated as wholly dirty.
    fn take(&mut self, out: &mut Vec<Rectangle>) -> Option<usize> {
        let (lock, _) = &*self.inbox;
        let mut inbox = lock.lock().expect("damage inbox poisoned");
        self.seen = inbox.generation;
        out.clear();
        if inbox.overflow {
            inbox.overflow = false;
            inbox.rects.clear();
            return None;
        }
        std::mem::swap(out, &mut inbox.rects);
        Some(out.len())
    }
}

/// Damage rectangles to track before giving up and treating the frame as
/// fully dirty. Bounds the per-frame cost when the screen is animating.
const MAX_DAMAGE_EVENTS: usize = 32;

/// A shared-memory segment mapped into both our address space and the X server's.
struct ShmBuffer {
    seg: shm::Seg,
    shmid: libc::c_int,
    addr: *mut libc::c_void,
    len: usize,
}

// The mapping is owned exclusively by the capture session, which is itself
// moved between threads as a unit.
unsafe impl Send for ShmBuffer {}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::shmdt(self.addr);
            libc::shmctl(self.shmid, libc::IPC_RMID, std::ptr::null_mut());
        }
    }
}

impl X11Capture {
    fn new(monitor: u32) -> anyhow::Result<Self> {
        let (conn, _) = connect_x11()?;
        let screen = conn
            .setup()
            .roots
            .get(monitor as usize)
            .with_context(|| format!("X11 screen {monitor} not found"))?
            .clone();
        let width = screen.width_in_pixels as u32;
        let height = screen.height_in_pixels as u32;

        let shm = Self::try_setup_shm(&conn, width, height).unwrap_or_else(|e| {
            tracing::warn!("MIT-SHM unavailable ({e:#}); falling back to GetImage");
            None
        });

        let damage = match DamageWatcher::start(None, width, height) {
            Ok(w) => {
                tracing::info!("using XDAMAGE to track screen changes");
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    "XDAMAGE unavailable ({e:#}); every frame will be grabbed and compared"
                );
                None
            }
        };

        Ok(Self {
            conn,
            screen,
            width,
            height,
            buffer: Vec::new(),
            prev: Vec::new(),
            have_frame: false,
            shm,
            damage,
            tile: Vec::new(),
            rects: Vec::new(),
            last_full_grab: std::time::Instant::now(),
        })
    }

    fn try_setup_shm(
        conn: &RustConnection,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Option<ShmBuffer>> {
        if conn.extension_information(shm::X11_EXTENSION_NAME)?.is_none() {
            return Ok(None);
        }
        let len = (width as usize) * (height as usize) * 4;
        unsafe {
            let shmid = libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600);
            anyhow::ensure!(shmid >= 0, "shmget failed");
            let addr = libc::shmat(shmid, std::ptr::null(), 0);
            if addr == usize::MAX as *mut libc::c_void {
                libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
                anyhow::bail!("shmat failed");
            }
            let seg = conn.generate_id()?;
            // Mark the segment for destruction now: it stays alive while
            // attached, and cannot leak if this process dies unexpectedly.
            let attached = conn.shm_attach(seg, shmid as u32, false);
            libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
            attached?.check().context("shm_attach rejected")?;
            Ok(Some(ShmBuffer { seg, shmid, addr, len }))
        }
    }


    /// Re-read just one rectangle and patch it into the full-frame buffer.
    fn capture_rect(&mut self, r: Rectangle) -> anyhow::Result<()> {
        let (rw, rh) = (r.width as usize, r.height as usize);
        let row = rw * 4;
        let stride = self.width as usize * 4;
        self.tile.resize(row * rh, 0);

        if let Some(shm) = self.shm.as_ref() {
            let seg = shm.seg;
            let addr = shm.addr;
            self.conn
                .shm_get_image(
                    self.screen.root,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    !0,
                    ImageFormat::Z_PIXMAP.into(),
                    seg,
                    0,
                )?
                .reply()
                .context("shm_get_image on a damaged rectangle failed")?;
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *const u8, self.tile.as_mut_ptr(), row * rh);
            }
        } else {
            let img = self
                .conn
                .get_image(ImageFormat::Z_PIXMAP, self.screen.root, r.x, r.y, r.width, r.height, !0)?
                .reply()
                .context("get_image on a damaged rectangle failed")?;
            self.tile.clear();
            self.tile.extend_from_slice(&img.data);
            self.tile.resize(row * rh, 0);
        }

        // Patch the tile into place, forcing alpha as we go.
        self.buffer.resize(stride * self.height as usize, 0);
        for y in 0..rh {
            let dst_start = (r.y as usize + y) * stride + r.x as usize * 4;
            let dst = &mut self.buffer[dst_start..dst_start + row];
            dst.copy_from_slice(&self.tile[y * row..(y + 1) * row]);
            for px in dst.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
        }
        self.have_frame = true;
        Ok(())
    }

    /// X11 gives us BGRX on little-endian TrueColor visuals; just force alpha.
    fn finish_bgra(&mut self) {
        for px in self.buffer.chunks_exact_mut(4) {
            px[3] = 0xff;
        }
        self.have_frame = true;
    }

    fn capture_shm(&mut self) -> anyhow::Result<()> {
        let shm = self.shm.as_ref().expect("checked by caller");
        self.conn
            .shm_get_image(
                self.screen.root,
                0,
                0,
                self.width as u16,
                self.height as u16,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                shm.seg,
                0,
            )?
            .reply()
            .context("shm_get_image failed")?;

        self.buffer.resize(shm.len, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                shm.addr as *const u8,
                self.buffer.as_mut_ptr(),
                shm.len,
            );
        }
        self.finish_bgra();
        Ok(())
    }

    fn capture_slow(&mut self) -> anyhow::Result<()> {
        let img = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.screen.root,
                0,
                0,
                self.width as u16,
                self.height as u16,
                !0,
            )?
            .reply()
            .context("get_image failed")?;
        self.buffer = img.data;
        self.buffer.resize((self.width * self.height * 4) as usize, 0);
        self.finish_bgra();
        Ok(())
    }
}

impl ScreenCapture for X11Capture {
    fn wait_for_change(&mut self, timeout: std::time::Duration) {
        // Without damage there is nothing to wait on: the caller's own pacing
        // is what keeps a compare-everything backend from spinning.
        let Some(watcher) = self.damage.as_mut() else {
            std::thread::sleep(timeout);
            return;
        };
        // Cap the wait so the periodic full resync still happens on a screen
        // the compositor repaints without reporting.
        let remaining = FULL_RESYNC.saturating_sub(self.last_full_grab.elapsed());
        watcher.wait(timeout.min(remaining.max(std::time::Duration::from_millis(1))));
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<Frame<'_>>> {
        let full_area = self.width as u64 * self.height as u64;
        // Grabbing a rectangle costs a round trip plus its pixels. Past a
        // fraction of the screen the round trip stops paying for itself and a
        // single full grab is cheaper.
        let partial_limit = full_area / 2;
        let resync_due = self.last_full_grab.elapsed() >= FULL_RESYNC;

        // A session that has never produced a frame must grab one now. Damage
        // only reports *changes*, so on a still desktop it reports nothing and
        // a freshly connected viewer would stare at an empty window until the
        // resync timer happened to fire.
        if self.have_frame && self.damage.is_some() {
            let taken = {
                let watcher = self.damage.as_mut().expect("checked above");
                let mut rects = std::mem::take(&mut self.rects);
                let taken = watcher.take(&mut rects);
                self.rects = rects;
                taken
            };
            match taken {
                // Too many rectangles to track: the screen is effectively
                // fully dirty, fall through to one full grab.
                None => {}
                Some(0) if !resync_due => return Ok(None),
                Some(0) => {}
                Some(_) => {
                    // Grab each rectangle rather than one box around them all:
                    // scattered small updates are the common case on a desktop
                    // and their bounding box is close to the whole screen.
                    let area: u64 = self
                        .rects
                        .iter()
                        .map(|r| r.width as u64 * r.height as u64)
                        .sum();
                    if area <= partial_limit {
                        for k in 0..self.rects.len() {
                            let r = self.rects[k];
                            self.capture_rect(r)?;
                        }
                        return Ok(Some(Frame {
                            width: self.width,
                            height: self.height,
                            bgra: &self.buffer,
                        }));
                    }
                }
            }
        }

        // Full grab: no damage extension, damage covered most of the screen,
        // the first frame, or the periodic resync.
        std::mem::swap(&mut self.buffer, &mut self.prev);
        if self.shm.is_some() {
            self.capture_shm()?;
        } else {
            self.capture_slow()?;
        }
        self.last_full_grab = std::time::Instant::now();

        // Compare so a resync that found nothing new does not cost an encode.
        if self.buffer == self.prev {
            return Ok(None);
        }
        Ok(Some(Frame {
            width: self.width,
            height: self.height,
            bgra: &self.buffer,
        }))
    }

    fn last_frame(&self) -> Option<Frame<'_>> {
        self.have_frame.then(|| Frame {
            width: self.width,
            height: self.height,
            bgra: &self.buffer,
        })
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}



/// Clip a rectangle to the screen. A compositor can report damage that
/// extends past the root window, and asking the server for pixels outside it
/// is an error rather than a clipped reply.
fn clamp_rect(r: Rectangle, width: u32, height: u32) -> Rectangle {
    let x0 = r.x.max(0) as i32;
    let y0 = r.y.max(0) as i32;
    let x1 = (r.x as i32 + r.width as i32).min(width as i32);
    let y1 = (r.y as i32 + r.height as i32).min(height as i32);
    Rectangle {
        x: x0 as i16,
        y: y0 as i16,
        width: (x1 - x0).max(0) as u16,
        height: (y1 - y0).max(0) as u16,
    }
}
