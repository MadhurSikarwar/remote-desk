//! What video encoding does on systems where it is not implemented yet: it
//! says so. The same names and signatures as the Windows version, so the
//! application builds everywhere.

use crate::{CodecError, EncodedFrame, EncoderConfig};

/// Cannot be created on this system.
pub struct H264Encoder {
    config: EncoderConfig,
}

impl H264Encoder {
    pub fn new(_config: EncoderConfig) -> Result<Self, CodecError> {
        Err(CodecError::UnsupportedSystem)
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    pub fn bitrate_kbps(&self) -> u32 {
        self.config.bitrate_kbps
    }

    pub fn set_bitrate_kbps(&mut self, _kbps: u32) -> Result<(), CodecError> {
        Err(CodecError::UnsupportedSystem)
    }

    pub fn request_keyframe(&mut self) -> Result<(), CodecError> {
        Err(CodecError::UnsupportedSystem)
    }

    pub fn encode(
        &mut self,
        _nv12: &[u8],
        _timestamp_us: u64,
    ) -> Result<Vec<EncodedFrame>, CodecError> {
        Err(CodecError::UnsupportedSystem)
    }
}
