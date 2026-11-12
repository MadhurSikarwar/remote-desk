//! Measures capture on this machine.
//!
//!     cargo run --release -p rc-capture --example capture_bench -- [seconds] [display] [out.bmp]
//!
//! Frames arrive only when the screen changes, so the rate reported is
//! bounded by how busy the screen was during the run.

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

use rc_capture::{list_displays, Capturer};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// CPU time (kernel + user) this process has used so far.
fn cpu_time() -> Duration {
    let mut times = [FILETIME::default(); 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: the four out-pointers reference live, distinct locals.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) };
    if ok.is_err() {
        return Duration::ZERO;
    }
    let ticks = |t: &FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// Writes top-down 32-bit BGRA pixels as an uncompressed BMP.
fn write_bmp(path: &str, width: u32, height: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    let pixels = u32::try_from(bgra.len()).map_err(std::io::Error::other)?;
    out.write_all(b"BM")?;
    out.write_all(&(54 + pixels).to_le_bytes())?;
    out.write_all(&[0; 4])?;
    out.write_all(&54u32.to_le_bytes())?;
    out.write_all(&40u32.to_le_bytes())?;
    out.write_all(&(width as i32).to_le_bytes())?;
    out.write_all(&(-(height as i32)).to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&32u16.to_le_bytes())?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&pixels.to_le_bytes())?;
    out.write_all(&[0; 16])?;
    out.write_all(bgra)?;
    out.flush()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(5);
    let display: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(0);
    let bmp_path = args.next();

    for d in list_displays()? {
        println!(
            "display {}: {} on {} — {}x{} at ({}, {}), rotation {:?}{}",
            d.index,
            d.name,
            d.adapter,
            d.width,
            d.height,
            d.x,
            d.y,
            d.rotation,
            if d.primary { ", primary" } else { "" }
        );
    }

    let mut capturer = Capturer::new(display)?;
    let run = Duration::from_secs(seconds);
    let (mut frames, mut updates, mut empty) = (0u64, 0u64, 0u64);
    let mut size = (0, 0);
    let mut saved = false;

    let cpu_start = cpu_time();
    let start = Instant::now();
    while start.elapsed() < run {
        match capturer.next_frame(100)? {
            Some(frame) => {
                frames += 1;
                updates += u64::from(frame.accumulated_updates);
                size = (frame.width, frame.height);
                if let (false, Some(path)) = (saved, &bmp_path) {
                    write_bmp(path, frame.width, frame.height, frame.data)?;
                    saved = true;
                }
            }
            None => empty += 1,
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cpu = (cpu_time() - cpu_start).as_secs_f64();

    println!(
        "captured display {display} for {elapsed:.2} s at {}x{}",
        size.0, size.1
    );
    println!(
        "frames delivered:   {frames} ({:.1} per second)",
        frames as f64 / elapsed
    );
    println!("screen updates:     {updates} (updates folded into those frames)");
    println!("waits with no frame: {empty}");
    println!(
        "CPU time:           {cpu:.3} s = {:.1}% of one core{}",
        100.0 * cpu / elapsed,
        if frames > 0 {
            format!(", {:.2} ms per frame", 1000.0 * cpu / frames as f64)
        } else {
            String::new()
        }
    );
    if saved {
        println!("first frame written to {}", bmp_path.unwrap_or_default());
    }
    Ok(())
}
