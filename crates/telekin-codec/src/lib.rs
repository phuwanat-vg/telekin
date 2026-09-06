//! Video codec layer.
//!
//! The rest of the system only sees [`VideoEncoder`] / [`VideoDecoder`], so
//! hardware encoders (NVENC on Jetson, VAAPI on x86 SBCs) can be added later
//! without touching capture or transport. The baseline implementation is
//! OpenH264 in software — it runs everywhere, including ARM boards.

use anyhow::Context;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{Encoder, EncoderConfig, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::OpenH264API;

/// Whether to bias the encoder towards speed over compression.
///
/// Off by default: it costs picture quality at a given bitrate. Worth turning
/// on when the host is a small board whose CPU is needed for something else —
/// which on a robot it always is.
fn low_cpu() -> bool {
    matches!(
        std::env::var("TELEKIN_LOW_CPU").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Slice count / thread count for the encoder.
///
/// More slices encode faster but cost a little compression efficiency, and
/// past a handful the returns flatten out — so this is capped well below the
/// core count on a big machine, and stays at least 2 on a small SBC.
fn encoder_threads() -> u16 {
    // An explicit cap matters on a shared board: on a robot the other half of
    // the CPU is running ROS 2, and taking half the cores by default may be
    // more than you want to give up.
    if let Some(n) = std::env::var("TELEKIN_ENCODER_THREADS")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        return n.clamp(1, 16);
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores / 2).clamp(2, 8) as u16
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub trait VideoEncoder: Send {
    /// Encode one BGRA frame. Returns `None` when the rate controller
    /// decided to skip the frame.
    fn encode_bgra(&mut self, width: u32, height: u32, bgra: &[u8])
        -> anyhow::Result<Option<EncodedFrame>>;
    fn force_keyframe(&mut self);
}

pub trait VideoDecoder: Send {
    /// Feed one encoded frame; on success writes BGRA pixels into `out`
    /// (resized as needed) and returns the frame dimensions. Returns
    /// `Ok(None)` when the decoder needs more data before producing output.
    fn decode_to_bgra(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> anyhow::Result<Option<(u32, u32)>>;

    /// Same, but leaving the channels in RGBA order.
    ///
    /// The decoder produces RGBA natively, so a consumer that wants RGBA (a
    /// GPU texture upload, say) should use this rather than pay for two
    /// channel swaps over a full frame.
    fn decode_to_rgba(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> anyhow::Result<Option<(u32, u32)>>;
}

// ---------------------------------------------------------------------------
// OpenH264 encoder
// ---------------------------------------------------------------------------

pub struct H264Encoder {
    encoder: Encoder,
    bitrate_kbps: u32,
    max_fps: u16,
    yuv: I420Buffer,
    dims: (u32, u32),
    pending_keyframe: bool,
}

impl H264Encoder {
    pub fn new(bitrate_kbps: u32, max_fps: u16) -> anyhow::Result<Self> {
        Ok(Self {
            encoder: Self::make_encoder(bitrate_kbps, max_fps)?,
            bitrate_kbps,
            max_fps,
            yuv: I420Buffer::default(),
            dims: (0, 0),
            pending_keyframe: false,
        })
    }

    /// Split each frame into slices so OpenH264's worker threads have
    /// something to divide.
    ///
    /// `iMultipleThreadIdc` alone does nothing: the default slice mode is one
    /// slice per frame, and a slice cannot be split across threads. The
    /// `openh264` crate does not expose slice configuration, so we read the
    /// live parameter block back through the raw API, set the slice mode, and
    /// write it again. Without this, a 20-core machine encodes 1080p at the
    /// same speed as a single core.
    fn enable_slice_threading(encoder: &mut Encoder, threads: u16) -> anyhow::Result<()> {
        use openh264_sys2::{
            ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, LOW_COMPLEXITY, SEncParamExt,
            SM_FIXEDSLCNUM_SLICE,
        };

        // One thread means one slice, which is the encoder's default. Asking
        // for it explicitly is a legitimate choice on a board where the CPU is
        // needed elsewhere, so this is a no-op rather than an error.
        if threads <= 1 {
            return Ok(());
        }

        unsafe {
            let api = encoder.raw_api();
            let mut params = SEncParamExt::default();
            let rc = api.get_option(
                ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                (&mut params as *mut SEncParamExt).cast(),
            );
            anyhow::ensure!(rc == 0, "could not read encoder parameters (code {rc})");

            params.iMultipleThreadIdc = threads;
            for layer in params.sSpatialLayers.iter_mut() {
                layer.sSliceArgument.uiSliceMode = SM_FIXEDSLCNUM_SLICE;
                layer.sSliceArgument.uiSliceNum = threads as u32;
            }

            if low_cpu() {
                // Trade compression for encoder time. On an SBC the encoder is
                // the whole latency and CPU budget, and a desktop stream is
                // mostly flat colour that survives these settings well.
                params.iComplexityMode = LOW_COMPLEXITY;
                // Deblocking runs over every edge of every frame; skipping it
                // is one of the larger single savings available here.
                params.iLoopFilterDisableIdc = 1;
                // Scene-change detection re-examines the frame to decide
                // whether to force an IDR. Damage tracking already tells us
                // when the screen changed.
                params.bEnableSceneChangeDetect = false;
                // One reference frame: less motion search per macroblock.
                params.iNumRefFrame = 1;
            }

            let rc = api.set_option(
                ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                (&mut params as *mut SEncParamExt).cast(),
            );
            anyhow::ensure!(rc == 0, "could not apply slice threading (code {rc})");

            // OpenH264 silently clamps settings it dislikes, so confirm the
            // encoder really took the slice configuration.
            let mut check = SEncParamExt::default();
            let rc = api.get_option(
                ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                (&mut check as *mut SEncParamExt).cast(),
            );
            anyhow::ensure!(rc == 0, "could not verify encoder parameters (code {rc})");
            let applied = check.sSpatialLayers[0].sSliceArgument.uiSliceNum;
            anyhow::ensure!(
                applied > 1,
                "encoder kept {applied} slice(s); frame-parallel encoding is unavailable"
            );
            tracing::debug!("encoding with {applied} slices across {threads} threads");
        }
        Ok(())
    }

    fn make_encoder(bitrate_kbps: u32, max_fps: u16) -> anyhow::Result<Encoder> {
        let config = EncoderConfig::new()
            // Desktops are mostly flat colour, sharp text and large static
            // regions; this mode tunes OpenH264 for exactly that, and is a
            // large quality win over the camera-oriented default.
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .set_bitrate_bps(bitrate_kbps.saturating_mul(1000))
            .max_frame_rate(max_fps as f32)
            .enable_skip_frame(true)
            .set_multiple_thread_idc(encoder_threads());
        Encoder::with_api_config(OpenH264API::from_source(), config)
            .context("failed to create OpenH264 encoder")
    }
}

impl VideoEncoder for H264Encoder {
    fn encode_bgra(
        &mut self,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> anyhow::Result<Option<EncodedFrame>> {
        // H.264 4:2:0 wants even dimensions; crop a stray row/column.
        let (w, h) = (width & !1, height & !1);
        anyhow::ensure!(w > 0 && h > 0, "empty frame");

        if self.dims != (w, h) {
            // Resolution changed (monitor switch, mode change): restart the
            // encoder so the stream begins with fresh SPS/PPS + IDR.
            self.encoder = Self::make_encoder(self.bitrate_kbps, self.max_fps)?;
            self.dims = (w, h);

            // OpenH264 initializes lazily on its first encode, and slice
            // threading can only be configured once it has. Burn one throwaway
            // encode here so threading is live before the first frame we
            // actually deliver — otherwise reconfiguring mid-stream forces a
            // second keyframe a frame later.
            self.yuv.fill_from_bgra(width, height, w, h, bgra);
            let _ = self.encoder.encode(&self.yuv).context("encoder warm-up failed")?;
            if let Err(e) = Self::enable_slice_threading(&mut self.encoder, encoder_threads()) {
                tracing::warn!("encoding single-threaded: {e:#}");
            }
            // The discarded frame consumed the automatic IDR.
            self.pending_keyframe = true;
        }
        if self.pending_keyframe {
            self.encoder.force_intra_frame();
            self.pending_keyframe = false;
        }

        self.yuv.fill_from_bgra(width, height, w, h, bgra);
        let bitstream = self.encoder.encode(&self.yuv).context("encode failed")?;
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(None); // rate controller skipped this frame
        }
        let keyframe = contains_idr(&data);
        Ok(Some(EncodedFrame { data, keyframe }))
    }

    fn force_keyframe(&mut self) {
        self.pending_keyframe = true;
    }
}

/// Scan an Annex-B bitstream for an IDR NAL unit (type 5).
fn contains_idr(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            let (start, next) = if data[i + 2] == 1 {
                (i + 3, i + 3)
            } else if i + 4 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                (i + 4, i + 4)
            } else {
                i += 1;
                continue;
            };
            if start < data.len() && data[start] & 0x1f == 5 {
                return true;
            }
            i = next;
        } else {
            i += 1;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// OpenH264 decoder
// ---------------------------------------------------------------------------

pub struct H264Decoder {
    decoder: Decoder,
}

impl H264Decoder {
    pub fn new() -> anyhow::Result<Self> {
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .context("failed to create OpenH264 decoder")?;
        Ok(Self { decoder })
    }
}

impl VideoDecoder for H264Decoder {
    fn decode_to_bgra(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> anyhow::Result<Option<(u32, u32)>> {
        let decoded = self.decode_to_rgba(data, out)?;
        if decoded.is_some() {
            for px in out.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        Ok(decoded)
    }

    fn decode_to_rgba(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> anyhow::Result<Option<(u32, u32)>> {
        let Some(yuv) = self.decoder.decode(data).context("decode failed")? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        out.resize(w * h * 4, 0);
        yuv.write_rgba8(out);
        Ok(Some((w as u32, h as u32)))
    }
}

// ---------------------------------------------------------------------------
// BGRA -> I420 conversion
// ---------------------------------------------------------------------------

/// BT.601 limited-range luma: `Y = 16 + (66R + 129G + 25B) / 256`.
#[inline(always)]
fn luma((r, g, b): (u32, u32, u32)) -> u8 {
    (16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)).min(235) as u8
}

/// BT.601 limited-range chroma, returned as `(Cb, Cr)`.
#[inline(always)]
fn chroma(r: u32, g: u32, b: u32) -> (u8, u8) {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let cb = 128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8);
    let cr = 128 + ((112 * r - 94 * g - 18 * b + 128) >> 8);
    (cb.clamp(16, 240) as u8, cr.clamp(16, 240) as u8)
}

#[derive(Default)]
struct I420Buffer {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420Buffer {
    /// Convert BGRA (`src_w` wide, tightly packed) into I420, cropping to
    /// `w` x `h` (both even). BT.601 limited range — what a decoder assumes
    /// unless told otherwise.
    ///
    /// Written as zipped row slices rather than indexed lookups: this runs on
    /// every pixel of every frame, and the iterator form lets the compiler drop
    /// the bounds checks and vectorize the inner loop.
    fn fill_from_bgra(&mut self, src_w: u32, _src_h: u32, w: u32, h: u32, bgra: &[u8]) {
        let (w, h, src_w) = (w as usize, h as usize, src_w as usize);
        self.width = w;
        self.height = h;
        self.y.resize(w * h, 0);
        self.u.resize(w * h / 4, 0);
        self.v.resize(w * h / 4, 0);

        let half_w = w / 2;
        // Disjoint field borrows, so the three planes can be written together.
        let (y_plane, u_plane, v_plane) = (&mut self.y, &mut self.u, &mut self.v);

        for r in 0..h / 2 {
            let top = 2 * r;
            let src_top = &bgra[top * src_w * 4..][..w * 4];
            let src_bot = &bgra[(top + 1) * src_w * 4..][..w * 4];

            let (y_top, y_after) = y_plane[top * w..].split_at_mut(w);
            let y_bot = &mut y_after[..w];
            let u_row = &mut u_plane[r * half_w..][..half_w];
            let v_row = &mut v_plane[r * half_w..][..half_w];

            // Each step covers one 2x2 block: 8 source bytes per row. Fixed-size
            // array chunks let the compiler prove every index below is in range.
            let blocks = src_top
                .as_chunks::<8>()
                .0
                .iter()
                .zip(src_bot.as_chunks::<8>().0.iter())
                .zip(y_top.as_chunks_mut::<2>().0.iter_mut())
                .zip(y_bot.as_chunks_mut::<2>().0.iter_mut())
                .zip(u_row.iter_mut())
                .zip(v_row.iter_mut());

            for (((((s_top, s_bot), yt), yb), u), v) in blocks {
                let px = [
                    (s_top[2] as u32, s_top[1] as u32, s_top[0] as u32),
                    (s_top[6] as u32, s_top[5] as u32, s_top[4] as u32),
                    (s_bot[2] as u32, s_bot[1] as u32, s_bot[0] as u32),
                    (s_bot[6] as u32, s_bot[5] as u32, s_bot[4] as u32),
                ];
                yt[0] = luma(px[0]);
                yt[1] = luma(px[1]);
                yb[0] = luma(px[2]);
                yb[1] = luma(px[3]);

                // Chroma is subsampled: average the block, then convert once.
                let r_avg = (px[0].0 + px[1].0 + px[2].0 + px[3].0) / 4;
                let g_avg = (px[0].1 + px[1].1 + px[2].1 + px[3].1) / 4;
                let b_avg = (px[0].2 + px[1].2 + px[2].2 + px[3].2) / 4;
                let (cu, cv) = chroma(r_avg, g_avg, b_avg);
                *u = cu;
                *v = cv;
            }
        }
    }
}

impl YUVSource for I420Buffer {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four solid quadrants: red, green, blue, white. Colour errors in the
    /// BGRA->I420 path show up immediately as swapped or washed-out corners.
    fn test_pattern(w: u32, h: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let (b, g, r) = match (x < w / 2, y < h / 2) {
                    (true, true) => (0, 0, 255),
                    (false, true) => (0, 255, 0),
                    (true, false) => (255, 0, 0),
                    (false, false) => (255, 255, 255),
                };
                let p = ((y * w + x) * 4) as usize;
                buf[p] = b;
                buf[p + 1] = g;
                buf[p + 2] = r;
                buf[p + 3] = 255;
            }
        }
        buf
    }

    fn sample(pixels: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8) {
        let p = ((y * w + x) * 4) as usize;
        (pixels[p], pixels[p + 1], pixels[p + 2]) // B, G, R
    }

    #[test]
    fn encode_decode_roundtrip_preserves_colours() {
        const W: u32 = 320;
        const H: u32 = 240;

        let mut encoder = H264Encoder::new(8_000, 30).unwrap();
        let mut decoder = H264Decoder::new().unwrap();
        let source = test_pattern(W, H);

        let encoded = encoder
            .encode_bgra(W, H, &source)
            .unwrap()
            .expect("first frame must produce a bitstream");
        assert!(encoded.keyframe, "the first frame must be an IDR");

        let mut out = Vec::new();
        let (dw, dh) = decoder
            .decode_to_bgra(&encoded.data, &mut out)
            .unwrap()
            .expect("keyframe must decode to a picture");
        assert_eq!((dw, dh), (W, H));
        assert_eq!(out.len(), (W * H * 4) as usize);

        // Sample well inside each quadrant, away from block edges. H.264 is
        // lossy, so assert the dominant channel rather than exact values.
        let (b, g, r) = sample(&out, W, W / 4, H / 4);
        assert!(r > 150 && g < 100 && b < 100, "top-left should be red, got {r},{g},{b}");

        let (b, g, r) = sample(&out, W, 3 * W / 4, H / 4);
        assert!(g > 150 && r < 120 && b < 120, "top-right should be green, got {r},{g},{b}");

        let (b, g, r) = sample(&out, W, W / 4, 3 * H / 4);
        assert!(b > 150 && r < 100 && g < 100, "bottom-left should be blue, got {r},{g},{b}");

        let (b, g, r) = sample(&out, W, 3 * W / 4, 3 * H / 4);
        assert!(
            r > 180 && g > 180 && b > 180,
            "bottom-right should be white, got {r},{g},{b}"
        );
    }

    #[test]
    fn odd_dimensions_are_cropped_to_even() {
        let mut encoder = H264Encoder::new(4_000, 30).unwrap();
        let source = test_pattern(101, 51);
        let encoded = encoder.encode_bgra(101, 51, &source).unwrap().unwrap();

        let mut decoder = H264Decoder::new().unwrap();
        let mut out = Vec::new();
        let (w, h) = decoder.decode_to_bgra(&encoded.data, &mut out).unwrap().unwrap();
        assert_eq!((w, h), (100, 50));
    }

    /// Diagnostic, not an assertion: splits the encode path into colour
    /// conversion and H.264 so it is clear which one to attack.
    /// `cargo test -p telekin-codec --release -- --nocapture where_time_goes`
    #[test]
    fn where_time_goes() {
        use std::time::Instant;
        const W: u32 = 1920;
        const H: u32 = 1080;
        const N: u32 = 30;

        let source = test_pattern(W, H);
        let mut yuv = I420Buffer::default();

        let t = Instant::now();
        for _ in 0..N {
            yuv.fill_from_bgra(W, H, W, H, &source);
        }
        let convert_ms = t.elapsed().as_secs_f32() * 1000.0 / N as f32;

        let mut encoder = H264Encoder::new(20_000, 60).unwrap();
        // The first encode initializes OpenH264 and applies slice threading.
        encoder.encode_bgra(W, H, &source).unwrap();
        // Surface a threading failure here rather than letting it hide as a
        // merely slow number.
        H264Encoder::enable_slice_threading(&mut encoder.encoder, encoder_threads())
            .expect("slice threading should be active after the first encode");
        let t = Instant::now();
        for _ in 0..N {
            encoder.encode_bgra(W, H, &source).unwrap();
        }
        let total_ms = t.elapsed().as_secs_f32() * 1000.0 / N as f32;

        println!(
            "1080p: convert {convert_ms:.1} ms + h264 {:.1} ms = {total_ms:.1} ms/frame \
             ({} encoder threads)",
            total_ms - convert_ms,
            encoder_threads()
        );
    }

    #[test]
    fn keyframe_is_requested_on_demand() {
        const W: u32 = 160;
        const H: u32 = 120;
        let mut encoder = H264Encoder::new(4_000, 30).unwrap();
        let source = test_pattern(W, H);

        // Frame 0 is always an IDR.
        assert!(encoder.encode_bgra(W, H, &source).unwrap().unwrap().keyframe);
        // An identical frame right after should not need one...
        let second = encoder.encode_bgra(W, H, &source).unwrap();
        assert!(second.is_none_or(|f| !f.keyframe));
        // ...until we ask.
        encoder.force_keyframe();
        let forced = encoder
            .encode_bgra(W, H, &source)
            .unwrap()
            .expect("a forced keyframe is never skipped");
        assert!(forced.keyframe);
    }
}
