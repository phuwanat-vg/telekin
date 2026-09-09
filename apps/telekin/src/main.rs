//! `telekin` — runs on the machine you sit at.
//!
//! Opens a window with a connection screen (type an address, or pick a robot
//! discovered on the LAN), then streams the host's screen and forwards input
//! back to it. Stream settings can be changed while connected: the host treats
//! a repeated `StartStream` as "stop this one and begin another".

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::Parser;
use tokio::sync::mpsc;
use telekin_proto::InputEvent;

mod files;
mod keymap;
mod net;
mod theme;
mod ui;
mod ui_files;
mod update;

use net::{FrameSlot, Status, StreamSettings};

/// How the decode thread reaches the UI. eframe repaints on request rather
/// than polling, so without this a new frame would not be drawn until some
/// other event happened to wake the window.
pub type Waker = Arc<std::sync::OnceLock<egui::Context>>;

/// How long to wait for hosts to answer an mDNS query.
pub const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Parser, Debug)]
#[command(name = "telekin", about = "Telekin viewer (the controlling machine)")]
struct Args {
    /// Host address, e.g. 192.168.1.50 or 192.168.1.50:9631. The port
    /// defaults to 9631, matching what the connection screen accepts.
    #[arg(long)]
    host: Option<String>,

    /// Name of a host on the LAN. Prefills the connection screen.
    #[arg(long, conflicts_with = "host")]
    name: Option<String>,

    /// Account on the host, created there with `chassis --add-user`.
    #[arg(long, env = "TELEKIN_USER")]
    user: Option<String>,

    /// Password for that account. Prefer typing it in the window; a password
    /// on a command line ends up in the shell history.
    #[arg(long, env = "TELEKIN_PASSWORD")]
    password: Option<String>,

    /// Host certificate fingerprint, as printed by chassis at startup.
    #[arg(long)]
    fingerprint: Option<String>,

    /// List the hosts visible on the local network, then exit.
    #[arg(long)]
    discover: bool,

    /// Connect immediately instead of showing the connection screen.
    #[arg(long)]
    connect: bool,

    #[arg(long, default_value_t = 20)]
    fps: u16,

    #[arg(long, default_value_t = 20_000)]
    bitrate_kbps: u32,

    /// Percentage of the host's resolution to stream, 25..=100.
    #[arg(long, default_value_t = 100)]
    scale: u8,

    /// Ceiling on the host's CPU use, as a percentage of one core.
    #[arg(long)]
    max_cpu_percent: Option<u8>,

    #[arg(long, default_value_t = 0)]
    monitor: u32,

    /// Watch without sending any input.
    #[arg(long)]
    view_only: bool,

    /// Ask the update server what the newest version is, print the answer,
    /// and exit. Exit status is non-zero when the server could not be
    /// reached, so a script can tell "up to date" from "could not check".
    #[arg(long)]
    check_update: bool,

    /// Do not look for a newer version at start.
    ///
    /// The check is one small HTTPS request for a JSON file, and it installs
    /// nothing. On a network with no internet it fails quietly on its own,
    /// but a lab that would rather it never tried can say so here or with
    /// `TELEKIN_UPDATE_URL` pointed at its own server.
    #[arg(long)]
    no_update_check: bool,
}

/// Icon size. 64 is large enough that Windows downscales rather than guesses.
const ICON_PX: u32 = 64;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_parent_console();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "telekin=info".into()),
        )
        .init();

    let args = Args::parse();

    if args.discover {
        return list_hosts();
    }
    if args.check_update {
        return report_update();
    }

    let mut app = ui::ViewerApp::new(&args);
    // Started here rather than inside `new`, so a headless test constructing
    // the app never reaches for the network.
    if !args.no_update_check {
        app.start_update_check();
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            // Sized to what the layout actually needs at this type size,
            // rather than to the smallest window the OS will allow. A window
            // that can be shrunk until Connect is below the fold is a window
            // that will be, on the day it matters.
            .with_min_inner_size([640.0, 560.0])
            .with_title(ui::NAME)
            // The taskbar is where someone picks this window out of twenty
            // others, so it gets the mark rather than a blank default.
            .with_icon(egui::IconData {
                rgba: theme::icon_rgba(ICON_PX),
                width: ICON_PX,
                height: ICON_PX,
            }),
        ..Default::default()
    };
    eframe::run_native(
        ui::NAME,
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
        .map_err(|e| anyhow::anyhow!("could not start the window: {e}"))
}

/// Print what the update check finds, for scripts and for diagnosing a lab
/// whose robots cannot see the update server.
fn report_update() -> anyhow::Result<()> {
    use update::Outcome;
    let slot = update::start(None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let outcome = slot.lock().expect("update slot poisoned").clone();
        match outcome {
            Outcome::Checking if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Outcome::Checking => anyhow::bail!("the update check did not finish in time"),
            Outcome::UpToDate => {
                println!("telekin {} is the newest version", update::CURRENT);
                return Ok(());
            }
            Outcome::Available { version, notes, url } => {
                println!("telekin {} is available (this is {})", version, update::CURRENT);
                if !notes.is_empty() {
                    println!("  {notes}");
                }
                println!("  {url}");
                return Ok(());
            }
            Outcome::Unavailable(why) => anyhow::bail!("could not check for updates: {why}"),
        }
    }
}

/// On Windows, join the terminal this was started from, if there was one.
///
/// The executable is built without a console so that a double-click opens
/// only the window. That also detaches `println!` — so when a flag needs to
/// print, take the parent's console back. When there is no parent console
/// this fails, quietly, which is exactly right.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    // SAFETY: plain Win32 calls with no pointers; failure is reported by
    // return value and ignored on purpose.
    unsafe {
        // Already somewhere to print — a pipe from a script, say. Attaching
        // would swap that for the console and silently break
        // `telekin --discover | grep robot`.
        if let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) {
            if h != HANDLE::default() && h != INVALID_HANDLE_VALUE {
                return;
            }
        }
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Print every host answering on the LAN.
fn list_hosts() -> anyhow::Result<()> {
    let hosts = telekin_transport::discovery::discover(DISCOVERY_TIMEOUT)?;
    if hosts.is_empty() {
        println!("No Telekin hosts answered on this network.");
        println!("Check that the host is running without --no-advertise, and that");
        println!("mDNS (UDP 5353) is not blocked between the two machines.");
        return Ok(());
    }
    println!("{:<18} {:<20} {:<22} FINGERPRINT", "NAME", "COMPUTER", "ADDRESS");
    for h in &hosts {
        // Every address, not just the best one: a robot on both Wi-Fi and a
        // cable is reachable twice, and when a scan "does not see the LAN"
        // the first thing worth knowing is whether the wired address came
        // back at all.
        let addr = if h.addrs.is_empty() {
            "(no address)".to_string()
        } else {
            h.addrs
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{:<18} {:<20} {:<22} {}",
            h.name,
            h.hostname.as_deref().unwrap_or("-"),
            addr,
            h.fingerprint.as_deref().unwrap_or("(not advertised)")
        );
    }
    Ok(())
}

/// A running session: the network thread plus the channels into it.
pub struct Session {
    // Options so teardown can release them before joining the thread: field
    // drop order runs *after* `Drop::drop`, which is too late.
    input_tx: Option<mpsc::UnboundedSender<InputEvent>>,
    settings_tx: Option<mpsc::UnboundedSender<net::StreamControl>>,
    clipboard_tx: Option<mpsc::UnboundedSender<String>>,
    files_tx: Option<mpsc::UnboundedSender<files::Command>>,
    /// Directory listings and transfer progress, shared with the file panes.
    pub files: files::Shared,
    pub status: Arc<Mutex<Status>>,
    pub frame: Arc<Mutex<FrameSlot>>,
    /// The live QUIC connection, once established.
    live: Arc<Mutex<Option<telekin_transport::quinn::Connection>>>,
    /// The endpoint behind it, from before the first packet: closing this is
    /// what cancels an attempt that is still waiting on a silent robot.
    endpoint: Arc<Mutex<Option<telekin_transport::quinn::Endpoint>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Signalled when the network thread returns; dropped if it panics.
    done: Option<std::sync::mpsc::Receiver<()>>,
}

/// How long teardown waits for the network thread before letting it go. The
/// window must never sit behind the network: a thread that is still winding
/// down after this is left to finish on its own.
const TEARDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

impl Session {
    /// Start a session in the background. Errors during connection surface
    /// through [`Status::message`] rather than here, so the UI can show them.
    pub fn start(
        hosts: Vec<SocketAddr>,
        fingerprint: Option<[u8; 32]>,
        username: String,
        password: String,
        settings: StreamSettings,
        // False for a files-only session: connect, but never ask the robot to
        // capture or encode anything.
        start_stream: bool,
        waker: Waker,
    ) -> Self {
        let frame = Arc::new(Mutex::new(FrameSlot::default()));
        let status = Arc::new(Mutex::new(Status {
            message: "Connecting…".into(),
            ..Default::default()
        }));
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (settings_tx, settings_rx) = mpsc::unbounded_channel();
        let (clipboard_tx, clipboard_rx) = mpsc::unbounded_channel();
        let (files_tx, files_rx) = mpsc::unbounded_channel();
        let file_state = files::shared();
        let live = Arc::new(Mutex::new(None));
        let endpoint = Arc::new(Mutex::new(None));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        let thread = std::thread::spawn({
            let frame = frame.clone();
            let status = status.clone();
            let live = live.clone();
            let endpoint = endpoint.clone();
            let file_state = file_state.clone();
            move || {
                // Sent on every way out, including a panic unwinding past it.
                let _done = DoneSignal(done_tx);
                let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        set_error(&status, format!("could not start the network runtime: {e}"));
                        return;
                    }
                };
                let params = net::Params {
                    hosts,
                    fingerprint,
                    username,
                    password,
                    settings,
                    start_stream,
                };
                let result = runtime.block_on(net::run(
                    params,
                    frame,
                    net::Channels {
                        input: input_rx,
                        settings: settings_rx,
                        clipboard: clipboard_rx,
                        files: files_rx,
                        file_state,
                        status: status.clone(),
                        live,
                        endpoint,
                        waker: waker.clone(),
                    },
                ));
                match result {
                    Ok(()) => set_error(&status, "Disconnected".into()),
                    Err(e) => set_error(&status, format!("{e:#}")),
                }
                if let Some(ctx) = waker.get() {
                    ctx.request_repaint();
                }
            }
        });

        Self {
            input_tx: Some(input_tx),
            settings_tx: Some(settings_tx),
            clipboard_tx: Some(clipboard_tx),
            files_tx: Some(files_tx),
            files: file_state,
            status,
            frame,
            live,
            endpoint,
            thread: Some(thread),
            done: Some(done_rx),
        }
    }

    pub fn snapshot(&self) -> Status {
        self.status.lock().expect("status poisoned").clone()
    }
}

impl Session {
    pub fn input(&self) -> Option<&mpsc::UnboundedSender<InputEvent>> {
        self.input_tx.as_ref()
    }

    pub fn settings(&self) -> Option<&mpsc::UnboundedSender<net::StreamControl>> {
        self.settings_tx.as_ref()
    }

    pub fn clipboard(&self) -> Option<&mpsc::UnboundedSender<String>> {
        self.clipboard_tx.as_ref()
    }

    pub fn files(&self) -> Option<&mpsc::UnboundedSender<files::Command>> {
        self.files_tx.as_ref()
    }
}

/// Fires the session's done signal however the network thread ends.
struct DoneSignal(std::sync::mpsc::Sender<()>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Order matters, and getting it wrong hangs the window.
        //
        // Closing the QUIC connection is what actually ends the session: the
        // network thread is parked in `accept_uni`, which only returns when
        // the connection goes away. Releasing the channels alone would leave
        // it waiting for the host to notice the closed control stream.
        //
        // Before there is a connection there is an attempt, parked in the
        // handshake with a robot that may be asleep; closing the endpoint is
        // what ends that. Disconnect pressed during "Connecting…" used to
        // wait here for the attempt to time out, which read as a hang.
        //
        // The channels are then dropped explicitly, because field drop runs
        // *after* this function — waiting first would wait on a thread whose
        // inputs are still open.
        if let Some(conn) = self.live.lock().expect("connection slot poisoned").take() {
            conn.close(0u32.into(), b"disconnected");
        }
        if let Some(endpoint) = self.endpoint.lock().expect("endpoint slot poisoned").take() {
            endpoint.close(0u32.into(), b"disconnected");
        }
        self.input_tx.take();
        self.settings_tx.take();
        self.clipboard_tx.take();
        self.files_tx.take();

        // Wait briefly for a clean exit, then let the thread go rather than
        // freeze the window behind whatever it is still doing.
        let finished = match self.done.take() {
            Some(done) => done.recv_timeout(TEARDOWN_WAIT).is_ok(),
            None => false,
        };
        if let Some(t) = self.thread.take() {
            if finished {
                let _ = t.join();
            } else {
                tracing::warn!("network thread still busy after {TEARDOWN_WAIT:?}; detaching it");
            }
        }
    }
}

fn set_error(status: &Arc<Mutex<Status>>, message: String) {
    let mut s = status.lock().expect("status poisoned");
    s.connected = false;
    s.message = message;
}

/// Resolve what the connection screen was given into addresses to dial.
pub fn resolve(
    address: &str,
    discovered: Option<&telekin_transport::discovery::Discovered>,
) -> anyhow::Result<Vec<SocketAddr>> {
    if let Some(d) = discovered {
        return Ok(d.addrs.clone());
    }
    let text = address.trim();
    anyhow::ensure!(!text.is_empty(), "enter an address, or scan for a robot");

    // An address, with the port filled in so nobody has to remember it.
    let with_port = if text.contains(':') {
        text.to_string()
    } else {
        format!("{text}:9631")
    };
    if let Ok(addr) = with_port.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }

    // Not an address, so treat it as a robot's name and look it up. This is
    // what makes the name shown by a scan work when it is typed rather than
    // clicked, and it survives DHCP moving the robot.
    let hosts = telekin_transport::discovery::discover(DISCOVERY_TIMEOUT)?;
    let found = hosts
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(text))
        .with_context(|| {
            format!("{text:?} is not an address, and no robot of that name answered on this network")
        })?;
    Ok(found.addrs.clone())
}
