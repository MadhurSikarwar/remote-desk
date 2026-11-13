//! Application state shared with the UI.

use std::sync::{Arc, Mutex, MutexGuard};

use rc_core::logging::{self, LogGuard};
use rc_core::{AppPaths, Config};
use serde::Serialize;

use crate::secure::Security;

pub struct AppState {
    paths: Option<AppPaths>,
    config: Mutex<Config>,
    /// Set when the configuration could not be loaded. The in-memory defaults
    /// are then used read-only so a damaged file is never overwritten.
    startup_error: Option<String>,
    /// This device's keys and approved devices, or why they are unavailable.
    /// Without them the device can neither share nor view.
    security: Result<Arc<Security>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub device_name: String,
    pub config_path: Option<String>,
    pub log_dir: Option<String>,
    pub config_error: Option<String>,
    /// Fingerprint of this device's key, for people to compare.
    pub fingerprint: Option<String>,
    pub security_error: Option<String>,
}

impl AppState {
    /// Resolves paths, loads the configuration and starts logging.
    pub fn start() -> (Self, Option<LogGuard>) {
        let paths = match AppPaths::resolve() {
            Ok(paths) => paths,
            Err(e) => {
                eprintln!("{e}");
                let state = Self {
                    paths: None,
                    config: Mutex::new(Config::default()),
                    startup_error: Some(e.to_string()),
                    security: Err(e.to_string()),
                };
                return (state, None);
            }
        };

        let state = Self::load(paths.clone());
        let guard = match logging::init(&state.config().logging, &paths.log_dir) {
            Ok(guard) => Some(guard),
            Err(e) => {
                eprintln!("logging disabled: {e}");
                None
            }
        };

        match &state.startup_error {
            None => tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                device = %state.config().device.name,
                config = %paths.config_file().display(),
                "application started"
            ),
            Some(error) => tracing::error!(%error, "configuration could not be loaded"),
        }
        match &state.security {
            Ok(security) => {
                tracing::info!(fingerprint = security.fingerprint(), "device key ready");
            }
            Err(error) => tracing::error!(%error, "device key unavailable"),
        }
        (state, guard)
    }

    /// Loads (or creates) the configuration under `paths`.
    pub fn load(paths: AppPaths) -> Self {
        let (config, startup_error) = match Config::load_or_create(&paths.config_file()) {
            Ok(config) => (config, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        let security = Security::load(&paths.config_dir).map(Arc::new);
        Self {
            paths: Some(paths),
            config: Mutex::new(config),
            startup_error,
            security,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Config> {
        // A panic while holding the lock cannot leave a half-written Config:
        // the value is only ever replaced whole.
        self.config.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> Config {
        self.lock().clone()
    }

    /// Where files received from the other end are saved: a folder of its
    /// own inside the user's Downloads, or inside the settings folder when
    /// there is no Downloads.
    pub fn received_folder(&self) -> std::path::PathBuf {
        directories::UserDirs::new()
            .and_then(|dirs| dirs.download_dir().map(|d| d.join("Prometheus")))
            .or_else(|| self.paths.as_ref().map(|p| p.config_dir.join("received")))
            .unwrap_or_else(|| std::path::PathBuf::from("Prometheus"))
    }

    pub fn security(&self) -> Option<Arc<Security>> {
        self.security.as_ref().ok().cloned()
    }

    pub fn status(&self) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION"),
            device_name: self.lock().device.name.clone(),
            config_path: self
                .paths
                .as_ref()
                .map(|p| p.config_file().display().to_string()),
            log_dir: self.paths.as_ref().map(|p| p.log_dir.display().to_string()),
            config_error: self.startup_error.clone(),
            fingerprint: self.security.as_ref().ok().map(|s| s.fingerprint()),
            security_error: self.security.as_ref().err().cloned(),
        }
    }

    /// Validates and persists `new`, then makes it the active configuration.
    /// Nothing changes in memory unless the file was written.
    pub fn update(&self, new: Config) -> Result<Config, String> {
        if let Some(error) = &self.startup_error {
            return Err(format!(
                "Settings cannot be saved because the configuration did not load: {error}"
            ));
        }
        let Some(paths) = &self.paths else {
            return Err("Settings cannot be saved: no configuration directory".to_owned());
        };

        new.save(&paths.config_file()).map_err(|e| {
            tracing::warn!(error = %e, "configuration rejected");
            e.to_string()
        })?;
        *self.lock() = new.clone();
        tracing::info!("configuration saved");
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn state_in(dir: &tempfile::TempDir) -> AppState {
        AppState::load(AppPaths::from_base(dir.path()))
    }

    #[test]
    fn first_start_creates_config_and_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir);
        let status = state.status();

        assert_eq!(status.config_error, None);
        assert_eq!(status.security_error, None);
        assert_eq!(status.fingerprint.as_ref().map(String::len), Some(19));
        // The key is created once and kept.
        assert_eq!(state_in(&dir).status().fingerprint, status.fingerprint);
        assert_eq!(status.device_name, Config::default().device.name);
        assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
        let config_path = status.config_path.unwrap();
        assert!(fs::metadata(&config_path).unwrap().is_file());
        assert!(status.log_dir.unwrap().ends_with("logs"));
    }

    #[test]
    fn update_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir);

        let mut config = state.config();
        config.device.name = "Lab bench".into();
        config.security.allow_input = true;
        assert_eq!(state.update(config.clone()).unwrap(), config);
        assert_eq!(state.status().device_name, "Lab bench");

        let restarted = state_in(&dir);
        assert_eq!(restarted.config(), config);
    }

    #[test]
    fn invalid_update_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir);
        let before = state.config();

        let mut config = before.clone();
        config.video.max_fps = 0;
        let error = state.update(config).unwrap_err();
        assert!(error.contains("video.max_fps"), "{error}");

        assert_eq!(state.config(), before);
        assert_eq!(state_in(&dir).config(), before);
    }

    #[test]
    fn damaged_file_is_reported_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_base(dir.path());
        let garbage = "not = [toml";
        fs::write(paths.config_file(), garbage).unwrap();

        let state = AppState::load(paths.clone());
        assert!(state.status().config_error.is_some());
        assert!(state.update(Config::default()).is_err());
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), garbage);
    }
}
