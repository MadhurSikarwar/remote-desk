//! A device's long-term key pair.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::channel::NOISE_PARAMS;
use crate::protect::{protect, unprotect};
use crate::CryptoError;

pub const KEY_LEN: usize = 32;
pub type PublicKey = [u8; KEY_LEN];

/// File format marker, in case the layout ever changes.
const MAGIC: &[u8; 8] = b"RCIDENT1";
/// Marks a key file that is stored without Windows protection.
const MAGIC_PLAIN: &[u8; 8] = b"RCSRVID1";

/// A public key as 64 lowercase hex digits.
pub fn to_hex(key: &PublicKey) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

/// The inverse of [`to_hex`]; `None` for anything that is not exactly 64 hex
/// digits.
pub fn from_hex(text: &str) -> Option<PublicKey> {
    if text.len() != 2 * KEY_LEN || !text.is_ascii() {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// A short, human-comparable form of a public key: 16 hex digits in groups
/// of four, taken from the SHA-256 of the key.
pub fn fingerprint(public: &PublicKey) -> String {
    let digest = Sha256::digest(public);
    digest[..8]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join("-")
}

/// The raw bytes behind [`fingerprint`]: the first 8 bytes of the key's
/// SHA-256. Used as a device's ID for lookups.
pub fn fingerprint_bytes(public: &PublicKey) -> [u8; 8] {
    let digest = Sha256::digest(public);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Reads a fingerprint as people type and paste it: 16 hex digits, in any
/// case, with dashes, spaces or nothing between the groups. `None` for
/// anything else.
pub fn parse_fingerprint(text: &str) -> Option<[u8; 8]> {
    let digits: String = text.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    if digits.len() != 16 || !digits.is_ascii() {
        return None;
    }
    let mut out = [0u8; 8];
    for (byte, pair) in out.iter_mut().zip(digits.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// This device's key pair. The private half never leaves this type except
/// to the handshake, and is stored only in encrypted form.
#[derive(Clone)]
pub struct Identity {
    private: [u8; KEY_LEN],
    public: PublicKey,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> CryptoError + '_ {
    move |source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    }
}

impl Identity {
    /// A fresh random key pair.
    pub fn generate() -> Result<Self, CryptoError> {
        let pair = snow::Builder::new(NOISE_PARAMS.parse()?).generate_keypair()?;
        let malformed = || CryptoError::Channel("unexpected key length".to_owned());
        Ok(Self {
            private: pair
                .private
                .as_slice()
                .try_into()
                .map_err(|_| malformed())?,
            public: pair.public.as_slice().try_into().map_err(|_| malformed())?,
        })
    }

    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    pub(crate) fn private(&self) -> &[u8; KEY_LEN] {
        &self.private
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }

    /// Loads the identity stored at `path`, creating and storing a new one
    /// if the file does not exist. A file that exists but cannot be read is
    /// an error: silently replacing it would change this device's identity
    /// and break every pairing.
    pub fn load_or_create(path: &Path) -> Result<Self, CryptoError> {
        match fs::read(path) {
            Ok(bytes) => Self::decode(&bytes, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                identity.save(path)?;
                Ok(identity)
            }
            Err(e) => Err(io_error(path)(e)),
        }
    }

    /// Like [`Identity::load_or_create`], but stores the key **unencrypted**,
    /// protected only by the file's permissions (owner read/write on Unix).
    /// For a server, which runs unattended and, on Linux, has no per-user
    /// encryption to rely on. Never use it for a personal device.
    pub fn load_or_create_unprotected(path: &Path) -> Result<Self, CryptoError> {
        match fs::read(path) {
            Ok(bytes) => {
                let malformed = |reason: &str| CryptoError::Malformed {
                    path: path.to_path_buf(),
                    reason: reason.to_owned(),
                };
                let body = bytes
                    .strip_prefix(MAGIC_PLAIN)
                    .ok_or_else(|| malformed("not a server key file"))?;
                let (private, public) = body
                    .split_at_checked(KEY_LEN)
                    .filter(|(_, public)| public.len() == KEY_LEN)
                    .ok_or_else(|| malformed("unexpected key length"))?;
                let mut identity = Self {
                    private: [0; KEY_LEN],
                    public: [0; KEY_LEN],
                };
                identity.private.copy_from_slice(private);
                identity.public.copy_from_slice(public);
                Ok(identity)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                let mut bytes = MAGIC_PLAIN.to_vec();
                bytes.extend_from_slice(&identity.private);
                bytes.extend_from_slice(&identity.public);
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    fs::create_dir_all(dir).map_err(io_error(path))?;
                }
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                let mut file = options.open(path).map_err(io_error(path))?;
                file.write_all(&bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(io_error(path))?;
                Ok(identity)
            }
            Err(e) => Err(io_error(path)(e)),
        }
    }

    fn decode(bytes: &[u8], path: &Path) -> Result<Self, CryptoError> {
        let malformed = |reason: &str| CryptoError::Malformed {
            path: path.to_path_buf(),
            reason: reason.to_owned(),
        };
        let protected = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| malformed("not a device identity file"))?;
        let plain = unprotect(protected)?;
        let (private, public) = plain
            .split_at_checked(KEY_LEN)
            .filter(|(_, public)| public.len() == KEY_LEN)
            .ok_or_else(|| malformed("unexpected key length"))?;
        let mut identity = Self {
            private: [0; KEY_LEN],
            public: [0; KEY_LEN],
        };
        identity.private.copy_from_slice(private);
        identity.public.copy_from_slice(public);
        Ok(identity)
    }

    fn save(&self, path: &Path) -> Result<(), CryptoError> {
        let mut plain = Vec::with_capacity(2 * KEY_LEN);
        plain.extend_from_slice(&self.private);
        plain.extend_from_slice(&self.public);
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&protect(&plain)?);

        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(io_error(path))?;
        }
        // `create_new` so that two instances starting together cannot each
        // write a different identity over the other's.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(io_error(path))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(io_error(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identities_differ_and_have_stable_fingerprints() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.public(), b.public());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), fingerprint(a.public()));
    }

    #[test]
    fn fingerprint_format_and_known_value() {
        // SHA-256 of 32 zero bytes begins 66 68 7A AD F8 62 BD 77.
        assert_eq!(fingerprint(&[0; 32]), "6668-7AAD-F862-BD77");
    }

    #[test]
    fn hex_round_trips_and_rejects_anything_else() {
        let key: PublicKey = std::array::from_fn(|i| i as u8 * 7);
        assert_eq!(from_hex(&to_hex(&key)), Some(key));
        assert_eq!(to_hex(&[0xAB; 32]), "ab".repeat(32));
        assert_eq!(from_hex(&"AB".repeat(32)), Some([0xAB; 32]));
        for bad in [
            "",
            "ab",
            &"zz".repeat(32),
            &"ab".repeat(31),
            &"ab".repeat(33),
            &"é".repeat(32),
        ] {
            assert_eq!(from_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fingerprint_bytes_match_the_text_form_and_parse_back() {
        let key: PublicKey = std::array::from_fn(|i| (i * 3) as u8);
        let text = fingerprint(&key);
        let bytes = fingerprint_bytes(&key);
        assert_eq!(parse_fingerprint(&text), Some(bytes));
        // As people type it.
        assert_eq!(parse_fingerprint(&text.to_lowercase()), Some(bytes));
        assert_eq!(parse_fingerprint(&text.replace('-', "")), Some(bytes));
        assert_eq!(parse_fingerprint(&text.replace('-', " ")), Some(bytes));
        assert_eq!(parse_fingerprint(&format!("  {text}  ")), Some(bytes));
    }

    #[test]
    fn malformed_fingerprints_are_rejected() {
        for bad in [
            "",
            "6668",
            "6668-7AAD-F862-BD7",
            "6668-7AAD-F862-BD77-00",
            "6668-7AAD-F862-BDZZ",
            "６６６８-7AAD-F862-BD77",
            "127.0.0.1:5000",
        ] {
            assert_eq!(parse_fingerprint(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_server_key_is_created_once_reloaded_and_never_replaced_when_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("server.key");
        let first = Identity::load_or_create_unprotected(&path).unwrap();
        let again = Identity::load_or_create_unprotected(&path).unwrap();
        assert_eq!(first.public(), again.public());
        assert_eq!(first.private(), again.private());

        // A personal key file is not accepted as a server key, and the
        // other way round.
        let personal = dir.path().join("personal.key");
        fs::write(&personal, b"RCIDENT1 something").unwrap();
        assert!(Identity::load_or_create_unprotected(&personal).is_err());

        for damage in [&b"junk"[..], &b"RCSRVID1 too short"[..], &[][..]] {
            fs::write(&path, damage).unwrap();
            assert!(Identity::load_or_create_unprotected(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), damage, "file was replaced");
        }
    }

    #[test]
    fn debug_output_does_not_reveal_the_private_key() {
        let identity = Identity::generate().unwrap();
        let shown = format!("{identity:?}");
        assert!(shown.contains(&identity.fingerprint()));
        assert!(!shown.contains("private"));
    }

    #[cfg(windows)]
    #[test]
    fn first_use_creates_the_file_and_later_uses_load_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("identity.key");

        let created = Identity::load_or_create(&path).unwrap();
        let loaded = Identity::load_or_create(&path).unwrap();
        assert_eq!(created.public(), loaded.public());
        assert_eq!(created.private(), loaded.private());

        // The private key is not in the file in the clear.
        let stored = fs::read(&path).unwrap();
        assert!(stored.starts_with(MAGIC));
        assert!(!stored.windows(KEY_LEN).any(|w| w == created.private()));
    }

    #[cfg(windows)]
    #[test]
    fn a_damaged_file_is_an_error_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        Identity::load_or_create(&path).unwrap();

        for damage in [
            &b"garbage"[..],
            &b"RCIDENT1 but not a valid blob"[..],
            &[][..],
        ] {
            fs::write(&path, damage).unwrap();
            assert!(Identity::load_or_create(&path).is_err(), "{damage:?}");
            assert_eq!(fs::read(&path).unwrap(), damage, "file was replaced");
        }
    }
}
