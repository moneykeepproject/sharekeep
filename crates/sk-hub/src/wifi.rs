//! The hub's Wi-Fi listener (copied from MoneyKeep's capture server, `wifi.rs`): devices
//! on the home network reach the household service directly, over HTTPS, with no
//! Cloudflare account. ShareKeep has its own certificate authority, keys and ports, so a
//! PC running both MoneyKeep's capture server and the ShareKeep hub keeps them apart.
//!
//! - **The certificate authority (CA)** made on this computer: ECDSA P-256, valid 10
//!   years, "ShareKeep Hub CA (<computer name>)", basicConstraints CA (no
//!   intermediates), key usage certificate signing only, and **name constraints**
//!   (RFC 5280 section 4.2.1.10) permitting only the chosen IP address (and the local
//!   name when there is one; otherwise the reserved name `invalid`, RFC 2606, so no
//!   DNS name passes) and excluding every IPv6 address, so trusting it on a phone
//!   cannot vouch for any other site.
//! - **The server certificate**: P-256, valid 825 days from one day before issue
//!   (small clock differences), subjectAltName the IP address and the local name;
//!   renewed by the server 30 days before it ends, with the same CA.
//! - **The keys** in the hub's key store ([`crate::keys`]).
//! - **The HTTPS listener** on `<address>:15443`: TLS (rustls with ring) in front of
//!   the request handling, which runs on a private loopback port; every request but
//!   pairing needs a device token.
//! - **The helper** on `<address>:15080`, plain HTTP: the certificate page `/`, the CA
//!   certificate `/sharekeep-ca.crt` (Android, computers) and
//!   `/sharekeep-ca.mobileconfig` (iPhone/iPad, a
//!   configuration profile with a `com.apple.security.root` payload, Apple
//!   Configuration Profile Reference); everything else redirects to HTTPS.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, SanType, SerialNumber,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::keys::KeyStore as Secrets;
use crate::util::{self as store, StoreResult};

pub const CA_CERT_FILE: &str = "hub-ca.crt";
pub const SERVER_CERT_FILE: &str = "hub-server.crt";
pub const INFO_FILE: &str = "hub-wifi.json";
/// The names of the two private keys ([`Secrets`]).
pub const CA_KEY: &str = "hub-ca-key";
pub const TLS_KEY: &str = "hub-tls-key";

const DAY: i64 = 86_400;
pub const CA_DAYS: i64 = 3652;
pub const SERVER_DAYS: i64 = 825;
pub const RENEW_DAYS: i64 = 30;
/// Phones connected at once over Wi-Fi (each connection has a relay thread).
const MAX_CONNECTIONS: usize = 64;
const IDLE: Duration = Duration::from_secs(120);

/// The address phones use: the IPv4 address and the optional local name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub name: Option<String>,
}

impl Target {
    /// The host phones open: the local name when there is one, else the IP address.
    pub fn host(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.ip.to_string())
    }
}

/// A local name as typed: lower case, letters, digits, dashes and dots, with at
/// least one dot (e.g. `sharekeep.home.arpa`, RFC 8375).
pub fn check_name(name: &str) -> Result<Option<String>, String> {
    let n = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if n.is_empty() {
        return Ok(None);
    }
    let ok = n.len() <= 253
        && n.contains('.')
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && n.parse::<IpAddr>().is_err();
    if ok {
        Ok(Some(n))
    } else {
        Err(format!(
            "\"{}\" is not a local name such as sharekeep.home.arpa.",
            name.trim()
        ))
    }
}

/// An IPv4 address of a private network (RFC 1918) or link-local, as the hub
/// needs.
pub fn check_address(text: &str) -> Result<Ipv4Addr, String> {
    let ip: Ipv4Addr = text.trim().parse().map_err(|_| {
        format!(
            "\"{}\" is not an IPv4 address such as 192.168.1.20.",
            text.trim()
        )
    })?;
    // Loopback addresses serve only this computer (tests, or trying it out).
    if ip.is_private() || ip.is_link_local() || ip.is_loopback() {
        Ok(ip)
    } else {
        Err(format!(
            "{ip} is not a home or office network address (10.x, 172.16–31.x or 192.168.x)."
        ))
    }
}

/// This computer has `ip` (binding a socket to it works only then).
pub fn address_present(ip: Ipv4Addr) -> bool {
    std::net::UdpSocket::bind((ip, 0)).is_ok()
}

/// A port is free on `ip`: `Ok`, or the message naming it with the next free one.
pub fn check_port(ip: Ipv4Addr, port: u16, other: u16) -> Result<(), String> {
    if TcpListener::bind((ip, port)).is_ok() {
        return Ok(());
    }
    let next = (port.saturating_add(1)..=65535)
        .find(|p| *p != other && TcpListener::bind((ip, *p)).is_ok());
    Err(match next {
        Some(n) => format!(
            "Port {port} is already used by another program on this computer. Use port {n} instead."
        ),
        None => format!("Port {port} is already used by another program on this computer."),
    })
}

/// The computer's name for the CA's name.
pub fn computer_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    clean_computer(&raw)
}

fn clean_computer(raw: &str) -> String {
    let c: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        .take(40)
        .collect();
    if c.trim().is_empty() {
        "this computer".to_owned()
    } else {
        c.trim().to_owned()
    }
}

/// What was made, kept next to the certificates (public facts only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Info {
    pub ca_name: String,
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
    pub ca_not_before: i64,
    pub ca_not_after: i64,
    pub cert_not_before: i64,
    pub cert_not_after: i64,
    pub fingerprint: String,
}

/// The CA's SHA-256 fingerprint as 8 groups of 4 upper-case hex digits (its first
/// 128 bits), for comparing by eye with what the phone shows.
pub fn fingerprint(der: &[u8]) -> String {
    let h = Sha256::digest(der);
    h.get(..16)
        .unwrap_or(&[])
        .chunks(2)
        .map(|c| store::hex(c).to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn when(secs: i64) -> Result<time::OffsetDateTime, String> {
    time::OffsetDateTime::from_unix_timestamp(secs).map_err(|e| e.to_string())
}

fn serial() -> StoreResult<SerialNumber> {
    let mut b = store::random::<16>()?;
    if let Some(first) = b.first_mut() {
        *first &= 0x7f;
    }
    Ok(SerialNumber::from_slice(&b))
}

fn rc(e: rcgen::Error) -> String {
    format!("the certificate could not be made: {e}")
}

/// The CA's settings (also rebuilt to sign a renewal: the issuer's name, key
/// identifier method and key usages come from here).
pub fn ca_params(
    ca_name: &str,
    target: &Target,
    not_before: i64,
) -> Result<CertificateParams, String> {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, ca_name);
    dn.push(DnType::OrganizationName, "ShareKeep");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    p.not_before = when(not_before)?;
    p.not_after = when(not_before.saturating_add(CA_DAYS * DAY))?;
    let dns = target.name.clone().unwrap_or_else(|| "invalid".to_owned());
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix(target.ip.octets(), 32)),
            GeneralSubtree::DnsName(dns),
        ],
        excluded_subtrees: vec![GeneralSubtree::IpAddress(CidrSubnet::from_v6_prefix(
            [0; 16], 0,
        ))],
    });
    Ok(p)
}

/// A new CA: its certificate (DER) and private key (PKCS#8).
pub fn new_ca(
    ca_name: &str,
    target: &Target,
    now: i64,
) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), String> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(rc)?;
    let mut p = ca_params(ca_name, target, now - DAY)?;
    p.serial_number = Some(serial()?);
    let cert = p.self_signed(&key).map_err(rc)?;
    Ok((cert.der().to_vec(), Zeroizing::new(key.serialize_der())))
}

/// A new server certificate signed by the CA: its certificate (DER) and private key.
pub fn new_server_cert(
    ca_name: &str,
    ca_target: &Target,
    ca_not_before: i64,
    ca_key: &[u8],
    target: &Target,
    now: i64,
) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), String> {
    let ca_pair = KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(ca_key),
        &rcgen::PKCS_ECDSA_P256_SHA256,
    )
    .map_err(rc)?;
    let issuer = Issuer::new(ca_params(ca_name, ca_target, ca_not_before)?, ca_pair);
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(rc)?;
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, target.host());
    p.distinguished_name = dn;
    p.subject_alt_names = vec![SanType::IpAddress(IpAddr::V4(target.ip))];
    if let Some(n) = &target.name {
        p.subject_alt_names
            .push(SanType::DnsName(n.clone().try_into().map_err(rc)?));
    }
    p.is_ca = IsCa::ExplicitNoCa;
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.use_authority_key_identifier_extension = true;
    p.serial_number = Some(serial()?);
    let start = now - DAY;
    p.not_before = when(start)?;
    p.not_after = when(start.saturating_add(SERVER_DAYS * DAY))?;
    let cert = p.signed_by(&key, &issuer).map_err(rc)?;
    Ok((cert.der().to_vec(), Zeroizing::new(key.serialize_der())))
}

/// PEM text of a certificate.
pub fn pem(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// The DER of PEM text (one certificate).
pub fn from_pem(text: &str) -> Option<Vec<u8>> {
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .concat();
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .ok()
}

/// The certificates and the server key in use.
pub struct Material {
    pub info: Info,
    pub ca_der: Vec<u8>,
    pub cert_der: Vec<u8>,
    pub key: Zeroizing<Vec<u8>>,
    /// A new CA was made by this call.
    pub ca_created: bool,
    /// A new server certificate was made by this call.
    pub renewed: bool,
}

impl std::fmt::Debug for Material {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Material")
            .field("info", &self.info)
            .field("key", &"(hidden)")
            .finish_non_exhaustive()
    }
}

pub fn read_info(dir: &Path) -> Option<Info> {
    std::fs::read(dir.join(INFO_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn read_cert(dir: &Path, file: &str) -> Option<Vec<u8>> {
    std::fs::read_to_string(dir.join(file))
        .ok()
        .and_then(|t| from_pem(&t))
}

/// Make what is missing for `target` and renew what is due: a new CA when there is
/// none, it ends, or the address (its name constraints) changed; a new server
/// certificate with a new CA, a changed address, or within 30 days of its end.
///
/// Hub setup only (it may make a new certificate authority, which every device must
/// install again); the server itself uses [`ensure_existing`].
pub fn ensure(
    dir: &Path,
    secrets: &Secrets,
    target: &Target,
    computer: &str,
    now: i64,
) -> Result<Material, String> {
    ensure_with(dir, secrets, target, computer, now, true)
}

/// The hub's damaged certificate files, as its status says it.
pub const DAMAGED: &str = "The ShareKeep hub can't start: its certificate files are damaged";

/// What the server uses at start and when renewing: the certificate
/// authority is never replaced here. When `wifi.json`, the CA certificate or the CA
/// key can't be read (or they are for another address, or the CA ended), [`DAMAGED`]
/// (with why) and nothing is written; the server certificate is renewed when due.
pub fn ensure_existing(
    dir: &Path,
    secrets: &Secrets,
    target: &Target,
    now: i64,
) -> Result<Material, String> {
    ensure_with(dir, secrets, target, "", now, false)
}

fn ensure_with(
    dir: &Path,
    secrets: &Secrets,
    target: &Target,
    computer: &str,
    now: i64,
    may_make_ca: bool,
) -> Result<Material, String> {
    let old = read_info(dir);
    let ca_key = match secrets.get(CA_KEY) {
        Ok(k) => k,
        // Setup replaces a key that can't be opened; the server never does.
        Err(_) if may_make_ca => None,
        Err(e) => return Err(format!("{DAMAGED} ({e})")),
    };
    let ca_der = read_cert(dir, CA_CERT_FILE);
    let same_place = |i: &Info| i.address == target.ip.to_string() && i.name == target.name;
    let (mut info, ca_der, ca_key, ca_created) = match (old, ca_key, ca_der) {
        (Some(i), Some(k), Some(d)) if same_place(&i) && now < i.ca_not_after - DAY => {
            (i, d, k, false)
        }
        (old, key, der) if !may_make_ca => {
            let why = match (old, key, der) {
                (None, ..) => format!("{INFO_FILE} can't be read"),
                (_, None, _) => "the certificate authority's key is missing".to_owned(),
                (_, _, None) => format!("{CA_CERT_FILE} can't be read"),
                (Some(i), ..) if !same_place(&i) => {
                    format!("the certificate is for {}, not {}", i.address, target.ip)
                }
                _ => "the certificate authority has ended".to_owned(),
            };
            return Err(format!("{DAMAGED} ({why})"));
        }
        _ => {
            let ca_name = format!("ShareKeep Hub CA ({})", clean_computer(computer));
            let (der, key) = new_ca(&ca_name, target, now)?;
            secrets.set(CA_KEY, &key)?;
            store::write_private(&dir.join(CA_CERT_FILE), pem(&der).as_bytes())?;
            let info = Info {
                ca_name,
                address: target.ip.to_string(),
                name: target.name.clone(),
                ca_not_before: now - DAY,
                ca_not_after: now - DAY + CA_DAYS * DAY,
                cert_not_before: 0,
                cert_not_after: 0,
                fingerprint: fingerprint(&der),
            };
            (info, der, key, true)
        }
    };
    let cert = read_cert(dir, SERVER_CERT_FILE);
    // A damaged server key is simply made again (phones trust the CA, not it).
    let tls = secrets.get(TLS_KEY).ok().flatten();
    let due = now >= info.cert_not_after - RENEW_DAYS * DAY;
    let (cert_der, key, renewed) = match (cert, tls) {
        (Some(c), Some(k)) if !ca_created && !due => (c, k, false),
        _ => {
            let ca_target = Target {
                ip: info
                    .address
                    .parse()
                    .map_err(|_| "wifi.json is damaged".to_owned())?,
                name: info.name.clone(),
            };
            let (der, key) = new_server_cert(
                &info.ca_name,
                &ca_target,
                info.ca_not_before,
                &ca_key,
                target,
                now,
            )?;
            secrets.set(TLS_KEY, &key)?;
            store::write_private(&dir.join(SERVER_CERT_FILE), pem(&der).as_bytes())?;
            info.cert_not_before = now - DAY;
            info.cert_not_after = now - DAY + SERVER_DAYS * DAY;
            (der, key, true)
        }
    };
    if ca_created || renewed {
        let json = serde_json::to_vec_pretty(&info).map_err(|e| e.to_string())?;
        store::write_private(&dir.join(INFO_FILE), &json)?;
    }
    Ok(Material {
        info,
        ca_der,
        cert_der,
        key,
        ca_created,
        renewed,
    })
}

/// Remove the certificates and keys.
pub fn remove(dir: &Path, secrets: &Secrets) -> Vec<String> {
    let mut done = Vec::new();
    for f in [CA_CERT_FILE, SERVER_CERT_FILE, INFO_FILE] {
        if std::fs::remove_file(dir.join(f)).is_ok() {
            done.push(format!("{f} removed"));
        }
    }
    for k in [CA_KEY, TLS_KEY] {
        if secrets.delete(k).is_ok() {
            done.push(format!("{k} removed"));
        }
    }
    done
}

// ----- the iPhone profile and the certificate page ------------------------------------------

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A UUID (version 4 form) from 16 bytes.
fn uuid(b: [u8; 16]) -> String {
    let h = store::hex(&b).to_ascii_uppercase();
    let p = |a: usize, z: usize| h.get(a..z).unwrap_or("").to_owned();
    let v = format!("4{}", h.get(13..16).unwrap_or(""));
    format!("{}-{}-{}-{}-{}", p(0, 8), p(8, 12), v, p(16, 20), p(20, 32))
}

/// The iPhone/iPad configuration profile carrying the CA certificate (payload type
/// `com.apple.security.root`). The identifiers come from the certificate, so the
/// same CA always gives the same profile.
pub fn mobileconfig(ca_der: &[u8], ca_name: &str) -> String {
    let h = Sha256::digest(ca_der);
    let mut a = [0u8; 16];
    let mut b = [0u8; 16];
    a.copy_from_slice(h.get(..16).unwrap_or(&[0; 16]));
    b.copy_from_slice(h.get(16..32).unwrap_or(&[0; 16]));
    let data = base64::engine::general_purpose::STANDARD.encode(ca_der);
    let id = store::hex(h.get(..6).unwrap_or(&[]));
    let name = xml(ca_name);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key>
      <string>sharekeep-ca.cer</string>
      <key>PayloadContent</key>
      <data>{data}</data>
      <key>PayloadDescription</key>
      <string>Lets this iPhone or iPad trust the ShareKeep household hub on its Wi-Fi address only.</string>
      <key>PayloadDisplayName</key>
      <string>{name}</string>
      <key>PayloadIdentifier</key>
      <string>com.sharekeep.hub.ca.{id}.cert</string>
      <key>PayloadType</key>
      <string>com.apple.security.root</string>
      <key>PayloadUUID</key>
      <string>{u1}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDescription</key>
  <string>Installs the certificate of the ShareKeep household hub. It is valid only for the hub computer's Wi-Fi address.</string>
  <key>PayloadDisplayName</key>
  <string>{name}</string>
  <key>PayloadIdentifier</key>
  <string>com.sharekeep.hub.ca.{id}</string>
  <key>PayloadRemovalDisallowed</key>
  <false/>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadUUID</key>
  <string>{u2}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
</dict>
</plist>
"#,
        u1 = uuid(a),
        u2 = uuid(b),
    )
}

/// Which phone asked (by its browser's User-Agent): iPhone/iPad or not.
pub fn is_apple(user_agent: &str) -> bool {
    ["iPhone", "iPad", "iPod"]
        .iter()
        .any(|k| user_agent.contains(k))
}

/// The certificate page's steps for a phone: the shared copy in `sk_view::phone` (the desktop's
/// EZ Start shows the same), and how to go on.
fn steps(title: &str, list: &[&str], pairing: bool) -> String {
    let mut items: String = list
        .iter()
        .map(|s| format!("<li>{}</li>", sk_view::phone::html(s)))
        .collect();
    if !pairing {
        items.push_str(&format!(
            "<li>{}</li>",
            sk_view::phone::html(sk_view::phone::THEN_PAIR)
        ));
    }
    format!("<section><h2>{title}</h2><ol>{items}</ol></section>")
}

/// The certificate-install page (helper port `/`): the fingerprint, both download
/// buttons and the steps for the phone that asked first, then the other.
pub fn install_page(info: &Info, apple_first: bool, pair: Option<&str>) -> String {
    // step 2, when the page was opened from a pairing QR code (the code is 8 letters and
    // digits; anything else is ignored, never echoed)
    let pair = pair.filter(|c| c.len() == 8 && c.bytes().all(|b| b.is_ascii_alphanumeric()));
    let step2 = pair
        .map(|c| {
            let shown = format!("{}-{}", &c[..4], &c[4..]);
            format!(
                r#"<div class="card"><h2>Step 2: join the household</h2>
<p>After the certificate is installed, open the household page:</p>
<a class="button" href="https://{{HOST}}:{{HTTPS_PORT}}/#pair={c}">Open ShareKeep</a>
<p class="muted">On a computer with the ShareKeep app, type this pairing code instead: <b>{shown}</b>. It works once, for 10 minutes.</p></div>"#
            )
        })
        .unwrap_or_default();
    let android = steps(
        "Android phone or tablet",
        &sk_view::phone::ANDROID,
        pair.is_some(),
    );
    let apple = steps("iPhone or iPad", &sk_view::phone::APPLE, pair.is_some());
    let (first, second) = if apple_first {
        (apple, android)
    } else {
        (android, apple)
    };
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>ShareKeep hub — certificate</title>
<style>
:root {{ --bg:#f1f5f4; --card:#ffffff; --text:#1e293b; --muted:#64748b; --accent:#0f766e; --line:#e2e8f0; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#15181c; --card:#20252b; --text:#eef1f4; --muted:#aab3bd; --accent:#5eead4; --line:#3a424b; }} }}
body {{ margin:0; background:var(--bg); color:var(--text); font:17px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }}
main {{ max-width:480px; margin:0 auto; padding:16px; }}
h1 {{ font-size:22px; margin:8px 0 4px; }} h2 {{ font-size:18px; margin:20px 0 6px; }}
.card {{ background:var(--card); border:1px solid var(--line); border-radius:12px; padding:14px 16px; margin:12px 0; }}
.muted {{ color:var(--muted); font-size:15px; }}
.fp {{ font:600 17px/1.6 ui-monospace, "SF Mono", Menlo, Consolas, monospace; letter-spacing:.5px; word-spacing:4px; }}
a.button {{ display:block; text-align:center; text-decoration:none; background:var(--accent); color:#fff; font-weight:600; border-radius:10px; padding:14px; margin:10px 0; min-height:24px; }}
@media (prefers-color-scheme: dark) {{ a.button {{ color:#0d1117; }} }}
ol {{ padding-left:22px; }} li {{ margin:6px 0; }}
</style></head>
<body><main>
<h1>ShareKeep household hub</h1>
<p class="muted">Step 1 of 2 on this device: trust the hub's certificate, so ShareKeep can open securely at <b>https://{host}:{port}</b>.</p>
<div class="card">
<div class="muted">Certificate</div><div><b>{ca}</b></div>
<div class="muted" style="margin-top:8px">Fingerprint (SHA-256, first 32 digits) — it must match the one ShareKeep shows on the hub computer:</div>
<div class="fp">{fp}</div>
<a class="button" href="{crt}">Download certificate (Android)</a>
<a class="button" href="{mc}">Download certificate (iPhone or iPad)</a>
<p class="muted">The certificate is only valid for the hub computer's address ({ip}{name}). It cannot be used for any other website.</p>
</div>
{step2}
<div class="card">{first}</div>
<div class="card">{second}</div>
<p class="muted">Already installed? Open <a href="https://{host}:{port}/">https://{host}:{port}/</a>.</p>
</main></body></html>
"#,
        host = xml(&info.name.clone().unwrap_or_else(|| info.address.clone())),
        port = "{HTTPS_PORT}",
        step2 = step2.replace(
            "{HOST}",
            &xml(&info.name.clone().unwrap_or_else(|| info.address.clone()))
        ),
        ca = xml(&info.ca_name),
        fp = xml(&info.fingerprint),
        crt = CA_CRT_PATH,
        mc = CA_MOBILECONFIG_PATH,
        ip = xml(&info.address),
        name = info
            .name
            .as_ref()
            .map(|n| format!(" and {}", xml(n)))
            .unwrap_or_default(),
    )
}

/// The certificate page for a phone, with the HTTPS port filled in.
pub fn page_for(info: &Info, https_port: u16, user_agent: &str, pair: Option<&str>) -> String {
    install_page(info, is_apple(user_agent), pair).replace("{HTTPS_PORT}", &https_port.to_string())
}

/// Headers of the helper's answers (no scripts at all on these pages).
/// The CA certificate for Android and computers.
pub const CA_CRT_PATH: &str = "/sharekeep-ca.crt";
/// The configuration profile for iPhone and iPad.
pub const CA_MOBILECONFIG_PATH: &str = "/sharekeep-ca.mobileconfig";

pub const HELPER_HEADERS: [(&str, &str); 4] = [
    (
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("X-Frame-Options", "DENY"),
];

// ----- the listeners ----------------------------------------------------------------------------

/// The server certificate in use; a renewal swaps it without dropping phones.
#[derive(Debug)]
pub struct Resolver {
    current: RwLock<Arc<rustls::sign::CertifiedKey>>,
}

fn certified(m: &Material) -> Result<Arc<rustls::sign::CertifiedKey>, String> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(m.key.to_vec()));
    let signer = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| format!("the server key cannot be used: {e}"))?;
    Ok(Arc::new(rustls::sign::CertifiedKey::new(
        vec![CertificateDer::from(m.cert_der.clone())],
        signer,
    )))
}

impl Resolver {
    pub fn new(m: &Material) -> Result<Self, String> {
        Ok(Self {
            current: RwLock::new(certified(m)?),
        })
    }
    pub fn set(&self, m: &Material) -> Result<(), String> {
        let c = certified(m)?;
        if let Ok(mut cur) = self.current.write() {
            *cur = c;
        }
        Ok(())
    }
}

impl rustls::server::ResolvesServerCert for Resolver {
    fn resolve(
        &self,
        _hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.current.read().ok().map(|c| c.clone())
    }
}

/// The TLS settings of the Wi-Fi listener.
pub fn server_config(resolver: Arc<Resolver>) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut c = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_no_client_auth()
    .with_cert_resolver(resolver);
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(c))
}

/// One phone connection on the Wi-Fi listener, by the relay's local port on the
/// loopback connection to the request handler.
#[derive(Debug)]
pub struct Peer {
    /// The phone's address (so failure counting sees the phone, not the relay).
    pub ip: IpAddr,
    /// The phone's side of the connection, to close it.
    stream: TcpStream,
    /// Close the connection at this time (an unauthenticated
    /// request's body must arrive within [`UNAUTH_BODY_TIMEOUT`]).
    deadline: Option<Instant>,
}

/// The phone connections being relayed.
pub type Peers = Arc<Mutex<HashMap<u16, Peer>>>;

/// How long an unauthenticated request may take to send its body.
pub const UNAUTH_BODY_TIMEOUT: Duration = Duration::from_secs(10);
/// Unauthenticated requests with a body (pairing) handled at once, in all: fewer than the
/// request handler's workers, so paired devices always get one (review M1).
pub const MAX_UNAUTH: usize = 2;
/// Unauthenticated requests handled at once from one address.
pub const MAX_UNAUTH_PER_ADDRESS: usize = 1;
/// Connections at once from one address, on each listener.
pub const MAX_PER_ADDRESS: usize = 8;
/// The TLS handshake must be done in this time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The phone's address on the connection relayed from `port`.
pub fn peer_ip(peers: &Peers, port: u16) -> Option<IpAddr> {
    peers.lock().ok().and_then(|m| m.get(&port).map(|p| p.ip))
}

/// Close the connection relayed from `port` now.
pub fn close_peer(peers: &Peers, port: u16) {
    if let Ok(m) = peers.lock() {
        if let Some(p) = m.get(&port) {
            let _ = p.stream.shutdown(Shutdown::Both);
        }
    }
}

/// Close the connection relayed from `port` at `deadline` (or never, with `None`).
pub fn set_deadline(peers: &Peers, port: u16, deadline: Option<Instant>) {
    if let Ok(mut m) = peers.lock() {
        if let Some(p) = m.get_mut(&port) {
            p.deadline = deadline;
        }
    }
}

/// Close the connections whose deadline has passed; how many were closed.
pub fn sweep(peers: &Peers) -> usize {
    let now = Instant::now();
    let Ok(mut m) = peers.lock() else {
        return 0;
    };
    let mut n = 0;
    for p in m.values_mut() {
        if p.deadline.is_some_and(|d| now >= d) {
            let _ = p.stream.shutdown(Shutdown::Both);
            p.deadline = None;
            n += 1;
        }
    }
    n
}

/// Admits unauthenticated requests: at most [`MAX_UNAUTH`] at a time, and
/// [`MAX_UNAUTH_PER_ADDRESS`] from one address.
#[derive(Debug, Default)]
pub struct Gate {
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// An admitted unauthenticated request; its place is given back when dropped.
#[derive(Debug)]
pub struct Ticket {
    gate: Arc<Gate>,
    ip: IpAddr,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A place for a request from `ip`, or `None` when the limits are reached.
    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Ticket> {
        let mut m = self.open.lock().ok()?;
        let total: usize = m.values().sum();
        let mine = m.get(&ip).copied().unwrap_or(0);
        if total >= MAX_UNAUTH || mine >= MAX_UNAUTH_PER_ADDRESS {
            return None;
        }
        m.insert(ip, mine + 1);
        Some(Ticket {
            gate: self.clone(),
            ip,
        })
    }

    /// Requests admitted now.
    pub fn open(&self) -> usize {
        self.open.lock().map(|m| m.values().sum()).unwrap_or(0)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Ok(mut m) = self.gate.open.lock() {
            if let Some(n) = m.get_mut(&self.ip) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.ip);
                }
            }
        }
    }
}

/// Accept TLS connections until `stopping` or `gone` (the address went away and
/// the listener is bound again when it is back); each is relayed to `backend`.
pub fn accept_loop(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    backend: SocketAddr,
    peers: Peers,
    stopping: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    let per = Arc::new(Mutex::new(HashMap::<IpAddr, usize>::new()));
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stopping.load(Ordering::SeqCst) && !gone.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((tcp, peer)) => {
                if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    drop(tcp);
                    continue;
                }
                let Some(slot) = Slot::take(&per, peer.ip()) else {
                    drop(tcp);
                    continue;
                };
                active.fetch_add(1, Ordering::SeqCst);
                let (config, peers, act) = (config.clone(), peers.clone(), active.clone());
                let spawned = std::thread::Builder::new()
                    .name("hub-tls".into())
                    .spawn(move || {
                        let _slot = slot;
                        let _ = relay(tcp, peer, &config, backend, &peers);
                        act.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// One connection's place in the per-address count; given back when dropped.
struct Slot {
    per: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ip: IpAddr,
}

impl Slot {
    fn take(per: &Arc<Mutex<HashMap<IpAddr, usize>>>, ip: IpAddr) -> Option<Slot> {
        let mut m = per.lock().ok()?;
        let n = m.entry(ip).or_default();
        if *n >= MAX_PER_ADDRESS {
            return None;
        }
        *n += 1;
        Some(Slot {
            per: per.clone(),
            ip,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut m) = self.per.lock() {
            if let Some(n) = m.get_mut(&self.ip) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.ip);
                }
            }
        }
    }
}

/// The certificate page's connections at once, in all.
const MAX_HELPER: usize = 32;
/// A certificate-page connection that sends or takes nothing for this long is closed.
const HELPER_IDLE: Duration = Duration::from_secs(15);

/// The certificate page (plain HTTP on the home network) through a small front: at most
/// [`MAX_HELPER`] connections, [`MAX_PER_ADDRESS`] from one address, each closed after
/// [`HELPER_IDLE`] without traffic, passed to the page's own server on `backend` (review M1:
/// idle connections each held a thread, without limit).
pub fn helper_loop(listener: TcpListener, backend: SocketAddr, stopping: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    let per = Arc::new(Mutex::new(HashMap::<IpAddr, usize>::new()));
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stopping.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((tcp, peer)) => {
                if active.load(Ordering::SeqCst) >= MAX_HELPER {
                    drop(tcp);
                    continue;
                }
                let Some(slot) = Slot::take(&per, peer.ip()) else {
                    drop(tcp);
                    continue;
                };
                active.fetch_add(1, Ordering::SeqCst);
                let act = active.clone();
                let spawned = std::thread::Builder::new()
                    .name("hub-cert-front".into())
                    .spawn(move || {
                        let _slot = slot;
                        let _ = pipe(tcp, backend);
                        act.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Bytes both ways between a certificate-page visitor and the page's server, until either
/// side closes or nothing moves for [`HELPER_IDLE`].
fn pipe(outer: TcpStream, backend: SocketAddr) -> std::io::Result<()> {
    outer.set_nonblocking(false)?;
    outer.set_read_timeout(Some(HELPER_IDLE))?;
    outer.set_write_timeout(Some(HELPER_IDLE))?;
    let inner = TcpStream::connect_timeout(&backend, Duration::from_secs(5))?;
    inner.set_read_timeout(Some(HELPER_IDLE))?;
    inner.set_write_timeout(Some(HELPER_IDLE))?;
    let (mut from_in, mut to_out) = (inner.try_clone()?, outer.try_clone()?);
    let back = std::thread::Builder::new()
        .name("hub-cert-back".into())
        .spawn(move || {
            let _ = std::io::copy(&mut from_in, &mut to_out);
            let _ = to_out.shutdown(Shutdown::Write);
        })?;
    let (mut from_out, mut to_in) = (outer.try_clone()?, inner.try_clone()?);
    let _ = std::io::copy(&mut from_out, &mut to_in);
    let _ = inner.shutdown(Shutdown::Both);
    let _ = back.join();
    let _ = outer.shutdown(Shutdown::Both);
    Ok(())
}

type Shared = Arc<Mutex<(rustls::ServerConnection, TcpStream)>>;

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Send what the TLS connection wants to write.
fn flush(conn: &mut rustls::ServerConnection, tcp: &mut TcpStream) -> std::io::Result<()> {
    while conn.wants_write() {
        conn.write_tls(tcp)?;
    }
    tcp.flush()
}

/// One phone connection: the TLS handshake, then bytes both ways between the phone
/// and the request handler until either side closes.
fn relay(
    tcp: TcpStream,
    peer: SocketAddr,
    config: &Arc<rustls::ServerConfig>,
    backend: SocketAddr,
    peers: &Peers,
) -> std::io::Result<()> {
    tcp.set_nonblocking(false)?;
    tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    tcp.set_write_timeout(Some(IDLE))?;
    let _ = tcp.set_nodelay(true);
    let mut conn = rustls::ServerConnection::new(config.clone()).map_err(io_err)?;
    let mut t = tcp.try_clone()?;
    let started = Instant::now();
    while conn.is_handshaking() {
        if started.elapsed() >= HANDSHAKE_TIMEOUT {
            return Err(io_err("the TLS handshake took too long"));
        }
        conn.complete_io(&mut t)?;
    }
    tcp.set_read_timeout(Some(IDLE))?;
    let inner = TcpStream::connect_timeout(&backend, Duration::from_secs(5))?;
    inner.set_read_timeout(Some(IDLE))?;
    let port = inner.local_addr()?.port();
    if let Ok(mut p) = peers.lock() {
        p.insert(
            port,
            Peer {
                ip: peer.ip(),
                stream: tcp.try_clone()?,
                // A request must arrive (headers, and a small body, which the
                // HTTP server reads itself) within the unauthenticated timeout.
                deadline: Some(Instant::now() + UNAUTH_BODY_TIMEOUT),
            },
        );
    }
    // Data the phone sent with the end of the handshake is already decrypted.
    let mut early: Vec<u8> = Vec::new();
    {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match conn.reader().read(&mut buf) {
                Ok(0) => break,
                Ok(k) => early.extend_from_slice(buf.get(..k).unwrap_or(&[])),
                Err(_) => break,
            }
        }
    }
    let shared: Shared = Arc::new(Mutex::new((conn, tcp.try_clone()?)));
    let back = {
        let shared = shared.clone();
        let mut from = inner.try_clone()?;
        let phone = tcp.try_clone()?;
        std::thread::Builder::new()
            .name("hub-tls-out".into())
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match from.read(&mut buf) {
                        Ok(0) | Err(_) => 0,
                        Ok(n) => n,
                    };
                    let Ok(mut g) = shared.lock() else { break };
                    let (c, w) = &mut *g;
                    if n == 0 {
                        c.send_close_notify();
                        let _ = flush(c, w);
                        let _ = phone.shutdown(Shutdown::Write);
                        break;
                    }
                    let data = buf.get(..n).unwrap_or(&[]);
                    if c.writer().write_all(data).is_err() || flush(c, w).is_err() {
                        break;
                    }
                }
            })?
    };
    let mut to = inner.try_clone()?;
    if !early.is_empty() {
        to.write_all(&early)?;
    }
    let mut raw = vec![0u8; 64 * 1024];
    let mut plain = vec![0u8; 64 * 1024];
    let result = (|| -> std::io::Result<()> {
        loop {
            let n = t.read(&mut raw)?;
            if n == 0 {
                return Ok(());
            }
            let mut rd = raw.get(..n).unwrap_or(&[]);
            let mut out: Vec<u8> = Vec::new();
            let mut closed = false;
            {
                let Ok(mut g) = shared.lock() else {
                    return Ok(());
                };
                let (c, w) = &mut *g;
                while !rd.is_empty() {
                    c.read_tls(&mut rd)?;
                    let state = match c.process_new_packets() {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = flush(c, w);
                            return Err(io_err(e));
                        }
                    };
                    closed |= state.peer_has_closed();
                    loop {
                        match c.reader().read(&mut plain) {
                            Ok(0) => break,
                            Ok(k) => out.extend_from_slice(plain.get(..k).unwrap_or(&[])),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }
                }
                flush(c, w)?;
            }
            if !out.is_empty() {
                to.write_all(&out)?;
            }
            if closed {
                return Ok(());
            }
        }
    })();
    let _ = inner.shutdown(Shutdown::Both);
    let _ = back.join();
    let _ = tcp.shutdown(Shutdown::Both);
    if let Ok(mut p) = peers.lock() {
        p.remove(&port);
    }
    result
}
