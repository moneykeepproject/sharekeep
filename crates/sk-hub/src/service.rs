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
        // the folder: owned by Administrators; every permission anyone put on anything in
        // it removed (`/reset`: only what the folder passes down remains), then the folder
        // itself passes down only SYSTEM and administrators full, the service modify.
        // `/L` works on a link itself, never on what it points to (`prepare` refuses links
        // anyway).
        vec![
            "icacls.exe".into(),
            data_s.clone(),
            "/setowner".into(),
            "*S-1-5-32-544".into(),
            "/T".into(),
            "/C".into(),
            "/L".into(),
        ],
        vec![
            "icacls.exe".into(),
            data_s.clone(),
            "/reset".into(),
            "/T".into(),
            "/C".into(),
            "/L".into(),
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
            "/C".into(),
            "/L".into(),
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

/// Before the elevated setup trusts a data folder that is already there: everything in it must
/// belong to the administrators, SYSTEM or a service, and nothing in it may be a link (review
/// H2: any user can create a folder in `%ProgramData%` before ShareKeep is installed). A
/// folder that fails is renamed aside, never used or deleted, and the hub starts fresh.
/// Answers what was done, for the setup's log.
pub fn prepare_data(data: &Path, stamp: &str) -> Result<String, String> {
    if std::fs::symlink_metadata(data).is_err() {
        return Ok(format!("{} is new.", data.display()));
    }
    match sk_client::owner::protected_tree(data) {
        Ok(()) => Ok(format!("{} is the hub's own.", data.display())),
        Err(why) => {
            let mut name = data.as_os_str().to_owned();
            name.push(format!(" (set aside {stamp})"));
            let aside = std::path::PathBuf::from(name);
            std::fs::rename(data, &aside).map_err(|e| {
                format!(
                    "{} was not made by ShareKeep's setup ({why}) and can't be moved aside ({e}). Move or delete it, then run the setup again.",
                    data.display()
                )
            })?;
            Ok(format!(
                "{} was not made by ShareKeep's setup ({why}): it was moved to {} and is not used.",
                data.display(),
                aside.display()
            ))
        }
    }
}

/// Windows: Setup's record of where the hub's data is (`HKLM\Software\ShareKeep\HubData`,
/// 64-bit view), which later silent updates read.
pub fn hub_data_registry(data: &Path) -> Step {
    vec![
        "reg.exe".into(),
        "add".into(),
        r"HKLM\Software\ShareKeep".into(),
        "/v".into(),
        "HubData".into(),
        "/t".into(),
        "REG_SZ".into(),
        "/d".into(),
        data.display().to_string(),
        "/f".into(),
        "/reg:64".into(),
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
    // a Mac path whatever system writes it (the tests also run on Windows)
    let log = format!(
        "{}/{LOG_FILE}",
        data.display().to_string().trim_end_matches('/')
    );
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
        log = xml(&log),
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
        assert!(pos("/setowner") < pos("/reset"));
        assert!(pos("/reset") < pos("/inheritance:r"));
        // every permission step works on links themselves, never their targets
        assert!(steps
            .iter()
            .filter(|s| s[0] == "icacls.exe" && s.len() > 3 && s[2] != "/grant")
            .all(|s| s.contains(&"/L".to_string())));
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

    #[cfg(unix)]
    #[test]
    fn a_data_folder_with_a_link_in_it_is_set_aside() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("ShareKeep Hub");
        assert!(prepare_data(&data, "x").unwrap().contains("is new"));
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("hub.json"), "{}").unwrap();
        assert!(prepare_data(&data, "x").unwrap().contains("own"));
        std::os::unix::fs::symlink("/etc", data.join("keys")).unwrap();
        let said = prepare_data(&data, "20261116").unwrap();
        assert!(said.contains("moved to"), "{said}");
        assert!(!data.exists());
        assert!(d
            .path()
            .join("ShareKeep Hub (set aside 20261116)")
            .join("hub.json")
            .exists());
    }

    /// The Mac's install script writes launchd's file itself (so nothing of ShareKeep's runs as
    /// root): it must be the same file as [`macos_plist`].
    #[test]
    fn the_mac_install_script_writes_the_same_daemon_file() {
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../installer/macos/install-hub.command"),
        )
        .unwrap()
        // a checkout made before .gitattributes covered .command files may have CRLF
        .replace("\r\n", "\n");
        let body = script
            .split_once("<<PLIST\n")
            .unwrap()
            .1
            .split_once("\nPLIST\n")
            .unwrap()
            .0;
        let exe = "/Applications/ShareKeep.app/Contents/MacOS/sharekeep-hub";
        let data = "/Users/jerry/Library/Application Support/ShareKeep Hub";
        let from_script = format!("{body}\n")
            .replace("$LABEL", LAUNCHD_LABEL)
            .replace("$HUB_X", exe)
            .replace("$DATA_X", data)
            .replace("$LOG_X", &format!("{data}/{LOG_FILE}"))
            .replace("$USER_X", "jerry");
        assert_eq!(
            from_script,
            macos_plist(Path::new(exe), Path::new(data), "jerry")
        );
        assert!(script.contains(&format!("LABEL=\"{LAUNCHD_LABEL}\"")));
        // root only places the file and starts launchd: never ShareKeep's own program
        for line in script
            .lines()
            .filter(|l| l.trim_start().starts_with("sudo "))
        {
            assert!(!line.contains("$HUB"), "{line}");
        }
        // the app bundle carries this same script, which the app opens in Terminal (0.1.4)
        let dmg = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../installer/macos/make-dmg.sh"),
        )
        .unwrap();
        assert!(dmg.contains(
            r#"cp "$HERE/install-hub.command" "$HERE/uninstall-hub.command" "$APP/Contents/Resources/""#
        ));
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
