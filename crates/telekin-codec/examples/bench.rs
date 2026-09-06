//! Rough timing for the 1080p encode path, split into colour conversion and
//! H.264 encode so it is obvious which one to optimize.
//!
//! Run with: cargo run --release -p telekin-codec --example bench

use std::time::Instant;

use telekin_codec::{H264Decoder, H264Encoder, VideoDecoder, VideoEncoder};

const W: u32 = 1920;
const H: u32 = 1080;
const FRAMES: u32 = 60;

fn main() -> anyhow::Result<()> {
    // Desktop-like content: mostly flat, with text-ish high-frequency detail
    // and a moving element, which is what the encoder will actually see.
    let mut frames = Vec::new();
    for f in 0..FRAMES {
        let mut buf = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let p = ((y * W + x) * 4) as usize;
                let (b, g, r) = if y < 40 {
                    (60, 50, 45) // title bar
                } else if x % 9 < 2 && y % 17 < 10 {
                    (220, 220, 220) // "text"
                } else if x > 1400 && y > 700 && (x + f * 7) % 200 < 100 {
                    (30, 160, 240) // something moving
                } else {
                    (250, 248, 246) // page background
                };
                buf[p] = b;
                buf[p + 1] = g;
                buf[p + 2] = r;
                buf[p + 3] = 255;
            }
        }
        frames.push(buf);
    }

    let mut encoder = H264Encoder::new(20_000, 60)?;
    let mut decoder = H264Decoder::new()?;
    let mut out = Vec::new();

    // Warm up: the first frame pays encoder init and IDR cost. The decoder
    // must see it too — without that IDR every later P-frame is undecodable.
    if let Some(e) = encoder.encode_bgra(W, H, &frames[0])? {
        decoder.decode_to_bgra(&e.data, &mut out)?;
    }

    let mut total_bytes = 0usize;
    let mut encode_total = 0.0f32;
    let mut decode_total = 0.0f32;
    let mut encode_worst = 0.0f32;

    for frame in &frames {
        let t = Instant::now();
        let encoded = encoder.encode_bgra(W, H, frame)?;
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        encode_total += ms;
        encode_worst = encode_worst.max(ms);

        if let Some(e) = encoded {
            total_bytes += e.data.len();
            let t = Instant::now();
            decoder.decode_to_bgra(&e.data, &mut out)?;
            decode_total += t.elapsed().as_secs_f32() * 1000.0;
        }
    }
    let n = FRAMES as f32;

    println!("frames:   {FRAMES} at {W}x{H}");
    println!(
        "encode:   {:.1} ms/frame avg, {encode_worst:.1} ms worst  -> {:.0} fps ceiling",
        encode_total / n,
        1000.0 / (encode_total / n)
    );
    println!("decode:   {:.1} ms/frame avg", decode_total / n);
    println!("bitrate:  {:.1} Mbps at 30fps", total_bytes as f32 * 8.0 * 30.0 / n / 1e6);
    Ok(())
}
