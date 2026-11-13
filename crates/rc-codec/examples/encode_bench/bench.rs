//! Captures the screen, encodes it to H.264 and writes the raw stream.
//!
//!     cargo run --release -p rc-codec --example encode_bench -- [seconds] [out.h264] [kbps] [fps]
//!
//! The output is an Annex B stream that players and `ffmpeg` read directly.

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

use rc_capture::Capturer;
use rc_codec::nv12::{bgra_to_nv12, even, nv12_len};
use rc_codec::{EncoderConfig, H264Encoder};

pub fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(5);
    let path = args.next().unwrap_or_else(|| "capture.h264".to_owned());
    let bitrate_kbps: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(8000);
    let fps: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(30);

    let mut capturer = Capturer::new(0)?;
    let mut out = BufWriter::new(File::create(&path)?);
    let mut encoder: Option<H264Encoder> = None;
    let mut nv12 = Vec::new();

    let (mut frames, mut keyframes, mut bytes) = (0u64, 0u64, 0u64);
    let (mut convert_time, mut encode_time) = (Duration::ZERO, Duration::ZERO);
    let mut slowest_encode = Duration::ZERO;
    let interval = Duration::from_secs(1) / fps;
    let start = Instant::now();
    let mut next = start;

    while start.elapsed() < Duration::from_secs(seconds) {
        let Some(frame) = capturer.next_frame(100)? else {
            continue;
        };
        let (width, height) = (even(frame.width), even(frame.height));
        let encoder = match &mut encoder {
            Some(encoder) => encoder,
            slot => slot.insert(H264Encoder::new(EncoderConfig {
                width,
                height,
                fps,
                bitrate_kbps,
            })?),
        };
        nv12.resize(nv12_len(width as usize, height as usize), 0);

        let t0 = Instant::now();
        bgra_to_nv12(
            frame.data,
            frame.stride(),
            width as usize,
            height as usize,
            &mut nv12,
        );
        let t1 = Instant::now();
        let encoded = encoder.encode(&nv12, start.elapsed().as_micros() as u64)?;
        let t2 = Instant::now();

        convert_time += t1 - t0;
        encode_time += t2 - t1;
        slowest_encode = slowest_encode.max(t2 - t1);
        for frame in encoded {
            frames += 1;
            keyframes += u64::from(frame.keyframe);
            bytes += frame.data.len() as u64;
            out.write_all(&frame.data)?;
        }

        next += interval;
        match next.checked_duration_since(Instant::now()) {
            Some(wait) => std::thread::sleep(wait),
            None => next = Instant::now(),
        }
    }
    out.flush()?;

    let elapsed = start.elapsed().as_secs_f64();
    let config = encoder.map(|e| e.config());
    println!("settings: {config:?}");
    println!(
        "frames: {frames} in {elapsed:.2} s ({:.1} per second), {keyframes} keyframes",
        frames as f64 / elapsed
    );
    println!(
        "output: {bytes} bytes = {:.0} kbit/s",
        bytes as f64 * 8.0 / 1000.0 / elapsed
    );
    if frames > 0 {
        let per = |d: Duration| d.as_secs_f64() * 1000.0 / frames as f64;
        println!("BGRA→NV12: {:.2} ms per frame", per(convert_time));
        println!(
            "H.264 encode: {:.2} ms per frame (slowest {:.1} ms)",
            per(encode_time),
            slowest_encode.as_secs_f64() * 1000.0
        );
    }
    println!("written to {path}");
    Ok(())
}
