//! Small helpers shared by the hub's modules (the names follow MoneyKeep's capture server,
//! whose `wifi.rs` and `pin.rs` were copied here).

use std::path::Path;

/// Errors as text: every message is meant for the person at the hub computer.
pub type StoreResult<T> = Result<T, String>;

/// `N` random bytes from the operating system.
pub fn random<const N: usize>() -> StoreResult<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| format!("no random source: {e}"))?;
    Ok(b)
}

/// Lower-case hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Compare in constant time (for hashes of secrets).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Write a file only its owner can read (on Unix; on Windows the hub's data folder's
/// permissions apply, set by the installer). Written through a temporary file.
pub fn write_private(path: &Path, bytes: &[u8]) -> StoreResult<()> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let result = (|| -> std::io::Result<()> {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{} can't be written: {e}", path.display())
    })
}

/// Milliseconds since 1970 UTC.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
