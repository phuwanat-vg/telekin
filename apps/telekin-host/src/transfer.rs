//! Session-side plumbing for file requests: what runs them, and where their
//! bytes go.
//!
//! Two rules shape everything here.
//!
//! **Nothing may hold the control loop.** That loop also carries keystrokes,
//! so a directory on a sleeping USB disk or a two-gigabyte copy must not sit
//! in front of it. Every request is handed to a task and answered later
//! through a channel; the reply channel is owned by the writer, never by the
//! worker.
//!
//! **No transfer sits in memory whole.** Files move a chunk at a time, with
//! the actual disk calls on the blocking pool, so a robot with a gigabyte of
//! RAM can still receive a file larger than that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use telekin_proto::{FileHeader, FileOp, FileReply, FileRequest, HostMsg, StreamKind};
use telekin_transport::quinn;
use tokio::sync::mpsc;

/// Uploads announced on the control stream but whose bytes have not arrived.
///
/// The announcement carries the destination; the stream that follows carries
/// only an id. Keeping them apart means a stream nobody announced is rejected
/// rather than being written somewhere guessed.
pub type PendingUploads = Arc<Mutex<HashMap<u64, String>>>;

pub fn pending() -> PendingUploads {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Act on one file request. Returns immediately; the answer arrives on `out`.
pub fn handle(
    request: FileRequest,
    conn: &quinn::Connection,
    out: &mpsc::UnboundedSender<HostMsg>,
    uploads: &PendingUploads,
) {
    let id = request.id;
    let out = out.clone();
    let conn = conn.clone();
    let uploads = uploads.clone();

    match request.op {
        FileOp::List { path } => {
            tokio::task::spawn_blocking(move || {
                let reply = match crate::files::list(&path) {
                    Ok(listing) => FileReply::Listing { id, listing },
                    Err(e) => failed(id, e),
                };
                let _ = out.send(HostMsg::Files(reply));
            });
        }
        FileOp::MakeDir { path } => {
            tokio::task::spawn_blocking(move || {
                let _ = out.send(HostMsg::Files(done_or_failed(id, crate::files::make_dir(&path))));
            });
        }
        FileOp::Remove { path } => {
            tokio::task::spawn_blocking(move || {
                let _ = out.send(HostMsg::Files(done_or_failed(id, crate::files::remove(&path))));
            });
        }
        FileOp::Get { path } => {
            tokio::spawn(async move {
                if let Err(e) = send_file(id, path, &conn, &out).await {
                    let _ = out.send(HostMsg::Files(failed(id, e)));
                }
            });
        }
        FileOp::Tree { path } => {
            tokio::task::spawn_blocking(move || {
                let reply = match crate::files::tree(&path) {
                    Ok(tree) => FileReply::Tree { id, tree },
                    Err(e) => failed(id, e),
                };
                let _ = out.send(HostMsg::Files(reply));
            });
        }
        FileOp::MakeDirAll { path } => {
            tokio::task::spawn_blocking(move || {
                let _ = out.send(HostMsg::Files(done_or_failed(
                    id,
                    crate::files::make_dir_all(&path),
                )));
            });
        }
        FileOp::ReadText { path } => {
            tokio::task::spawn_blocking(move || {
                let reply = match crate::files::read_text(&path) {
                    Ok(text) => FileReply::Text { id, path, text },
                    Err(e) => failed(id, e),
                };
                let _ = out.send(HostMsg::Files(reply));
            });
        }
        FileOp::WriteText { path, text } => {
            tokio::task::spawn_blocking(move || {
                let _ = out.send(HostMsg::Files(done_or_failed(
                    id,
                    crate::files::write_text(&path, &text),
                )));
            });
        }
        FileOp::Put { path, len } => {
            if len > telekin_proto::MAX_FILE_BYTES {
                let _ = out.send(HostMsg::Files(FileReply::Failed {
                    id,
                    reason: format!(
                        "{len} bytes is past this build's {} byte transfer limit",
                        telekin_proto::MAX_FILE_BYTES
                    ),
                }));
                return;
            }
            uploads.lock().expect("uploads poisoned").insert(id, path);
        }
    }
}

fn failed(id: u64, e: anyhow::Error) -> FileReply {
    FileReply::Failed { id, reason: format!("{e:#}") }
}

fn done_or_failed(id: u64, result: anyhow::Result<()>) -> FileReply {
    match result {
        Ok(()) => FileReply::Done { id },
        Err(e) => failed(id, e),
    }
}

/// Send one file on its own unidirectional stream.
async fn send_file(
    id: u64,
    path: String,
    conn: &quinn::Connection,
    out: &mpsc::UnboundedSender<HostMsg>,
) -> anyhow::Result<()> {
    use std::io::Read;

    let (mut file, len) =
        tokio::task::spawn_blocking(move || crate::files::open_for_read(&path)).await??;

    // Announced before any bytes, so the viewer can size a progress bar and
    // knows the transfer is real rather than about to fail.
    let _ = out.send(HostMsg::Files(FileReply::Sending { id, len }));

    let mut send = conn.open_uni().await?;
    telekin_proto::write_stream_kind(&mut send, StreamKind::File).await?;
    telekin_proto::write_msg(&mut send, &FileHeader { id }).await?;

    let mut buf = vec![0u8; telekin_proto::FILE_CHUNK];
    loop {
        // The read happens on the blocking pool and the buffer is handed back,
        // which keeps one allocation for the whole transfer.
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
    }
    send.finish()?;
    Ok(())
}

/// Take the unidirectional streams a viewer opens, and write them to disk.
pub async fn accept_uploads(
    conn: quinn::Connection,
    uploads: PendingUploads,
    out: mpsc::UnboundedSender<HostMsg>,
) {
    loop {
        let recv = match conn.accept_uni().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("no more incoming streams: {e}");
                break;
            }
        };
        let uploads = uploads.clone();
        let out = out.clone();
        tokio::spawn(async move {
            match receive(recv, &uploads).await {
                Ok(id) => {
                    let _ = out.send(HostMsg::Files(FileReply::Done { id }));
                }
                Err(Failure { id, error }) => {
                    tracing::warn!("upload failed: {error:#}");
                    // Without an id there is no transfer to attach this to;
                    // the log is all that can be offered.
                    if let Some(id) = id {
                        let _ = out.send(HostMsg::Files(failed(id, error)));
                    }
                }
            }
        });
    }
}

/// An upload that went wrong, and the transfer it belonged to when known.
struct Failure {
    id: Option<u64>,
    error: anyhow::Error,
}

fn anon(error: anyhow::Error) -> Failure {
    Failure { id: None, error }
}

fn during(id: u64) -> impl Fn(anyhow::Error) -> Failure {
    move |error| Failure { id: Some(id), error }
}

async fn receive(mut recv: quinn::RecvStream, uploads: &PendingUploads) -> Result<u64, Failure> {
    use std::io::Write;

    let kind = telekin_proto::read_stream_kind(&mut recv)
        .await
        .map_err(anon)?;
    if kind != StreamKind::File {
        return Err(anon(anyhow::anyhow!(
            "a viewer opened a {kind:?} stream, which only the host may send"
        )));
    }
    let header: FileHeader = telekin_proto::read_msg(&mut recv).await.map_err(anon)?;
    let id = header.id;
    let blame = during(id);

    let path = uploads
        .lock()
        .expect("uploads poisoned")
        .remove(&id)
        .ok_or_else(|| blame(anyhow::anyhow!("no upload was announced for this stream")))?;

    let (mut file, incoming) = tokio::task::spawn_blocking(move || crate::files::begin_write(&path))
        .await
        .map_err(|e| blame(e.into()))?
        .map_err(&blame)?;

    // From here on every failure has to clear the part file: a half-written
    // upload that looks finished is worse than no upload at all.
    let mut buf = vec![0u8; telekin_proto::FILE_CHUNK];
    let mut written: u64 = 0;
    loop {
        let read = recv.read(&mut buf).await;
        let n = match read {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(e) => return Err(discard(incoming, blame(e.into())).await),
        };
        if n == 0 {
            continue;
        }
        written += n as u64;
        if written > telekin_proto::MAX_FILE_BYTES {
            let e = anyhow::anyhow!("upload ran past the transfer limit");
            return Err(discard(incoming, blame(e)).await);
        }

        let chunk = buf[..n].to_vec();
        let (returned_file, result) = match tokio::task::spawn_blocking(move || {
            let r = file.write_all(&chunk);
            (file, r)
        })
        .await
        {
            Ok(pair) => pair,
            Err(e) => return Err(discard(incoming, blame(e.into())).await),
        };
        file = returned_file;
        if let Err(e) = result {
            return Err(discard(incoming, blame(e.into())).await);
        }
    }

    drop(file);
    tokio::task::spawn_blocking(move || incoming.commit())
        .await
        .map_err(|e| blame(e.into()))?
        .map_err(&blame)?;
    Ok(id)
}

/// Throw away a part file, then report why.
async fn discard(incoming: crate::files::Incoming, failure: Failure) -> Failure {
    let _ = tokio::task::spawn_blocking(move || incoming.abandon()).await;
    failure
}
