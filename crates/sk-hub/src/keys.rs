//! The hub's private keys: its certificate authority's key and its server key.
//!
//! Kept in the hub's data folder, one file per key. On Windows each file is sealed by DPAPI
//! for this computer (`sk_client::seal`, machine scope), and the installer lets only the
//! hub's service account and administrators into the folder; elsewhere the files are readable
//! only by their owner (root, for the macOS daemon). A key is never sent to a device, put in
//! `hub.json` or written to a log. A plain key file from before sealing is sealed the first
//! time it's read. Tests keep keys in memory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use zeroize::Zeroizing;

use crate::util::{write_private, StoreResult};
use sk_client::seal::{self, Scope};

#[derive(Debug)]
enum Backend {
    Files(PathBuf),
    Memory(Mutex<HashMap<String, Vec<u8>>>),
}

/// Named private keys.
#[derive(Debug)]
pub struct KeyStore {
    backend: Backend,
}

fn check_name(name: &str) -> StoreResult<()> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(())
    } else {
        Err(format!("\"{name}\" isn't a key name"))
    }
}

impl KeyStore {
    /// Keys in `<dir>/keys/`.
    pub fn files(dir: PathBuf) -> Self {
        KeyStore {
            backend: Backend::Files(dir.join("keys")),
        }
    }

    pub fn memory() -> Self {
        KeyStore {
            backend: Backend::Memory(Mutex::new(HashMap::new())),
        }
    }

    pub fn get(&self, name: &str) -> StoreResult<Option<Zeroizing<Vec<u8>>>> {
        check_name(name)?;
        match &self.backend {
            Backend::Files(dir) => match std::fs::read(dir.join(format!("{name}.key"))) {
                Ok(b) => {
                    let (key, sealed) = seal::unseal(&b, Scope::Machine)
                        .map_err(|e| format!("the key {name} can't be opened: {e}"))?;
                    if !sealed && seal::SEALS {
                        // written before sealing: seal it now (best effort)
                        let _ = self.set(name, &key);
                    }
                    Ok(Some(key))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("the key {name} can't be read: {e}")),
            },
            Backend::Memory(m) => Ok(m
                .lock()
                .map_err(|_| "keys unavailable".to_string())?
                .get(name)
                .map(|v| Zeroizing::new(v.clone()))),
        }
    }

    pub fn set(&self, name: &str, value: &[u8]) -> StoreResult<()> {
        check_name(name)?;
        match &self.backend {
            Backend::Files(dir) => {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                let bytes = Zeroizing::new(seal::seal(value, Scope::Machine)?);
                write_private(&dir.join(format!("{name}.key")), &bytes)
            }
            Backend::Memory(m) => {
                m.lock()
                    .map_err(|_| "keys unavailable".to_string())?
                    .insert(name.into(), value.to_vec());
                Ok(())
            }
        }
    }

    pub fn delete(&self, name: &str) -> StoreResult<()> {
        check_name(name)?;
        match &self.backend {
            Backend::Files(dir) => match std::fs::remove_file(dir.join(format!("{name}.key"))) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            },
            Backend::Memory(m) => {
                m.lock()
                    .map_err(|_| "keys unavailable".to_string())?
                    .remove(name);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_keys_round_trip_and_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let k = KeyStore::files(dir.path().to_path_buf());
        assert_eq!(k.get("hub-ca-key").unwrap(), None);
        k.set("hub-ca-key", b"secret").unwrap();
        assert_eq!(k.get("hub-ca-key").unwrap().unwrap().as_slice(), b"secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("keys/hub-ca-key.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
        k.delete("hub-ca-key").unwrap();
        assert_eq!(k.get("hub-ca-key").unwrap(), None);
        assert!(k.set("../escape", b"x").is_err());
    }
}
