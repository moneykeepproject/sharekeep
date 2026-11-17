//! The running household service: three listeners in front of the books.
//!
//! - **Devices** (`https://<address>:15443`): TLS relayed to a private loopback HTTP
//!   server. Pairing needs a pairing code (shown on the hub computer, one use, 10
//!   minutes) and the household PIN; everything else needs the device's token. Each
//!   device belongs to one member of one book; the hub stamps every record with that
//!   member and device, whatever the request says.
//! - **The certificate page** (`http://<address>:15080`): how to trust the hub's
//!   certificate authority on a phone or a computer.
//! - **The desktop app on the hub computer** (`http://127.0.0.1:15081`, loopback only):
//!   creating and unlocking books, the PIN, pairing codes, devices, backups. It needs the
//!   admin token in the hub's data folder.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sk_chain::Author;
use sk_client::api::{BookFacts, MeAnswer, PairAnswer, PairRequest, SubmitRequest};
use sk_core::{Currency, MemberId, NaiveDate};
use sk_ledger::Ledger;
use sk_store::crypt::KdfParams;
use sk_store::{BookInfo, BookKind, Encryption, NewBook, Secret, Store, StoreError};
use tiny_http::{Header, Request, Response, Server};

use crate::config::HubConfig;
use crate::keys::KeyStore;
use crate::registry::{Device, Registry};
use crate::util::{self, StoreResult};
use crate::{pin, wifi};

/// Kinds a paired device may add. Every record is also checked by the agreement engine
/// ([`sk_ledger`]) before it's written: a record that would make the book wrong is refused
/// (422) with a message for the person who sent it.
pub const DEVICE_KINDS: &[&str] = &[
    "expense",
    "payment",
    "approval",
    "dispute",
    "entry-edit",
    "entry-delete",
    "rule-proposal",
    "deposit",
    "member-join",
    "member-leave",
    "period-close",
    "schedule",
    "recurring",
    "restore-accept",
];

/// Kinds the hub computer may add besides those.
pub const ADMIN_KINDS: &[&str] = &["member-add", "agreement-add"];

pub const ADMIN_TOKEN_FILE: &str = "admin.token";
pub const BOOKS_DIR: &str = "books";
pub const BACKUPS_DIR: &str = "backups";
pub const STATEMENTS_DIR: &str = "statements";

const MAX_BODY: u64 = 1 << 20;
const MAX_PAGE: u64 = 1000;
const CODE_MINUTES: i64 = 10;
const CODE_TRIES: u32 = 5;
const FAILURE_WINDOW_MS: i64 = 10 * 60 * 1000;
const MAX_FAILURES: usize = 10;
const WORKERS: usize = 4;

/// The hub's clock: milliseconds since 1970 UTC.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
/// Where log lines go. Lines name the method, path and status only: never a token, a
/// PIN or a record's contents.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// How the hub runs.
pub struct HubOptions {
    pub data: PathBuf,
    pub keys: KeyStore,
    pub clock: Clock,
    pub log: Log,
    /// The PIN's hash cost (cheaper in tests).
    pub pin_cost: pin::Cost,
    /// Key derivation for new encrypted books (cheaper in tests).
    pub kdf: KdfParams,
}

impl HubOptions {
    /// The production settings for a hub keeping its data in `data`.
    pub fn new(data: PathBuf) -> Self {
        HubOptions {
            keys: KeyStore::files(data.clone()),
            data,
            clock: Arc::new(util::now_ms),
            log: Arc::new(|l: &str| eprintln!("{l}")),
            pin_cost: pin::COST,
            kdf: KdfParams::DEFAULT,
        }
    }
}

/// Set the hub up in `data`: its folders, `hub.json`, the certificate authority and
/// server certificate, and the admin token. Run once (again only to change the address,
/// which makes a new certificate authority every device must trust again).
pub fn setup(
    data: &Path,
    cfg: &HubConfig,
    keys: &KeyStore,
    computer: &str,
    now_ms: i64,
) -> StoreResult<wifi::Info> {
    for d in [
        data.to_path_buf(),
        data.join(BOOKS_DIR),
        data.join(BACKUPS_DIR),
    ] {
        std::fs::create_dir_all(&d).map_err(|e| format!("{} can't be made: {e}", d.display()))?;
    }
    cfg.save(data)?;
    let target = wifi::Target {
        ip: cfg.address,
        name: cfg.name.clone(),
    };
    let m = wifi::ensure(data, keys, &target, computer, now_ms / 1000)?;
    if !data.join(ADMIN_TOKEN_FILE).exists() {
        let t = util::hex(&util::random::<32>()?);
        util::write_private(&data.join(ADMIN_TOKEN_FILE), t.as_bytes())?;
    }
    Ok(m.info)
}

/// The admin token the desktop app on the hub computer uses.
pub fn admin_token(data: &Path) -> StoreResult<String> {
    std::fs::read_to_string(data.join(ADMIN_TOKEN_FILE))
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("the admin token can't be read: {e}"))
}

struct Book {
    file: String,
    path: PathBuf,
    store: Option<Store>,
    /// The book's state, replayed from its history when it opens.
    ledger: Ledger,
    /// Why the book isn't open (closed, locked, damaged…).
    problem: Option<String>,
    /// Its name, remembered while it's closed (a closed book's file isn't read).
    name: Option<String>,
    encrypted: bool,
    /// A Hyperbook whose history doesn't verify: where it breaks (guided recovery).
    broken: Option<u64>,
}

/// Replay a book's history into its state.
fn replay(store: &Store, log: &Log) -> Ledger {
    match store.records(0, u64::MAX) {
        Ok(records) => {
            let (l, skipped) = Ledger::replay(&records);
            if !skipped.is_empty() {
                log(&format!(
                    "{}: {} old record(s) don't apply and are skipped",
                    store.info().name,
                    skipped.len()
                ));
            }
            l
        }
        Err(e) => {
            log(&e.to_string());
            Ledger::new()
        }
    }
}

impl Book {
    fn open(file: String, path: PathBuf, store: Store, log: &Log) -> Book {
        let ledger = replay(&store, log);
        Book {
            file,
            path,
            name: Some(store.info().name.clone()),
            encrypted: store.info().encrypted,
            store: Some(store),
            ledger,
            problem: None,
            broken: None,
        }
    }

    /// Close the book: its file is released and its devices get "closed" until it's
    /// opened again.
    fn close(&mut self) {
        self.store = None;
        self.ledger = Ledger::new();
        self.problem = Some(CLOSED.into());
    }

    fn info(&self) -> Option<&BookInfo> {
        self.store.as_ref().map(Store::info)
    }

    fn summary(&self) -> Value {
        let info = self.info();
        json!({
            "file": self.file,
            "id": info.map(|i| i.book_id.clone()),
            "name": info.map(|i| i.name.clone()).or_else(|| self.name.clone()),
            "hyperbook": info.map(|i| i.hyperbook),
            "encrypted": info.map(|i| i.encrypted).unwrap_or(self.encrypted),
            "head": self.store.as_ref().and_then(Store::head).map(|h| h.seq),
            "open": self.store.is_some(),
            "problem": self.problem,
            "broken": self.broken,
        })
    }
}

#[derive(Debug, Clone)]
struct Pairing {
    book_id: String,
    member: u32,
    expires: i64,
    tries: u32,
}

struct State {
    books: Vec<Book>,
    registry: Registry,
    codes: HashMap<String, Pairing>,
    failures: VecDeque<i64>,
}

struct Shared {
    state: Mutex<State>,
    data: PathBuf,
    cfg: HubConfig,
    clock: Clock,
    log: Log,
    pin_cost: pin::Cost,
    kdf: KdfParams,
    admin_token: String,
    https: SocketAddr,
    /// The certificate page's address (its port may differ from the config in tests).
    helper: SocketAddr,
    host: String,
    peers: wifi::Peers,
    stopping: Arc<AtomicBool>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic in one request must not stop the hub: the state is still consistent
        // (every change is one database transaction).
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }
}

fn facts(info: &BookInfo) -> BookFacts {
    BookFacts {
        id: info.book_id.clone(),
        name: info.name.clone(),
        currency: info.currency.code().to_string(),
        hyperbook: info.hyperbook,
    }
}

/// Why a book that isn't the current one is closed.
const CLOSED: &str = "closed";
/// The registry key naming the book the hub works in.
const CURRENT_BOOK: &str = "current_book";

/// The hub's books. Only one is open at a time: `current` (the one last opened), or, on a
/// hub that never chose, the first unencrypted one. The others are listed, closed.
fn load_books(dir: &Path, current: Option<&str>, log: &Log) -> Vec<Book> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == sk_store::EXTENSION))
        .collect();
    paths.sort();
    for path in paths {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let chosen = match current {
            Some(c) => c == file,
            None => !out.iter().any(|b: &Book| b.store.is_some()),
        };
        let kind = sk_store::book_kind(&path);
        let encrypted = matches!(kind, Ok(BookKind::Encrypted));
        let mut broken = None;
        let problem = match kind {
            Ok(BookKind::Encrypted) if chosen => {
                "locked: enter its password on the hub computer".to_string()
            }
            Ok(BookKind::Plain) if chosen => match Store::open(&path, None) {
                Ok(s) => {
                    out.push(Book::open(file, path, s, log));
                    continue;
                }
                Err(StoreError::HistoryBroken(b)) => {
                    broken = Some(b.at);
                    StoreError::HistoryBroken(b).to_string()
                }
                Err(e) => e.to_string(),
            },
            Ok(_) => CLOSED.to_string(),
            Err(e) => e.to_string(),
        };
        out.push(Book {
            file,
            path,
            store: None,
            ledger: Ledger::new(),
            problem: Some(problem),
            name: None,
            encrypted,
            broken,
        });
    }
    out
}

/// A running hub.
pub struct Running {
    /// Where devices connect (HTTPS).
    pub https: SocketAddr,
    /// The certificate page (plain HTTP).
    pub helper: SocketAddr,
    /// The desktop app's loopback port.
    pub admin: SocketAddr,
    pub info: wifi::Info,
    pub ca_der: Vec<u8>,
    stopping: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("https", &self.https)
            .field("helper", &self.helper)
            .field("admin", &self.admin)
            .finish_non_exhaustive()
    }
}

impl Running {
    /// Stop every listener and wait for them.
    pub fn stop(mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Run until the process ends.
    pub fn wait(mut self) {
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> StoreResult<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| format!("the hub can't start a thread: {e}"))
}

fn serve(
    name: &str,
    server: Arc<Server>,
    shared: Arc<Shared>,
    handler: fn(&Shared, Request),
    threads: &mut Vec<JoinHandle<()>>,
) -> StoreResult<()> {
    for i in 0..WORKERS {
        let (server, shared) = (server.clone(), shared.clone());
        threads.push(spawn(&format!("{name}-{i}"), move || {
            while !shared.stopping.load(Ordering::SeqCst) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(req)) => handler(&shared, req),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        })?);
    }
    Ok(())
}

fn http_server(addr: SocketAddr) -> StoreResult<(Arc<Server>, SocketAddr)> {
    let s = Server::http(addr).map_err(|e| {
        format!("the hub can't listen on {addr}: {e}. Is another program using that port?")
    })?;
    let bound = s
        .server_addr()
        .to_ip()
        .ok_or_else(|| "the hub's listener has no address".to_string())?;
    Ok((Arc::new(s), bound))
}

/// Start the hub set up in `opts.data`.
pub fn start(opts: HubOptions) -> StoreResult<Running> {
    let cfg = HubConfig::load(&opts.data)?;
    let now = (opts.clock)();
    let target = wifi::Target {
        ip: cfg.address,
        name: cfg.name.clone(),
    };
    let material = wifi::ensure_existing(&opts.data, &opts.keys, &target, now / 1000)?;
    let resolver = Arc::new(wifi::Resolver::new(&material)?);
    let tls = wifi::server_config(resolver.clone())?;
    let registry = Registry::open(&opts.data)?;
    let current = registry.get(CURRENT_BOOK)?;
    let books = load_books(&opts.data.join(BOOKS_DIR), current.as_deref(), &opts.log);
    let admin_token = admin_token(&opts.data)?;
    let stopping = Arc::new(AtomicBool::new(false));
    let peers: wifi::Peers = Arc::new(Mutex::new(HashMap::new()));

    let (backend, backend_addr) = http_server(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let listener = TcpListener::bind((cfg.address, cfg.https_port)).map_err(|e| {
        format!(
            "the hub can't listen on {}:{}: {e}",
            cfg.address, cfg.https_port
        )
    })?;
    let https = listener.local_addr().map_err(|e| e.to_string())?;
    let (helper, helper_addr) = http_server(SocketAddr::from((cfg.address, cfg.helper_port)))?;
    let (admin, admin_addr) = http_server(SocketAddr::from((Ipv4Addr::LOCALHOST, cfg.admin_port)))?;

    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            books,
            registry,
            codes: HashMap::new(),
            failures: VecDeque::new(),
        }),
        data: opts.data.clone(),
        host: target.host(),
        cfg,
        clock: opts.clock.clone(),
        log: opts.log.clone(),
        pin_cost: opts.pin_cost,
        kdf: opts.kdf,
        admin_token,
        https,
        helper: helper_addr,
        peers: peers.clone(),
        stopping: stopping.clone(),
    });

    let mut threads = Vec::new();
    serve(
        "hub-api",
        backend,
        shared.clone(),
        handle_device,
        &mut threads,
    )?;
    serve(
        "hub-cert",
        helper,
        shared.clone(),
        handle_helper,
        &mut threads,
    )?;
    serve(
        "hub-admin",
        admin,
        shared.clone(),
        handle_admin,
        &mut threads,
    )?;
    {
        let (peers, stop) = (peers.clone(), stopping.clone());
        let gone = Arc::new(AtomicBool::new(false));
        threads.push(spawn("hub-tls", move || {
            wifi::accept_loop(listener, tls, backend_addr, peers, stop, gone)
        })?);
    }
    {
        // Close connections that sent nothing useful in time; renew the server
        // certificate when it's due (hourly check), without dropping devices.
        let (peers, stop, shared) = (peers, stopping.clone(), shared.clone());
        let keys = opts.keys;
        threads.push(spawn("hub-upkeep", move || {
            let mut last_check = shared.now();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
                wifi::sweep(&peers);
                let now = shared.now();
                if now - last_check >= 3_600_000 {
                    last_check = now;
                    match wifi::ensure_existing(&shared.data, &keys, &target, now / 1000) {
                        Ok(m) if m.renewed => {
                            let _ = resolver.set(&m);
                            (shared.log)("certificate renewed");
                        }
                        Ok(_) => {}
                        Err(e) => (shared.log)(&e),
                    }
                }
            }
        })?);
    }
    Ok(Running {
        https,
        helper: helper_addr,
        admin: admin_addr,
        info: material.info,
        ca_der: material.ca_der,
        stopping,
        threads,
    })
}

// ----- answering ------------------------------------------------------------------------------

fn header(k: &str, v: &str) -> Option<Header> {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).ok()
}

fn send(shared: &Shared, req: Request, status: u16, body: &Value) {
    let line = format!(
        "{} {} {status}",
        req.method(),
        req.url().split('?').next().unwrap_or("")
    );
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let mut resp = Response::from_data(bytes).with_status_code(status);
    for (k, v) in [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Connection", "close"),
    ] {
        if let Some(h) = header(k, v) {
            resp.add_header(h);
        }
    }
    let _ = req.respond(resp);
    (shared.log)(&line);
}

fn fail(shared: &Shared, req: Request, status: u16, message: &str) {
    send(shared, req, status, &json!({ "error": message }));
}

fn read_json<T: for<'de> Deserialize<'de>>(req: &mut Request) -> Result<T, String> {
    let mut buf = Vec::new();
    req.as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "the request couldn't be read".to_string())?;
    if buf.len() as u64 > MAX_BODY {
        return Err("the request is too large".into());
    }
    serde_json::from_slice(&buf).map_err(|e| format!("the request isn't valid ({e})"))
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer ").map(str::to_string))
}

fn query(url: &str, key: &str) -> Option<u64> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

fn store_status(e: &StoreError) -> u16 {
    match e {
        StoreError::Invalid(_) | StoreError::Record(_) => 400,
        StoreError::WrongPassword | StoreError::InvalidPassword(_) => 403,
        StoreError::BookInUse => 409,
        _ => 500,
    }
}

/// The open book with `id`, or why it can't be used.
/// The open book's name, if one is open.
fn open_name(st: &State) -> Option<String> {
    st.books
        .iter()
        .find(|b| b.store.is_some())
        .map(|b| b.name.clone().unwrap_or_else(|| b.file.clone()))
}

fn book_by_id<'a>(state: &'a mut State, id: &str) -> Result<&'a mut Store, (u16, &'static str)> {
    open_book(state, id)?
        .store
        .as_mut()
        .ok_or((423, "This book is locked."))
}

fn open_book<'a>(state: &'a mut State, id: &str) -> Result<&'a mut Book, (u16, &'static str)> {
    state
        .books
        .iter_mut()
        .find(|b| b.info().is_some_and(|i| i.book_id == id))
        .ok_or((
            423,
            "This book is locked or unavailable on the hub. Unlock it on the hub computer.",
        ))
}

/// Add a record once per request key, after the agreement engine has checked it. A period
/// close also saves the statement PDF and, in a Hyperbook, seals it into the history.
fn add_record(
    shared: &Shared,
    st: &mut State,
    book_id: &str,
    key: &str,
    author: Author,
    kind: &str,
    body: Value,
) -> Result<(sk_chain::Record, bool), (u16, String)> {
    let now = shared.now();
    let book = open_book(st, book_id).map_err(|(s, m)| (s, m.to_string()))?;
    let store = book
        .store
        .as_mut()
        .ok_or((423, "This book is locked.".to_string()))?;
    let err = |e: StoreError| (store_status(&e), e.to_string());
    if let Some(rec) = store.made_by(key).map_err(err)? {
        return Ok((rec, false));
    }
    let candidate = store
        .candidate(now, author.clone(), kind, body.clone())
        .map_err(err)?;
    book.ledger.check(&candidate).map_err(|e| (422, e.0))?;
    let (rec, fresh) = store
        .append_once(key, now, author, kind, body)
        .map_err(err)?;
    let effects = book.ledger.apply(&rec).map_err(|e| (500, e.0))?;
    if let Some(statement) = effects.statement {
        let info = store.info().clone();
        let pdf = sk_report::statement_pdf(&statement, &info.name, info.currency)
            .map_err(|e| (500, e))?;
        let dir = shared.data.join(STATEMENTS_DIR).join(&info.book_id);
        std::fs::create_dir_all(&dir).map_err(|e| (500, e.to_string()))?;
        let name = format!(
            "{}-{}-{}.pdf",
            statement.agreement.0, statement.period.first, statement.period.last
        );
        util::write_private(&dir.join(name), &pdf).map_err(|e| (500, e))?;
        if info.hyperbook {
            let seal = store
                .seal(
                    now,
                    statement.agreement.0,
                    &statement.period.first.to_string(),
                    &statement.period.last.to_string(),
                    Some(sk_chain::Hash::of(&pdf)),
                )
                .map_err(err)?;
            let _ = book.ledger.apply(&seal);
        }
    }
    if fresh {
        daily_backup(shared, store);
    }
    Ok((rec, fresh))
}

/// One backup a day, made with the day's first change (`<name> backup <date>.sharekeep` in
/// the backups folder): what guided recovery restores from if the book ever breaks.
fn daily_backup(shared: &Shared, store: &Store) {
    let label = sk_ledger::day_of(shared.now()).to_string();
    let dir = shared.data.join(BACKUPS_DIR);
    let name = sk_store::file_name_for(&format!("{} backup {label}", store.info().name));
    if dir.join(&name).exists() {
        return;
    }
    if let Err(e) = store.backup(&dir, &label) {
        (shared.log)(&format!("the daily backup failed: {e}"));
    }
}

// ----- devices --------------------------------------------------------------------------------

fn normalise_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn new_code() -> StoreResult<String> {
    let r = util::random::<8>()?;
    Ok(r.iter()
        .map(|b| char::from(CROCKFORD[usize::from(*b) % 32]))
        .collect())
}

fn handle_device(shared: &Shared, mut req: Request) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().as_str().to_string();
    let remote_port = req.remote_addr().map(|a| a.port());
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/v1/health") => send(
            shared,
            req,
            200,
            &json!({"app": "sharekeep-hub", "version": env!("CARGO_PKG_VERSION")}),
        ),
        ("GET", p) if !p.starts_with("/api/") => serve_webapp(shared, req, p),
        ("POST", "/api/v1/pair") => {
            let r: Result<PairRequest, String> = read_json(&mut req);
            match r {
                Ok(p) => match pair(shared, &p) {
                    Ok(a) => send(shared, req, 200, &json!(a)),
                    Err((s, m)) => fail(shared, req, s, &m),
                },
                Err(e) => fail(shared, req, 400, &e),
            }
        }
        _ => {
            let Some(token) = bearer(&req) else {
                return fail(shared, req, 401, "This device isn't paired with the hub.");
            };
            let device = {
                let st = shared.lock();
                st.registry.by_token(&token)
            };
            let device =
                match device {
                    Ok(Some(d)) => d,
                    Ok(None) => return fail(
                        shared,
                        req,
                        401,
                        "This device isn't paired with the hub (or was removed). Pair it again.",
                    ),
                    Err(e) => return fail(shared, req, 500, &e),
                };
            // Paired: no unauthenticated time limit on this connection.
            if let Some(port) = remote_port {
                wifi::set_deadline(&shared.peers, port, None);
            }
            let _ = shared.lock().registry.touch(device.id, shared.now());
            device_call(shared, req, &method, &path, &url, &device);
        }
    }
}

/// The web app's files (phones and PC browsers).
fn serve_webapp(shared: &Shared, req: Request, path: &str) {
    let Some(a) = sk_webapp::asset(path) else {
        return fail(shared, req, 404, "No such page.");
    };
    let mut resp = Response::from_data(a.body.to_vec()).with_status_code(200);
    let mut headers = vec![("Content-Type", a.content_type), ("Connection", "close")];
    headers.extend(sk_webapp::HEADERS);
    for (k, v) in headers {
        if let Some(h) = header(k, v) {
            resp.add_header(h);
        }
    }
    let _ = req.respond(resp);
    (shared.log)(&format!("GET {path} 200"));
}

/// The device's book state, its currency, and today (the hub's day), for the web app's views.
fn device_view<T>(
    shared: &Shared,
    d: &Device,
    f: impl FnOnce(&Ledger, Currency, NaiveDate) -> T,
) -> Result<T, (u16, String)> {
    let today = sk_ledger::day_of(shared.now());
    let mut st = shared.lock();
    let book = open_book(&mut st, &d.book_id).map_err(|(s, m)| (s, m.to_string()))?;
    let cur = book
        .info()
        .map(|i| i.currency)
        .ok_or((423, "This book is locked.".to_string()))?;
    Ok(f(&book.ledger, cur, today))
}

#[derive(Deserialize)]
struct PreviewRequest {
    expense: sk_ledger::bodies::Expense,
}

#[derive(Deserialize)]
struct ExamplesRequest {
    rule: sk_split::method::Rule,
}

fn device_call(shared: &Shared, mut req: Request, method: &str, path: &str, url: &str, d: &Device) {
    let me = MemberId(d.member);
    match (method, path) {
        ("GET", "/api/v1/view") => {
            match device_view(shared, d, |l, cur, today| {
                json!(sk_view::view::snapshot(l, me, cur, today))
            }) {
                Ok(mut v) => {
                    // the head, so the web app can remember it and notice a rewrite
                    let head = book_by_id(&mut shared.lock(), &d.book_id)
                        .ok()
                        .and_then(|s| s.head());
                    v["head"] = json!(head);
                    send(shared, req, 200, &v)
                }
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", "/api/v1/preview") => {
            let b: PreviewRequest = match read_json(&mut req) {
                Ok(b) => b,
                Err(e) => return fail(shared, req, 400, &e),
            };
            // a rule by days or usage asks for them: the answer says what's needed
            match device_view(shared, d, |l, cur, _| {
                let needs = sk_view::view::needs(l, &b.expense);
                let no_rule = sk_view::view::rule_missing(l, &b.expense);
                (
                    needs,
                    no_rule,
                    sk_view::view::preview(l, me, cur, b.expense),
                )
            }) {
                Ok((needs, _, Ok(rows))) => {
                    send(shared, req, 200, &json!({ "rows": rows, "needs": needs }))
                }
                // no rule divides it yet: why, so the app offers to propose one
                Ok((_, Some(no_rule), Err(_))) => send(
                    shared,
                    req,
                    200,
                    &json!({ "rows": [], "error": no_rule, "no_rule": true }),
                ),
                Ok((Some(needs), None, Err(why))) => send(
                    shared,
                    req,
                    200,
                    &json!({ "rows": [], "needs": needs, "error": why }),
                ),
                Ok((None, None, Err(why))) => fail(shared, req, 422, &why),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", "/api/v1/examples") => {
            let b: ExamplesRequest = match read_json(&mut req) {
                Ok(b) => b,
                Err(e) => return fail(shared, req, 400, &e),
            };
            match device_view(shared, d, |l, cur, _| {
                sk_view::view::examples(&b.rule, l, cur)
            }) {
                Ok(Ok(ex)) => {
                    let ex: Vec<Value> = ex
                        .into_iter()
                        .map(|(amount, rows)| json!({"amount": amount, "rows": rows}))
                        .collect();
                    send(shared, req, 200, &json!({ "examples": ex }))
                }
                Ok(Err(why)) => fail(shared, req, 422, &why),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("GET", "/api/v1/me") => {
            let mut st = shared.lock();
            let book = match book_by_id(&mut st, &d.book_id) {
                Ok(s) => facts(s.info()),
                Err((s, m)) => {
                    drop(st);
                    return fail(shared, req, s, m);
                }
            };
            drop(st);
            send(
                shared,
                req,
                200,
                &json!(MeAnswer {
                    device_id: d.id,
                    device: d.name.clone(),
                    member: d.member,
                    book,
                }),
            )
        }
        ("GET", "/api/v1/records") => {
            let from = query(url, "from").unwrap_or(0);
            let limit = query(url, "limit").unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE);
            let mut st = shared.lock();
            let answer = match book_by_id(&mut st, &d.book_id) {
                Ok(s) => s
                    .records(from, limit)
                    .map(|r| json!({"records": r, "head": s.head()}))
                    .map_err(|e| (store_status(&e), e.to_string())),
                Err((s, m)) => Err((s, m.to_string())),
            };
            drop(st);
            match answer {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", "/api/v1/records") => {
            let sub: SubmitRequest = match read_json(&mut req) {
                Ok(s) => s,
                Err(e) => return fail(shared, req, 400, &e),
            };
            if !DEVICE_KINDS.contains(&sub.kind.as_str()) {
                return fail(
                    shared,
                    req,
                    403,
                    &format!("A device can't add \"{}\" records.", sub.kind),
                );
            }
            if !sub.body.is_object() {
                return fail(shared, req, 400, "A record's body must be an object.");
            }
            let author = Author {
                member: Some(sk_core::MemberId(d.member)),
                device: d.name.clone(),
            };
            let mut st = shared.lock();
            let result = add_record(
                shared,
                &mut st,
                &d.book_id,
                &format!("d{}:{}", d.id, sub.client_id),
                author,
                &sub.kind,
                sub.body,
            );
            drop(st);
            match result {
                Ok((rec, fresh)) => send(shared, req, if fresh { 201 } else { 200 }, &json!(rec)),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("GET", "/api/v1/verify") => {
            // Verify, answered by the hub from the whole history
            let mut st = shared.lock();
            let answer = match book_by_id(&mut st, &d.book_id) {
                Ok(s) => {
                    let hyper = s.info().hyperbook;
                    s.records(0, u64::MAX)
                        .map(|r| json!(sk_ledger::audit::audit(&r, hyper)))
                        .map_err(|e| (store_status(&e), e.to_string()))
                }
                Err((s, m)) => Err((s, m.to_string())),
            };
            drop(st);
            match answer {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("GET", p) if p.starts_with("/api/v1/csv/") => {
            // /api/v1/csv/<agreement>/<first day>
            let rest: Vec<&str> = p.trim_start_matches("/api/v1/csv/").split('/').collect();
            let a = rest.first().and_then(|a| a.parse::<u32>().ok());
            let first = rest
                .get(1)
                .and_then(|f| f.parse::<sk_core::NaiveDate>().ok());
            let (Some(a), Some(first)) = (a, first) else {
                return fail(shared, req, 404, "No such period.");
            };
            let head = book_by_id(&mut shared.lock(), &d.book_id)
                .ok()
                .filter(|s| s.info().hyperbook)
                .and_then(|s| s.head());
            match device_view(shared, d, |l, _, _| {
                sk_view::view::csv(l, sk_core::AgreementId(a), first, head)
            }) {
                Ok(Ok((file, text))) => {
                    send(shared, req, 200, &json!({"file": file, "text": text}))
                }
                Ok(Err(why)) => fail(shared, req, 404, &why),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("GET", p) if p.starts_with("/api/v1/statements/") => {
            // /api/v1/statements/<agreement>/<first day>
            let rest: Vec<&str> = p
                .trim_start_matches("/api/v1/statements/")
                .split('/')
                .collect();
            let (Some(a), Some(first)) = (rest.first(), rest.get(1)) else {
                return fail(shared, req, 404, "No such statement.");
            };
            let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'-');
            if !ok(a) || !ok(first) {
                return fail(shared, req, 404, "No such statement.");
            }
            let dir = shared.data.join(STATEMENTS_DIR).join(&d.book_id);
            let prefix = format!("{a}-{first}-");
            let found = std::fs::read_dir(&dir).ok().and_then(|it| {
                it.flatten().map(|e| e.path()).find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                })
            });
            match found.and_then(|p| std::fs::read(p).ok()) {
                Some(pdf) => {
                    let mut resp = Response::from_data(pdf).with_status_code(200);
                    for (k, v) in [
                        ("Content-Type", "application/pdf"),
                        ("Cache-Control", "no-store"),
                    ] {
                        if let Some(h) = header(k, v) {
                            resp.add_header(h);
                        }
                    }
                    let _ = req.respond(resp);
                    (shared.log)(&format!("GET {p} 200"));
                }
                None => fail(shared, req, 404, "No such statement."),
            }
        }
        _ => fail(shared, req, 404, "No such request."),
    }
}

fn pair(shared: &Shared, p: &PairRequest) -> Result<PairAnswer, (u16, String)> {
    let wrong = || {
        (
            403,
            "That pairing code or PIN isn't right, or the code has expired. Make a new code on the hub computer."
                .to_string(),
        )
    };
    let device_name = p.device.trim();
    if device_name.is_empty() || device_name.len() > 64 {
        return Err((
            400,
            "Give this device a name of up to 64 characters.".into(),
        ));
    }
    let code = normalise_code(&p.code);
    let now = shared.now();
    let pin_hash = {
        let mut st = shared.lock();
        while st
            .failures
            .front()
            .is_some_and(|t| now - *t > FAILURE_WINDOW_MS)
        {
            st.failures.pop_front();
        }
        if st.failures.len() >= MAX_FAILURES {
            return Err((
                429,
                "Too many wrong tries: pairing is paused for 10 minutes.".into(),
            ));
        }
        let valid = st.codes.get(&code).is_some_and(|c| c.expires > now);
        let Some(pin_hash) = st.registry.get("pin").map_err(|e| (500, e))? else {
            return Err((
                409,
                "Set the household PIN on the hub computer before pairing.".into(),
            ));
        };
        if !valid {
            st.failures.push_back(now);
            return Err(wrong());
        }
        pin_hash
    };
    // Slow on purpose (Argon2id); the hub stays responsive meanwhile.
    let pin_ok = pin::verify(p.pin.trim(), &pin_hash);
    let mut st = shared.lock();
    if !pin_ok {
        st.failures.push_back(now);
        let burn = match st.codes.get_mut(&code) {
            Some(c) => {
                c.tries += 1;
                c.tries >= CODE_TRIES
            }
            None => false,
        };
        if burn {
            st.codes.remove(&code);
        }
        return Err(wrong());
    }
    let Some(pairing) = st.codes.remove(&code) else {
        return Err(wrong());
    };
    let book = book_by_id(&mut st, &pairing.book_id)
        .map(|s| facts(s.info()))
        .map_err(|(s, m)| (s, m.to_string()))?;
    let token = util::hex(&util::random::<32>().map_err(|e| (500, e))?);
    let device_id = st
        .registry
        .add(&token, &pairing.book_id, pairing.member, device_name, now)
        .map_err(|e| (500, e))?;
    drop(st);
    (shared.log)(&format!("device {device_id} paired"));
    Ok(PairAnswer {
        token,
        device_id,
        member: pairing.member,
        book,
    })
}

// ----- the certificate page ----------------------------------------------------------------------

fn handle_helper(shared: &Shared, req: Request) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let pair = req
        .url()
        .split_once('?')
        .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("pair=")))
        .map(str::to_string);
    let ua = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("User-Agent"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let info = wifi::read_info(&shared.data).unwrap_or_default();
    let ca = std::fs::read_to_string(shared.data.join(wifi::CA_CERT_FILE)).unwrap_or_default();
    let (status, ctype, body, extra): (u16, &str, Vec<u8>, Option<(&str, String)>) =
        match path.as_str() {
            "/" => (
                200,
                "text/html; charset=utf-8",
                wifi::page_for(&info, shared.https.port(), &ua, pair.as_deref()).into_bytes(),
                None,
            ),
            wifi::CA_CRT_PATH => (
                200,
                "application/x-x509-ca-cert",
                ca.into_bytes(),
                Some((
                    "Content-Disposition",
                    "attachment; filename=\"sharekeep-ca.crt\"".into(),
                )),
            ),
            wifi::CA_MOBILECONFIG_PATH => (
                200,
                "application/x-apple-aspen-config",
                wifi::from_pem(&ca)
                    .map(|der| wifi::mobileconfig(&der, &info.ca_name))
                    .unwrap_or_default()
                    .into_bytes(),
                None,
            ),
            _ => (
                302,
                "text/plain",
                Vec::new(),
                Some((
                    "Location",
                    format!("https://{}:{}/", shared.host, shared.https.port()),
                )),
            ),
        };
    let mut resp = Response::from_data(body).with_status_code(status);
    let mut headers: Vec<(&str, String)> = vec![("Content-Type", ctype.into())];
    headers.extend(
        wifi::HELPER_HEADERS
            .iter()
            .map(|(k, v)| (*k, v.to_string())),
    );
    headers.extend(extra);
    for (k, v) in headers {
        if let Some(h) = header(k, &v) {
            resp.add_header(h);
        }
    }
    let line = format!("{} {} {status}", req.method(), path);
    let _ = req.respond(resp);
    (shared.log)(&line);
}

// ----- the hub computer ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewBookRequest {
    name: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    hyperbook: bool,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    recovery_key: bool,
}

#[derive(Deserialize)]
struct UnlockRequest {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    recovery_key: Option<String>,
}

#[derive(Deserialize)]
struct RecoveryRequest {
    backup: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    recovery_key: Option<String>,
}

fn secret_of(password: &Option<String>, recovery_key: &Option<String>) -> Option<Secret> {
    match (
        password.clone().filter(|p| !p.is_empty()),
        recovery_key.clone(),
    ) {
        (Some(p), _) => Some(Secret::Password(p)),
        (None, Some(r)) => Some(Secret::RecoveryKey(r)),
        _ => None,
    }
}

type Answer = Result<Value, (u16, String)>;

/// The broken book's path and id (read without checking its history).
fn broken_book(
    shared: &Shared,
    file: &str,
    secret: Option<&Secret>,
) -> Result<(PathBuf, Store), (u16, String)> {
    let st = shared.lock();
    let book = st
        .books
        .iter()
        .find(|b| b.file == file)
        .ok_or((404, "No such book on the hub.".to_string()))?;
    if book.broken.is_none() {
        return Err((409, "That book isn't broken.".to_string()));
    }
    let path = book.path.clone();
    drop(st);
    let s =
        Store::open_for_recovery(&path, secret).map_err(|e| (store_status(&e), e.to_string()))?;
    Ok((path, s))
}

/// The recovery screen: what's wrong, and every backup of the book, checked.
fn recovery(shared: &Shared, file: &str, b: &UnlockRequest) -> Answer {
    let secret = secret_of(&b.password, &b.recovery_key);
    let (_, store) = broken_book(shared, file, secret.as_ref())?;
    let problem = match store.verify() {
        Err(e) => e.to_string(),
        Ok(_) => "The history checks out now.".into(),
    };
    let info = store.info().clone();
    drop(store);
    let backups = sk_store::scan_backups(
        &shared.data.join(BACKUPS_DIR),
        &info.book_id,
        secret.as_ref(),
    );
    Ok(json!({"name": info.name, "problem": problem, "backups": backups}))
}

/// Records in the broken book after a backup's last record: what restoring it would lose.
fn lost(shared: &Shared, file: &str, b: &RecoveryRequest) -> Answer {
    let secret = secret_of(&b.password, &b.recovery_key);
    let backup = backup_path(shared, &b.backup)?;
    let head = Store::open_for_recovery(&backup, secret.as_ref())
        .map_err(|e| (store_status(&e), e.to_string()))?
        .head()
        .map_or(0, |h| h.seq);
    let (_, store) = broken_book(shared, file, secret.as_ref())?;
    let records = store
        .records(head + 1, u64::MAX)
        .map_err(|e| (store_status(&e), e.to_string()))?;
    let rows: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "seq": r.seq,
                "kind": r.kind,
                "when": sk_ledger::day_of(r.ts),
                "member": r.author.member,
                "device": r.author.device,
                "body": r.body,
            })
        })
        .collect();
    // the same list in plain words, to save and enter again later
    let mut text = format!(
        "ShareKeep: records not in the backup {}\r\nThe backup ends at record {head}. These records are as the broken book has them: they can't be checked.\r\n\r\n",
        b.backup
    );
    for r in &records {
        text.push_str(&format!(
            "record {} · {} · {} · by {} on {} · {}\r\n",
            r.seq,
            sk_ledger::day_of(r.ts),
            r.kind,
            r.author
                .member
                .map_or("the hub computer".to_string(), |m| format!(
                    "member {}",
                    m.0
                )),
            r.author.device,
            r.body
        ));
    }
    Ok(json!({"after": head, "records": rows, "verified": false, "text": text}))
}

/// The broken history as it is (one record per line), with where it breaks.
fn export_broken(shared: &Shared, file: &str, b: &UnlockRequest) -> Answer {
    let secret = secret_of(&b.password, &b.recovery_key);
    let (_, store) = broken_book(shared, file, secret.as_ref())?;
    let at = store.verify().err().and_then(|e| match e {
        StoreError::HistoryBroken(b) => Some(b.at),
        _ => None,
    });
    let text = store
        .export_history()
        .map_err(|e| (store_status(&e), e.to_string()))?;
    Ok(json!({"broken_at": at, "text": text}))
}

fn backup_path(shared: &Shared, name: &str) -> Result<PathBuf, (u16, String)> {
    let ok = !name.is_empty()
        && !name.contains(['/', '\\'])
        && !name.starts_with('.')
        && name.ends_with(".sharekeep");
    if !ok {
        return Err((400, "That isn't a backup's name.".into()));
    }
    let p = shared.data.join(BACKUPS_DIR).join(name);
    if !p.is_file() {
        return Err((404, "No such backup.".into()));
    }
    Ok(p)
}

/// Carry on from a backup: a separate restored copy becomes the book the hub works in, the
/// broken file is set aside (kept, renamed), and a `restore` record says so in the history.
fn restore(shared: &Shared, file: &str, b: &RecoveryRequest) -> Answer {
    let secret = secret_of(&b.password, &b.recovery_key);
    let backup = backup_path(shared, &b.backup)?;
    let (broken_path, broken) = broken_book(shared, file, secret.as_ref())?;
    let broken_at = broken.verify().err().and_then(|e| match e {
        StoreError::HistoryBroken(x) => Some(x.at),
        _ => None,
    });
    let book_id = broken.info().book_id.clone();
    drop(broken);
    if let Some(name) = open_name(&shared.lock()) {
        return Err((
            409,
            format!("{name} is open. Close it first, then restore."),
        ));
    }
    let label = sk_ledger::day_of(shared.now()).to_string();
    let books = shared.data.join(BOOKS_DIR);
    let copy = sk_store::restore_copy(&backup, &books, &label)
        .map_err(|e| (store_status(&e), e.to_string()))?;
    let mut store = match Store::open(&copy, secret.as_ref()) {
        Ok(s) if s.info().book_id == book_id => s,
        Ok(_) => {
            let _ = std::fs::remove_file(&copy);
            return Err((400, "That backup is of another book.".into()));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&copy);
            return Err((store_status(&e), format!("That backup can't be used: {e}")));
        }
    };
    let head = store
        .head()
        .ok_or((500, "The backup is empty.".to_string()))?;
    let aside = sk_store::set_aside_broken(&broken_path, &label)
        .map_err(|e| (store_status(&e), e.to_string()))?;
    let body = json!({
        "backup": b.backup,
        "restored_to": {"seq": head.seq, "hash": head.hash.to_hex()},
        "broken_at": broken_at,
    });
    store
        .append(shared.now(), Author::hub(), "restore", body)
        .map_err(|e| (store_status(&e), e.to_string()))?;
    let copy_file = copy
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut st = shared.lock();
    st.books.retain(|x| x.file != file);
    let book = Book::open(copy_file.clone(), copy, store, &shared.log);
    let summary = book.summary();
    st.books.push(book);
    let saved = st.registry.set(CURRENT_BOOK, &copy_file);
    drop(st);
    if let Err(e) = saved {
        (shared.log)(&e);
    }
    Ok(json!({
        "book": summary,
        "set_aside": aside.file_name().map(|n| n.to_string_lossy().into_owned()),
        "restored_to": head.seq,
    }))
}

#[derive(Deserialize)]
struct AdminRecord {
    client_id: String,
    kind: String,
    body: Value,
    #[serde(default)]
    member: Option<u32>,
}

#[derive(Deserialize)]
struct PairingRequest {
    book_id: String,
    member: u32,
}

#[derive(Deserialize)]
struct PinRequest {
    pin: String,
}

#[derive(Deserialize)]
struct SettingRequest {
    key: String,
    value: Value,
}

#[derive(Deserialize)]
struct BackupRequest {
    label: String,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle_admin(shared: &Shared, req: Request) {
    let ok = bearer(&req).is_some_and(|t| util::ct_eq(t.as_bytes(), shared.admin_token.as_bytes()));
    if !ok {
        return fail(shared, req, 401, "The admin token is missing or wrong.");
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().as_str().to_string();
    let parts: Vec<String> = path
        .trim_start_matches("/admin/v1/")
        .split('/')
        .map(percent_decode)
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    admin_call(shared, req, &method, &url, &parts);
}

fn admin_call(shared: &Shared, mut req: Request, method: &str, url: &str, parts: &[&str]) {
    let now = shared.now();
    macro_rules! body {
        ($t:ty) => {
            match read_json::<$t>(&mut req) {
                Ok(b) => b,
                Err(e) => return fail(shared, req, 400, &e),
            }
        };
    }
    match (method, parts) {
        ("GET", ["status"]) => {
            let st = shared.lock();
            let books: Vec<Value> = st.books.iter().map(Book::summary).collect();
            let pin_set = st.registry.get("pin").ok().flatten().is_some();
            let devices = st.registry.list().map(|d| d.len()).unwrap_or(0);
            drop(st);
            let info = wifi::read_info(&shared.data).unwrap_or_default();
            send(
                shared,
                req,
                200,
                &json!({
                    "address": shared.cfg.address.to_string(),
                    "host": shared.host,
                    "https_port": shared.https.port(),
                    "ca_name": info.ca_name,
                    "fingerprint": info.fingerprint,
                    "pin_set": pin_set,
                    "current": open_name(&shared.lock()),
                    "devices": devices,
                    "books": books,
                }),
            )
        }
        ("POST", ["pin"]) => {
            let b = body!(PinRequest);
            if let Err(e) = pin::check(b.pin.trim()) {
                return fail(shared, req, 400, &e);
            }
            let r = pin::hash_with(b.pin.trim(), shared.pin_cost)
                .and_then(|h| shared.lock().registry.set("pin", &h));
            match r {
                Ok(()) => send(shared, req, 200, &json!({"pin_set": true})),
                Err(e) => fail(shared, req, 500, &e),
            }
        }
        ("POST", ["books"]) => {
            let b = body!(NewBookRequest);
            if let Some(name) = open_name(&shared.lock()) {
                return fail(
                    shared,
                    req,
                    409,
                    &format!("{name} is open. Close it first, then create the new book."),
                );
            }
            let code = b.currency.unwrap_or_else(|| "USD".into());
            let Some(currency) = Currency::ALL.into_iter().find(|c| c.code() == code) else {
                return fail(shared, req, 400, &format!("Unknown currency {code}."));
            };
            let encryption = b.password.map(|password| Encryption {
                password,
                recovery_key: b.recovery_key,
                kdf: shared.kdf,
            });
            let created = Store::create(
                &shared.data.join(BOOKS_DIR),
                NewBook {
                    name: b.name,
                    currency,
                    hyperbook: b.hyperbook,
                    encryption,
                    ts: now,
                    author: Author::hub(),
                },
            );
            match created {
                Ok(c) => {
                    let file = c
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let answer = json!({
                        "file": file,
                        "book": facts(c.store.info()),
                        "recovery_key": c.recovery_key,
                    });
                    let book = Book::open(file.clone(), c.path, c.store, &shared.log);
                    let mut st = shared.lock();
                    st.books.push(book);
                    let saved = st.registry.set(CURRENT_BOOK, &file);
                    drop(st);
                    if let Err(e) = saved {
                        (shared.log)(&e);
                    }
                    send(shared, req, 201, &answer)
                }
                Err(e) => fail(shared, req, store_status(&e), &e.to_string()),
            }
        }
        ("POST", ["books", file, "unlock"]) => {
            let b = body!(UnlockRequest);
            // an unencrypted book opens without a secret
            let secret = match (b.password.filter(|p| !p.is_empty()), b.recovery_key) {
                (Some(p), _) => Some(Secret::Password(p)),
                (None, Some(r)) => Some(Secret::RecoveryKey(r)),
                _ => None,
            };
            let mut st = shared.lock();
            let other = st
                .books
                .iter()
                .find(|bk| bk.store.is_some() && bk.file != *file)
                .map(|bk| bk.name.clone().unwrap_or_else(|| bk.file.clone()));
            if let Some(name) = other {
                drop(st);
                return fail(
                    shared,
                    req,
                    409,
                    &format!("{name} is open. Close it first, then open this book."),
                );
            }
            let Some(book) = st.books.iter_mut().find(|bk| bk.file == *file) else {
                drop(st);
                return fail(shared, req, 404, "No such book on the hub.");
            };
            if book.store.is_some() {
                let s = book.summary();
                drop(st);
                return send(shared, req, 200, &s);
            }
            if book.encrypted && secret.is_none() {
                drop(st);
                return fail(shared, req, 400, "Give the password or the recovery key.");
            }
            match Store::open(&book.path, secret.as_ref()) {
                Ok(s) => {
                    book.ledger = replay(&s, &shared.log);
                    book.name = Some(s.info().name.clone());
                    book.encrypted = s.info().encrypted;
                    book.store = Some(s);
                    book.problem = None;
                    let s = book.summary();
                    let saved = st.registry.set(CURRENT_BOOK, file);
                    drop(st);
                    if let Err(e) = saved {
                        (shared.log)(&e);
                    }
                    send(shared, req, 200, &s)
                }
                Err(StoreError::HistoryBroken(b)) => {
                    book.broken = Some(b.at);
                    let e = StoreError::HistoryBroken(b);
                    book.problem = Some(e.to_string());
                    drop(st);
                    fail(shared, req, 423, &e.to_string())
                }
                Err(e) => {
                    drop(st);
                    fail(shared, req, store_status(&e), &e.to_string())
                }
            }
        }
        ("POST", ["books", file, "recovery"]) => {
            let b = body!(UnlockRequest);
            match recovery(shared, file, &b) {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", file, "lost"]) => {
            let b = body!(RecoveryRequest);
            match lost(shared, file, &b) {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", file, "export-broken"]) => {
            let b = body!(UnlockRequest);
            match export_broken(shared, file, &b) {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", file, "restore"]) => {
            let b = body!(RecoveryRequest);
            match restore(shared, file, &b) {
                Ok(v) => send(shared, req, 201, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", file, "close"]) => {
            let mut st = shared.lock();
            let Some(book) = st.books.iter_mut().find(|bk| bk.file == *file) else {
                drop(st);
                return fail(shared, req, 404, "No such book on the hub.");
            };
            let id = book.info().map(|i| i.book_id.clone());
            book.close();
            let s = book.summary();
            // its pairing codes stop working; its devices wait until it's opened again
            if let Some(id) = id {
                st.codes.retain(|_, p| p.book_id != id);
            }
            let saved = st.registry.set(CURRENT_BOOK, "");
            drop(st);
            if let Err(e) = saved {
                (shared.log)(&e);
            }
            send(shared, req, 200, &s)
        }
        ("POST", ["books", id, "records"]) => {
            let b = body!(AdminRecord);
            if !(DEVICE_KINDS.contains(&b.kind.as_str()) || ADMIN_KINDS.contains(&b.kind.as_str()))
            {
                return fail(
                    shared,
                    req,
                    403,
                    &format!("\"{}\" can't be added here.", b.kind),
                );
            }
            let author = Author {
                member: b.member.map(sk_core::MemberId),
                device: "hub".into(),
            };
            let mut st = shared.lock();
            let r = add_record(
                shared,
                &mut st,
                id,
                &format!("admin:{}", b.client_id),
                author,
                &b.kind,
                b.body,
            );
            drop(st);
            match r {
                Ok((rec, fresh)) => send(shared, req, if fresh { 201 } else { 200 }, &json!(rec)),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("GET", ["books", id, "records"]) => {
            let from = query(url, "from").unwrap_or(0);
            let limit = query(url, "limit").unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE);
            let mut st = shared.lock();
            let r = match book_by_id(&mut st, id) {
                Ok(s) => s
                    .records(from, limit)
                    .map(|r| json!({"records": r, "head": s.head()}))
                    .map_err(|e| (store_status(&e), e.to_string())),
                Err((s, m)) => Err((s, m.to_string())),
            };
            drop(st);
            match r {
                Ok(v) => send(shared, req, 200, &v),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", id, "settings"]) => {
            let b = body!(SettingRequest);
            let mut st = shared.lock();
            let r = match open_book(&mut st, id) {
                Ok(book) => match book.store.as_mut() {
                    Some(s) => s
                        .set_setting(now, Author::hub(), &b.key, b.value)
                        .map_err(|e| (store_status(&e), e.to_string()))
                        .inspect(|rec| {
                            let _ = book.ledger.apply(rec);
                        }),
                    None => Err((423, "This book is locked.".to_string())),
                },
                Err((s, m)) => Err((s, m.to_string())),
            };
            drop(st);
            match r {
                Ok(rec) => send(shared, req, 201, &json!(rec)),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["books", id, "backup"]) => {
            let b = body!(BackupRequest);
            let mut st = shared.lock();
            let r = match book_by_id(&mut st, id) {
                Ok(s) => s
                    .backup(&shared.data.join(BACKUPS_DIR), &b.label)
                    .map_err(|e| (store_status(&e), e.to_string())),
                Err((s, m)) => Err((s, m.to_string())),
            };
            drop(st);
            match r {
                Ok(p) => send(shared, req, 201, &json!({"path": p})),
                Err((s, m)) => fail(shared, req, s, &m),
            }
        }
        ("POST", ["pairing"]) => {
            let b = body!(PairingRequest);
            let mut st = shared.lock();
            let known = match book_by_id(&mut st, &b.book_id) {
                Ok(s) => s.has_record_with("member-add", "id", i64::from(b.member)),
                Err((s, m)) => {
                    drop(st);
                    return fail(shared, req, s, m);
                }
            };
            match known {
                Ok(true) => {}
                Ok(false) => {
                    drop(st);
                    return fail(shared, req, 400, "Add that member to the book first.");
                }
                Err(e) => {
                    drop(st);
                    return fail(shared, req, 500, &e.to_string());
                }
            }
            let code = match new_code() {
                Ok(c) => c,
                Err(e) => {
                    drop(st);
                    return fail(shared, req, 500, &e);
                }
            };
            let expires = now + CODE_MINUTES * 60_000;
            st.codes.retain(|_, p| p.expires > now);
            st.codes.insert(
                code.clone(),
                Pairing {
                    book_id: b.book_id,
                    member: b.member,
                    expires,
                    tries: 0,
                },
            );
            drop(st);
            let shown = format!("{}-{}", &code[..4], &code[4..]);
            send(
                shared,
                req,
                201,
                &json!({
                    "code": shown,
                    "expires": expires,
                    "url": format!("https://{}:{}/#pair={code}", shared.host, shared.https.port()),
                    // the QR code: the certificate page first, which then links to the url
                    "qr": format!("http://{}:{}/?pair={code}", shared.cfg.address, shared.helper.port()),
                    "certificate_page": format!("http://{}:{}/", shared.cfg.address, shared.helper.port()),
                }),
            )
        }
        ("GET", ["devices"]) => {
            let r = shared.lock().registry.list();
            match r {
                Ok(d) => send(shared, req, 200, &json!({"devices": d})),
                Err(e) => fail(shared, req, 500, &e),
            }
        }
        ("POST", ["devices", id, "revoke"]) => {
            let Ok(id) = id.parse::<u32>() else {
                return fail(shared, req, 404, "No such device.");
            };
            let r = shared.lock().registry.revoke(id, now);
            match r {
                Ok(true) => send(shared, req, 200, &json!({"revoked": id})),
                Ok(false) => fail(
                    shared,
                    req,
                    404,
                    "No such device (or it was already removed).",
                ),
                Err(e) => fail(shared, req, 500, &e),
            }
        }
        _ => fail(shared, req, 404, "No such request."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_typed_forgivingly() {
        assert_eq!(normalise_code("abcd-efgh"), "ABCDEFGH");
        assert_eq!(normalise_code(" O1L I "), "0111");
        let c = new_code().unwrap();
        assert_eq!(c.len(), 8);
        assert_eq!(normalise_code(&c), c);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("Home%20(2).sharekeep"), "Home (2).sharekeep");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn query_values() {
        assert_eq!(query("/x?from=5&limit=9", "limit"), Some(9));
        assert_eq!(query("/x", "from"), None);
    }
}
