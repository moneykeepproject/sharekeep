//! `sharekeep-hub`: the household service.
//!
//! ```text
//! sharekeep-hub setup       --data <folder> --address <hub IPv4 | auto> [--name <local name>]
//! sharekeep-hub run         --data <folder> [--log <file>]
//! sharekeep-hub status      --data <folder>
//! sharekeep-hub install     --data <folder> --user <installing user>   (Windows, elevated;
//!                                                                     macOS, with sudo)
//! sharekeep-hub uninstall   [--data <folder>]
//! sharekeep-hub service-run --data <folder>                             (the Windows service)
//! ```
//!
//! Users never run this by hand: the installer sets it up and runs it as a service. The
//! commands are for the installer, support and tests.

use sk_hub::{service, wifi, HubConfig, HubOptions, KeyStore};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if cmd == "uninstall" {
        return uninstall(arg(&args, "--data").map(PathBuf::from).as_deref());
    }
    let data = arg(&args, "--data")
        .map(PathBuf::from)
        .ok_or("--data <folder> is needed")?;
    match cmd {
        "setup" => {
            let address = arg(&args, "--address").ok_or("--address <hub IPv4> is needed")?;
            let ip = if address == "auto" {
                service::detect_address().ok_or(
                    "This computer doesn't seem to be on a home network. Connect it, then try again.",
                )?
            } else {
                wifi::check_address(&address)?
            };
            if !wifi::address_present(ip) {
                return Err(format!(
                    "This computer doesn't have the address {ip}. Give it a fixed address on the home network first (see the help: a DHCP reservation)."
                ));
            }
            let name = match arg(&args, "--name") {
                Some(n) => wifi::check_name(&n)?,
                None => None,
            };
            let cfg = HubConfig::new(ip, name);
            let keys = KeyStore::files(data.clone());
            let info = sk_hub::setup(
                &data,
                &cfg,
                &keys,
                &wifi::computer_name(),
                sk_hub::util::now_ms(),
            )?;
            println!("The hub is set up.");
            println!("Certificate: {}", info.ca_name);
            println!("Fingerprint: {}", info.fingerprint);
            println!(
                "Devices trust it from http://{}:{}/",
                cfg.address, cfg.helper_port
            );
            Ok(())
        }
        "run" => {
            let mut opts = HubOptions::new(data);
            if let Some(log) = arg(&args, "--log") {
                opts.log = service::file_log(PathBuf::from(log));
            }
            let running = sk_hub::start(opts)?;
            eprintln!(
                "ShareKeep hub running: devices https://{}  certificate page http://{}  this computer http://{}",
                running.https, running.helper, running.admin
            );
            running.wait();
            Ok(())
        }
        "status" => {
            let cfg = HubConfig::load(&data)?;
            let info = wifi::read_info(&data).unwrap_or_default();
            println!(
                "{}",
                serde_json::json!({"config": cfg, "certificate": info})
            );
            Ok(())
        }
        "install" => install(&data, arg(&args, "--user")),
        "service-run" => service_run(data),
        _ => Err(
            "usage: sharekeep-hub <setup|run|status|install|uninstall|service-run> --data <folder> …"
                .into(),
        ),
    }
}

fn echo(line: &str) {
    println!("{line}");
}

/// Register the hub to run in the background and start it.
fn install(data: &Path, user: Option<String>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cfg = HubConfig::load(data)?;
    if cfg!(windows) {
        let user = user
            .or_else(|| std::env::var("USERNAME").ok())
            .ok_or("--user <installing user> is needed")?;
        let steps = service::windows_install(&exe, data, &user, (cfg.https_port, cfg.helper_port));
        service::run_steps(&steps, &echo)
    } else if cfg!(target_os = "macos") {
        install_mac(&exe, data, user)
    } else {
        Err("On Linux, run `sharekeep-hub run --data <folder>` from a systemd unit.".into())
    }
}

#[cfg(unix)]
fn install_mac(exe: &Path, data: &Path, user: Option<String>) -> Result<(), String> {
    // run with sudo by the person installing: the daemon runs as them
    let user = user
        .or_else(|| std::env::var("SUDO_USER").ok())
        .ok_or("--user <installing user> is needed (or run it with sudo)")?;
    let plist = service::macos_plist(exe, data, &user);
    let path = service::macos_plist_path();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("system/{}", service::LAUNCHD_LABEL)])
        .output();
    std::fs::write(&path, plist)
        .map_err(|e| format!("{path} can't be written ({e}). Use sudo."))?;
    // the data folder and everything in it belong to the user, owner-only
    if let (Ok(uid), Ok(gid)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) {
        let (uid, gid) = (uid.parse().ok(), gid.parse().ok());
        chown_tree(data, uid, gid)?;
    }
    service::run_steps(
        &[vec![
            "launchctl".into(),
            "bootstrap".into(),
            "system".into(),
            path.clone(),
        ]],
        &echo,
    )
}

#[cfg(unix)]
fn chown_tree(p: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<(), String> {
    std::os::unix::fs::chown(p, uid, gid).map_err(|e| format!("{}: {e}", p.display()))?;
    if p.is_dir() {
        for e in std::fs::read_dir(p).map_err(|e| e.to_string())?.flatten() {
            chown_tree(&e.path(), uid, gid)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_mac(_: &Path, _: &Path, _: Option<String>) -> Result<(), String> {
    Err("not a Mac".into())
}

fn uninstall(_data: Option<&Path>) -> Result<(), String> {
    if cfg!(windows) {
        service::run_steps(&service::windows_uninstall(), &echo)
    } else if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("system/{}", service::LAUNCHD_LABEL)])
            .output();
        match std::fs::remove_file(service::macos_plist_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{e}. Use sudo.")),
        }
    } else {
        Ok(())
    }
}

#[cfg(windows)]
mod winsvc {
    use sk_hub::{service, HubOptions};
    use std::path::PathBuf;
    use std::sync::{mpsc, OnceLock};
    use std::time::Duration;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    pub static DATA: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    fn status(state: ServiceState, code: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::ServiceSpecific(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        }
    }

    fn service_main(_arguments: Vec<std::ffi::OsString>) {
        let Some(data) = DATA.get().cloned() else {
            return;
        };
        let log = service::file_log(data.join(service::LOG_FILE));
        let (tx, rx) = mpsc::channel();
        let handler = move |c: ServiceControl| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(handle) = service_control_handler::register(service::SERVICE_NAME, handler) else {
            log("the service control handler couldn't be registered");
            return;
        };
        let mut opts = HubOptions::new(data);
        opts.log = log.clone();
        match sk_hub::start(opts) {
            Ok(running) => {
                log(&format!("started: devices https://{}", running.https));
                let _ = handle.set_service_status(status(ServiceState::Running, 0));
                let _ = rx.recv();
                running.stop();
                log("stopped");
                let _ = handle.set_service_status(status(ServiceState::Stopped, 0));
            }
            Err(e) => {
                log(&format!("couldn't start: {e}"));
                let _ = handle.set_service_status(status(ServiceState::Stopped, 1));
            }
        }
    }

    pub fn run(data: PathBuf) -> Result<(), String> {
        let _ = DATA.set(data);
        service_dispatcher::start(service::SERVICE_NAME, ffi_service_main)
            .map_err(|e| format!("not started by Windows' service manager ({e})"))
    }
}

#[cfg(windows)]
fn service_run(data: PathBuf) -> Result<(), String> {
    winsvc::run(data)
}

#[cfg(not(windows))]
fn service_run(_data: PathBuf) -> Result<(), String> {
    Err("service-run is for Windows; elsewhere use `run`.".into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
