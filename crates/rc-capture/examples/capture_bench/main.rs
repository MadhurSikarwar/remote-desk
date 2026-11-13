//! Measures capture on this machine (Windows only for now).
//!
//!     cargo run --release -p rc-capture --example capture_bench -- [seconds] [display] [out.bmp]

#[cfg(windows)]
mod bench;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    bench::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("capture_bench measures screen capture, which only exists on Windows so far");
}
