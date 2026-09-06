//! Networking half of the viewer: owns the QUIC connection, decodes incoming
//! frames, and forwards input from the UI thread.
//!
//! The UI thread never awaits. It publishes input into an unbounded channel
//! and reads pixels out of a shared [`FrameSlot`].
//!
//! Frames arrive on independent QUIC streams, so they can *complete* out of
//! order — but H.264 must be decoded in order. Stream bodies are therefore
//! read concurrently and passed through a small reorder buffer that feeds a
//! single decoder.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::sync::mpsc;
use telekin_codec::{H264Decoder, VideoDecoder};
use telekin_proto::{ClientMsg, FrameHeader, HostMsg, InputEvent, MonitorInfo, PROTO_VERSION};
use telekin_transport::quinn;

/// How far ahead of the next expected frame we wait before giving up on a
/// missing one. Beyond this, waiting costs more latency than the skip costs
/// quality — so we drop forward and ask for a keyframe.
const REORDER_WINDOW: usize = 8;

/// The most recently decoded frame, handed to the UI thread for blitting.
#[derive(Default)]
pub struct FrameSlot {
    pub width: u32,
    pub height: u32,
    /// RGBA, `width * height * 4` bytes — the order egui uploads directly.
    pub pixels: Vec<u8>,
    /// Bumped on each decode so the UI knows when to redraw.
    pub generation: u64,
}

/// Stream settings. Changing any of these re-issues `StartStream`, which the
/// host treats as "stop the old stream and begin a new one", so the UI can
/// apply them live without dropping the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSettings {
    pub monitor: u32,
    pub max_fps: u16,
    pub bitrate_kbps: u32,
    pub scale_percent: u8,
    pub max_cpu_percent: Option<u8>,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            monitor: 0,
            max_fps: 20,
            bitrate_kbps: 20_000,
            scale_percent: 100,
            max_cpu_percent: None,
        }
    }
}

impl StreamSettings {
    fn to_msg(self) -> ClientMsg {
        ClientMsg::StartStream {
            monitor: self.monitor,
            max_fps: self.max_fps.clamp(1, 240),
            bitrate_kbps: self.bitrate_kbps,
            scale_percent: self.scale_percent.clamp(25, 100),
            max_cpu_percent: self.max_cpu_percent,
        }
    }
}

/// What the viewer wants the host's encoder to be doing.
///
/// A session does not have to be watching anything. Browsing files with the
/// stream stopped is the cheapest thing a robot can be asked to do — no
/// capture, no encoder, no frames — which matters when the CPU it is sharing
/// is also driving the robot.
#[derive(Debug, Clone, Copy)]
pub enum StreamControl {
    Start(StreamSettings),
    Stop,
}

impl StreamControl {
    fn to_msg(self) -> ClientMsg {
        match self {
            Self::Start(s) => s.to_msg(),
            Self::Stop => ClientMsg::StopStream,
        }
    }
}

/// What the UI shows about the session.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub connected: bool,
    pub message: String,
    pub host_name: String,
    pub monitors: Vec<MonitorInfo>,
    /// Latest host-reported pipeline cost.
    pub host_fps: f32,
    pub bitrate_kbps: u32,
    pub capture_ms: f32,
    pub encode_ms: f32,
    pub rtt_ms: f32,
    /// Frames per second actually decoded here.
    pub view_fps: f32,
    pub frame_size: (u32, u32),
    /// Text the host copied that has not yet been mirrored locally. The UI
    /// takes it, so a repeated copy of the same text still arrives.
    pub incoming_clipboard: Option<String>,
}

pub struct Params {
    /// Candidate addresses, best first. Tried in order until one connects.
    pub hosts: Vec<SocketAddr>,
    pub fingerprint: Option<[u8; 32]>,
    pub username: String,
    pub password: String,
    pub settings: StreamSettings,
    /// Whether to ask for video at all. False for a files-only session.
    pub start_stream: bool,
}

/// One decoded-and-ordered frame on its way to the decoder.
struct Arrived {
    header: FrameHeader,
    data: Vec<u8>,
}

/// Connect, handshake, request the stream, then run until the connection ends.
/// The channels and shared state a session runs on. Grouped so the entry
/// point stays readable as the viewer grows features.
pub struct Channels {
    pub input: mpsc::UnboundedReceiver<InputEvent>,
    pub settings: mpsc::UnboundedReceiver<StreamControl>,
    pub clipboard: mpsc::UnboundedReceiver<String>,
    /// File-pane requests. Separate from `input` so a copy can never sit
    /// behind a keystroke, or a keystroke behind a copy.
    pub files: mpsc::UnboundedReceiver<crate::files::Command>,
    pub file_state: crate::files::Shared,
    pub status: Arc<Mutex<Status>>,
    /// Filled in once connected, so the UI can close the session promptly.
    pub live: Arc<Mutex<Option<quinn::Connection>>>,
    pub waker: crate::Waker,
}

pub async fn run(
    params: Params,
    slot: Arc<Mutex<FrameSlot>>,
    channels: Channels,
) -> anyhow::Result<()> {
    let Channels {
        input: mut input_rx,
        settings: mut settings_rx,
        clipboard: mut clipboard_rx,
        files: mut files_rx,
        file_state,
        status,
        live,
        waker,
    } = channels;
    let endpoint = telekin_transport::client_endpoint(params.fingerprint)?;
    let conn = connect_any(&endpoint, &params.hosts).await?;
    // Publish it so a disconnect can tear the session down immediately rather
    // than waiting for a channel to close and a round trip to notice.
    *live.lock().expect("connection slot poisoned") = Some(conn.clone());

    let (mut send, mut recv) = conn.open_bi().await?;
    telekin_proto::write_msg(
        &mut send,
        &ClientMsg::Hello {
            proto_version: PROTO_VERSION,
            username: params.username.clone(),
            password: params.password.clone(),
            client_name: whoami(),
        },
    )
    .await?;

    let monitors: Vec<MonitorInfo> = match telekin_proto::read_msg(&mut recv).await? {
        HostMsg::HelloOk { host_name, monitors } => {
            tracing::info!("host {host_name} offers {} monitor(s)", monitors.len());
            {
                let mut st = status.lock().expect("status poisoned");
                st.connected = true;
                st.message = format!("Connected to {host_name}");
                st.host_name = host_name;
                st.monitors = monitors.clone();
            }
            monitors
        }
        HostMsg::HelloErr { reason } => anyhow::bail!("host rejected the connection: {reason}"),
        other => anyhow::bail!("unexpected handshake reply: {other:?}"),
    };

    let mut settings = params.settings;
    if !monitors.iter().any(|m| m.id == settings.monitor) {
        // The saved monitor may not exist on this host; fall back rather than
        // refusing to connect.
        settings.monitor = monitors.first().map(|m| m.id).unwrap_or(0);
    }
    // Only if the operator actually asked to watch. A files-only session
    // never sends this, and the robot never starts its encoder.
    if params.start_stream {
        telekin_proto::write_msg(&mut send, &settings.to_msg()).await?;
    }

    // Ordered frames -> decoder.
    let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Arrived>();
    // Decoder -> control stream, for keyframe requests after a gap.
    let (keyframe_tx, mut keyframe_rx) = mpsc::unbounded_channel::<()>();
    // Reader -> writer, so a host ping is answered on the one stream the
    // writer task owns.
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<u64>();

    let decode_task = tokio::task::spawn_blocking({
        let slot = slot.clone();
        let keyframe_tx = keyframe_tx.clone();
        let status = status.clone();
        // Cloned rather than moved: the file paths below need to wake the
        // window too, so the decoder cannot take sole ownership of it.
        let waker = waker.clone();
        move || decode_loop(frame_rx, slot, keyframe_tx, waker, status)
    });

    // Control-stream writer: input events and keyframe requests share it.
    // The writer turns file commands into protocol messages, and starts the
    // upload streams that some of them imply.
    let writer_conn = conn.clone();
    let writer_files = file_state.clone();
    let writer_waker = waker.clone();

    let writer = tokio::spawn(async move {
        let mut input_open = true;
        let mut keyframe_open = true;
        let mut pong_open = true;
        let mut settings_open = true;
        let mut clipboard_open = true;
        let mut files_open = true;
        while input_open
            || keyframe_open
            || pong_open
            || settings_open
            || clipboard_open
            || files_open
        {
            let msg = tokio::select! {
                event = input_rx.recv(), if input_open => match event {
                    Some(e) => Some(ClientMsg::Input(e)),
                    None => { input_open = false; None }
                },
                req = keyframe_rx.recv(), if keyframe_open => match req {
                    Some(()) => Some(ClientMsg::RequestKeyframe),
                    None => { keyframe_open = false; None }
                },
                nonce = pong_rx.recv(), if pong_open => match nonce {
                    // Answered immediately: the host times the round trip, so
                    // any delay here would be charged to the network.
                    Some(nonce) => Some(ClientMsg::Pong { nonce }),
                    None => { pong_open = false; None }
                },
                changed = settings_rx.recv(), if settings_open => match changed {
                    Some(s) => Some(s.to_msg()),
                    None => { settings_open = false; None }
                },
                text = clipboard_rx.recv(), if clipboard_open => match text {
                    Some(text) => Some(ClientMsg::Clipboard { text }),
                    None => { clipboard_open = false; None }
                },
                command = files_rx.recv(), if files_open => match command {
                    Some(command) => file_request(
                        command,
                        &writer_conn,
                        &writer_files,
                        &writer_waker,
                    ),
                    None => { files_open = false; None }
                },
            };
            let Some(msg) = msg else { continue };
            if telekin_proto::write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
        // Hold the control stream open: the host reads its EOF as "the
        // session is over" and stops streaming. This task is aborted when
        // the connection actually ends.
        std::future::pending::<()>().await;
    });

    // Host -> viewer control messages (pings, stats).
    let reader_status = status.clone();
    let reader_files = file_state.clone();
    let reader_waker = waker.clone();
    let reader = tokio::spawn(async move {
        while let Ok(msg) = telekin_proto::read_msg::<HostMsg, _>(&mut recv).await {
            match msg {
                HostMsg::Ping { nonce } => {
                    if pong_tx.send(nonce).is_err() {
                        break;
                    }
                }
                HostMsg::Stats { fps, bitrate_kbps, capture_ms, encode_ms, rtt_ms } => {
                    {
                        let mut st = reader_status.lock().expect("status poisoned");
                        st.host_fps = fps;
                        st.bitrate_kbps = bitrate_kbps;
                        st.capture_ms = capture_ms;
                        st.encode_ms = encode_ms;
                        st.rtt_ms = rtt_ms;
                    }
                    // The host-side budget, so a slow session can be attributed
                    // without guessing: grab + encode happen before a frame can
                    // even leave the robot.
                    tracing::info!(
                        "host: {fps:.1} fps, {bitrate_kbps} kbps | capture {capture_ms:.1} ms + encode {encode_ms:.1} ms = {:.1} ms before send | rtt {rtt_ms:.1} ms",
                        capture_ms + encode_ms,
                    );
                }
                HostMsg::Clipboard { text } => {
                    let mut st = reader_status.lock().expect("status poisoned");
                    st.incoming_clipboard = Some(text);
                }
                HostMsg::Files(reply) => {
                    apply_file_reply(reply, &reader_files);
                    reader_waker.get().map(egui::Context::request_repaint);
                }
                _ => {}
            }
        }
    });

    // Accept one uni stream per frame; read bodies concurrently, order them
    // in `collector`, and forward in frame_id order.
    let (arrived_tx, arrived_rx) = mpsc::unbounded_channel::<Arrived>();
    let collector = tokio::spawn(reorder_loop(arrived_rx, frame_tx, keyframe_tx));

    let accept_result = loop {
        match conn.accept_uni().await {
            Ok(recv) => {
                let tx = arrived_tx.clone();
                let files = file_state.clone();
                let woken = waker.clone();
                tokio::spawn(async move {
                    match route_stream(recv, tx, files, woken).await {
                        Ok(()) => {}
                        Err(e) => tracing::debug!("dropped an incoming stream: {e:#}"),
                    }
                });
            }
            Err(e) => break e,
        }
    };
    tracing::info!("stream ended: {accept_result}");

    live.lock().expect("connection slot poisoned").take();
    drop(arrived_tx);
    let _ = collector.await;
    let _ = decode_task.await;
    writer.abort();
    reader.abort();
    Ok(())
}

/// Dial each candidate in turn. A host discovered over mDNS advertises every
/// interface address it has, including ones it cannot actually serve (an IPv6
/// address on a host bound to `0.0.0.0`), so the first address failing is
/// normal rather than an error worth surfacing.
async fn connect_any(
    endpoint: &quinn::Endpoint,
    hosts: &[SocketAddr],
) -> anyhow::Result<quinn::Connection> {
    anyhow::ensure!(!hosts.is_empty(), "no host address to connect to");
    let mut last_err = None;

    for (i, addr) in hosts.iter().enumerate() {
        match endpoint.connect(*addr, "chassis")?.await {
            Ok(conn) => {
                tracing::info!("connected to {addr}");
                return Ok(conn);
            }
            Err(e) => {
                if i + 1 < hosts.len() {
                    tracing::debug!("{addr} did not answer ({e}); trying the next address");
                }
                last_err = Some((*addr, e));
            }
        }
    }

    let (addr, e) = last_err.expect("at least one address was tried");
    Err(anyhow::anyhow!(e)).with_context(|| {
        if hosts.len() > 1 {
            format!("none of the {} advertised addresses answered (last: {addr})", hosts.len())
        } else {
            format!("could not reach {addr}")
        }
    })
}

fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "telekin".into())
}

/// Read one frame stream fully: length-prefixed header, then the bitstream.
async fn read_frame_stream(mut recv: quinn::RecvStream) -> anyhow::Result<Arrived> {
    let header: FrameHeader = telekin_proto::read_msg(&mut recv).await?;
    let data = recv
        .read_to_end(telekin_proto::MAX_FRAME_BYTES as usize)
        .await
        .context("truncated video stream")?;
    Ok(Arrived { header, data })
}

/// Release frames in `frame_id` order, skipping past ones that never showed
/// up rather than stalling the stream behind them.
async fn reorder_loop(
    mut rx: mpsc::UnboundedReceiver<Arrived>,
    tx: mpsc::UnboundedSender<Arrived>,
    keyframe_tx: mpsc::UnboundedSender<()>,
) {
    let mut pending: BTreeMap<u64, Arrived> = BTreeMap::new();
    let mut next_id: Option<u64> = None;

    while let Some(frame) = rx.recv().await {
        let expected = *next_id.get_or_insert(frame.header.frame_id);

        // A frame older than what we've already released is useless.
        if frame.header.frame_id < expected {
            continue;
        }
        pending.insert(frame.header.frame_id, frame);

        // Release everything contiguous from `expected` onward.
        loop {
            let expected = next_id.expect("initialized above");
            if let Some(frame) = pending.remove(&expected) {
                if tx.send(frame).is_err() {
                    return;
                }
                next_id = Some(expected + 1);
                continue;
            }
            // A gap. Wait a little, but not past the window.
            if pending.len() <= REORDER_WINDOW {
                break;
            }
            let skip_to = *pending.keys().next().expect("non-empty");
            tracing::debug!("frame {expected} never arrived; skipping to {skip_to}");
            next_id = Some(skip_to);
            // Decoding mid-GOP after a gap produces garbage until the next IDR.
            let _ = keyframe_tx.send(());
        }
    }

    // Flush whatever is left so the last picture still lands.
    for (_, frame) in pending {
        if tx.send(frame).is_err() {
            return;
        }
    }
}

/// Decode frames sequentially into the shared slot. Runs on a blocking thread:
/// software H.264 decode is CPU-bound.
fn decode_loop(
    mut rx: mpsc::UnboundedReceiver<Arrived>,
    slot: Arc<Mutex<FrameSlot>>,
    keyframe_tx: mpsc::UnboundedSender<()>,
    waker: crate::Waker,
    status: Arc<Mutex<Status>>,
) {
    let mut decoder = match H264Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("cannot create decoder: {e:#}");
            return;
        }
    };
    let mut pixels = Vec::new();
    let mut window_start = std::time::Instant::now();
    let mut window_frames = 0u32;
    let mut window_bytes = 0usize;

    while let Some(frame) = rx.blocking_recv() {
        match decoder.decode_to_rgba(&frame.data, &mut pixels) {
            Ok(Some((w, h))) => {
                {
                    let mut slot = slot.lock().expect("frame slot poisoned");
                    slot.width = w;
                    slot.height = h;
                    std::mem::swap(&mut slot.pixels, &mut pixels);
                    slot.generation += 1;
                }
                // Wake the UI; it repaints on request rather than polling, so
                // without this a new frame would sit undrawn.
                if let Some(ctx) = waker.get() {
                    ctx.request_repaint();
                }

                window_frames += 1;
                window_bytes += frame.data.len();
                let elapsed = window_start.elapsed();
                if elapsed >= std::time::Duration::from_secs(2) {
                    let secs = elapsed.as_secs_f32();
                    let fps = window_frames as f32 / secs;
                    tracing::info!(
                        "{fps:.1} fps, {:.1} Mbps, {w}x{h}",
                        window_bytes as f32 * 8.0 / secs / 1e6,
                    );
                    {
                        let mut st = status.lock().expect("status poisoned");
                        st.view_fps = fps;
                        st.frame_size = (w, h);
                    }
                    window_start = std::time::Instant::now();
                    window_frames = 0;
                    window_bytes = 0;
                }
            }
            Ok(None) => {} // decoder wants more data
            Err(e) => {
                tracing::debug!("decode error on frame {}: {e:#}", frame.header.frame_id);
                let _ = keyframe_tx.send(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Turn a file-pane command into a control message.
///
/// Uploads also need bytes to move, which happens on a task of its own: the
/// writer must come straight back for the next keystroke, not sit here for
/// the length of a copy.
fn file_request(
    command: crate::files::Command,
    conn: &quinn::Connection,
    state: &crate::files::Shared,
    waker: &crate::Waker,
) -> Option<ClientMsg> {
    use crate::files::{Command, Direction};
    use telekin_proto::{FileOp, FileRequest};

    let id = state.lock().expect("file state poisoned").next_id();
    let op = match command {
        Command::List { path } => {
            state.lock().expect("file state poisoned").loading = true;
            FileOp::List { path }
        }
        Command::MakeDir { path } => FileOp::MakeDir { path },
        Command::Remove { path } => FileOp::Remove { path },
        Command::Download { remote, local, batch } => {
            let mut st = state.lock().expect("file state poisoned");
            // The reply carries only an id, so where this lands is recorded
            // here before the request goes out.
            st.remember_destination(id, local);
            st.begin(
                id,
                crate::files::remote_file_name(&remote).to_string(),
                Direction::FromRobot,
                0,
                batch,
            );
            FileOp::Get { path: remote }
        }
        Command::MakeDirAll { path } => FileOp::MakeDirAll { path },
        Command::DownloadTree { remote, local_parent } => {
            let mut st = state.lock().expect("file state poisoned");
            let name = crate::files::remote_file_name(&remote).to_string();
            let batch = st.new_batch(name, Direction::FromRobot);
            st.remember_tree(id, batch, local_parent);
            FileOp::Tree { path: remote }
        }
        Command::UploadTree { local, remote_parent } => {
            let name = local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "folder".into());
            let batch = state
                .lock()
                .expect("file state poisoned")
                .new_batch(name.clone(), Direction::ToRobot);
            // The walk is disk work of unknown size; it runs on its own
            // thread and feeds the queue, which the window drains.
            let state = state.clone();
            let waker = waker.clone();
            std::thread::spawn(move || {
                walk_local_tree(local, remote_parent, name, batch, &state);
                waker.get().map(egui::Context::request_repaint);
            });
            return None;
        }
        Command::Upload { local, remote, batch } => {
            let len = match std::fs::metadata(&local) {
                Ok(m) => m.len(),
                Err(e) => {
                    let mut st = state.lock().expect("file state poisoned");
                    st.error = Some(format!("{}: {e}", local.display()));
                    return None;
                }
            };
            state.lock().expect("file state poisoned").begin(
                id,
                crate::files::remote_file_name(&remote).to_string(),
                Direction::ToRobot,
                len,
                batch,
            );
            let conn = conn.clone();
            let state = state.clone();
            let waker = waker.clone();
            tokio::spawn(async move {
                if let Err(e) = send_upload(id, local, &conn, &state, &waker).await {
                    state
                        .lock()
                        .expect("file state poisoned")
                        .finish(id, Err(format!("{e:#}")));
                    waker.get().map(egui::Context::request_repaint);
                }
            });
            FileOp::Put { path: remote, len }
        }
        Command::Open { path } => {
            state
                .lock()
                .expect("file state poisoned")
                .begin_open(path.clone());
            FileOp::ReadText { path }
        }
        Command::Save { path, text } => {
            if let Some(editor) = state
                .lock()
                .expect("file state poisoned")
                .editor
                .as_mut()
            {
                editor.saving = true;
            }
            // Remembered so the reply, which carries only an id, can be told
            // apart from a delete or a new folder finishing.
            state
                .lock()
                .expect("file state poisoned")
                .remember_save(id);
            FileOp::WriteText { path, text }
        }
    };
    Some(ClientMsg::Files(FileRequest { id, op }))
}

/// Find every file under a local folder and queue it for upload.
///
/// Directories are queued first as `MakeDirAll`, so empty folders are
/// mirrored too; files do not depend on that, because the host creates a
/// missing parent when a file arrives.
fn walk_local_tree(
    local: std::path::PathBuf,
    remote_parent: String,
    name: String,
    batch: u64,
    state: &crate::files::Shared,
) {
    use crate::files::{remote_join, remote_join_rel, Command};

    let remote_root = remote_join(&remote_parent, &name);
    let mut dirs: Vec<Command> = vec![Command::MakeDirAll { path: remote_root.clone() }];
    let mut files: Vec<Command> = Vec::new();
    let mut stack = vec![(local.clone(), String::new())];
    let mut count = 0usize;

    while let Some((dir, rel)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let part = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { part } else { format!("{rel}/{part}") };
            count += 1;
            if count > telekin_proto::MAX_TREE_ENTRIES {
                if let Some(b) = state.lock().expect("file state poisoned").batch_mut(batch) {
                    b.truncated = true;
                }
                break;
            }
            if kind.is_dir() {
                dirs.push(Command::MakeDirAll {
                    path: remote_join_rel(&remote_root, &child_rel),
                });
                stack.push((entry.path(), child_rel));
            } else if kind.is_file() {
                files.push(Command::Upload {
                    local: entry.path(),
                    remote: remote_join_rel(&remote_root, &child_rel),
                    batch: Some(batch),
                });
            }
        }
    }

    let mut st = state.lock().expect("file state poisoned");
    if files.is_empty() && dirs.len() == 1 {
        // An empty folder still gets created on the other side; the row just
        // has nothing to count.
    }
    for cmd in dirs.into_iter().chain(files) {
        st.enqueue(cmd);
    }
}

/// Push one local file up on its own stream.
async fn send_upload(
    id: u64,
    local: std::path::PathBuf,
    conn: &quinn::Connection,
    state: &crate::files::Shared,
    waker: &crate::Waker,
) -> anyhow::Result<()> {
    use std::io::Read;

    let mut file = std::fs::File::open(&local)
        .with_context(|| format!("opening {}", local.display()))?;

    let mut send = conn.open_uni().await?;
    telekin_proto::write_stream_kind(&mut send, telekin_proto::StreamKind::File).await?;
    telekin_proto::write_msg(&mut send, &telekin_proto::FileHeader { id }).await?;

    let mut buf = vec![0u8; telekin_proto::FILE_CHUNK];
    let mut sent: u64 = 0;
    loop {
        let (returned_file, returned_buf, read) = tokio::task::spawn_blocking(move || {
            let n = file.read(&mut buf);
            (file, buf, n)
        })
        .await?;
        file = returned_file;
        buf = returned_buf;

        let n = read?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
        sent += n as u64;
        state.lock().expect("file state poisoned").advance(id, sent);
        waker.get().map(egui::Context::request_repaint);
    }
    send.finish()?;
    // The host confirms with `Done` once the file is committed on its side —
    // this task only knows the bytes left here, which is not the same thing.
    Ok(())
}

/// Fold a reply into the shared state the panes read.
fn apply_file_reply(reply: telekin_proto::FileReply, state: &crate::files::Shared) {
    use telekin_proto::FileReply;

    let mut st = state.lock().expect("file state poisoned");
    match reply {
        FileReply::Listing { listing, .. } => {
            st.loading = false;
            st.error = None;
            st.listing = Some(listing);
        }
        FileReply::Sending { id, len } => st.set_total(id, len),
        FileReply::Text { path, text, .. } => st.opened(&path, text),
        FileReply::Tree { id, tree } => {
            let Some((batch, local_parent)) = st.take_tree(id) else { return };
            let name = crate::files::remote_file_name(&tree.root).to_string();
            let local_root = local_parent.join(&name);
            // Directories first, in the order the host listed them, then
            // every file joins the queue with its batch.
            let mut failed = None;
            if let Err(e) = std::fs::create_dir_all(&local_root) {
                failed = Some(format!("{}: {e}", local_root.display()));
            }
            for d in &tree.dirs {
                let path = crate::files::rel_to_local(&local_root, d);
                if let Err(e) = std::fs::create_dir_all(&path) {
                    failed.get_or_insert_with(|| format!("{}: {e}", path.display()));
                }
            }
            if let Some(b) = st.batch_mut(batch) {
                b.truncated = tree.truncated;
                b.error = failed.clone();
            }
            if failed.is_none() {
                for f in &tree.files {
                    st.enqueue(crate::files::Command::Download {
                        remote: crate::files::remote_join_rel(&tree.root, &f.rel),
                        local: crate::files::rel_to_local(&local_root, &f.rel),
                        batch: Some(batch),
                    });
                }
            }
        }
        FileReply::Done { id } => {
            if st.was_save(id) {
                st.finish_save(Ok(()));
            } else {
                st.finish(id, Ok(()));
            }
        }
        FileReply::Failed { id, reason } => {
            // A failed save belongs on the editor, where the operator is
            // looking, rather than on the file pane behind it.
            if st.was_save(id) {
                st.finish_save(Err(reason));
            } else if let Some((batch, _)) = st.take_tree(id) {
                if let Some(b) = st.batch_mut(batch) {
                    b.error = Some(reason);
                }
            } else {
                st.loading = false;
                st.finish(id, Err(reason.clone()));
                st.error = Some(reason);
            }
        }
    }
}

/// Send an incoming unidirectional stream to whatever handles its kind.
async fn route_stream(
    mut recv: quinn::RecvStream,
    frames: mpsc::UnboundedSender<Arrived>,
    state: crate::files::Shared,
    waker: crate::Waker,
) -> anyhow::Result<()> {
    match telekin_proto::read_stream_kind(&mut recv).await? {
        telekin_proto::StreamKind::Video => {
            let frame = read_frame_stream(recv).await?;
            let _ = frames.send(frame);
            Ok(())
        }
        telekin_proto::StreamKind::File => receive_download(recv, &state, &waker).await,
    }
}

/// Write an incoming file to the destination recorded when it was asked for.
///
/// Written to a `.part` beside the destination and renamed at the end, so an
/// interrupted download cannot be mistaken for a complete file.
async fn receive_download(
    mut recv: quinn::RecvStream,
    state: &crate::files::Shared,
    waker: &crate::Waker,
) -> anyhow::Result<()> {
    use std::io::Write;

    let header: telekin_proto::FileHeader = telekin_proto::read_msg(&mut recv).await?;
    let id = header.id;
    let destination = state
        .lock()
        .expect("file state poisoned")
        .destination(id)
        .context("a file arrived that nothing had asked for")?;

    let part = destination.with_extension("telekin-part");
    let mut file = std::fs::File::create(&part)
        .with_context(|| format!("creating {}", part.display()))?;

    let mut buf = vec![0u8; telekin_proto::FILE_CHUNK];
    let mut got: u64 = 0;
    loop {
        let n = match recv.read(&mut buf).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(e.into());
            }
        };
        if n == 0 {
            continue;
        }
        if let Err(e) = file.write_all(&buf[..n]) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(e.into());
        }
        got += n as u64;
        state.lock().expect("file state poisoned").advance(id, got);
        waker.get().map(egui::Context::request_repaint);
    }

    drop(file);
    std::fs::rename(&part, &destination).with_context(|| {
        format!("moving {} into place at {}", part.display(), destination.display())
    })?;
    state
        .lock()
        .expect("file state poisoned")
        .finish(id, Ok(()));
    waker.get().map(egui::Context::request_repaint);
    Ok(())
}
