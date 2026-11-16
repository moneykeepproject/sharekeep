//! Secrets sealed by the operating system before they are written to a file.
//!
//! On Windows, DPAPI (`CryptProtectData`): a sealed file can't be opened on another computer
//! (and, for [`Scope::User`], not by another Windows user). [`Scope::Machine`] is for the
//! hub's service, whose keys are made by the installer (an administrator) and read by the
//! service's own account; the data folder's permissions keep everyone else out. Elsewhere
//! (macOS, Linux) the file is kept as it is, readable only by its owner (root, for the hub's
//! daemon).
//!
//! A sealed file starts with [`MAGIC`]; a file without it is read as plain (written before
//! sealing existed, or on another system), so callers can seal it again.

/// Who may open a sealed secret on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only this Windows user.
    User,
    /// Any program on this computer (the folder's permissions do the rest).
    Machine,
}

/// The start of a DPAPI-sealed file.
pub const MAGIC: &[u8; 4] = b"SKD1";

/// Extra entropy, so another program's DPAPI blob is never mistaken for ours.
#[cfg(windows)]
const ENTROPY: &[u8] = b"ShareKeep sealed secret v1";

/// Whether this system seals (Windows) or keeps owner-only files (everything else).
pub const SEALS: bool = cfg!(windows);

/// The bytes to write for `secret`.
pub fn seal(secret: &[u8], scope: Scope) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        let mut out = MAGIC.to_vec();
        out.extend(dpapi::protect(secret, scope)?);
        Ok(out)
    }
    #[cfg(not(windows))]
    {
        let _ = scope;
        Ok(secret.to_vec())
    }
}

/// The secret in bytes read from a file, and whether they were sealed.
pub fn unseal(bytes: &[u8], scope: Scope) -> Result<(zeroize::Zeroizing<Vec<u8>>, bool), String> {
    match bytes.strip_prefix(MAGIC.as_slice()) {
        Some(blob) => {
            #[cfg(windows)]
            {
                Ok((
                    zeroize::Zeroizing::new(dpapi::unprotect(blob, scope)?),
                    true,
                ))
            }
            #[cfg(not(windows))]
            {
                let _ = (blob, scope);
                Err("this secret was sealed by Windows on another computer and can't be opened here".into())
            }
        }
        None => Ok((zeroize::Zeroizing::new(bytes.to_vec()), false)),
    }
}

#[cfg(windows)]
mod dpapi {
    use super::{Scope, ENTROPY};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE,
        CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: b.len() as u32,
            pbData: b.as_ptr() as *mut u8,
        }
    }

    fn flags(scope: Scope) -> u32 {
        CRYPTPROTECT_UI_FORBIDDEN
            | match scope {
                Scope::Machine => CRYPTPROTECT_LOCAL_MACHINE,
                Scope::User => 0,
            }
    }

    /// Copy DPAPI's output and free it.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        v
    }

    pub fn protect(secret: &[u8], scope: Scope) -> Result<Vec<u8>, String> {
        let input = blob(secret);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blobs point at live slices for the call; DPAPI allocates `out`,
        // which `take` copies and frees.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                flags(scope),
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Windows couldn't seal the secret ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(unsafe { take(out) })
    }

    pub fn unprotect(sealed: &[u8], scope: Scope) -> Result<Vec<u8>, String> {
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                flags(scope),
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Windows couldn't open the sealed secret: it was sealed on another computer or by another user ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(unsafe { take(out) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_round_trips_and_plain_files_still_read() {
        for scope in [Scope::User, Scope::Machine] {
            let sealed = seal(b"top secret", scope).unwrap();
            assert_eq!(sealed.starts_with(MAGIC), SEALS);
            if SEALS {
                assert!(!sealed.windows(10).any(|w| w == b"top secret"));
            }
            let (back, was) = unseal(&sealed, scope).unwrap();
            assert_eq!(back.as_slice(), b"top secret");
            assert_eq!(was, SEALS);
        }
        let (plain, was) = unseal(b"old key", Scope::Machine).unwrap();
        assert_eq!(plain.as_slice(), b"old key");
        assert!(!was);
    }
}
