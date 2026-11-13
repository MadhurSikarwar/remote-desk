//! Who a device is, how two devices talk privately, and which devices the
//! user has approved before.
//!
//! - [`Identity`]: a long-term key pair, generated on first run and stored
//!   encrypted for the current Windows user.
//! - [`channel`]: a Noise XX handshake in which both devices prove
//!   possession of their keys, followed by authenticated encryption of
//!   everything they exchange.
//! - [`TrustStore`]: the public keys of devices approved earlier.
//!
//! No cryptographic primitive is implemented here; they come from `snow`.

pub mod channel;
mod identity;
mod protect;
mod trust;

pub use identity::{fingerprint, from_hex, to_hex, Identity, PublicKey};
pub use trust::{TrustStore, TrustedDevice};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("cannot access {}: {source}", .path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("connection error: {0}")]
    Connection(#[from] std::io::Error),
    /// The stored key could not be decrypted: it belongs to another Windows
    /// user or computer, or the file is damaged.
    #[error("the stored device key cannot be read: {0}")]
    KeyUnreadable(String),
    #[error("{} is not valid: {reason}", .path.display())]
    Malformed {
        path: std::path::PathBuf,
        reason: String,
    },
    /// The handshake failed or received data did not authenticate. The
    /// connection must be dropped.
    #[error("secure channel failure: {0}")]
    Channel(String),
}

impl From<snow::Error> for CryptoError {
    fn from(e: snow::Error) -> Self {
        Self::Channel(e.to_string())
    }
}
