//! Measures H.264 encoding on this machine (Windows only for now).

#[cfg(windows)]
mod bench;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    bench::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("encode_bench measures video encoding, which only exists on Windows so far");
}
