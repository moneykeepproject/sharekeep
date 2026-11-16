//! Running the hub in the background: a Windows service, or a macOS launchd daemon.
//!
//! Lessons from MoneyKeep's capture service:
//! - The Windows service runs as its own virtual account (`NT SERVICE\ShareKeepHub`), never
//!   LocalSystem, on a data folder in `%ProgramData%` that only it, SYSTEM and administrators
//!   can change. The installing user may read just `admin.token` and `hub.json` (the desktop
//!   app's Hub tab needs them) and list the folder.
//! - The folder's owner is set to Administrators explicitly (on a PC with UAC off a new
//!   folder's owner is the user, which made MoneyKeep's checks refuse it).
//! - No PowerShell: `sc.exe`, `icacls.exe` and `netsh.exe` only (a Windows PowerShell started
//!   from PowerShell 7 can't load its own modules: MoneyKeep's `PSModulePath` failure).
//! - On the Mac, the daemon runs as the installing user, not root, with the data folder in
//!   that user's Library (owner-only).
//!
//! The steps are plain data ([`windows_install`], [`macos_plist`]) so they are tested on any
//! system; [`run_steps`] runs them.

use std::path::Path;

/// The Windows service's name (also its virtual account: `NT SERVICE\ShareKeepHub`).
pub const SERVICE_NAME: &str = "ShareKeepHub";
pub const DISPLAY_NAME: &str = "ShareKeep Hub";
pub const DESCRIPTION: &str =
    "Keeps the household's ShareKeep books and serves the household's devices on the home network.";
/// The firewall rule's name.
pub const FIREWALL_RULE: &str = "ShareKeep Hub";
/// The macOS launchd label.
pub const LAUNCHD_LABEL: &str = "org.moneykeep.sharekeep-hub";
/// The log file in the data folder (the service has no console).
pub const LOG_FILE: &str = "hub.log";

/// One program run: the program and its arguments.
pub type Step = Vec<String>;

fn step(parts: &[&str]) -> Step {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Windows: what the installer runs (elevated) after `sharekeep-hub setup`, in order.
/// `user` is the installing user (`DOMAIN\name` or `name`), who may read the admin token.
pub fn windows_install(exe: &Path, data: &Path, user: &str, ports: (u16, u16)) -> Vec<Step> {
    let exe = exe.display().to_string();
    let data_s = data.display().to_string();
    let account = format!("NT SERVICE\\{SERVICE_NAME}");
    // sc.exe takes `binPath= <value>` as two arguments; the value quotes paths with spaces
    let bin = format!("\"{exe}\" service-run --data \"{data_s}\"");
    vec![
        step(&["sc.exe", "stop", SERVICE_NAME]),
        step(&["sc.exe", "delete", SERVICE_NAME]),
        vec![
            "sc.exe".into(),
            "create".into(),
            SERVICE_NAME.into(),
            "binPath=".into(),
            bin,
            "start=".into(),
            "auto".into(),
            "obj=".into(),
            account.clone(),
            "DisplayName=".into(),
            DISPLAY_NAME.into(),
        ],
        step(&["sc.exe", "description", SERVICE_NAME, DESCRIPTION]),
        step(&[
            "sc.exe",
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/60000",
        ]),
        // the folder: owned by Administrators, no inherited permissions; SYSTEM and
        // administrators full, the service modify, everyone else nothing
        vec![
            "icacls.exe".into(),
            data_s.clone(),
            "/setowner".into(),
            "*S-1-5-32-544".into(),
            "/T".into(),
            "/C".into(),
        ],
        vec![
            "icacls.exe".into(),
            data_s.clone(),
            "/inheritance:r".into(),
            "/grant:r".into(),
            "*S-1-5-18:(OI)(CI)F".into(),
            "/grant:r".into(),
            "*S-1-5-32-544:(OI)(CI)F".into(),
            "/grant:r".into(),
            format!("{account}:(OI)(CI)M"),
            "/T".into(),
            "/C".into(),
        ],
        // the installing user: list the folder (this folder only), read two files
        vec![
            "icacls.exe".into(),
            data_s.clone(),
            "/grant".into(),
            format!("{user}:(RX)"),
        ],
        vec![
            "icacls.exe".into(),
            data.join("admin.token").display().to_string(),
            "/grant".into(),
            format!("{user}:(R)"),
        ],
        vec![
            "icacls.exe".into(),
            data.join("hub.json").display().to_string(),
            "/grant".into(),
            format!("{user}:(R)"),
        ],
        step(&[
            "netsh.exe",
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={FIREWALL_RULE}"),
        ]),
        vec![
            "netsh.exe".into(),
            "advfirewall".into(),
            "firewall".into(),
            "add".into(),
            "rule".into(),
            format!("name={FIREWALL_RULE}"),
            "dir=in".into(),
            "action=allow".into(),
            "protocol=TCP".into(),
            format!("localport={},{}", ports.0, ports.1),
            "profile=private,domain".into(),
            format!("program={exe}"),
        ],
        step(&["sc.exe", "start", SERVICE_NAME]),
    ]
}

/// Windows: removing the service and the firewall rule (the data stays).
pub fn windows_uninstall() -> Vec<Step> {
    vec![
        step(&["sc.exe", "stop", SERVICE_NAME]),
        step(&["sc.exe", "delete", SERVICE_NAME]),
        step(&[
            "netsh.exe",
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={FIREWALL_RULE}"),
        ]),
    ]
}

/// Steps that may fail without stopping the install (removing what isn't there).
pub fn may_fail(s: &Step) -> bool {
    let a: Vec<&str> = s.iter().map(String::as_str).collect();
    matches!(
        a.as_slice(),
        ["sc.exe", "stop", ..] | ["sc.exe", "delete", ..] | ["netsh.exe", _, _, "delete", ..]
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// macOS: the launchd daemon's property list. It runs as `user` and restarts if it stops.
pub fn macos_plist(exe: &Path, data: &Path, user: &str) -> String {
    let log = data.join(LOG_FILE);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>run</string>
    <string>--data</string>
    <string>{data}</string>
    <string>--log</string>
    <string>{log}</string>
  </array>
  <key>UserName</key><string>{user}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        exe = xml(&exe.display().to_string()),
        data = xml(&data.display().to_string()),
        log = xml(&log.display().to_string()),
        user = xml(user),
    )
}

/// Where the macOS daemon's property list goes.
pub fn macos_plist_path() -> String {
    format!("/Library/LaunchDaemons/{LAUNCHD_LABEL}.plist")
}

/// Run the steps in order; a step that may fail is reported but doesn't stop the rest.
pub fn run_steps(steps: &[Step], echo: &dyn Fn(&str)) -> Result<(), String> {
    for s in steps {
        let Some((prog, args)) = s.split_first() else {
            continue;
        };
        echo(&format!("> {}", s.join(" ")));
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .map_err(|e| format!("{prog} couldn't start: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !text.is_empty() {
            echo(&text);
        }
        if !out.status.success() && !may_fail(s) {
            return Err(format!(
                "{} failed: {}{}",
                s.join(" "),
                text,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// The first private IPv4 address this computer would use on the home network (the
/// address a UDP socket gets toward a private address; nothing is sent).
pub fn detect_address() -> Option<std::net::Ipv4Addr> {
    for probe in ["192.168.1.1:9", "10.0.0.1:9", "172.16.0.1:9"] {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(probe).is_err() {
            continue;
        }
        if let Ok(std::net::SocketAddr::V4(a)) = sock.local_addr() {
            let ip = *a.ip();
            if ip.is_private() {
                return Some(ip);
            }
        }
    }
    None
}

/// A log that appends to `path`, starting over at 1 MB (the old one is kept as `.1`).
pub fn file_log(path: std::path::PathBuf) -> crate::server::Log {
    let lock = std::sync::Mutex::new(());
    std::sync::Arc::new(move |line: &str| {
        use std::io::Write;
        let _g = lock.lock();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let _ = std::fs::rename(&path, old);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let now = chrono_like_now();
            let _ = writeln!(f, "{now} {line}");
        }
    })
}

fn chrono_like_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_windows_install_uses_a_virtual_account_a_locked_folder_and_no_powershell() {
        let exe = PathBuf::from(r"C:\Program Files\ShareKeep\sharekeep-hub.exe");
        let data = PathBuf::from(r"C:\ProgramData\ShareKeep Hub");
        let steps = windows_install(&exe, &data, r"PC\jerry", (15443, 15080));
        let flat: Vec<String> = steps.iter().map(|s| s.join(" ")).collect();
        assert!(steps
            .iter()
            .all(|s| !s[0].to_lowercase().contains("powershell")));
        let create = steps.iter().find(|s| s[1] == "create").unwrap();
        assert_eq!(create[3], "binPath=");
        assert_eq!(
            create[4],
            r#""C:\Program Files\ShareKeep\sharekeep-hub.exe" service-run --data "C:\ProgramData\ShareKeep Hub""#
        );
        assert!(create.contains(&r"NT SERVICE\ShareKeepHub".to_string()));
        // owner first, then the permissions, then the user's two files, then start
        let pos = |needle: &str| flat.iter().position(|s| s.contains(needle)).unwrap();
        assert!(pos("/setowner") < pos("/inheritance:r"));
        assert!(pos("/inheritance:r") < pos(r"PC\jerry:(RX)"));
        assert!(flat
            .iter()
            .any(|s| s.contains("admin.token") && s.contains(r"PC\jerry:(R)")));
        assert!(!flat
            .iter()
            .any(|s| s.contains("keys") && s.contains("jerry")));
        assert!(flat[pos("localport")].contains("localport=15443,15080"));
        assert!(flat[pos("localport")].contains("profile=private,domain"));
        assert_eq!(flat.last().unwrap(), "sc.exe start ShareKeepHub");
        assert!(may_fail(&steps[0]) && may_fail(&steps[1]));
        assert!(!may_fail(create));
    }

    #[test]
    fn the_mac_daemon_runs_as_the_user_and_quotes_paths() {
        let p = macos_plist(
            Path::new("/Applications/ShareKeep.app/Contents/MacOS/sharekeep-hub"),
            Path::new("/Users/jerry/Library/Application Support/ShareKeep Hub"),
            "jerry",
        );
        assert!(p.contains("<key>UserName</key><string>jerry</string>"));
        assert!(
            p.contains("<string>/Users/jerry/Library/Application Support/ShareKeep Hub</string>")
        );
        assert!(p.contains("ShareKeep Hub/hub.log</string>"));
        assert!(!macos_plist(Path::new("/a&b"), Path::new("/d"), "u<").contains("a&b"));
    }

    #[test]
    fn the_log_starts_over_when_it_grows() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(LOG_FILE);
        let log = file_log(path.clone());
        log("first");
        assert!(std::fs::read_to_string(&path).unwrap().contains("first"));
        std::fs::write(&path, vec![b'x'; 1_000_001]).unwrap();
        log("second");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .ends_with("second\n"));
        assert!(d.path().join("hub.log.1").exists());
    }
}
