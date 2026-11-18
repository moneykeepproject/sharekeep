//! Is this computer the household's hub, and can it become one? (0.1.4)
//!
//! The app asks "join a hub, or make this computer the hub" itself: an install from MoneyKeep
//! Launcher runs Setup silently, which never makes a hub. On Windows the app runs
//! `sharekeep-hub make-hub` with one administrator prompt; on a Mac it opens the bundled
//! "Install the household hub" script in Terminal (nothing of ShareKeep's runs as root).

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

/// The hub's admin port and token.
pub type Admin = (SocketAddr, String);

/// What the hub's folder says about this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubFolder {
    /// This computer is the hub.
    Here(Admin),
    /// A hub's folder is here, but the app can't use it: why, and what to do.
    Unusable(String),
    /// No hub here.
    None,
}

/// Look at the hub's folder. On Windows the folder and both files must belong to the
/// administrators (or the hub's service): an ordinary program can make a folder of that name
/// in `%ProgramData%` (review H2).
pub fn find(data: Option<&Path>) -> HubFolder {
    let Some(data) = data else {
        return HubFolder::None;
    };
    if !data.join("hub.json").exists() && !data.join("admin.token").exists() {
        return HubFolder::None;
    }
    for p in [
        data.to_path_buf(),
        data.join("admin.token"),
        data.join("hub.json"),
    ] {
        if let Err(why) = sk_client::owner::protected(&p) {
            return HubFolder::Unusable(format!(
                "A hub's folder was found on this computer ({}) but ShareKeep can't use it: {why}. If you made it by hand, remove it; otherwise make this computer the hub again.",
                data.display()
            ));
        }
    }
    let token = match std::fs::read_to_string(data.join("admin.token")) {
        Ok(t) => t,
        Err(e) => {
            return HubFolder::Unusable(format!(
                "This computer is a hub, but this account may not read its key ({e}). Make this computer the hub again from this account: it lets this account manage the hub."
            ))
        }
    };
    let port = std::fs::read(data.join("hub.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["admin_port"].as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(sk_client::ADMIN_PORT);
    HubFolder::Here((
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        token.trim().to_string(),
    ))
}

/// The app's pages at start (`app.slint`).
pub const PAGE_JOIN: i32 = 0;
pub const PAGE_HOME: i32 = 1;
pub const PAGE_HUB: i32 = 5;
pub const PAGE_EZ: i32 = 6;
pub const PAGE_CHOICE: i32 = 7;

/// The first screen: the hub's EZ Start until it's done, then the app once paired, the Hub
/// tab on a hub computer, otherwise the choice between joining and making this the hub.
pub fn first_page(is_hub: bool, ez_done: bool, paired: bool) -> i32 {
    if is_hub && !ez_done {
        PAGE_EZ
    } else if paired {
        PAGE_HOME
    } else if is_hub {
        PAGE_HUB
    } else {
        PAGE_CHOICE
    }
}

/// How this computer can become the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// Windows: run this `sharekeep-hub.exe` elevated.
    Windows(PathBuf),
    /// Mac: open this "Install the household hub" script in Terminal.
    Mac(PathBuf),
    /// Not from here: why.
    No(String),
}

/// Whether the app can offer "Make this computer the hub". On Windows only when the app runs
/// from Program Files: the administrator prompt would otherwise run a program an ordinary
/// program could have replaced (review H3, on Windows).
pub fn offer() -> Offer {
    let Ok(exe) = std::env::current_exe() else {
        return Offer::No("ShareKeep can't tell where it is installed.".into());
    };
    offer_for(&exe, cfg!(windows), cfg!(target_os = "macos"))
}

fn offer_for(exe: &Path, windows: bool, mac: bool) -> Offer {
    let dir = exe.parent().unwrap_or(Path::new("."));
    if windows {
        if !sk_client::makehub::under(exe, &sk_client::makehub::program_files()) {
            return Offer::No(
                "Install ShareKeep with its setup to make this computer the hub (this copy isn't in Program Files).".into(),
            );
        }
        let hub = dir.join("sharekeep-hub.exe");
        if !hub.is_file() {
            return Offer::No(
                "The hub program isn't installed beside ShareKeep: run ShareKeep's setup again."
                    .into(),
            );
        }
        Offer::Windows(hub)
    } else if mac {
        // ShareKeep.app/Contents/MacOS/sharekeep → Contents/Resources/install-hub.command
        let script = dir
            .parent()
            .map(|c| c.join("Resources").join("install-hub.command"))
            .unwrap_or_default();
        if !script.is_file() {
            return Offer::No(
                "Copy ShareKeep to Applications from its disk image to make this Mac the hub."
                    .into(),
            );
        }
        Offer::Mac(script)
    } else {
        Offer::No("On Linux, run `sharekeep-hub run --data <folder>` from a systemd unit.".into())
    }
}

/// The account running the app, for `make-hub --user` (so this account may read the hub's
/// key, even when the administrator account that approves the prompt is another one).
pub fn this_user() -> Option<String> {
    let name = std::env::var("USERNAME").ok()?;
    Some(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{name}"),
        _ => name,
    })
}

/// What happened when the app asked Windows to make this computer the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevated {
    /// The program ran: its exit code.
    Exited(i64),
    /// The person said no at the prompt.
    Declined,
    /// It couldn't be started.
    Failed(String),
}

/// Run `exe args` with Windows' administrator prompt, hidden, and wait for it (the worker
/// thread calls this). No shell, no PowerShell.
#[cfg(windows)]
pub fn run_elevated(exe: &Path, args: &str) -> Elevated {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(Some(0)).collect() };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params = wide(std::ffi::OsStr::new(args));
    // SAFETY: a zeroed SHELLEXECUTEINFOW is its documented empty value; every pointer set
    // below outlives the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: `info` is filled in as the API requires.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // SAFETY: plain call.
        let err = unsafe { GetLastError() };
        return if err == ERROR_CANCELLED {
            Elevated::Declined
        } else {
            Elevated::Failed(format!(
                "Windows couldn't start the hub program (error {err})."
            ))
        };
    }
    if info.hProcess.is_null() {
        return Elevated::Failed("Windows didn't say how the hub program ended.".into());
    }
    let mut code: u32 = 0;
    // SAFETY: a process handle from ShellExecuteExW (SEE_MASK_NOCLOSEPROCESS), closed here.
    let got = unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        got
    };
    if got == 0 {
        return Elevated::Failed("Windows didn't say how the hub program ended.".into());
    }
    Elevated::Exited(i64::from(code))
}

#[cfg(not(windows))]
pub fn run_elevated(_exe: &Path, _args: &str) -> Elevated {
    Elevated::Failed("Only on Windows.".into())
}

/// Open the Mac's "Install the household hub" script in Terminal (it asks for the password
/// there; nothing of ShareKeep's runs as root).
pub fn open_in_terminal(script: &Path) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(script)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            format!(
                "Terminal couldn't be opened ({e}). Open {} yourself.",
                script.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_screen_follows_the_start_up_table() {
        // hub here, EZ Start not finished: EZ Start, paired or not
        assert_eq!(first_page(true, false, false), PAGE_EZ);
        assert_eq!(first_page(true, false, true), PAGE_EZ);
        // paired: the app, hub or not
        assert_eq!(first_page(false, false, true), PAGE_HOME);
        assert_eq!(first_page(true, true, true), PAGE_HOME);
        // hub, EZ Start finished, not paired: the Hub tab
        assert_eq!(first_page(true, true, false), PAGE_HUB);
        // no hub, not paired: the choice, whatever EZ Start says
        assert_eq!(first_page(false, false, false), PAGE_CHOICE);
        assert_eq!(first_page(false, true, false), PAGE_CHOICE);
    }

    #[test]
    fn no_folder_is_no_hub_and_a_folder_without_its_key_is_said_so() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(find(None), HubFolder::None);
        assert_eq!(find(Some(&d.path().join("ShareKeep Hub"))), HubFolder::None);
        let data = d.path().join("ShareKeep Hub");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("hub.json"), r#"{"admin_port": 15999}"#).unwrap();
        if cfg!(windows) {
            // a folder an ordinary account made is never a hub there (review H2)
            assert!(
                matches!(find(Some(&data)), HubFolder::Unusable(why) if why.contains("can't use it"))
            );
            return;
        }
        assert!(
            matches!(find(Some(&data)), HubFolder::Unusable(why) if why.contains("may not read its key"))
        );
        std::fs::write(data.join("admin.token"), "tok\n").unwrap();
        assert_eq!(
            find(Some(&data)),
            HubFolder::Here((SocketAddr::from((Ipv4Addr::LOCALHOST, 15999)), "tok".into()))
        );
    }

    #[test]
    fn the_offer_depends_on_where_the_app_runs() {
        let d = tempfile::tempdir().unwrap();
        // a Mac app with the script in its Resources
        let app = d.path().join("ShareKeep.app").join("Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Resources")).unwrap();
        let exe = app.join("MacOS").join("sharekeep");
        assert!(matches!(offer_for(&exe, false, true), Offer::No(_)));
        std::fs::write(
            app.join("Resources").join("install-hub.command"),
            "#!/bin/bash\n",
        )
        .unwrap();
        assert_eq!(
            offer_for(&exe, false, true),
            Offer::Mac(app.join("Resources").join("install-hub.command"))
        );
        // Windows, not under Program Files (no such folder here): explained, not offered
        assert!(
            matches!(offer_for(&exe, true, false), Offer::No(why) if why.contains("Program Files"))
        );
        assert!(matches!(offer_for(&exe, false, false), Offer::No(_)));
    }
}
