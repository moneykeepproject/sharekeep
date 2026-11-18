//! `sharekeep-hub`: the household service.
//!
//! ```text
//! sharekeep-hub setup       --data <folder> --address <hub IPv4 | auto> [--name <local name>]
//! sharekeep-hub run         --data <folder> [--log <file>]
//! sharekeep-hub status      --data <folder>
//! sharekeep-hub install     --data <folder> --user <installing user>   (Windows, elevated;
//!                                                                     macOS, with sudo)
//! sharekeep-hub make-hub    --user <DOMAIN\user>   (Windows, elevated: prepare, setup the first
//!                                                  time, install; the folder is always
//!                                                  %ProgramData%\ShareKeep Hub; exit codes
//!                                                  in sk_client::makehub)
//! sharekeep-hub uninstall   [--data <folder>]
//! sharekeep-hub service-run --data <folder>                             (the Windows service)
//! ```
//!
//! Users never run this by hand: the installer sets it up and runs it as a service. The
//! commands are for the installer, support and tests.

use sk_client::makehub::{self, Exit};
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
    if cmd == "make-hub" {
        let exit = make_hub(&args);
        if exit != Exit::Done {
            let data = makehub::windows_data().unwrap_or_default();
            eprintln!("{}", exit.message(&data));
        }
        std::process::exit(i32::from(exit.code()));
    }
    let data = arg(&args, "--data")
        .map(PathBuf::from)
        .ok_or("--data <folder> is needed")?;
    match cmd {
        "setup" => {
            let address = arg(&args, "--address").ok_or("--address <hub IPv4> is needed")?;
            setup(&data, &address, arg(&args, "--name")).map_err(|(_, e)| e)
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
        "prepare" => {
            let ms = sk_hub::util::now_ms();
            let stamp = format!("{} {}", sk_ledger::day_of(ms), ms / 1000);
            service::prepare_data(&data, &stamp).map(|said| println!("{said}"))
        }
        "service-run" => service_run(data),
        _ => Err(
            "usage: sharekeep-hub <make-hub|prepare|setup|run|status|install|uninstall|service-run> --data <folder> …"
                .into(),
        ),
    }
}

/// `setup`: certificates and settings in `data`. The error says whether it was the network.
fn setup(data: &Path, address: &str, name: Option<String>) -> Result<(), (bool, String)> {
    let ip = if address == "auto" {
        service::detect_address().ok_or((
            true,
            "This computer doesn't seem to be on a home network. Connect it, then try again."
                .to_string(),
        ))?
    } else {
        wifi::check_address(address).map_err(|e| (false, e))?
    };
    if !wifi::address_present(ip) {
        return Err((true, format!(
            "This computer doesn't have the address {ip}. Give it a fixed address on the home network first (see the help: a DHCP reservation)."
        )));
    }
    let name = match name {
        Some(n) => wifi::check_name(&n).map_err(|e| (false, e))?,
        None => None,
    };
    let cfg = HubConfig::new(ip, name);
    let data = data.to_path_buf();
    let keys = KeyStore::files(data.clone());
    let info = sk_hub::setup(
        &data,
        &cfg,
        &keys,
        &wifi::computer_name(),
        sk_hub::util::now_ms(),
    )
    .map_err(|e| (false, e))?;
    println!("The hub is set up.");
    println!("Certificate: {}", info.ca_name);
    println!("Fingerprint: {}", info.fingerprint);
    println!(
        "Devices trust it from http://{}:{}/",
        cfg.address, cfg.helper_port
    );
    Ok(())
}

/// `make-hub`: everything Setup's hub section does, in one elevated program (0.1.4: the app's
/// "Make this computer the hub" and Setup both run it). The folder is never the caller's: an
/// ordinary program asking for the same prompt can't point it elsewhere.
fn make_hub(args: &[String]) -> Exit {
    if !cfg!(windows) || arg(args, "--data").is_some() {
        return Exit::BadArguments;
    }
    let Some(user) = arg(args, "--user").filter(|u| makehub::user_ok(u)) else {
        return Exit::BadArguments;
    };
    let Ok(exe) = std::env::current_exe() else {
        return Exit::BadArguments;
    };
    if !makehub::under(&exe, &makehub::program_files()) {
        return Exit::NotInProgramFiles;
    }
    let Some(data) = makehub::windows_data() else {
        return Exit::BadArguments;
    };
    let ms = sk_hub::util::now_ms();
    let stamp = format!("{} {}", sk_ledger::day_of(ms), ms / 1000);
    match service::prepare_data(&data, &stamp) {
        Ok(said) => println!("{said}"),
        Err(e) => {
            eprintln!("{e}");
            return Exit::FolderInTheWay;
        }
    }
    // the first time only: an existing hub keeps its certificate (devices stay paired)
    if !data.join("hub.json").exists() {
        if let Err((network, e)) = setup(&data, "auto", None) {
            eprintln!("{e}");
            return if network {
                Exit::NoHomeNetwork
            } else {
                Exit::SetupFailed
            };
        }
    }
    if let Err(e) = install(&data, Some(user)) {
        eprintln!("{e}");
        return Exit::ServiceDidntStart;
    }
    // what Setup writes too: later silent updates find the hub's folder here
    if let Err(e) = service::run_steps(&[service::hub_data_registry(&data)], &echo) {
        eprintln!("{e}");
    }
    Exit::Done
}

fn echo(line: &str) {
    println!("{line}");
}

/// On a Mac, ShareKeep's own program never runs as root (review H3): the disk image's two
/// scripts place and remove launchd's file themselves.
const MAC_INSTALL: &str = "On a Mac, open \"Install the household hub\" (or \"Stop the household hub\") from the ShareKeep disk image: they register the hub with launchd without running ShareKeep as root.";

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
        let _ = (exe, user);
        Err(MAC_INSTALL.into())
    } else {
        Err("On Linux, run `sharekeep-hub run --data <folder>` from a systemd unit.".into())
    }
}

fn uninstall(_data: Option<&Path>) -> Result<(), String> {
    if cfg!(windows) {
        service::run_steps(&service::windows_uninstall(), &echo)
    } else if cfg!(target_os = "macos") {
        Err(MAC_INSTALL.into())
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
