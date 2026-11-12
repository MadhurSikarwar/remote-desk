//! Devices the user has approved and chosen to remember.
//!
//! The store holds public keys only, so it is not secret. Being listed does
//! not grant access by itself: the host still asks before every session.
//! It lets the prompt say "you approved this device before" instead of
//! "unknown device".

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::{fingerprint, PublicKey, KEY_LEN};
use crate::CryptoError;

const MAX_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDevice {
    /// The device's public key, as 64 hex digits.
    pub public_key: String,
    /// The name the device gave when it was approved. Informational: a
    /// device can call itself anything, the key is what identifies it.
    pub name: String,
    /// When it was first approved, in seconds since the Unix epoch.
    pub approved_at: u64,
}

impl TrustedDevice {
    pub fn fingerprint(&self) -> Option<String> {
        decode_hex(&self.public_key).map(|key| fingerprint(&key))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    device: Vec<TrustedDevice>,
}

fn encode_hex(key: &PublicKey) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(text: &str) -> Option<PublicKey> {
    if text.len() != 2 * KEY_LEN || !text.is_ascii() {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// Keeps a device name printable and of bounded length.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Unnamed device".to_owned()
    } else {
        cleaned.to_owned()
    }
}

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    devices: Vec<TrustedDevice>,
}

impl TrustStore {
    /// Loads the store, which is empty if the file does not exist yet. A
    /// file that exists but cannot be parsed is an error rather than an
    /// empty store, so that saving never wipes it by accident.
    pub fn load(path: &Path) -> Result<Self, CryptoError> {
        let io_error = |source| CryptoError::Io {
            path: path.to_path_buf(),
            source,
        };
        let malformed = |reason: String| CryptoError::Malformed {
            path: path.to_path_buf(),
            reason,
        };
        let devices = match fs::read_to_string(path) {
            Ok(text) => {
                let file: File = toml::from_str(&text).map_err(|e| malformed(e.to_string()))?;
                if let Some(bad) = file
                    .device
                    .iter()
                    .find(|d| decode_hex(&d.public_key).is_none())
                {
                    return Err(malformed(format!(
                        "`{}` is not a public key",
                        bad.public_key
                    )));
                }
                file.device
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io_error(e)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            devices,
        })
    }

    pub fn devices(&self) -> &[TrustedDevice] {
        &self.devices
    }

    pub fn get(&self, key: &PublicKey) -> Option<&TrustedDevice> {
        let hex = encode_hex(key);
        self.devices.iter().find(|d| d.public_key == hex)
    }

    /// Remembers a device and saves the store. A device that is already
    /// remembered keeps its original approval time and takes the new name.
    pub fn remember(&mut self, key: &PublicKey, name: &str, now: u64) -> Result<(), CryptoError> {
        let hex = encode_hex(key);
        let name = clean_name(name);
        match self.devices.iter_mut().find(|d| d.public_key == hex) {
            Some(existing) => existing.name = name,
            None => self.devices.push(TrustedDevice {
                public_key: hex,
                name,
                approved_at: now,
            }),
        }
        self.save()
    }

    /// Forgets a device and saves the store. Returns whether it was known.
    pub fn forget(&mut self, key: &PublicKey) -> Result<bool, CryptoError> {
        let hex = encode_hex(key);
        let before = self.devices.len();
        self.devices.retain(|d| d.public_key != hex);
        let removed = self.devices.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<(), CryptoError> {
        let path = &self.path;
        let io_error = |source| CryptoError::Io {
            path: path.clone(),
            source,
        };
        let text = toml::to_string_pretty(&File {
            device: self.devices.clone(),
        })
        .map_err(|e| CryptoError::Malformed {
            path: path.clone(),
            reason: e.to_string(),
        })?;

        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(io_error)?;
        // Write beside the target, then rename over it, so a crash cannot
        // leave half a file.
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
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
        result.map_err(io_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: PublicKey = [0xA1; KEY_LEN];
    const KEY_B: PublicKey = [0xB2; KEY_LEN];

    fn store(dir: &tempfile::TempDir) -> TrustStore {
        TrustStore::load(&dir.path().join("trusted.toml")).unwrap()
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        assert!(store.devices().is_empty());
        assert!(store.get(&KEY_A).is_none());
    }

    #[test]
    fn remembered_devices_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = store(&dir);
        first.remember(&KEY_A, "Laptop", 1000).unwrap();
        first.remember(&KEY_B, "Phone", 2000).unwrap();

        let reloaded = store(&dir);
        assert_eq!(reloaded.devices().len(), 2);
        let laptop = reloaded.get(&KEY_A).unwrap();
        assert_eq!((laptop.name.as_str(), laptop.approved_at), ("Laptop", 1000));
        assert_eq!(laptop.fingerprint().unwrap(), fingerprint(&KEY_A));
        assert!(reloaded.get(&[0; KEY_LEN]).is_none());
    }

    #[test]
    fn remembering_again_renames_but_keeps_the_first_approval_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        store.remember(&KEY_A, "Laptop", 1000).unwrap();
        store.remember(&KEY_A, "Work laptop", 9999).unwrap();

        assert_eq!(store.devices().len(), 1);
        let device = store.get(&KEY_A).unwrap();
        assert_eq!(
            (device.name.as_str(), device.approved_at),
            ("Work laptop", 1000)
        );
    }

    #[test]
    fn forgetting_removes_only_that_device() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = store(&dir);
        first.remember(&KEY_A, "Laptop", 1).unwrap();
        first.remember(&KEY_B, "Phone", 2).unwrap();

        assert!(first.forget(&KEY_A).unwrap());
        assert!(!first.forget(&KEY_A).unwrap());
        let reloaded = store(&dir);
        assert!(reloaded.get(&KEY_A).is_none());
        assert!(reloaded.get(&KEY_B).is_some());
    }

    #[test]
    fn names_are_made_printable_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        store.remember(&KEY_A, "  Evil\n\tname\u{7}  ", 1).unwrap();
        store.remember(&KEY_B, &"x".repeat(500), 1).unwrap();
        assert_eq!(store.get(&KEY_A).unwrap().name, "Evilname");
        assert_eq!(store.get(&KEY_B).unwrap().name.len(), MAX_NAME_CHARS);

        store.remember(&KEY_A, "\n\n", 1).unwrap();
        assert_eq!(store.get(&KEY_A).unwrap().name, "Unnamed device");
    }

    #[test]
    fn a_damaged_file_is_an_error_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.toml");
        for damage in [
            "this is not = [toml",
            "[[device]]\npublic_key = \"abc\"\nname = \"x\"\napproved_at = 1\n",
        ] {
            fs::write(&path, damage).unwrap();
            assert!(matches!(
                TrustStore::load(&path),
                Err(CryptoError::Malformed { .. })
            ));
            assert_eq!(fs::read_to_string(&path).unwrap(), damage);
        }
    }

    #[test]
    fn saving_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        store.remember(&KEY_A, "Laptop", 1).unwrap();
        store.remember(&KEY_B, "Phone", 2).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["trusted.toml"]);
    }
}
