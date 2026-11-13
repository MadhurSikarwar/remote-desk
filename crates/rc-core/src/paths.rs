//! Where the application keeps its files.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::config::CONFIG_FILE_NAME;

/// Environment variable that relocates all application files, for tests and
/// portable installs.
pub const CONFIG_DIR_ENV: &str = "RC_CONFIG_DIR";

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("cannot determine the user's home directory")]
    NoHomeDirectory,
}

/// Resolved per-user directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    /// Uses `RC_CONFIG_DIR` when set, otherwise the platform's per-user
    /// locations (`%APPDATA%\Prometheus` and `%LOCALAPPDATA%\Prometheus`
    /// on Windows).
    pub fn resolve() -> Result<Self, PathError> {
        if let Some(base) = std::env::var_os(CONFIG_DIR_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::from_base(Path::new(&base)));
        }
        let dirs = ProjectDirs::from("", "", "Prometheus").ok_or(PathError::NoHomeDirectory)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            log_dir: dirs.data_local_dir().join("logs"),
        })
    }

    /// Keeps everything under one directory.
    pub fn from_base(base: &Path) -> Self {
        Self {
            config_dir: base.to_path_buf(),
            log_dir: base.join("logs"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }
}
