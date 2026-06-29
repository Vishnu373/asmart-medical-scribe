//! DPAPI-wrapped random AES-256 database key. Design §10.1. B2.
//!
//! On first run we generate a random 32-byte key and hand it to Windows DPAPI
//! (`CryptProtectData`), scoped to the logged-in user. Only the wrapped blob is
//! written to disk; the raw key never is. On launch we unwrap it
//! (`CryptUnprotectData`) and hand it to SQLCipher — no password prompt.

use anyhow::{anyhow, Context, Result};
use rand::RngCore;
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

/// AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;

/// Returns the database key, creating and persisting a fresh wrapped key on
/// first run. `path` holds the DPAPI-wrapped blob, not the raw key.
pub fn load_or_create_key(path: &Path) -> Result<[u8; KEY_LEN]> {
    if path.exists() {
        let blob = fs::read(path).context("read wrapped key blob")?;
        let mut raw = dpapi_unwrap(&blob).context("DPAPI unwrap key")?;
        let key = raw
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("wrapped key has wrong length ({} bytes)", raw.len()));
        // Scrub the transient plaintext copy whether or not the length matched.
        raw.zeroize();
        key
    } else {
        let mut key = [0u8; KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let blob = dpapi_wrap(&key).context("DPAPI wrap key")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).context("create key directory")?;
        }
        fs::write(path, &blob).context("write wrapped key blob")?;
        Ok(key)
    }
}

#[cfg(windows)]
fn dpapi_wrap(data: &[u8]) -> Result<Vec<u8>> {
    unsafe { dpapi(data, true) }
}

#[cfg(windows)]
fn dpapi_unwrap(data: &[u8]) -> Result<Vec<u8>> {
    unsafe { dpapi(data, false) }
}

#[cfg(windows)]
unsafe fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    if protect {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("CryptProtectData failed")?;
    } else {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("CryptUnprotectData failed")?;
    }

    let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
    // DPAPI allocates the output with LocalAlloc; we own it and must free it.
    let _ = LocalFree(HLOCAL(output.pbData as *mut core::ffi::c_void));
    Ok(result)
}

#[cfg(not(windows))]
fn dpapi_wrap(_data: &[u8]) -> Result<Vec<u8>> {
    Err(anyhow!("DPAPI key protection is only available on Windows"))
}

#[cfg(not(windows))]
fn dpapi_unwrap(_data: &[u8]) -> Result<Vec<u8>> {
    Err(anyhow!("DPAPI key protection is only available on Windows"))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_round_trips() {
        let mut key = [0u8; KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let blob = dpapi_wrap(&key).unwrap();
        assert_ne!(blob.as_slice(), &key[..], "blob must not be the raw key");
        let back = dpapi_unwrap(&blob).unwrap();
        assert_eq!(back.as_slice(), &key[..]);
    }

    #[test]
    fn load_or_create_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");
        let first = load_or_create_key(&path).unwrap();
        assert!(path.exists());
        let second = load_or_create_key(&path).unwrap();
        assert_eq!(first, second, "reload must return the same key");
    }
}
