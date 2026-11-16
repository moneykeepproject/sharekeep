//! `sharekeep-hub`: the household service.
//!
//! ```text
//! sharekeep-hub setup  --data <folder> --address <hub IPv4> [--name <local name>]
//! sharekeep-hub run    --data <folder>
//! sharekeep-hub status --data <folder>
//! ```
//!
//! Users never run this by hand: the installer sets it up and runs it as a service. The
//! commands are for the installer, support and tests.

use sk_hub::{wifi, HubConfig, HubOptions, KeyStore};
use std::path::PathBuf;
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
    let data = arg(&args, "--data")
        .map(PathBuf::from)
        .ok_or("--data <folder> is needed")?;
    match cmd {
        "setup" => {
            let address = arg(&args, "--address").ok_or("--address <hub IPv4> is needed")?;
            let ip = wifi::check_address(&address)?;
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
            let running = sk_hub::start(HubOptions::new(data))?;
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
        _ => Err("usage: sharekeep-hub <setup|run|status> --data <folder> …".into()),
    }
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
