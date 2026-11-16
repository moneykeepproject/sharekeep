//! ShareKeep's household service (the hub).
//!
//! One computer in the household runs it. It keeps the books (in its own folder: creating a
//! book never opens a file dialog), is the only writer to them, and lets paired devices on
//! the home Wi-Fi read and add records over HTTPS. See [`server`].
//!
//! Copied from MoneyKeep's capture server: the certificate authority, server certificate,
//! TLS relay and certificate page ([`wifi`]) and the PIN hashing ([`pin`]). ShareKeep keeps
//! its own service, ports and certificate, separate from MoneyKeep's.

pub mod config;
pub mod keys;
pub mod pin;
pub mod registry;
pub mod server;
pub mod util;
pub mod wifi;

pub use config::HubConfig;
pub use keys::KeyStore;
pub use server::{admin_token, setup, start, HubOptions, Running};
