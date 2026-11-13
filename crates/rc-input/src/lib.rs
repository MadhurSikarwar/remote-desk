//! Turns a viewer's input events into mouse and keyboard input on the host.
//!
//! Whether input is allowed at all is decided by the caller. This crate
//! injects what it is given, and remembers what is held down so it can let
//! go of everything when control ends.

pub mod keymap;
pub mod mapping;
pub mod pressed;

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows_injector;

#[cfg(not(windows))]
pub use unsupported::Injector;
#[cfg(windows)]
pub use windows_injector::Injector;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// Windows refused the input. This happens when the foreground window
    /// belongs to a program running as administrator, or on the secure
    /// desktop.
    #[error("the system did not accept the input")]
    Rejected,
    /// Input injection is not implemented for this operating system yet.
    #[error("controlling this computer is not supported on this operating system yet (it works on Windows only)")]
    UnsupportedSystem,
}
