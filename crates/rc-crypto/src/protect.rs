//! Encryption of secrets at rest with the Windows Data Protection API.
//!
//! DPAPI ties the ciphertext to the current Windows user: another account,
//! or the same file copied to another computer, cannot decrypt it.

use crate::CryptoError;

/// Mixed into the encryption so that another program running as the same
/// user does not decrypt this by accident. Not a secret.
#[cfg(windows)]
const PURPOSE: &[u8] = b"RemoteControl device identity v1";

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    use super::{CryptoError, PURPOSE};

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr().cast_mut(),
        }
    }

    /// Copies the output blob and frees the system's allocation.
    ///
    /// # Safety
    /// `out` must have been filled by a successful DPAPI call.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        unsafe {
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(out.pbData.cast())));
            bytes
        }
    }

    pub fn protect(secret: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the input blobs point at live slices that DPAPI only
        // reads; `out` is a live local that DPAPI fills.
        unsafe {
            CryptProtectData(
                &blob(secret),
                None,
                Some(&blob(PURPOSE)),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| CryptoError::KeyUnreadable(e.message()))?;
            Ok(take(out))
        }
    }

    pub fn unprotect(protected: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`.
        unsafe {
            CryptUnprotectData(
                &blob(protected),
                None,
                Some(&blob(PURPOSE)),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| CryptoError::KeyUnreadable(e.message()))?;
            Ok(take(out))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::CryptoError;

    const UNSUPPORTED: &str = "protected storage is only implemented on Windows";

    pub fn protect(_secret: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Err(CryptoError::KeyUnreadable(UNSUPPORTED.to_owned()))
    }

    pub fn unprotect(_protected: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Err(CryptoError::KeyUnreadable(UNSUPPORTED.to_owned()))
    }
}

pub use imp::{protect, unprotect};

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_hides_the_secret() {
        let secret = b"thirty-two bytes of key material";
        let protected = protect(secret).unwrap();
        assert!(protected.len() > secret.len());
        assert!(!protected.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect(&protected).unwrap(), secret);
    }

    #[test]
    fn rejects_tampered_and_foreign_data() {
        let mut protected = protect(b"secret").unwrap();
        let last = protected.len() - 1;
        protected[last] ^= 0x01;
        assert!(matches!(
            unprotect(&protected),
            Err(CryptoError::KeyUnreadable(_))
        ));
        assert!(unprotect(b"not a DPAPI blob").is_err());
        assert!(unprotect(&[]).is_err());
    }
}
