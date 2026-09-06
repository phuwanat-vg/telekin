//! TK/1 — the Telekin wire protocol.
//!
//! Runs on top of QUIC (see `telekin-transport`) and is split across QUIC's
//! stream types so that video never blocks input and stale frames never
//! delay fresh ones:
//!
//! * **Control stream** — the first bidirectional stream, opened by the
//!   client. Carries handshake, session control, input events and
//!   keepalives as length-prefixed [`postcard`] messages. Reliable and
//!   ordered, which is exactly what input needs.
//! * **Unidirectional streams** — opened by either side, each beginning with
//!   a one-byte [`StreamKind`]:
//!   * **Video**, one per encoded frame, opened by the host. Frames are
//!     independent at the transport level, so the loss or retransmit of one
//!     never head-of-line-blocks the next — the core advantage over
//!     TCP-based protocols (NX, VNC).
//!   * **File**, one per transfer, opened by whichever side is sending.
//!     Because it is a separate stream, copying a large file cannot delay a
//!     frame or a keystroke; and because file work never touches the
//!     encoder, transfers cost the robot almost nothing when no one is
//!     watching its screen.
//!
//! File transfer is deliberately independent of streaming: a session can
//! browse and copy files without ever sending `StartStream`, which is the
//! cheapest thing a robot can be asked to do.
//!
//! All multi-byte framing integers are little-endian.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Protocol version. Bump on incompatible changes.
pub const PROTO_VERSION: u16 = 7;

/// Hard cap for a single control message (sanity bound, not a limit hit in practice).
pub const MAX_CONTROL_MSG: u32 = 16 * 1024 * 1024;

/// Hard cap for one encoded video frame.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Control-stream messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on the control stream.
    Hello {
        proto_version: u16,
        /// Account configured on the host with `chassis --add-user`.
        username: String,
        /// Sent in the clear inside the TLS session, and checked against an
        /// Argon2 hash on the host — the host never stores anything that
        /// could be replayed if its disk were read.
        password: String,
        client_name: String,
    },
    /// Ask the host to start streaming a monitor.
    StartStream {
        monitor: u32,
        max_fps: u16,
        bitrate_kbps: u32,
        /// Percentage of the monitor's own resolution to encode, 25..=100.
        ///
        /// Downscaling happens on the host, before the encoder. Encode cost
        /// falls with pixel count, so this is the effective latency and CPU
        /// control — and unlike changing the robot's display mode it does not
        /// disturb anyone sitting in front of it.
        scale_percent: u8,
        /// Ceiling on the share of one host core the pipeline may use.
        /// `None` leaves the host's own default in place.
        max_cpu_percent: Option<u8>,
    },
    StopStream,
    /// Ask the encoder for an immediate keyframe (e.g. after decode errors).
    RequestKeyframe,
    Input(InputEvent),
    Pong { nonce: u64 },
    /// Text copied on the viewer, to be placed on the host's clipboard.
    Clipboard { text: String },
    /// Browse or transfer files on the host. Answered by [`HostMsg::Files`].
    Files(FileRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostMsg {
    HelloOk {
        host_name: String,
        monitors: Vec<MonitorInfo>,
    },
    HelloErr { reason: String },
    Ping { nonce: u64 },
    /// Periodic host-side stats, for display in the viewer.
    Stats {
        fps: f32,
        bitrate_kbps: u32,
        /// Time to grab one frame from the display.
        capture_ms: f32,
        /// Time to colour-convert and H.264 encode one frame.
        encode_ms: f32,
        /// Control-stream round trip, measured by the host's ping.
        rtt_ms: f32,
    },
    /// The host's clipboard changed; mirror it locally.
    Clipboard { text: String },
    /// Answer to a [`ClientMsg::Files`], matched by its `id`.
    Files(FileReply),
}

/// Largest clipboard transfer accepted in either direction.
///
/// Clipboards are for text a person copied, so this is generous for that and
/// still small enough that a runaway paste cannot stall the control stream —
/// which also carries input, where latency actually matters.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute position, normalized to the streamed monitor (0.0..=1.0).
    MouseMove { x: f32, y: f32 },
    MouseButton { button: MouseButton, down: bool },
    /// Scroll in lines; positive dy scrolls up.
    MouseWheel { dx: f32, dy: f32 },
    /// Physical-key event for keys that matter as press/release (teleop!).
    Key { key: KeyCode, down: bool },
    /// Unicode text that doesn't map to a physical [`KeyCode`].
    Text { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Portable physical-key identifiers (a pragmatic subset of the USB HID /
/// W3C `code` set). The client maps its windowing-system keycodes into
/// these; the host maps them into OS injection calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyCode {
    // Letters
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    // Top-row digits
    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,
    // Function keys
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    // Modifiers
    ShiftLeft, ShiftRight, ControlLeft, ControlRight,
    AltLeft, AltRight, MetaLeft, MetaRight,
    // Navigation / editing
    Escape, Tab, CapsLock, Space, Enter, Backspace, Delete, Insert,
    Home, End, PageUp, PageDown,
    ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
    // Punctuation
    Minus, Equal, BracketLeft, BracketRight, Backslash,
    Semicolon, Quote, Backquote, Comma, Period, Slash,
}

// ---------------------------------------------------------------------------
// Video-stream framing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    H264,
}

/// Header written (length-prefixed) at the start of each per-frame
/// unidirectional stream, followed by the raw encoded bitstream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameHeader {
    pub frame_id: u64,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    /// Host-side capture timestamp, microseconds since stream start.
    pub timestamp_us: u64,
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// A request to look at or change the host's filesystem.
///
/// The host does this work as the account that signed in, so the answer to
/// "what may this reach?" is the same as for that user's own shell — no more
/// and no less.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRequest {
    /// Matches the reply to the request. Chosen by the viewer.
    pub id: u64,
    pub op: FileOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileOp {
    /// List a directory. An empty path means the account's home directory,
    /// which is where a session should start.
    List { path: String },
    MakeDir { path: String },
    /// Delete a file, or an empty directory. Deleting a directory tree is
    /// deliberately not offered: one mistaken click on a robot mid-experiment
    /// is not something a progress bar can undo.
    Remove { path: String },
    /// Ask for a file. The host answers [`FileReply::Sending`] and then opens
    /// a [`StreamKind::File`] stream carrying the bytes.
    Get { path: String },
    /// Announce a file the viewer is about to send on its own
    /// [`StreamKind::File`] stream.
    Put { path: String, len: u64 },
    /// Fetch a small text file to edit in place.
    ///
    /// Sent whole on the control stream rather than as a file stream. The
    /// things this is for — a launch file, a YAML parameter set, a systemd
    /// unit — are a few kilobytes, and the round trip of announcing a stream
    /// and waiting for it is more machinery than the payload deserves. The
    /// clipboard already moves up to a megabyte the same way, so the bound is
    /// a familiar one.
    ReadText { path: String },
    /// Write an edited file back.
    WriteText { path: String, text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileReply {
    Listing { id: u64, listing: DirListing },
    /// The contents of a file asked for with [`FileOp::ReadText`]. Carries the
    /// path back so a reply that arrives late cannot be shown against a file
    /// the operator has since moved on from.
    Text { id: u64, path: String, text: String },
    /// A file stream carrying `id` follows.
    Sending { id: u64, len: u64 },
    Done { id: u64 },
    Failed { id: u64, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirListing {
    /// The directory listed, absolute and cleaned up.
    pub path: String,
    /// Its parent, or `None` at the root. Saves the viewer from having to
    /// know how the host's paths are spelled.
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes. Meaningless for directories, which report 0.
    pub len: u64,
    /// Seconds since the Unix epoch, when the host could tell.
    pub modified: Option<u64>,
    /// A symlink. Followed for its metadata, but worth showing.
    pub is_link: bool,
}

/// Header at the start of a [`StreamKind::File`] stream, followed by the
/// file's bytes until the stream ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHeader {
    /// The [`FileRequest::id`] this transfer belongs to.
    pub id: u64,
}

/// Largest single file this will move, in either direction.
///
/// Not a technical limit — a bound that turns a corrupt length into an error
/// instead of a machine that fills its disk.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Largest file the in-place editor will open or save.
///
/// Not a limit on what can be *transferred* — that is [`MAX_FILE_BYTES`], and
/// a big file should be copied rather than edited over a link. This is the
/// point past which a file stops being something a person edits by hand and
/// the whole-message approach stops being appropriate.
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// Bytes per read/write while copying. Large enough to keep QUIC fed, small
/// enough that progress moves visibly and a cancel is noticed promptly.
pub const FILE_CHUNK: usize = 128 * 1024;

// ---------------------------------------------------------------------------
// Unidirectional stream tagging
// ---------------------------------------------------------------------------

/// What a unidirectional stream carries, written as its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum StreamKind {
    Video = 0,
    File = 1,
}

impl StreamKind {
    fn from_byte(b: u8) -> anyhow::Result<Self> {
        match b {
            0 => Ok(Self::Video),
            1 => Ok(Self::File),
            other => anyhow::bail!("unknown stream kind {other}"),
        }
    }
}

/// Tag a unidirectional stream. Must be the first thing written to it.
pub async fn write_stream_kind<W>(w: &mut W, kind: StreamKind) -> anyhow::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    w.write_all(&[kind as u8]).await?;
    Ok(())
}

/// Read the tag from a unidirectional stream.
pub async fn read_stream_kind<R>(r: &mut R) -> anyhow::Result<StreamKind>
where
    R: AsyncReadExt + Unpin,
{
    let mut b = [0u8; 1];
    r.read_exact(&mut b).await?;
    StreamKind::from_byte(b[0])
}

// ---------------------------------------------------------------------------
// Length-prefixed postcard framing over async streams
// ---------------------------------------------------------------------------

/// Write one message: u32-LE length, then postcard bytes.
pub async fn write_msg<T, W>(w: &mut W, msg: &T) -> anyhow::Result<()>
where
    T: Serialize,
    W: AsyncWriteExt + Unpin,
{
    let bytes = postcard::to_stdvec(msg)?;
    let len = u32::try_from(bytes.len())?;
    anyhow::ensure!(len <= MAX_CONTROL_MSG, "message too large: {len} bytes");
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed postcard message.
pub async fn read_msg<T, R>(r: &mut R) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
    R: AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    anyhow::ensure!(len <= MAX_CONTROL_MSG, "incoming message too large: {len} bytes");
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_control_msg() {
        let msg = ClientMsg::Input(InputEvent::MouseMove { x: 0.25, y: 0.75 });
        let mut buf = Vec::new();
        write_msg(&mut buf, &msg).await.unwrap();
        let back: ClientMsg = read_msg(&mut buf.as_slice()).await.unwrap();
        match back {
            ClientMsg::Input(InputEvent::MouseMove { x, y }) => {
                assert_eq!(x, 0.25);
                assert_eq!(y, 0.75);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
