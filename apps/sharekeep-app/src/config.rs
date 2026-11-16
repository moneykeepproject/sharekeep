//! The app's own settings: which hub, the device's token, who this device belongs to, and
//! (on the hub computer) where the hub keeps its data. Kept in the user's private app-data
//! folder; the token never goes in a log or on screen. (Moving the token into the OS's
//! sealed key storage comes with the installers, milestone 4d.)

use serde::{Deserialize, Serialize};
use sk_client::api::BookFacts;
use sk_client::HubAddress;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "sharekeep.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub hub: Option<HubAddress>,
    #[serde(default)]
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
        std::fs::read(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("the settings folder can't be made ({e})"))?;
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
