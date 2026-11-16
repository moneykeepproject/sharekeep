//! The client side of the household service, used by the desktop app on every PC.
//!
//! - [`Client`]: HTTPS calls to the hub, trusting only the hub's own certificate authority
//!   (installed once per device) and carrying the device's token.
//! - [`Outbox`]: new expenses, payments and approvals made while the hub can't be reached,
//!   kept on disk and sent in order when it can. Each carries an id, so a resend never adds
//!   a second record. Edits and rule changes need the hub, so the outbox never merges.
//! - [`Replica`]: this device's copy of a book's history, checked record by record against
//!   the chain, and against the last head this device saw: a hub whose history no longer
//!   contains it has been rewritten, and the copy is kept as it was.

pub mod api;
pub mod seal;
pub mod tls;

use api::{ErrorAnswer, PairAnswer, PairRequest, RecordsAnswer, SubmitRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sk_chain::{Broken, Head, Record};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Kinds a device may queue while offline: new things only.
pub const OUTBOX_KINDS: &[&str] = &["expense", "payment", "approval"];

/// Where the hub is and the certificate authority to trust for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubAddress {
    /// The hub's address and HTTPS port.
    pub addr: SocketAddr,
    /// The name the certificate is for: the IP address, or the hub's local name.
    pub host: String,
    /// The hub's certificate authority (DER), as hex.
    pub ca_hex: String,
}

impl HubAddress {
    pub fn new(addr: SocketAddr, host: impl Into<String>, ca_der: &[u8]) -> Self {
        HubAddress {
            addr,
            host: host.into(),
            ca_hex: ca_der.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    pub fn ca_der(&self) -> Vec<u8> {
        (0..self.ca_hex.len() / 2)
            .filter_map(|i| {
                self.ca_hex
                    .get(2 * i..2 * i + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect()
    }
}

/// Why a call didn't work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The hub couldn't be reached (asleep, off, another network) or the connection broke.
    Offline(String),
    /// The hub answered with an error.
    Http { status: u16, message: String },
    /// The hub's answer couldn't be read.
    BadAnswer(String),
}

impl CallError {
    /// Whether trying again later may work (the request itself isn't wrong).
    pub fn is_retryable(&self) -> bool {
        match self {
            CallError::Offline(_) | CallError::BadAnswer(_) => true,
            CallError::Http { status, .. } => *status >= 500 || *status == 408 || *status == 429,
        }
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Offline(why) => write!(f, "the household hub can't be reached ({why})"),
            CallError::Http { message, .. } => write!(f, "{message}"),
            CallError::BadAnswer(why) => write!(f, "the hub's answer couldn't be read ({why})"),
        }
    }
}

impl std::error::Error for CallError {}

/// A device's connection to its hub.
#[derive(Clone)]
pub struct Client {
    pub hub: HubAddress,
    token: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // never the token
        f.debug_struct("Client")
            .field("hub", &self.hub.addr)
            .field("paired", &self.token.is_some())
            .finish()
    }
}

fn random_hex<const N: usize>() -> Result<String, String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| format!("no random source: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

impl Client {
    /// A client for a paired device.
    pub fn new(hub: HubAddress, token: impl Into<String>) -> Self {
        Client {
            hub,
            token: Some(token.into()),
        }
    }

    /// The device's token, for the app to keep in its private settings.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Pair this device: the code from the hub's screen (or its QR code) and the
    /// household's PIN. Returns the paired client and what the hub said.
    pub fn pair(
        hub: HubAddress,
        code: &str,
        pin: &str,
        device: &str,
    ) -> Result<(Client, PairAnswer), CallError> {
        let c = Client { hub, token: None };
        let answer: PairAnswer = c.call(
            "POST",
            "/api/v1/pair",
            Some(&PairRequest {
                code: code.into(),
                pin: pin.into(),
                device: device.into(),
            }),
        )?;
        Ok((Client::new(c.hub, answer.token.clone()), answer))
    }

    fn call<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        method: &str,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, CallError> {
        let bytes = match body {
            Some(b) => {
                Some(serde_json::to_vec(b).map_err(|e| CallError::BadAnswer(e.to_string()))?)
            }
            None => None,
        };
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
        if bytes.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        if let Some(a) = &auth {
            headers.push(("Authorization", a));
        }
        let (status, answer) = tls::https_call(
            self.hub.addr,
            &self.hub.host,
            &self.hub.ca_der(),
            method,
            path,
            &headers,
            bytes.as_deref(),
        )
        .map_err(CallError::Offline)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<ErrorAnswer>(&answer)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("the hub answered {status}"));
            return Err(CallError::Http { status, message });
        }
        serde_json::from_slice(&answer).map_err(|e| CallError::BadAnswer(e.to_string()))
    }

    pub fn me(&self) -> Result<api::MeAnswer, CallError> {
        self.call::<_, ()>("GET", "/api/v1/me", None)
    }

    /// Up to `limit` records from `from`, and the hub's head.
    pub fn records(&self, from: u64, limit: u64) -> Result<RecordsAnswer, CallError> {
        self.call::<_, ()>(
            "GET",
            &format!("/api/v1/records?from={from}&limit={limit}"),
            None,
        )
    }

    /// Send one request; `client_id` makes a resend safe.
    pub fn submit(&self, client_id: &str, kind: &str, body: &Value) -> Result<Record, CallError> {
        self.call(
            "POST",
            "/api/v1/records",
            Some(&SubmitRequest {
                client_id: client_id.into(),
                kind: kind.into(),
                body: body.clone(),
            }),
        )
    }

    /// A closed period's statement (PDF bytes).
    pub fn statement(&self, agreement: u32, first: &str) -> Result<Vec<u8>, CallError> {
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/pdf")];
        if let Some(a) = &auth {
            headers.push(("Authorization", a));
        }
        let (status, answer) = tls::https_call(
            self.hub.addr,
            &self.hub.host,
            &self.hub.ca_der(),
            "GET",
            &format!("/api/v1/statements/{agreement}/{first}"),
            &headers,
            None,
        )
        .map_err(CallError::Offline)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<ErrorAnswer>(&answer)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("the hub answered {status}"));
            return Err(CallError::Http { status, message });
        }
        Ok(answer)
    }

    /// An online-only change (an edit, a rule proposal…): sent now or not at all.
    pub fn submit_now(&self, kind: &str, body: &Value) -> Result<Record, CallError> {
        let id = random_hex::<16>().map_err(CallError::Offline)?;
        self.submit(&id, kind, body)
    }
}

/// The hub's certificate helper port (plain HTTP, home network).
pub const HELPER_PORT: u16 = 15080;
/// The hub's device port (HTTPS).
pub const HTTPS_PORT: u16 = 15443;
/// The hub computer's admin port (loopback only).
pub const ADMIN_PORT: u16 = 15081;

/// Fetch the hub's certificate authority from its helper page (`ip`:[`HELPER_PORT`]): the DER. The
/// person then compares [`fingerprint`] with the hub computer's screen before trusting it.
pub fn fetch_ca(helper: SocketAddr) -> Result<Vec<u8>, CallError> {
    let (status, body) = tls::http_call(helper, "GET", "/sharekeep-ca.crt", &[], None)
        .map_err(CallError::Offline)?;
    if status != 200 {
        return Err(CallError::Http {
            status,
            message: format!("the hub's certificate page answered {status}"),
        });
    }
    from_pem(&String::from_utf8_lossy(&body))
        .ok_or_else(|| CallError::BadAnswer("that isn't a certificate".into()))
}

/// The DER of PEM text (one certificate).
pub fn from_pem(text: &str) -> Option<Vec<u8>> {
    if !text.contains("-----BEGIN CERTIFICATE-----") {
        return None;
    }
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    base64_decode(&body).filter(|d| !d.is_empty())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = A.iter().position(|a| *a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The certificate fingerprint the hub computer shows: the first 16 bytes of its SHA-256,
/// in groups of four hex digits (as MoneyKeep's capture server).
pub fn fingerprint(der: &[u8]) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(der);
    h.get(..16)
        .unwrap_or(&[])
        .chunks(2)
        .map(|c| c.iter().map(|b| format!("{b:02X}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Write `bytes` to `path` through a temporary file in the same folder, so a crash
/// never leaves half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Something queued while offline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub client_id: String,
    pub kind: String,
    pub body: Value,
    /// When it was made (ms since 1970 UTC).
    pub made: i64,
    pub attempts: u32,
}

/// Something the hub refused: kept for the person to see, never resent on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub item: Pending,
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OutboxFile {
    pending: Vec<Pending>,
    refused: Vec<Refused>,
}

/// New expenses, payments and approvals waiting to reach the hub, on disk.
#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    file: OutboxFile,
}

/// What one [`Outbox::flush`] did.
#[derive(Debug, Default)]
pub struct FlushReport {
    pub sent: Vec<Record>,
    pub refused: usize,
    pub remaining: usize,
    /// Why sending stopped early (the hub couldn't be reached, or answered "try later").
    pub stopped: Option<CallError>,
}

impl Outbox {
    /// Open (or start) the outbox kept at `path`.
    pub fn open(path: &Path) -> Result<Outbox, String> {
        let file = match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| format!("the outbox file is damaged ({e})"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => OutboxFile::default(),
            Err(e) => return Err(format!("the outbox can't be read ({e})")),
        };
        Ok(Outbox {
            path: path.to_path_buf(),
            file,
        })
    }

    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.file).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &bytes).map_err(|e| format!("the outbox can't be saved ({e})"))
    }

    /// Queue a new expense, payment or approval. Returns its id.
    pub fn push(&mut self, kind: &str, body: Value, now: i64) -> Result<String, String> {
        if !OUTBOX_KINDS.contains(&kind) {
            return Err(format!(
                "\"{kind}\" needs the hub: it can't wait in the outbox"
            ));
        }
        let client_id = random_hex::<16>()?;
        self.file.pending.push(Pending {
            client_id: client_id.clone(),
            kind: kind.into(),
            body,
            made: now,
            attempts: 0,
        });
        if let Err(e) = self.save() {
            self.file.pending.pop();
            return Err(e);
        }
        Ok(client_id)
    }

    pub fn pending(&self) -> &[Pending] {
        &self.file.pending
    }

    pub fn refused(&self) -> &[Refused] {
        &self.file.refused
    }

    /// Clear the refused list once the person has seen it.
    pub fn dismiss_refused(&mut self) -> Result<(), String> {
        self.file.refused.clear();
        self.save()
    }

    /// Send what is waiting, oldest first. Stops at the first item the hub can't take
    /// now (it stays, in order); an item the hub refuses outright moves to the refused
    /// list so it doesn't hold up the rest.
    pub fn flush(&mut self, client: &Client) -> FlushReport {
        let mut report = FlushReport::default();
        while let Some(item) = self.file.pending.first().cloned() {
            match client.submit(&item.client_id, &item.kind, &item.body) {
                Ok(rec) => {
                    self.file.pending.remove(0);
                    report.sent.push(rec);
                }
                Err(e) if e.is_retryable() => {
                    if let Some(first) = self.file.pending.first_mut() {
                        first.attempts += 1;
                    }
                    report.stopped = Some(e);
                    break;
                }
                Err(e) => {
                    self.file.pending.remove(0);
                    self.file.refused.push(Refused {
                        item,
                        reason: e.to_string(),
                    });
                    report.refused += 1;
                }
            }
            // Saved after every item: a crash part way resends at most one, and the
            // hub ignores that resend.
            if let Err(e) = self.save() {
                report.stopped = Some(CallError::Offline(e));
                break;
            }
        }
        if report.stopped.is_some() {
            let _ = self.save();
        }
        report.remaining = self.file.pending.len();
        report
    }
}

/// Why a sync didn't complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Offline(CallError),
    /// The hub's history no longer contains what this device saw before: it was
    /// rewritten. The local copy is kept as it was.
    Rewritten(Broken),
    /// A record from the hub doesn't check out.
    Broken(Broken),
    /// The household restored a backup: the hub carries on from a point before this
    /// device's last record. The local copy is kept until the person accepts.
    Restored(RestoreOffer),
    /// The hub's history doesn't contain the checkpoint given when pairing. Nothing is kept.
    CheckpointNotKept(String),
    /// The hub's history has a record kind this version doesn't know: the hub runs a newer
    /// ShareKeep. Nothing new is taken in until this device is updated.
    NeedsUpdate {
        seq: u64,
        kind: String,
    },
    Local(String),
}

/// A restore this device can accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOffer {
    /// The `restore` record.
    pub seq: u64,
    pub when: chrono::NaiveDate,
    pub backup: String,
    /// The backup's last record: what both histories share.
    pub to_seq: u64,
    /// This device's records after that, which the restored book doesn't have.
    pub lost_from: u64,
    pub lost_to: u64,
}

impl RestoreOffer {
    pub fn text(&self) -> String {
        let lost = if self.lost_from == self.lost_to {
            format!("record {} is", self.lost_from)
        } else {
            format!("records {} to {} are", self.lost_from, self.lost_to)
        };
        format!(
            "The household restored a backup on {}. The book carries on from record {}; {lost} \
             not in it and will need to be entered again. Accept to carry on from the backup; \
             until then this device only shows its own copy.",
            self.when, self.to_seq
        )
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Offline(e) => write!(f, "{e}"),
            SyncError::Rewritten(b) => write!(
                f,
                "Warning: the hub's history was changed since this device last saw it ({b})."
            ),
            SyncError::Broken(b) => {
                write!(f, "The hub sent a history that doesn't check out ({b}).")
            }
            SyncError::Restored(o) => write!(f, "{}", o.text()),
            SyncError::CheckpointNotKept(why) => write!(f, "{why}"),
            SyncError::NeedsUpdate { seq, kind } => write!(
                f,
                "The hub runs a newer ShareKeep: record {seq} is a \"{kind}\" this version doesn't know. Update ShareKeep on this device; until then it shows the history up to record {}.",
                seq.saturating_sub(1)
            ),
            SyncError::Local(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SyncError {}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReplicaMeta {
    head: Option<Head>,
    /// A checkpoint given when pairing: the hub's history must contain it before this
    /// device keeps anything.
    #[serde(default)]
    expect: Option<Head>,
    /// When the last sync finished (ms since 1970 UTC).
    last_synced: Option<i64>,
}

/// This device's copy of a book's history.
#[derive(Debug)]
pub struct Replica {
    dir: PathBuf,
    records: Vec<Record>,
    meta: ReplicaMeta,
}

const PAGE: u64 = 500;

impl Replica {
    /// Open (or start) the copy kept in `dir`. The copy is checked as it loads.
    pub fn open(dir: &Path) -> Result<Replica, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let records = match std::fs::read_to_string(dir.join("history.jsonl")) {
            Ok(t) => sk_chain::import_lines(&t).map_err(|b| b.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.to_string()),
        };
        let meta: ReplicaMeta = match std::fs::read(dir.join("replica.json")) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| e.to_string())?,
            Err(_) => ReplicaMeta::default(),
        };
        if !records.is_empty() {
            let head = sk_chain::verify(&records).map_err(|b| b.to_string())?;
            if meta.head.is_some_and(|h| h != head) {
                return Err("this device's copy of the history is damaged".into());
            }
        }
        Ok(Replica {
            dir: dir.to_path_buf(),
            records,
            meta,
        })
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    pub fn head(&self) -> Option<Head> {
        self.meta.head
    }

    /// When the copy last matched the hub: shown as "last synced … ago" when offline.
    pub fn last_synced(&self) -> Option<i64> {
        self.meta.last_synced
    }

    fn save(&self) -> Result<(), String> {
        write_atomic(
            &self.dir.join("history.jsonl"),
            sk_chain::export_lines(&self.records).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let meta = serde_json::to_vec_pretty(&self.meta).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join("replica.json"), &meta).map_err(|e| e.to_string())
    }

    /// Fetch what's new from the hub and check it. Returns how many records were added.
    pub fn sync(&mut self, client: &Client, now: i64) -> Result<usize, SyncError> {
        let mut added = 0;
        loop {
            let head = self.meta.head;
            // Ask again for our own head too: the hub must still have it, unchanged.
            let from = head.map_or(0, |h| h.seq);
            let page = client
                .records(from, PAGE)
                .map_err(SyncError::Offline)?
                .records;
            let fresh: &[Record] = match head {
                None => {
                    if page.is_empty() {
                        break;
                    }
                    sk_chain::verify(&page).map_err(SyncError::Broken)?;
                    &page
                }
                Some(h) => {
                    let still_there = page
                        .first()
                        .is_some_and(|r| r.seq == h.seq && r.hash == h.hash);
                    if !still_there {
                        if let Some(o) = self.find_restore(client) {
                            return Err(SyncError::Restored(o));
                        }
                        return Err(SyncError::Rewritten(Broken {
                            at: h.seq,
                            problem: sk_chain::Problem::Rewritten,
                        }));
                    }
                    let rest = page.get(1..).unwrap_or(&[]);
                    if rest.is_empty() {
                        break;
                    }
                    sk_chain::verify_after(Some(h), rest).map_err(SyncError::Broken)?;
                    rest
                }
            };
            // a kind this version doesn't know: keep what came before it, then stop
            let known = fresh
                .iter()
                .position(|r| !sk_ledger::KINDS.contains(&r.kind.as_str()))
                .unwrap_or(fresh.len());
            added += known;
            self.records.extend_from_slice(&fresh[..known]);
            if let Some(r) = fresh.get(known) {
                self.meta.head = self.records.last().map(Record::head);
                self.save().map_err(SyncError::Local)?;
                return Err(SyncError::NeedsUpdate {
                    seq: r.seq,
                    kind: r.kind.clone(),
                });
            }
            self.meta.head = self.records.last().map(Record::head);
            if (page.len() as u64) < PAGE {
                break;
            }
        }
        self.check_expected()?;
        self.meta.last_synced = Some(now);
        self.save().map_err(SyncError::Local)?;
        Ok(added)
    }

    /// Start from a checkpoint: the next sync keeps the hub's history only if it contains it.
    pub fn expect(&mut self, head: Head) -> Result<(), String> {
        self.meta.expect = Some(head);
        self.save()
    }

    /// The checkpoint this device still waits to see, if any.
    pub fn expected(&self) -> Option<Head> {
        self.meta.expect
    }

    /// Give up the checkpoint (a typing mistake, or a restore the household agreed to).
    pub fn forget_expected(&mut self) -> Result<(), String> {
        self.meta.expect = None;
        self.save()
    }

    fn check_expected(&mut self) -> Result<(), SyncError> {
        use sk_chain::checkpoint::{check, Found};
        let Some(e) = self.meta.expect else {
            return Ok(());
        };
        let why = match check(&self.records, e) {
            Found::Kept => {
                self.meta.expect = None;
                return Ok(());
            }
            Found::Rewritten => format!(
                "The hub's history doesn't match your checkpoint: record {} is different there. This device kept nothing. If the checkpoint was typed wrong, or the household restored a backup and agreed to it, forget the checkpoint in Settings.",
                e.seq
            ),
            Found::Missing { have } => format!(
                "The hub's history ends at record {}, before your checkpoint (record {}): records were removed. This device kept nothing. If the checkpoint was typed wrong, forget it in Settings.",
                have.map_or(0, |h| h),
                e.seq
            ),
        };
        self.records.clear();
        self.meta.head = None;
        self.save().map_err(SyncError::Local)?;
        Err(SyncError::CheckpointNotKept(why))
    }

    /// When the hub's history differs from ours: is it because a backup was restored? That
    /// is, a `restore` record continuing from a record this device has, unchanged.
    fn find_restore(&self, client: &Client) -> Option<RestoreOffer> {
        let ours = self.meta.head?;
        let mut from = 0;
        let mut found = None;
        loop {
            let page = client.records(from, PAGE).ok()?.records;
            for r in &page {
                if r.kind != "restore" {
                    continue;
                }
                let to = r.body["restored_to"]["seq"].as_u64()?;
                let hash = r.body["restored_to"]["hash"].as_str().unwrap_or("");
                let shared = self
                    .records
                    .iter()
                    .find(|x| x.seq == to)
                    .is_some_and(|x| x.hash.to_hex() == hash);
                if shared && to < ours.seq {
                    found = Some(RestoreOffer {
                        seq: r.seq,
                        when: chrono::DateTime::from_timestamp_millis(r.ts)
                            .map_or_else(chrono::NaiveDate::default, |d| d.date_naive()),
                        backup: r.body["backup"].as_str().unwrap_or("").to_string(),
                        to_seq: to,
                        lost_from: to + 1,
                        lost_to: ours.seq,
                    });
                }
            }
            match page.last() {
                Some(l) if (page.len() as u64) >= PAGE => from = l.seq + 1,
                _ => break,
            }
        }
        found
    }

    /// Carry on from a restored backup: drop this device's records after it (the offer
    /// lists them) and fetch the hub's history from there.
    pub fn accept_restore(
        &mut self,
        client: &Client,
        offer: &RestoreOffer,
        now: i64,
    ) -> Result<usize, SyncError> {
        let again = self.find_restore(client);
        if again.as_ref().map(|o| o.seq) != Some(offer.seq) {
            return Err(SyncError::Local(
                "The hub's history changed again; sync and look at the offer again.".into(),
            ));
        }
        self.records.retain(|r| r.seq <= offer.to_seq);
        self.meta.head = self.records.last().map(Record::head);
        self.save().map_err(SyncError::Local)?;
        self.sync(client, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nowhere() -> Client {
        // a port nothing listens on
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        Client::new(HubAddress::new(addr, "127.0.0.1", b"x"), "token")
    }

    #[test]
    fn only_new_things_can_wait_in_the_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = Outbox::open(&dir.path().join("outbox.json")).unwrap();
        assert!(o.push("expense", json!({"amount": 5}), 1).is_ok());
        assert!(o.push("rule-proposal", json!({}), 1).is_err());
        assert!(o.push("edit", json!({}), 1).is_err());
        assert_eq!(o.pending().len(), 1);
    }

    #[test]
    fn the_outbox_survives_a_restart_and_keeps_order_while_offline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.json");
        let mut o = Outbox::open(&path).unwrap();
        let a = o.push("expense", json!({"amount": 1}), 1).unwrap();
        let b = o.push("payment", json!({"amount": 2}), 2).unwrap();
        let r = o.flush(&nowhere());
        assert!(matches!(r.stopped, Some(CallError::Offline(_))));
        assert_eq!(r.remaining, 2);
        drop(o);
        let o = Outbox::open(&path).unwrap();
        let ids: Vec<&str> = o.pending().iter().map(|p| p.client_id.as_str()).collect();
        assert_eq!(ids, vec![a.as_str(), b.as_str()]);
        assert_eq!(o.pending()[0].attempts, 1);
    }

    #[test]
    fn retryable_errors() {
        assert!(CallError::Offline("x".into()).is_retryable());
        assert!(CallError::Http {
            status: 503,
            message: String::new()
        }
        .is_retryable());
        assert!(!CallError::Http {
            status: 403,
            message: String::new()
        }
        .is_retryable());
    }

    #[test]
    fn hub_address_round_trip() {
        let a = HubAddress::new(
            "192.168.1.20:15443".parse().unwrap(),
            "192.168.1.20",
            &[1, 2, 255],
        );
        assert_eq!(a.ca_der(), vec![1, 2, 255]);
    }

    #[test]
    fn an_empty_replica_starts_clean_and_a_damaged_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let r = Replica::open(dir.path()).unwrap();
        assert!(r.head().is_none());
        std::fs::write(dir.path().join("history.jsonl"), "{broken\n").unwrap();
        assert!(Replica::open(dir.path()).is_err());
    }
}
