//! What screen capture does on systems where it is not implemented yet: it
//! says so, and nothing is captured. The same names and signatures as the
//! Windows version, so the application builds everywhere.

use crate::{CaptureError, DisplayInfo, Frame};

pub fn list_displays() -> Result<Vec<DisplayInfo>, CaptureError> {
    Err(CaptureError::UnsupportedSystem)
}

/// Cannot be created on this system.
pub struct Capturer {
    display: DisplayInfo,
}

impl Capturer {
    pub fn new(_display: usize) -> Result<Self, CaptureError> {
        Err(CaptureError::UnsupportedSystem)
    }

    pub fn display(&self) -> &DisplayInfo {
        &self.display
    }

    pub fn next_frame(&mut self, _timeout_ms: u32) -> Result<Option<Frame<'_>>, CaptureError> {
        Err(CaptureError::UnsupportedSystem)
    }
}
