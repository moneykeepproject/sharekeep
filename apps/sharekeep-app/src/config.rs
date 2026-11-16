//! The app's own settings: which hub, the device's token, who this device belongs to, and
//! (on the hub computer) where the hub keeps its data. Kept in the user's private app-data
//! folder. The device token is kept apart, in `device.token`, sealed by Windows for this
//! user (DPAPI; owner-only elsewhere); it never goes in a log or on screen.

use serde::{Deserialize, Serialize};
use sk_client::api::BookFacts;
use sk_client::seal::{self, Scope};
use sk_client::HubAddress;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "sharekeep.json";
pub const TOKEN_FILE: &str = "device.token";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub hub: Option<HubAddress>,
    /// Read from and written to [`TOKEN_FILE`]; only an old settings file has it here.
    #[serde(default, skip_serializing)]
    pub token: Option<String>,
    #[serde(default)]
    pub member: Option<u32>,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub book: Option<BookFacts>,
    /// On the hub computer: the hub's data folder (its admin token is read from there).
    #[serde(default)]
    pub hub_data: Option<PathBuf>,
}

impl AppConfig {
    pub fn paired(&self) -> bool {
        self.hub.is_some() && self.token.is_some() && self.member.is_some()
    }

    pub fn load(dir: &Path) -> AppConfig {
        let mut c: AppConfig = std::fs::read(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let old = c.token.is_some();
        if let Ok(b) = std::fs::read(dir.join(TOKEN_FILE)) {
            if let Ok((t, sealed)) = seal::unseal(&b, Scope::User) {
                c.token = String::from_utf8(t.to_vec()).ok();
                if !sealed && seal::SEALS {
                    let _ = c.save(dir);
                }
            }
        }
        if old {
            // a token from before it had its own sealed file: move it there
            let _ = c.save(dir);
        }
        c
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("the settings folder can't be made ({e})"))?;
        match &self.token {
            Some(t) => {
                let sealed = seal::seal(t.as_bytes(), Scope::User)?;
                write_private(&dir.join(TOKEN_FILE), &sealed)
                    .map_err(|e| format!("the device token can't be saved ({e})"))?;
            }
            None => {
                let _ = std::fs::remove_file(dir.join(TOKEN_FILE));
            }
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_private(&dir.join(CONFIG_FILE), &bytes)
            .map_err(|e| format!("the settings can't be saved ({e})"))
    }
}

impl std::fmt::Display for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never the token
        write!(
            f,
            "hub {:?}, member {:?}, paired {}",
            self.hub.as_ref().map(|h| h.addr),
            self.member,
            self.token.is_some()
        )
    }
}

/// Write a file only this user can read (on Unix; Windows' per-user app-data folder is
/// already private), through a temporary file so a crash never leaves half of it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
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
    }
    std::fs::rename(&tmp, path)
}

/// Where the installer puts the hub's data on this computer: `%ProgramData%\ShareKeep Hub`
/// on Windows, `~/Library/Application Support/ShareKeep Hub` on the Mac.
pub fn default_hub_data() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("ShareKeep Hub"))
    } else if cfg!(target_os = "macos") {
        directories::BaseDirs::new().map(|b| b.data_dir().join("ShareKeep Hub"))
    } else {
        None
    }
}

/// The app's data folder: `%APPDATA%\MoneyKeep Project\ShareKeep\data` on Windows,
/// `~/Library/Application Support/org.MoneyKeep-Project.ShareKeep` on the Mac.
pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("org", "MoneyKeep Project", "ShareKeep")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("sharekeep-data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_never_show_the_token() {
        let d = tempfile::tempdir().unwrap();
        assert!(!AppConfig::load(d.path()).paired());
        let c = AppConfig {
            hub: Some(HubAddress::new(
                "192.168.1.20:15443".parse().unwrap(),
                "192.168.1.20",
                &[1, 2],
            )),
            token: Some("secret-token".into()),
            member: Some(2),
            ..Default::default()
        };
        c.save(d.path()).unwrap();
        let back = AppConfig::load(d.path());
        assert!(back.paired());
        assert_eq!(back.token.as_deref(), Some("secret-token"));
        assert!(!format!("{back}").contains("secret"));
        // the token isn't in the settings file
        let text = std::fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap();
        assert!(!text.contains("secret-token"));
        // an old settings file with the token in it moves it to its own file
        std::fs::remove_file(d.path().join(TOKEN_FILE)).unwrap();
        let old = text.trim_end().trim_end_matches('}').to_string() + ",\"token\":\"old-token\"}";
        std::fs::write(d.path().join(CONFIG_FILE), old).unwrap();
        assert_eq!(
            AppConfig::load(d.path()).token.as_deref(),
            Some("old-token")
        );
        assert!(d.path().join(TOKEN_FILE).exists());
        assert!(!std::fs::read_to_string(d.path().join(CONFIG_FILE))
            .unwrap()
            .contains("old-token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.path().join(CONFIG_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
