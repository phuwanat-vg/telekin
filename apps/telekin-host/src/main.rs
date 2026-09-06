//! `chassis` — runs on the machine being controlled (the robot's onboard
//! computer). Captures the screen, encodes it, and serves it over QUIC while
//! applying the viewer's input.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tokio::sync::mpsc;
use telekin_proto::{ClientMsg, HostMsg, InputEvent, PROTO_VERSION};
use telekin_transport::quinn;

mod cpu;
mod files;
mod stream;
mod transfer;
mod users;

#[derive(Parser, Debug, Clone)]
#[command(name = "chassis", about = "Telekin host (the controlled machine)")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:9631")]
    listen: SocketAddr,

    /// Add or replace an account, then exit. The password is read from the
    /// terminal, or from stdin when it is not a terminal.
    #[arg(long, value_name = "USERNAME")]
    add_user: Option<String>,

    /// List the accounts this host will accept, then exit.
    #[arg(long)]
    list_users: bool,

    /// Name shown in the viewer's scan results.
    ///
    /// Defaults to this machine's hostname, which is the one thing that
    /// differs between robots imaged from the same card. The account to sign
    /// in with is advertised separately, so picking a robot still fills the
    /// username in.
    #[arg(long)]
    name: Option<String>,

    /// Print the monitors this host can stream, then exit.
    #[arg(long)]
    list_monitors: bool,

    /// Stream the screen but ignore all input events.
    #[arg(long)]
    view_only: bool,

    /// Do not advertise this host on the local network over mDNS.
    #[arg(long)]
    no_advertise: bool,

    /// Directory holding this host's persistent TLS identity.
    ///
    /// Created on first run. Keeping it means the fingerprint viewers pin
    /// stays the same across reboots.
    #[arg(long)]
    identity_dir: Option<std::path::PathBuf>,

    /// X11 display to capture, e.g. `:0` or `:99` (Linux only).
    ///
    /// Needed when the host has no `DISPLAY` in its environment — running
    /// under systemd, or against a virtual display on a headless robot.
    #[arg(long)]
    display: Option<String>,

    /// Ceiling on the share of one CPU core the capture and encode pipeline
    /// may use, as a percentage.
    ///
    /// The pipeline stays responsive when the screen is still — it costs
    /// nothing then — and trades frame rate for CPU when the screen is busy.
    /// On a robot sharing the board with ROS 2 this is the setting that
    /// matters.
    #[arg(long)]
    max_cpu_percent: Option<u8>,

    /// Diagnostic: grab one real frame from the display and time capture and
    /// encode in isolation, then exit. Separates "the screen is expensive to
    /// encode" from "something else on the box is slowing the pipeline".
    #[arg(long)]
    bench_capture: bool,

    /// Diagnostic: check a password the same way a login does, and report
    /// which source accepted it. The password is read from the terminal, or
    /// from stdin when there is none.
    #[arg(long, value_name = "USERNAME")]
    test_password: Option<String>,

    /// Diagnostic: read the session clipboard, then write a marker to it and
    /// read it back, so the round trip can be checked without a viewer.
    #[arg(long)]
    test_clipboard: bool,

    /// Diagnostic: inject a few synthetic key and mouse events into the
    /// display, then exit. Proves the input path works on this machine
    /// without needing a viewer, and moves the screen enough to measure a
    /// non-static capture.
    #[arg(long)]
    test_input: bool,
}

/// Work items for the blocking input thread.
enum InputCmd {
    Event(InputEvent),
    /// The streamed monitor changed; rebuild the injector against it.
    Retarget { monitor: u32, width: u32, height: u32 },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Mutating the environment is only sound while the process is still
    // single-threaded, so this has to happen before the async runtime starts.
    if let Some(display) = &args.display {
        std::env::set_var("DISPLAY", display);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chassis=info,telekin_capture=info".into()),
        )
        .init();

    serve(args)
}

#[tokio::main]
async fn serve(args: Args) -> anyhow::Result<()> {

    let identity_dir = args
        .identity_dir
        .clone()
        .unwrap_or_else(telekin_transport::default_identity_dir);

    if let Some(name) = &args.add_user {
        let password = read_password()?;
        return users::add(&identity_dir, name, password.trim_end());
    }

    if args.list_users {
        let accounts = users::load(&identity_dir)?;
        if accounts.is_empty() {
            println!("No accounts yet. Add one with: chassis --add-user <name>");
        } else {
            let mut names: Vec<_> = accounts.keys().collect();
            names.sort();
            for name in names {
                println!("{name}");
            }
        }
        return Ok(());
    }

    if args.bench_capture {
        return bench_capture();
    }

    if args.test_input {
        return test_input();
    }

    if args.test_clipboard {
        return test_clipboard();
    }

    if let Some(name) = &args.test_password {
        let password = read_password()?;
        let password = password.trim_end_matches(['\r', '\n']);
        let accounts = users::load(&identity_dir)?;
        println!(
            "system account available here: {}",
            users::system_auth_available()
        );
        println!("accepted by this machine's account: {}", users::system_only(name, password));
        println!("accepted by a Telekin account:   {}", users::verify(&accounts, name, password));
        println!("login would succeed:                {}", users::authenticate(&accounts, name, password));
        return Ok(());
    }

    if args.list_monitors {
        for m in telekin_capture::list_monitors()? {
            println!("{}: {} ({}x{})", m.id, m.name, m.width, m.height);
        }
        return Ok(());
    }

    let identity_dir = args
        .identity_dir
        .clone()
        .unwrap_or_else(telekin_transport::default_identity_dir);

    // The hostname, not the account.
    //
    // The account was the original default because it is what the operator has
    // to type to sign in. That reasoning holds for one robot and fails for a
    // fleet: image twenty cards from one master and every robot advertises
    // `tangox`. The hostname is at least different per machine, and the
    // account still travels in its own record so the username can be filled
    // in automatically.
    let advertised_name = args
        .name
        .clone()
        .unwrap_or_else(machine_hostname)
        .trim()
        .to_string();
    let advertised_name = if advertised_name.is_empty() {
        current_user()
    } else {
        advertised_name
    };
    let hostname = machine_hostname();

    let accounts = users::load(&identity_dir)?;
    let system_auth = users::system_auth_available();
    anyhow::ensure!(
        system_auth || !accounts.is_empty(),
        "no way to check a password on this host. Either run it as the user who should be able to log in, or create an account with: chassis --add-user <name>"
    );
    if system_auth {
        tracing::info!(
            "sign in with this machine's own account for {:?}",
            std::env::var("USER").unwrap_or_else(|_| "the user running this host".into())
        );
    }
    if !accounts.is_empty() {
        tracing::info!("{} Telekin account(s) also accepted", accounts.len());
    }
    let (endpoint, fingerprint) = telekin_transport::server_endpoint(args.listen, &identity_dir)?;
    tracing::info!("listening on {}", args.listen);
    tracing::info!("certificate fingerprint: {fingerprint}");
    tracing::info!("viewer command: telekin --host <this-address> --fingerprint {fingerprint}");

    // Held for the process lifetime; dropping it withdraws the record.
    let _advert = if args.no_advertise {
        None
    } else {
        match telekin_transport::discovery::advertise(
            &advertised_name,
            args.listen.port(),
            &fingerprint,
            PROTO_VERSION,
            &hostname,
            &current_user(),
        ) {
            Ok(a) => {
                tracing::info!(
                    "advertising as \"{advertised_name}\" on {hostname} over the local network"
                );
                Some(a)
            }
            Err(e) => {
                // Discovery is a convenience; a host that cannot advertise is
                // still perfectly usable by address.
                tracing::warn!("not advertising over mDNS: {e:#}");
                None
            }
        }
    };

    let args = Arc::new(Serving { args, accounts, advertised_name });
    while let Some(incoming) = endpoint.accept().await {
        let args = args.clone();
        tokio::spawn(async move {
            let peer = incoming.remote_address();
            match handle_connection(incoming, args).await {
                Ok(()) => tracing::info!("session with {peer} ended"),
                Err(e) => tracing::warn!("session with {peer} failed: {e:#}"),
            }
        });
    }
    Ok(())
}

/// Everything a session needs from startup, shared across connections.
struct Serving {
    args: Args,
    accounts: std::collections::HashMap<String, String>,
    /// What this host calls itself to viewers.
    advertised_name: String,
}

async fn handle_connection(incoming: quinn::Incoming, args: Arc<Serving>) -> anyhow::Result<()> {
    let conn = incoming.await.context("QUIC handshake failed")?;
    let peer = conn.remote_address();
    tracing::info!("connection from {peer}");

    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .context("client never opened the control stream")?;

    // --- handshake ---
    let hello: ClientMsg = telekin_proto::read_msg(&mut recv).await?;
    let ClientMsg::Hello { proto_version, username, password, client_name } = hello else {
        anyhow::bail!("first control message was not Hello");
    };
    if proto_version != PROTO_VERSION {
        let reason =
            format!("protocol version mismatch: host {PROTO_VERSION}, client {proto_version}");
        telekin_proto::write_msg(&mut send, &HostMsg::HelloErr { reason: reason.clone() }).await?;
        anyhow::bail!(reason);
    }
    // Both paths block — Argon2 is deliberately slow, and the system check
    // spawns a helper — so keep them off the runtime that is also pumping
    // video for any already-connected viewer.
    let accounts = args.accounts.clone();
    let (check_user, check_pass) = (username.clone(), password);
    let ok = tokio::task::spawn_blocking(move || {
        users::authenticate(&accounts, &check_user, &check_pass)
    })
    .await
    .unwrap_or(false);

    if !ok {
        telekin_proto::write_msg(
            &mut send,
            &HostMsg::HelloErr { reason: "wrong username or password".into() },
        )
        .await?;
        anyhow::bail!("failed login for {username:?} from {peer}");
    }

    // A robot sitting at its login screen has nothing to capture, but it can
    // still browse and transfer files — which is the cheapest thing to ask of
    // it and often the only thing wanted. Refusing the whole session here made
    // the file panes unreachable exactly when they were most useful.
    let monitors = match telekin_capture::list_monitors() {
        Ok(monitors) if !monitors.is_empty() => monitors,
        Ok(_) => {
            tracing::warn!("no monitors found; this session can transfer files but not stream");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                "no screen to capture ({e:#}); this session can transfer files but not stream"
            );
            Vec::new()
        }
    };
    telekin_proto::write_msg(
        &mut send,
        &HostMsg::HelloOk {
            host_name: args.advertised_name.clone(),
            monitors: monitors.clone(),
        },
    )
    .await?;
    tracing::info!("{username} ({client_name}) authenticated from {peer}");

    // Injection calls into the OS and can block; keep it off the runtime that
    // is also pumping video.
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputCmd>();
    if args.args.view_only {
        drop(input_rx);
        tracing::info!("view-only mode: input from {peer} will be ignored");
    } else {
        spawn_input_thread(input_rx);
    }

    let mut streamer: Option<stream::StreamHandle> = None;

    // Pipeline cost, written by the capture thread and read by the reporter.
    let pipeline = Arc::new(std::sync::Mutex::new(stream::PipelineStats::default()));
    // Round trip of the last completed ping, in milliseconds.
    let rtt = Arc::new(std::sync::Mutex::new(PingState::default()));

    // Clipboard sharing is a convenience; a session without it is still fully
    // usable, so a failure here is logged rather than fatal.
    let clipboard = match telekin_input::clipboard::start() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("clipboard sharing unavailable: {e:#}");
            None
        }
    };
    let clipboard_set = clipboard.as_ref().map(|c| c.set.clone());
    let (host_clip_tx, host_clip_rx) = mpsc::unbounded_channel::<String>();
    if let Some(c) = clipboard {
        // The bridge's receiver is a blocking std channel; give it a thread
        // rather than blocking the runtime that is pumping video.
        std::thread::spawn(move || {
            while let Ok(text) = c.changed.recv() {
                if text.len() > telekin_proto::MAX_CLIPBOARD_BYTES {
                    tracing::debug!("host clipboard too large to share ({} bytes)", text.len());
                    continue;
                }
                if host_clip_tx.send(text).is_err() {
                    break;
                }
            }
        });
    }

    // The reporter owns the write half: nothing else writes after the
    // handshake, so no lock is needed on the stream itself.
    // File work answers on the control stream, but must never own it: a slow
    // disk would otherwise stall stats, pings and the clipboard behind it.
    let (out_tx, out_rx) = mpsc::unbounded_channel::<HostMsg>();
    let reporter = tokio::spawn(report_loop(
        send,
        pipeline.clone(),
        rtt.clone(),
        host_clip_rx,
        out_rx,
    ));

    // Uploads arrive as unidirectional streams the viewer opens. Announced
    // first on the control stream, so this only has to route by id.
    let uploads = transfer::pending();
    let upload_accepter = tokio::spawn(transfer::accept_uploads(
        conn.clone(),
        uploads.clone(),
        out_tx.clone(),
    ));

    // --- control loop ---
    loop {
        let msg: ClientMsg = match telekin_proto::read_msg(&mut recv).await {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("control stream closed: {e:#}");
                break;
            }
        };
        match msg {
            ClientMsg::Hello { .. } => anyhow::bail!("duplicate Hello"),
            ClientMsg::StartStream {
                monitor,
                max_fps,
                bitrate_kbps,
                scale_percent,
                max_cpu_percent,
            } => {
                // A stream that cannot start must not take the session down
                // with it: the file panes on the other side are still working.
                let Some(info) = monitors.iter().find(|m| m.id == monitor) else {
                    if monitors.is_empty() {
                        tracing::warn!(
                            "a viewer asked for video, but no desktop session was found to capture; sign in on the robot, or use Files only"
                        );
                    } else {
                        tracing::warn!("a viewer asked for monitor {monitor}, which does not exist");
                    }
                    continue;
                };
                if let Some(old) = streamer.take() {
                    old.stop();
                }
                tracing::info!(
                    "starting stream: monitor {monitor} ({}x{}) @ {max_fps}fps, {bitrate_kbps}kbps, scale {scale_percent}%, cpu cap {:?}",
                    info.width,
                    info.height,
                    max_cpu_percent.or(args.args.max_cpu_percent),
                );
                streamer = Some(stream::spawn(stream::Config {
                    conn: conn.clone(),
                    monitor,
                    max_fps: max_fps.clamp(1, 240),
                    bitrate_kbps: bitrate_kbps.clamp(200, 200_000),
                    stats: pipeline.clone(),
                    // The viewer's request wins; the flag is the default for
                    // clients that do not ask.
                    max_cpu_percent: max_cpu_percent.or(args.args.max_cpu_percent),
                    scale_percent,
                }));
                let _ = input_tx.send(InputCmd::Retarget {
                    monitor,
                    width: info.width,
                    height: info.height,
                });
            }
            ClientMsg::StopStream => {
                if let Some(s) = streamer.take() {
                    s.stop();
                }
            }
            ClientMsg::RequestKeyframe => {
                if let Some(s) = &streamer {
                    s.request_keyframe();
                }
            }
            ClientMsg::Input(event) => {
                let _ = input_tx.send(InputCmd::Event(event));
            }
            ClientMsg::Clipboard { text } => {
                if text.len() > telekin_proto::MAX_CLIPBOARD_BYTES {
                    tracing::debug!("ignoring an oversized clipboard payload");
                } else if let Some(set) = &clipboard_set {
                    let _ = set.send(text);
                }
            }
            ClientMsg::Files(request) => {
                transfer::handle(request, &conn, &out_tx, &uploads);
            }
            ClientMsg::Pong { nonce } => {
                let mut state = rtt.lock().expect("ping state poisoned");
                if state.nonce == nonce {
                    if let Some(sent) = state.sent_at.take() {
                        state.rtt_ms = sent.elapsed().as_secs_f32() * 1000.0;
                    }
                }
            }
        }
    }

    if let Some(s) = streamer.take() {
        s.stop();
    }
    reporter.abort();
    upload_accepter.abort();
    drop(input_tx); // tells the input thread to release held keys and exit
    Ok(())
}

/// The most recent ping, and the round trip it measured.
#[derive(Debug, Default)]
struct PingState {
    nonce: u64,
    sent_at: Option<std::time::Instant>,
    rtt_ms: f32,
}

/// Ping the viewer and report pipeline cost back to it, so the operator can
/// see whether latency is coming from the robot or from the network.
async fn report_loop(
    mut send: quinn::SendStream,
    pipeline: Arc<std::sync::Mutex<stream::PipelineStats>>,
    rtt: Arc<std::sync::Mutex<PingState>>,
    mut clipboard_rx: mpsc::UnboundedReceiver<String>,
    // Anything else that needs the control stream. File replies arrive here
    // from worker tasks, which must not touch the writer themselves.
    mut out_rx: mpsc::UnboundedReceiver<HostMsg>,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut nonce = 0u64;
    loop {
        // Clipboard updates go out as soon as they happen; stats are on the
        // slow tick. Sharing one writer keeps the control stream single-owner.
        tokio::select! {
            text = clipboard_rx.recv() => {
                let Some(text) = text else { break };
                if telekin_proto::write_msg(&mut send, &HostMsg::Clipboard { text })
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            out = out_rx.recv() => {
                let Some(msg) = out else { break };
                if telekin_proto::write_msg(&mut send, &msg).await.is_err() {
                    break;
                }
                continue;
            }
            _ = ticker.tick() => {}
        }
        nonce += 1;

        let (stats, rtt_ms) = {
            let p = *pipeline.lock().expect("stats poisoned");
            let mut state = rtt.lock().expect("ping state poisoned");
            // A ping still outstanding means the last one was never answered;
            // report the previous value rather than inventing one.
            state.nonce = nonce;
            state.sent_at = Some(std::time::Instant::now());
            (p, state.rtt_ms)
        };

        if telekin_proto::write_msg(&mut send, &HostMsg::Ping { nonce }).await.is_err() {
            break;
        }
        let msg = HostMsg::Stats {
            fps: stats.fps,
            bitrate_kbps: stats.bitrate_kbps,
            capture_ms: stats.capture_ms,
            encode_ms: stats.encode_ms,
            rtt_ms,
        };
        if telekin_proto::write_msg(&mut send, &msg).await.is_err() {
            break;
        }
    }
}

fn spawn_input_thread(mut rx: mpsc::UnboundedReceiver<InputCmd>) {
    std::thread::spawn(move || {
        let mut injector: Option<Box<dyn telekin_input::InputInjector>> = None;
        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                InputCmd::Retarget { monitor, width, height } => {
                    if let Some(old) = &mut injector {
                        let _ = old.release_all();
                    }
                    injector = match telekin_input::open(monitor, width, height) {
                        Ok(i) => Some(i),
                        Err(e) => {
                            tracing::error!("input injection unavailable: {e:#}");
                            None
                        }
                    };
                }
                InputCmd::Event(event) => {
                    // Events before the first StartStream have nowhere to go.
                    let Some(injector) = &mut injector else { continue };
                    if let Err(e) = injector.inject(&event) {
                        tracing::warn!("input injection failed: {e:#}");
                    }
                }
            }
        }
        // Connection gone: never leave a key held down on a robot.
        if let Some(mut injector) = injector {
            if let Err(e) = injector.release_all() {
                tracing::warn!("failed to release held input: {e:#}");
            }
        }
    });
}

/// The account this process runs as. Used as the advertised name, so the
/// scan list shows what to sign in with rather than an arbitrary label.
fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "chassis".into())
}

/// This machine's hostname, advertised so two robots sharing an operator
/// account can still be told apart.
fn machine_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

/// Read a password without echoing it, falling back to stdin when the
/// process has no terminal — which is how a provisioning script would call it.
fn read_password() -> anyhow::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Password: ").context("could not read the password")
    } else {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("could not read the password from stdin")?;
        Ok(line)
    }
}

/// Exercise the clipboard bridge on its own.
fn test_clipboard() -> anyhow::Result<()> {
    use std::time::Duration;

    let clip = telekin_input::clipboard::start()?;
    println!("clipboard bridge started");

    // Whatever the session already holds should arrive as a change.
    match clip.changed.recv_timeout(Duration::from_secs(2)) {
        Ok(text) => println!("read from session clipboard: {:?}", truncate(&text)),
        Err(_) => println!("read from session clipboard: (empty, or nothing owns it)"),
    }

    let marker = format!("telekin-clipboard-{}", std::process::id());
    clip.set.send(marker.clone())?;
    println!("published to session clipboard: {marker:?}");
    println!("now read it with another application to confirm ownership works");

    // Stay alive: on X11 the clipboard only holds text for as long as its
    // owner is running, so exiting here would drop what we just published.
    std::thread::sleep(Duration::from_secs(20));
    Ok(())
}

fn truncate(s: &str) -> String {
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Drive the input injector directly, with no viewer in the loop.
///
/// Tapping the Meta key opens and closes the desktop's overview on most Linux
/// desktops, which is both a visible confirmation that injection reached the
/// session and a large, repeatable change for the capture path to chew on.
fn test_input() -> anyhow::Result<()> {
    use std::time::Duration;
    use telekin_proto::{InputEvent, KeyCode, MouseButton};

    let monitors = telekin_capture::list_monitors()?;
    let m = monitors.first().context("no monitor to target")?;
    println!("injecting into monitor 0 ({}x{})", m.width, m.height);

    let mut injector = telekin_input::open(0, m.width, m.height)?;

    let tap = |injector: &mut Box<dyn telekin_input::InputInjector>, key: KeyCode| -> anyhow::Result<()> {
        injector.inject(&InputEvent::Key { key, down: true })?;
        std::thread::sleep(Duration::from_millis(40));
        injector.inject(&InputEvent::Key { key, down: false })?;
        Ok(())
    };

    for i in 1..=6 {
        println!("  round {i}: Meta (overview on), mouse sweep, Escape (overview off)");
        tap(&mut injector, KeyCode::MetaLeft)?;
        std::thread::sleep(Duration::from_millis(700));

        for step in 0..10 {
            let t = step as f32 / 9.0;
            injector.inject(&InputEvent::MouseMove { x: 0.1 + t * 0.8, y: 0.2 + t * 0.6 })?;
            std::thread::sleep(Duration::from_millis(40));
        }

        tap(&mut injector, KeyCode::Escape)?;
        std::thread::sleep(Duration::from_millis(700));
    }

    // Exercise the button path too, on the desktop where it cannot open
    // anything destructive.
    injector.inject(&InputEvent::MouseMove { x: 0.5, y: 0.5 })?;
    injector.inject(&InputEvent::MouseButton { button: MouseButton::Left, down: true })?;
    std::thread::sleep(Duration::from_millis(40));
    injector.inject(&InputEvent::MouseButton { button: MouseButton::Left, down: false })?;

    injector.release_all()?;
    println!("injection completed without error");
    Ok(())
}

/// Time the pipeline stages against the real desktop, with nothing else in
/// the way. Output is meant to be read next to the host's per-window log line.
fn bench_capture() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    use telekin_codec::{H264Encoder, VideoEncoder};

    let mut cap = telekin_capture::open(0)?;
    let grab = |cap: &mut Box<dyn telekin_capture::ScreenCapture>| -> anyhow::Result<(u32, u32, Vec<u8>)> {
        loop {
            if let Some(f) = cap.next_frame()? {
                return Ok((f.width, f.height, f.bgra.to_vec()));
            }
        }
    };

    let (w, h, a) = grab(&mut cap)?;
    std::thread::sleep(Duration::from_millis(300));
    let (_, _, b) = grab(&mut cap)?;
    let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    println!(
        "frame {w}x{h}: {differing} of {} bytes differ between two grabs 300 ms apart ({:.4}%)",
        a.len(),
        differing as f64 * 100.0 / a.len() as f64
    );

    let n = 20;
    let t = Instant::now();
    for _ in 0..n {
        let _ = cap.next_frame()?;
    }
    println!("capture only:                    {:.1} ms/frame", t.elapsed().as_secs_f32() * 1000.0 / n as f32);

    let mut enc = H264Encoder::new(20_000, 15)?;
    enc.encode_bgra(w, h, &a)?; // init + IDR, not timed

    let n = 30;
    let mut bytes = 0usize;
    let t = Instant::now();
    for _ in 0..n {
        if let Some(e) = enc.encode_bgra(w, h, &a)? {
            bytes += e.data.len();
        }
    }
    println!(
        "encode, identical real frame:    {:.1} ms/frame  ({} B avg out)",
        t.elapsed().as_secs_f32() * 1000.0 / n as f32,
        bytes / n
    );

    let mut bytes = 0usize;
    let t = Instant::now();
    for i in 0..n {
        let src = if i % 2 == 0 { &a } else { &b };
        if let Some(e) = enc.encode_bgra(w, h, src)? {
            bytes += e.data.len();
        }
    }
    println!(
        "encode, alternating two grabs:   {:.1} ms/frame  ({} B avg out)",
        t.elapsed().as_secs_f32() * 1000.0 / n as f32,
        bytes / n
    );

    let n = 5;
    let mut bytes = 0usize;
    let t = Instant::now();
    for _ in 0..n {
        enc.force_keyframe();
        if let Some(e) = enc.encode_bgra(w, h, &a)? {
            bytes += e.data.len();
        }
    }
    println!(
        "encode, forced keyframe:         {:.1} ms/frame  ({} B avg out)",
        t.elapsed().as_secs_f32() * 1000.0 / n as f32,
        bytes / n
    );
    Ok(())
}

