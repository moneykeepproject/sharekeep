//! `hub.json`: where the hub listens. Public facts only (no keys, no PIN).

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::Path;

use crate::util::{write_private, StoreResult};

/// The HTTPS port devices use. MoneyKeep's capture server uses 14443, so the two can run
/// on one PC.
pub const HTTPS_PORT: u16 = 15443;
/// The plain-HTTP certificate page.
pub const HELPER_PORT: u16 = 15080;
/// The loopback port the desktop app on the hub PC uses.
pub const ADMIN_PORT: u16 = 15081;

pub const CONFIG_FILE: &str = "hub.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubConfig {
    /// The hub PC's fixed address on the home network.
    pub address: Ipv4Addr,
    /// An optional local name (`sharekeep.home.arpa`).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "https_port")]
    pub https_port: u16,
    #[serde(default = "helper_port")]
    pub helper_port: u16,
    #[serde(default = "admin_port")]
    pub admin_port: u16,
}

fn https_port() -> u16 {
    HTTPS_PORT
}
fn helper_port() -> u16 {
    HELPER_PORT
}
fn admin_port() -> u16 {
    ADMIN_PORT
}

impl HubConfig {
    pub fn new(address: Ipv4Addr, name: Option<String>) -> Self {
        HubConfig {
            address,
            name,
            https_port: HTTPS_PORT,
            helper_port: HELPER_PORT,
            admin_port: ADMIN_PORT,
        }
    }

    pub fn load(dir: &Path) -> StoreResult<HubConfig> {
        let b = std::fs::read(dir.join(CONFIG_FILE))
            .map_err(|e| format!("The hub isn't set up yet ({CONFIG_FILE} can't be read: {e})."))?;
        serde_json::from_slice(&b).map_err(|e| format!("{CONFIG_FILE} is damaged ({e})"))
    }

    pub fn save(&self, dir: &Path) -> StoreResult<()> {
        let b = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_private(&dir.join(CONFIG_FILE), &b)
    }
}
