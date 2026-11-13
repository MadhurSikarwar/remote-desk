//! Screen capture.
//!
//! This crate only reads pixels. Deciding *whether* capture may run — the
//! user's explicit action and the visible sharing indicator — is the caller's
//! job, and nothing here starts capturing on its own.

mod frame;

#[cfg(windows)]
mod dxgi;
#[cfg(not(windows))]
mod unsupported;

pub use frame::{copy_rows, Frame};

#[cfg(windows)]
pub use dxgi::{list_displays, Capturer};
#[cfg(not(windows))]
pub use unsupported::{list_displays, Capturer};

/// A monitor attached to the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Position in [`list_displays`]; pass it to [`Capturer::new`].
    pub index: usize,
    /// Device name, for example `\\.\DISPLAY1`.
    pub name: String,
    pub adapter: String,
    /// Top-left corner in virtual-desktop coordinates.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: Rotation,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    None,
    Deg90,
    Deg180,
    Deg270,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("display {0} does not exist")]
    NoSuchDisplay(usize),
    /// The desktop cannot be captured right now: the lock screen or a UAC
    /// prompt is showing. Retry later.
    #[error("the desktop is not accessible (lock screen or secure desktop)")]
    AccessDenied,
    #[error("desktop duplication is not available on this display: {0}")]
    Unsupported(String),
    #[error("capture failed: {0}")]
    Platform(String),
    /// Screen capture is not implemented for this operating system yet.
    #[error("sharing this computer's screen is not supported on this operating system yet (it works on Windows only)")]
    UnsupportedSystem,
}
