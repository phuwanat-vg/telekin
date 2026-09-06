//! The capture -> encode -> send pipeline.
//!
//! Capture and encode are CPU-bound and live on a dedicated OS thread.
//! Encoded frames cross into the async runtime through a *depth-1* channel:
//! if the network is behind, the newest frame replaces the queued one rather
//! than piling up. Showing the current screen late is useless; showing it
//! now is the entire point.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use telekin_codec::{H264Encoder, VideoEncoder};
use telekin_proto::{Codec, FrameHeader};
use telekin_transport::quinn;

/// Backstop re-encode of an unchanged screen.
///
/// This is deliberately long. A viewer that joins mid-session already gets an
/// IDR (the encoder is created fresh per stream), and one that loses frames or
/// hits a decode error asks for a keyframe over the control stream. So this
/// only covers the case where both of those fail silently.
///
/// It used to be 500 ms, which meant a robot sitting on a still desktop
/// encoded two full 1080p keyframes every second — about 11% of a Pi 5 core
/// spent retransmitting a picture the viewer already had.
const IDLE_REFRESH: Duration = Duration::from_secs(10);

/// Rolling pipeline cost, published for the control stream to report back.
/// The viewer needs these to tell "the robot cannot encode fast enough" apart
/// from "the network is slow".
#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineStats {
    pub fps: f32,
    pub capture_ms: f32,
    pub encode_ms: f32,
    pub bitrate_kbps: u32,
}

pub struct Config {
    pub conn: quinn::Connection,
    pub monitor: u32,
    pub max_fps: u16,
    pub bitrate_kbps: u32,
    pub stats: Arc<std::sync::Mutex<PipelineStats>>,
    /// Hard ceiling on how much of one core the pipeline may use, as a
    /// percentage. `None` leaves it uncapped.
    pub max_cpu_percent: Option<u8>,
    /// Percentage of the captured resolution to encode, 25..=100.
    pub scale_percent: u8,
}

/// Box-filter downscale of a BGRA image.
///
/// Encode cost tracks pixel count, so shrinking here is the cheapest large
/// saving available on a small board. Averaging the source block rather than
/// point-sampling matters for text, which is most of a robot's screen —
/// nearest-neighbour turns thin glyph strokes into aliased noise that the
/// encoder then spends bits and time on.
fn downscale_bgra(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize, dst: &mut Vec<u8>) {
    dst.resize(dw * dh * 4, 0);
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = (((y + 1) * sh).div_ceil(dh)).min(sh).max(y0 + 1);
        for x in 0..dw {
            let x0 = x * sw / dw;
            let x1 = (((x + 1) * sw).div_ceil(dw)).min(sw).max(x0 + 1);

            let (mut b, mut g, mut r, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let row = sy * sw * 4;
                for sx in x0..x1 {
                    let p = row + sx * 4;
                    b += src[p] as u32;
                    g += src[p + 1] as u32;
                    r += src[p + 2] as u32;
                    n += 1;
                }
            }
            let o = (y * dw + x) * 4;
            dst[o] = (b / n) as u8;
            dst[o + 1] = (g / n) as u8;
            dst[o + 2] = (r / n) as u8;
            dst[o + 3] = 0xff;
        }
    }
}

pub struct StreamHandle {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
}

impl StreamHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }
}

struct Encoded {
    header: FrameHeader,
    data: Vec<u8>,
}

pub fn spawn(config: Config) -> StreamHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let keyframe = Arc::new(AtomicBool::new(false));

    // Depth 1: the sender overwrites rather than queues (see module docs).
    let (tx, rx) = mpsc::channel::<Encoded>(1);

    std::thread::spawn({
        let stop = stop.clone();
        let keyframe = keyframe.clone();
        let stats = config.stats.clone();
        let cfg = LoopConfig {
            monitor: config.monitor,
            max_fps: config.max_fps,
            bitrate_kbps: config.bitrate_kbps,
            max_cpu_percent: config.max_cpu_percent,
            scale_percent: config.scale_percent,
        };
        move || {
            if let Err(e) = capture_loop(cfg, tx, stop, keyframe, stats) {
                tracing::error!("capture loop stopped: {e:#}");
            }
        }
    });

    tokio::spawn({
        let stop = stop.clone();
        async move {
            if let Err(e) = send_loop(config.conn, rx, stop).await {
                tracing::debug!("send loop stopped: {e:#}");
            }
        }
    });

    StreamHandle { stop, keyframe }
}

/// Everything the capture thread needs, kept together so the signature stays
/// readable as the pipeline grows knobs.
struct LoopConfig {
    monitor: u32,
    max_fps: u16,
    bitrate_kbps: u32,
    max_cpu_percent: Option<u8>,
    scale_percent: u8,
}

fn capture_loop(
    cfg: LoopConfig,
    tx: mpsc::Sender<Encoded>,
    stop: Arc<AtomicBool>,
    keyframe_req: Arc<AtomicBool>,
    stats: Arc<std::sync::Mutex<PipelineStats>>,
) -> anyhow::Result<()> {
    let LoopConfig { monitor, max_fps, bitrate_kbps, max_cpu_percent, scale_percent } = cfg;
    let scale_percent = scale_percent.clamp(25, 100);
    let mut scaled: Vec<u8> = Vec::new();
    let mut capture = telekin_capture::open(monitor)?;
    let mut encoder = H264Encoder::new(bitrate_kbps, max_fps)?;

    let frame_budget = Duration::from_secs_f64(1.0 / max_fps as f64);
    let started = Instant::now();
    let mut frame_id: u64 = 0;
    let mut last_sent = Instant::now();

    // Per-window stats so a slow stage shows up in the log instead of just
    // as a low frame rate. Capture and encode are the two costs on a host.
    let mut win_start = Instant::now();
    let (mut win_frames, mut win_capture_ms, mut win_encode_ms) = (0u32, 0f32, 0f32);
    let mut win_bytes = 0usize;

    // CPU accounting for the ceiling. Sampling the whole process catches the
    // encoder's worker threads and the QUIC send path, neither of which the
    // per-stage timers see.
    let mut cpu_at_tick = crate::cpu::process_time();
    if max_cpu_percent.is_some() && cpu_at_tick.is_none() {
        tracing::warn!(
            "cannot read this process's CPU time on this platform; \
             --max-cpu-percent will only limit the frame rate"
        );
    }

    while !stop.load(Ordering::Relaxed) {
        // Sleep until the screen actually changes rather than waking on a
        // timer to ask. On an idle robot this is the difference between a few
        // percent of a core and none at all, and a change is picked up the
        // moment it happens instead of up to a frame late.
        capture.wait_for_change(frame_budget);

        let tick = Instant::now();
        let cpu_before = cpu_at_tick.take().or_else(crate::cpu::process_time);

        let capture_start = Instant::now();
        let fresh = capture.next_frame()?;
        let capture_ms = capture_start.elapsed().as_secs_f32() * 1000.0;
        // On an idle desktop, refresh periodically instead of going silent.
        let idle_refresh = fresh.is_none() && last_sent.elapsed() >= IDLE_REFRESH;
        let frame = match (fresh, idle_refresh) {
            (Some(f), _) => Some(f),
            (None, true) => capture.last_frame(),
            (None, false) => None,
        };

        let Some(frame) = frame else {
            // Nothing to send; the capture backend already waited.
            sleep_remaining(tick, frame_budget);
            continue;
        };

        if idle_refresh || keyframe_req.swap(false, Ordering::Relaxed) {
            encoder.force_keyframe();
        }

        let encode_start = Instant::now();
        // Scaling is charged to encode: it exists to make encoding cheaper, so
        // hiding it elsewhere would misreport the trade.
        let (enc_w, enc_h, pixels) = if scale_percent == 100 {
            (frame.width, frame.height, frame.bgra)
        } else {
            let dw = (frame.width as usize * scale_percent as usize / 100).max(2);
            let dh = (frame.height as usize * scale_percent as usize / 100).max(2);
            downscale_bgra(
                frame.bgra,
                frame.width as usize,
                frame.height as usize,
                dw,
                dh,
                &mut scaled,
            );
            (dw as u32, dh as u32, scaled.as_slice())
        };

        let Some(encoded) = encoder.encode_bgra(enc_w, enc_h, pixels)? else {
            sleep_remaining(tick, frame_budget);
            continue;
        };
        let encode_ms = encode_start.elapsed().as_secs_f32() * 1000.0;

        let header = FrameHeader {
            frame_id,
            codec: Codec::H264,
            width: enc_w & !1,
            height: enc_h & !1,
            keyframe: encoded.keyframe,
            timestamp_us: started.elapsed().as_micros() as u64,
        };
        frame_id += 1;

        let encoded_len = encoded.data.len();
        let item = Encoded { header, data: encoded.data };
        // Overwrite-on-full: drop the stale queued frame, keep the new one.
        match tx.try_send(item) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(item)) => {
                tracing::trace!("network behind; replacing queued frame");
                if tx.blocking_send(item).is_err() {
                    break; // receiver gone
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
        last_sent = Instant::now();

        win_frames += 1;
        win_capture_ms += capture_ms;
        win_encode_ms += encode_ms;
        win_bytes += encoded_len;
        let elapsed = win_start.elapsed();
        if elapsed >= Duration::from_secs(2) {
            let n = win_frames.max(1) as f32;
            let secs = elapsed.as_secs_f32();
            let snapshot = PipelineStats {
                fps: win_frames as f32 / secs,
                capture_ms: win_capture_ms / n,
                encode_ms: win_encode_ms / n,
                bitrate_kbps: (win_bytes as f32 * 8.0 / secs / 1000.0) as u32,
            };
            tracing::info!(
                "{:.1} fps: capture {:.1} ms, encode {:.1} ms per frame (budget {:.1} ms)",
                snapshot.fps,
                snapshot.capture_ms,
                snapshot.encode_ms,
                frame_budget.as_secs_f32() * 1000.0,
            );
            *stats.lock().expect("stats poisoned") = snapshot;
            win_start = Instant::now();
            win_frames = 0;
            win_capture_ms = 0.0;
            win_encode_ms = 0.0;
            win_bytes = 0;
        }
        // Encoding is where this pipeline's CPU goes, and its cost is per
        // frame — so the only honest way to cap CPU is to run less often.
        //
        // The budget is measured, not estimated: CPU actually charged to this
        // process over the frame, divided by the cap, is the wall time this
        // frame has to occupy. Sleep off whatever is left of it.
        let pacing = match (max_cpu_percent, cpu_before) {
            (Some(cap), Some(before)) => {
                let after = crate::cpu::process_time().unwrap_or(before);
                let used = after.saturating_sub(before);
                cpu_at_tick = Some(after);

                let cap = (cap.clamp(1, 100) as f32) / 100.0;
                // Cap the stretch so one expensive frame cannot stall the
                // pipeline for minutes; the ceiling is then met across
                // frames rather than within one.
                let needed = used.div_f32(cap).min(Duration::from_secs(2));
                frame_budget.max(needed)
            }
            // No cap, or no way to measure: pace by frame rate alone.
            _ => frame_budget,
        };
        sleep_remaining(tick, pacing);
    }
    Ok(())
}

fn sleep_remaining(tick: Instant, budget: Duration) {
    if let Some(rest) = budget.checked_sub(tick.elapsed()) {
        std::thread::sleep(rest);
    }
}

/// Send each frame on its own unidirectional QUIC stream, so a retransmit of
/// one frame never delays the next — the property TCP-based remote-desktop
/// protocols cannot offer.
async fn send_loop(
    conn: quinn::Connection,
    mut rx: mpsc::Receiver<Encoded>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    while let Some(frame) = rx.recv().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut send = conn.open_uni().await?;
        telekin_proto::write_stream_kind(&mut send, telekin_proto::StreamKind::Video).await?;
        telekin_proto::write_msg(&mut send, &frame.header).await?;
        send.write_all(&frame.data).await?;
        send.finish()?;
    }
    Ok(())
}
