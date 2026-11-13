//! Video encoding.
//!
//! Captured BGRA frames are converted to NV12 and compressed to H.264
//! (Annex B byte stream) with the encoder that ships with Windows.

pub mod nv12;

#[cfg(windows)]
mod mf_encoder;
#[cfg(not(windows))]
mod unsupported;

#[cfg(windows)]
pub use mf_encoder::H264Encoder;
#[cfg(not(windows))]
pub use unsupported::H264Encoder;

/// What to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Frame size in pixels; both must be even and non-zero.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// One compressed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// H.264 NAL units in Annex B format (start-code prefixed).
    pub data: Vec<u8>,
    /// Decodable without any earlier frame; carries SPS and PPS.
    pub keyframe: bool,
    /// Presentation time in microseconds, as passed to `encode`.
    pub timestamp_us: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid encoder settings: {0}")]
    InvalidConfig(&'static str),
    #[error("frame has {actual} bytes, expected {expected}")]
    WrongFrameSize { expected: usize, actual: usize },
    #[error("encoder failed: {0}")]
    Platform(String),
    /// Video encoding is not implemented for this operating system yet.
    #[error(
        "video encoding is not supported on this operating system yet (it works on Windows only)"
    )]
    UnsupportedSystem,
}
