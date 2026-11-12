//! Shared foundation for Remote Control: configuration, file locations and
//! structured logging. Nothing in this crate touches the screen, the network
//! or input devices.

#![forbid(unsafe_code)]

pub mod config;
pub mod logging;
pub mod paths;

pub use config::{Config, ConfigError};
pub use paths::{AppPaths, PathError};
