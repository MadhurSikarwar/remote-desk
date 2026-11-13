//! Screen capture.
//!
//! This crate only reads pixels. Deciding *whether* capture may run — the
//! user's explicit action and the visible sharing indicator — is the caller's
//! job, and nothing here starts capturing on its own.

mod frame;
pub mod rotate;

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

/// The place in `displays` of the display called `name` (for example
/// `\\.\DISPLAY2`), or `None` if it is no longer there.
///
/// A display is found again by name, not by its place in the list: when
/// monitors are added or removed the places can move, and sharing must not
/// silently carry on with a different monitor.
pub fn index_of(displays: &[DisplayInfo], name: &str) -> Option<usize> {
    displays.iter().position(|d| d.name == name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn display(index: usize, name: &str) -> DisplayInfo {
        DisplayInfo {
            index,
            name: name.to_owned(),
            adapter: "adapter".to_owned(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            rotation: Rotation::None,
            primary: index == 0,
        }
    }

    #[test]
    fn a_display_is_found_by_name_wherever_it_has_moved_to() {
        let before = [display(0, r"\\.\DISPLAY1"), display(1, r"\\.\DISPLAY2")];
        assert_eq!(index_of(&before, r"\\.\DISPLAY2"), Some(1));
        // The first monitor was unplugged: the second is now at place 0.
        let after = [display(0, r"\\.\DISPLAY2")];
        assert_eq!(index_of(&after, r"\\.\DISPLAY2"), Some(0));
    }

    #[test]
    fn a_display_that_is_gone_is_not_replaced_by_another() {
        let now = [display(0, r"\\.\DISPLAY1")];
        assert_eq!(index_of(&now, r"\\.\DISPLAY2"), None);
        assert_eq!(index_of(&[], r"\\.\DISPLAY1"), None);
    }
}
