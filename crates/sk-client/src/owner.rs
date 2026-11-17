//! Who owns a file or folder (review H2). The hub's data folder lives in `%ProgramData%`,
//! where any user may create folders: before the elevated setup or the desktop app trusts
//! anything in it, everything there must belong to the administrators, SYSTEM or a service
//! account, none of which an ordinary program can be, and nothing there may be a link.
//! On other systems these checks pass (the Mac hub keeps its data in the user's own Library).

use std::path::Path;

/// Owners an ordinary program can't be: Administrators, SYSTEM, TrustedInstaller and the
/// other service accounts (`NT SERVICE\…`, S-1-5-80-…), such as the hub's own.
pub fn trusted_owner(sid: &str) -> bool {
    sid == "S-1-5-32-544" || sid == "S-1-5-18" || sid.starts_with("S-1-5-80-")
}

/// The owner of a file or folder, as a SID string ("S-1-5-32-544").
#[cfg(windows)]
pub fn owner(path: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut sid: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the name is NUL-terminated; the descriptor is freed below, and `sid` points into it.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut sid,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(format!(
            "{}: its owner can't be read (error {rc})",
            path.display()
        ));
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is valid while `sd` lives; `text` is allocated by the call and freed below.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut text) };
    let out = if ok != 0 && !text.is_null() {
        let mut len = 0;
        // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
        unsafe {
            while *text.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as _);
            Ok(s)
        }
    } else {
        Err(format!("{}: its owner can't be read", path.display()))
    };
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd as _);
    }
    out
}

/// Whether `path` is owned by an account an ordinary program can't be.
pub fn protected(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let sid = owner(path)?;
        if !trusted_owner(&sid) {
            return Err(format!(
                "{} belongs to {sid}, not to the administrators",
                path.display()
            ));
        }
    }
    let _ = path;
    Ok(())
}

/// `dir` and everything in it is [`protected`], and nothing in it is a link or junction.
/// Links are never followed.
pub fn protected_tree(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a link", dir.display()));
    }
    protected(dir)?;
    if meta.is_dir() {
        for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            protected_tree(&e.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_an_ordinary_program_cant_be() {
        assert!(trusted_owner("S-1-5-32-544"));
        assert!(trusted_owner("S-1-5-18"));
        // NT SERVICE\ShareKeepHub
        assert!(trusted_owner(
            "S-1-5-80-1234567890-1234567890-1234567890-1234567890-1234567890"
        ));
        // a person's account, Users, Everyone
        assert!(!trusted_owner("S-1-5-21-1-2-3-1001"));
        assert!(!trusted_owner("S-1-5-32-545"));
        assert!(!trusted_owner("S-1-1-0"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_is_refused_and_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("hub");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("hub.json"), "{}").unwrap();
        assert!(protected_tree(&data).is_ok());
        std::os::unix::fs::symlink(dir.path(), data.join("elsewhere")).unwrap();
        assert!(protected_tree(&data).unwrap_err().contains("is a link"));
    }
}
