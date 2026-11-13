//! Application settings, stored as TOML.
//!
//! The file holds settings only. Keys, pairing records and anything else
//! secret live in a separate protected store and must never be added here.
//!
//! There is deliberately no setting that disables the host's approval prompt:
//! every inbound session has to be accepted by the person at the host.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE_NAME: &str = "config.toml";

const MAX_DEVICE_NAME_CHARS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot write {}: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("{} is not valid: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("cannot serialise configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("invalid configuration: {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub device: DeviceConfig,
    pub network: NetworkConfig,
    pub security: SecurityConfig,
    pub video: VideoConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    /// Name shown to the other side in prompts and device lists.
    pub name: String,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        let name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .filter(|n| validate_device_name(n).is_ok())
            .unwrap_or_else(|| "My device".to_owned());
        Self { name }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportPolicy {
    /// Try a direct path first and fall back to the relay.
    #[default]
    Auto,
    /// Never use the relay.
    DirectOnly,
    /// Always use the relay.
    RelayOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Rendezvous and relay server as `host:port` (one server does both;
    /// only the first entry is used). Empty means no server: devices can
    /// then be reached by address or on the local network only.
    pub rendezvous_servers: Vec<String>,
    /// The server's key fingerprint, 16 hex digits, as printed when the
    /// server starts. Devices refuse a server whose key does not match, so
    /// this is required whenever a server is set.
    pub server_fingerprint: String,
    /// Local port for direct connections; 0 lets the OS choose.
    pub direct_port: u16,
    /// Let other computers connect to this one while sharing is on. Off by
    /// default: sharing then accepts viewers on this computer only. Every
    /// viewer is still encrypted and still needs the user's approval.
    pub allow_remote_connections: bool,
    /// While remote connections are allowed, answer "who is sharing?"
    /// queries from the local network.
    pub lan_discovery: bool,
    pub transport: TransportPolicy,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            rendezvous_servers: Vec::new(),
            server_fingerprint: String::new(),
            direct_port: 0,
            allow_remote_connections: false,
            lan_discovery: true,
            transport: TransportPolicy::Auto,
        }
    }
}

/// What a controller may do once the host has approved the session. Every
/// capability beyond viewing starts off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// How long an approval prompt waits before it denies the request.
    pub approval_timeout_secs: u32,
    pub allow_input: bool,
    pub allow_clipboard: bool,
    pub allow_file_transfer: bool,
    /// End a session after this much inactivity; 0 disables the limit.
    pub idle_timeout_minutes: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            approval_timeout_secs: 30,
            allow_input: false,
            allow_clipboard: false,
            allow_file_transfer: false,
            idle_timeout_minutes: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    #[default]
    H264,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub codec: VideoCodec,
    pub max_fps: u32,
    pub max_bitrate_kbps: u32,
    pub adaptive_quality: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            max_fps: 30,
            max_bitrate_kbps: 8_000,
            adaptive_quality: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub file_enabled: bool,
    /// Daily log files kept before the oldest is deleted.
    pub retention_days: u32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            file_enabled: true,
            retention_days: 14,
        }
    }
}

fn validate_device_name(name: &str) -> Result<(), ConfigError> {
    if name.trim().is_empty() {
        return Err(invalid("device.name", "must not be empty"));
    }
    if name.chars().count() > MAX_DEVICE_NAME_CHARS {
        return Err(invalid(
            "device.name",
            format!("must be at most {MAX_DEVICE_NAME_CHARS} characters"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid(
            "device.name",
            "must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_server(field: &'static str, entry: &str) -> Result<(), ConfigError> {
    let bad = || invalid(field, format!("`{entry}` is not `host:port`"));
    let (host, port) = entry.rsplit_once(':').ok_or_else(bad)?;
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad());
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok(()),
        _ => Err(bad()),
    }
}

/// 16 hex digits, with dashes or spaces allowed between them.
fn is_fingerprint(text: &str) -> bool {
    let digits: Vec<char> = text.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    digits.len() == 16 && digits.iter().all(char::is_ascii_hexdigit)
}

fn check_range(field: &'static str, value: u32, min: u32, max: u32) -> Result<(), ConfigError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("{value} is outside {min}..={max}")))
    }
}

impl Config {
    /// Checks every field. Called on load and before save, so an invalid
    /// configuration is never used and never written.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_device_name(&self.device.name)?;

        for s in &self.network.rendezvous_servers {
            validate_server("network.rendezvous_servers", s)?;
        }
        if !self.network.rendezvous_servers.is_empty()
            && !is_fingerprint(&self.network.server_fingerprint)
        {
            return Err(invalid(
                "network.server_fingerprint",
                "must be the server's key fingerprint: 16 hex digits, optionally grouped with dashes",
            ));
        }
        if self.network.transport != TransportPolicy::Auto
            && self.network.rendezvous_servers.is_empty()
        {
            return Err(invalid(
                "network.transport",
                "`direct-only` and `relay-only` apply to connections through a server; set a server first",
            ));
        }

        check_range(
            "security.approval_timeout_secs",
            self.security.approval_timeout_secs,
            5,
            300,
        )?;
        check_range(
            "security.idle_timeout_minutes",
            self.security.idle_timeout_minutes,
            0,
            24 * 60,
        )?;
        check_range("video.max_fps", self.video.max_fps, 1, 120)?;
        check_range(
            "video.max_bitrate_kbps",
            self.video.max_bitrate_kbps,
            100,
            100_000,
        )?;
        check_range(
            "logging.retention_days",
            self.logging.retention_days,
            1,
            365,
        )?;
        Ok(())
    }

    /// Reads and validates an existing file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Loads the file, or writes and returns the defaults when it does not
    /// exist. An existing file that cannot be parsed or validated is an error
    /// and is left untouched.
    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        match Self::load(path) {
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                let config = Self::default();
                config.save(path)?;
                Ok(config)
            }
            other => other,
        }
    }

    /// Validates, then replaces the file atomically: the new content is
    /// written next to the target and renamed over it.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let text = toml::to_string_pretty(self)?;

        let write_err = |source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        };
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(write_err)?;

        let file_name = path
            .file_name()
            .ok_or_else(|| write_err(io::Error::new(io::ErrorKind::InvalidInput, "no file name")))?
            .to_string_lossy();
        let tmp = dir.join(format!("{file_name}.tmp-{}", std::process::id()));

        let result = (|| {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(write_err)
    }
}
