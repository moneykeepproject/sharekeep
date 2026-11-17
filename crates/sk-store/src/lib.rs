//! ShareKeep book files.
//!
//! A book is one SQLite database: its settings (`meta`) and its history (`records`), which
//! is append-only (triggers refuse any change or deletion) and hash-chained with
//! [`sk_chain`]. In a Hyperbook the chain is verified every time the book opens and
//! periods are sealed into it; the flag is set when the book is made and never changes.
//!
//! An encrypted book (optional, chosen when the book is made) is never written in plain
//! form: it lives in an in-memory database and every change saves the whole encrypted
//! image atomically, keeping the previous file as `<file>.prev`. The format is MoneyKeep's
//! (Argon2id password key and recovery key wrapping one data key; XChaCha20-Poly1305).
//!
//! New books, backups and restored copies are made under a free name and never replace a
//! file. Nothing here opens a file dialog: the household service keeps books in its own
//! folder and names them from the name the user typed.

pub mod crypt;
pub mod newfile;

use crypt::{Header, KdfParams, Key, SlotKind};
use fs4::fs_std::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, MAIN_DB};
use serde_json::Value;
use sk_chain::{Author, Broken, Hash, Head, Record, RecordError};
use sk_core::{Currency, MemberId};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub use crypt::Secret;

/// The file extension of a book.
pub const EXTENSION: &str = "sharekeep";
/// The schema this build writes.
pub const SCHEMA_VERSION: u32 = 4;
const FORMAT: &str = "sharekeep-book";

/// Why a book can't be made, opened or changed.
#[derive(Debug)]
pub enum StoreError {
    Io {
        action: &'static str,
        path: String,
        message: String,
    },
    Sqlite(String),
    /// The file isn't a ShareKeep book.
    NotABook,
    /// Another program (another hub) has the book open.
    BookInUse,
    /// The book is encrypted and no password was given.
    NeedsPassword,
    WrongPassword,
    InvalidPassword(String),
    /// The encrypted file is damaged.
    BookDamaged(String),
    /// The encrypted file asks for key-derivation settings ShareKeep never makes.
    UnusualKdf,
    Corrupt(String),
    /// The book was made by a newer ShareKeep.
    NewerVersion {
        found: u32,
    },
    /// A Hyperbook's history doesn't verify.
    HistoryBroken(Broken),
    Record(RecordError),
    Invalid(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io {
                action,
                path,
                message,
            } => write!(f, "couldn't {action} {path}: {message}"),
            StoreError::Sqlite(m) => write!(f, "database error: {m}"),
            StoreError::NotABook => write!(f, "This file isn't a ShareKeep book."),
            StoreError::BookInUse => write!(
                f,
                "This book is already open in another ShareKeep (another household hub)."
            ),
            StoreError::NeedsPassword => write!(f, "This book is encrypted: enter its password."),
            StoreError::WrongPassword => write!(f, "That password or recovery key doesn't open this book."),
            StoreError::InvalidPassword(m) => write!(f, "{m}"),
            StoreError::BookDamaged(m) if m.is_empty() => write!(f, "This book's file is damaged."),
            StoreError::BookDamaged(m) => write!(f, "This book's file is damaged ({m})."),
            StoreError::UnusualKdf => {
                write!(f, "This book's password settings aren't ones ShareKeep makes.")
            }
            StoreError::Corrupt(m) => write!(f, "{m}"),
            StoreError::NewerVersion { found } => write!(
                f,
                "This book was made by a newer ShareKeep (book version {found}); update ShareKeep to open it."
            ),
            StoreError::HistoryBroken(b) => write!(f, "This Hyperbook's {b}."),
            StoreError::Record(e) => write!(f, "{e}"),
            StoreError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, _) = &e {
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) {
                return StoreError::BookInUse;
            }
            if f.code == rusqlite::ErrorCode::NotADatabase {
                return StoreError::NotABook;
            }
        }
        StoreError::Sqlite(e.to_string())
    }
}

impl From<RecordError> for StoreError {
    fn from(e: RecordError) -> Self {
        StoreError::Record(e)
    }
}

fn io_err(action: &'static str, path: &Path, e: &dyn fmt::Display) -> StoreError {
    StoreError::Io {
        action,
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

/// Encryption for a new book.
pub struct Encryption {
    pub password: String,
    /// Make a recovery key too (shown once, to be written down).
    pub recovery_key: bool,
    pub kdf: KdfParams,
}

impl fmt::Debug for Encryption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encryption")
            .field("recovery_key", &self.recovery_key)
            .finish_non_exhaustive()
    }
}

/// What a new book starts with.
#[derive(Debug)]
pub struct NewBook {
    /// The name the user typed; the file name is made from it.
    pub name: String,
    pub currency: Currency,
    /// Make this a Hyperbook. Can't be changed later.
    pub hyperbook: bool,
    pub encryption: Option<Encryption>,
    /// The hub's clock, milliseconds since 1970 UTC.
    pub ts: i64,
    pub author: Author,
}

/// A book's fixed facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookInfo {
    pub book_id: String,
    pub name: String,
    pub currency: Currency,
    pub hyperbook: bool,
    pub encrypted: bool,
    pub created: i64,
}

/// What [`Store::create`] made.
#[derive(Debug)]
pub struct Created {
    pub store: Store,
    pub path: PathBuf,
    /// The recovery key, when one was asked for: shown once, never stored.
    pub recovery_key: Option<String>,
}

/// Book settings members can change; each change is a history record.
pub const SETTINGS: &[&str] = &[
    // interest on held deposits, basis points a year (default 0)
    "deposit_interest_bp",
    // "in_balances" (default) or "held_aside"
    "dispute_mode",
    // late entries accepted for this many days after a period ends (default 7)
    "grace_days",
    // a household run by one keeper: their approval alone is enough
    "one_keeper",
    // the book's time zone (IANA name), chosen when the book is made: its days are counted
    // there (UTC for books made before it existed)
    "time_zone",
];

/// The kind of a settings record.
pub const SETTING: &str = "setting";

struct Encrypted {
    path: PathBuf,
    /// Handle on the current file, holding the exclusive lock.
    file: File,
    header: Header,
    data_key: Key,
    /// The last saved file, to roll the in-memory book back after a failed save.
    last: Vec<u8>,
}

enum Backing {
    Plain { path: PathBuf },
    Encrypted(Box<Encrypted>),
}

/// An open book.
pub struct Store {
    conn: Connection,
    backing: Backing,
    info: BookInfo,
    head: Option<Record>,
    /// Tests only: make the next encrypted save fail.
    fail_next_save: bool,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path())
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Whether a book file is plain SQLite or encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookKind {
    Plain,
    Encrypted,
}

/// Windows' error for reading bytes another handle has locked.
const ERROR_LOCK_VIOLATION: i32 = 33;

/// What kind of book `path` holds, from its first bytes.
pub fn book_kind(path: &Path) -> StoreResult<BookKind> {
    let mut head = [0u8; 16];
    let mut f = File::open(path).map_err(|e| io_err("open", path, &e))?;
    let n = f.read(&mut head).map_err(|e| {
        if cfg!(windows) && e.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            StoreError::BookInUse
        } else {
            io_err("open", path, &e)
        }
    })?;
    kind_of(head.get(..n).unwrap_or(&[]))
}

fn kind_of(head: &[u8]) -> StoreResult<BookKind> {
    if head.starts_with(crypt::MAGIC) {
        Ok(BookKind::Encrypted)
    } else if head.starts_with(crypt::SQLITE_MAGIC) {
        Ok(BookKind::Plain)
    } else {
        Err(StoreError::NotABook)
    }
}

/// A file name for a book from the name the user typed: letters, digits, spaces and
/// `-_.,()'&` kept, anything else replaced, at most 80 characters, and never a name
/// Windows reserves.
pub fn file_name_for(name: &str) -> String {
    let mut s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.,()'&".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    s = s.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    if s.is_empty() {
        s = "Book".into();
    }
    let upper = s.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem)
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        s = format!("Book {s}");
    }
    format!("{s}.{EXTENSION}")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `<file>.prev`: the encrypted book as it was before its last save.
pub fn prev_path(path: &Path) -> PathBuf {
    sibling(path, ".prev")
}

/// Open options that let a locked, open file be renamed on Windows.
fn options() -> OpenOptions {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut o = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        o.share_mode(0x1 | 0x2 | 0x4);
    }
    o
}

fn lock(file: &File, path: &Path) -> StoreResult<()> {
    match file.try_lock_exclusive() {
        Ok(true) => Ok(()),
        Ok(false) => Err(StoreError::BookInUse),
        Err(e) => Err(io_err("lock", path, &e)),
    }
}

fn random_tag() -> StoreResult<String> {
    Ok(crypt::random::<6>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Write `bytes` as `path` atomically (a locked temp file, synced, the old file kept as
/// `<file>.prev`, then the rename). Returns the new file's handle, holding the lock.
fn write_atomic(path: &Path, bytes: &[u8], fail: bool) -> StoreResult<File> {
    let tmp = sibling(path, &format!(".tmp-{}", random_tag()?));
    let result = (|| -> std::io::Result<File> {
        let mut f = options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if fail {
            return Err(std::io::Error::other("simulated write failure"));
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        match f.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => return Err(std::io::Error::other("the new file is locked")),
            Err(e) => return Err(e),
        }
        let prev = prev_path(path);
        let moved = path.exists();
        if moved {
            std::fs::rename(path, &prev)?;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            if moved {
                let _ = std::fs::rename(&prev, path);
            }
            return Err(e);
        }
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(f)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err("save", path, &e)
    })
}

/// Write `bytes` into a new file at `path` or the next free numbered name; never
/// replaces a file. Returns where it went.
fn write_new(path: &Path, bytes: &[u8]) -> StoreResult<PathBuf> {
    let reserved = newfile::reserve_free(path).map_err(|e| io_err("create", path, &e))?;
    let tmp = sibling(path, &format!(".part-{}", random_tag()?));
    let written = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        reserved.release();
        return Err(io_err("write", path, &e));
    }
    reserved.fill_from(&tmp).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err("create", path, &e)
    })
}

fn memory_connection() -> StoreResult<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(conn)
}

fn image_of(conn: &Connection) -> StoreResult<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(conn.serialize(MAIN_DB)?.to_vec()))
}

fn load_image(conn: &mut Connection, image: &[u8]) -> StoreResult<()> {
    conn.deserialize_read_exact(MAIN_DB, image, image.len(), false)?;
    Ok(())
}

/// Schema changes, in order: entry `i` takes a book from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // 0 → 1: the first schema
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE records (
         seq    INTEGER PRIMARY KEY,
         ts     INTEGER NOT NULL,
         member INTEGER,
         device TEXT NOT NULL,
         kind   TEXT NOT NULL,
         body   TEXT NOT NULL,
         prev   BLOB NOT NULL,
         hash   BLOB NOT NULL
     );
     CREATE TRIGGER records_no_update BEFORE UPDATE ON records
         BEGIN SELECT RAISE(ABORT, 'the history is append-only'); END;
     CREATE TRIGGER records_no_delete BEFORE DELETE ON records
         BEGIN SELECT RAISE(ABORT, 'the history is append-only'); END;",
    // 1 → 2: which record a device's request made, so a resent request (an outbox
    // retry) never adds a second record
    "CREATE TABLE client_ids (client_id TEXT PRIMARY KEY, seq INTEGER NOT NULL);",
    // 2 → 3: no table changes. The history may now hold record kinds a version-2 ShareKeep
    // doesn't know (`recurring`, and in a Hyperbook `restore` and `restore-accept` after a
    // guided recovery); an older ShareKeep refuses the book ("made by a newer ShareKeep")
    // instead of skipping those records and showing wrong balances.
    "INSERT OR REPLACE INTO meta (key, value) VALUES ('record_kinds', '3');",
    // 3 → 4: no table changes. The history may now hold `attachment` records (a picture or
    // PDF on an expense; the hub keeps the file beside the book), which a version-3 ShareKeep
    // doesn't know.
    "INSERT OR REPLACE INTO meta (key, value) VALUES ('record_kinds', '4');",
];

fn schema_version(conn: &Connection) -> StoreResult<u32> {
    let has_meta: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if !has_meta {
        return Ok(0);
    }
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    v.and_then(|s| s.parse().ok())
        .ok_or_else(|| StoreError::Corrupt("the book has no version".into()))
}

/// A book from a newer ShareKeep can't be read, even for recovery.
fn refuse_newer(conn: &Connection) -> StoreResult<()> {
    match schema_version(conn)? {
        v if v > SCHEMA_VERSION => Err(StoreError::NewerVersion { found: v }),
        _ => Ok(()),
    }
}

/// Bring the schema up to [`SCHEMA_VERSION`], in one transaction.
fn migrate(conn: &mut Connection, migrations: &[&str]) -> StoreResult<()> {
    let from = schema_version(conn)?;
    let to = migrations.len() as u32;
    if from > to {
        return Err(StoreError::NewerVersion { found: from });
    }
    if from == to {
        return Ok(());
    }
    let tx = conn.transaction()?;
    for m in migrations.iter().skip(from as usize) {
        tx.execute_batch(m)?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
        [to.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> StoreResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

fn read_info(conn: &Connection, encrypted: bool) -> StoreResult<BookInfo> {
    if get_meta(conn, "format")?.as_deref() != Some(FORMAT) {
        return Err(StoreError::NotABook);
    }
    let need = |k: &str| -> StoreResult<String> {
        get_meta(conn, k)?.ok_or_else(|| StoreError::Corrupt(format!("the book has no {k}")))
    };
    let code = need("currency")?;
    let currency = Currency::ALL
        .into_iter()
        .find(|c| c.code() == code)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown currency {code}")))?;
    Ok(BookInfo {
        book_id: need("book_id")?,
        name: need("name")?,
        currency,
        hyperbook: need("hyperbook")? == "1",
        encrypted,
        created: need("created")?
            .parse()
            .map_err(|_| StoreError::Corrupt("bad creation time".into()))?,
    })
}

fn row_to_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Record, bool)> {
    let seq: i64 = r.get(0)?;
    let member: Option<i64> = r.get(2)?;
    let body: String = r.get(5)?;
    let prev: Vec<u8> = r.get(6)?;
    let hash: Vec<u8> = r.get(7)?;
    let to_hash = |v: &[u8]| -> Option<Hash> { v.try_into().ok().map(Hash) };
    let body: Option<Value> = serde_json::from_str(&body).ok();
    let ok = body.is_some();
    let (prev, hash) = match (to_hash(&prev), to_hash(&hash)) {
        (Some(p), Some(h)) => (p, h),
        _ => (Hash::ZERO, Hash::ZERO),
    };
    Ok((
        Record {
            seq: seq as u64,
            ts: r.get(1)?,
            author: Author {
                member: member.map(|m| MemberId(m as u32)),
                device: r.get(3)?,
            },
            kind: r.get(4)?,
            body: body.unwrap_or(Value::Null),
            prev,
            hash,
        },
        ok,
    ))
}

const RECORD_COLUMNS: &str = "seq, ts, member, device, kind, body, prev, hash";

fn load_records(conn: &Connection, from: u64, limit: u64) -> StoreResult<Vec<Record>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RECORD_COLUMNS} FROM records WHERE seq >= ?1 ORDER BY seq LIMIT ?2"
    ))?;
    let rows = stmt.query_map(
        [from as i64, limit.min(i64::MAX as u64) as i64],
        row_to_record,
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (rec, readable) = row?;
        if !readable {
            return Err(StoreError::HistoryBroken(Broken {
                at: rec.seq,
                problem: sk_chain::Problem::Unreadable("its body isn't JSON".into()),
            }));
        }
        out.push(rec);
    }
    Ok(out)
}

fn last_record(conn: &Connection) -> StoreResult<Option<Record>> {
    let rec = conn
        .query_row(
            &format!("SELECT {RECORD_COLUMNS} FROM records ORDER BY seq DESC LIMIT 1"),
            [],
            row_to_record,
        )
        .optional()?;
    Ok(rec.map(|(r, _)| r))
}

fn insert_record(conn: &Connection, r: &Record) -> StoreResult<()> {
    let body = String::from_utf8(sk_chain::canonical_json(&r.body)?)
        .map_err(|_| StoreError::Corrupt("record body isn't text".into()))?;
    conn.execute(
        &format!("INSERT INTO records ({RECORD_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
        rusqlite::params![
            r.seq as i64,
            r.ts,
            r.author.member.map(|m| i64::from(m.0)),
            r.author.device,
            r.kind,
            body,
            r.prev.0.to_vec(),
            r.hash.0.to_vec(),
        ],
    )?;
    Ok(())
}

/// Build a header with a password slot and optionally a recovery slot; returns the
/// recovery key's display form.
fn new_header(enc: &Encryption, data_key: &Key) -> StoreResult<(Header, Option<String>)> {
    crypt::check_password_rules(&enc.password)?;
    let mut h = Header {
        kdf: enc.kdf,
        salt: crypt::random::<16>()?,
        slots: Vec::new(),
    };
    let pk = crypt::derive(&enc.password, enc.kdf, &h.salt)?;
    h.slots
        .push(crypt::wrap(&h, SlotKind::Password, &pk, data_key)?);
    let mut shown = None;
    if enc.recovery_key {
        let rk = Zeroizing::new(crypt::random::<32>()?);
        h.slots
            .push(crypt::wrap(&h, SlotKind::Recovery, &rk, data_key)?);
        shown = Some(crypt::format_recovery_key(&rk));
    }
    Ok((h, shown))
}

impl Store {
    /// Make a new book in `dir`, named from `new.name` (the next free numbered name if
    /// that file exists). Never opens a file dialog and never replaces a file.
    pub fn create(dir: &Path, new: NewBook) -> StoreResult<Created> {
        let mut conn = memory_connection()?;
        migrate(&mut conn, MIGRATIONS)?;
        let book_id = crypt::random::<16>()?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let name = new.name.trim().to_string();
        if name.is_empty() {
            return Err(StoreError::Invalid("a book needs a name".into()));
        }
        {
            let tx = conn.transaction()?;
            for (k, v) in [
                ("format", FORMAT.to_string()),
                ("book_id", book_id.clone()),
                ("name", name.clone()),
                ("currency", new.currency.code().to_string()),
                (
                    "hyperbook",
                    if new.hyperbook { "1" } else { "0" }.to_string(),
                ),
                ("created", new.ts.to_string()),
            ] {
                tx.execute("INSERT INTO meta (key, value) VALUES (?1, ?2)", [k, &v])?;
            }
            let genesis = sk_chain::next(
                None,
                new.ts,
                new.author.clone(),
                sk_chain::GENESIS,
                serde_json::json!({
                    "format": FORMAT,
                    "book_id": book_id,
                    "name": name,
                    "currency": new.currency.code(),
                    "hyperbook": new.hyperbook,
                }),
            )?;
            insert_record(&tx, &genesis)?;
            tx.commit()?;
        }
        let target = dir.join(file_name_for(&name));
        let (bytes, recovery_key): (Zeroizing<Vec<u8>>, Option<String>) = match &new.encryption {
            None => (image_of(&conn)?, None),
            Some(enc) => {
                let data_key = crypt::random_key()?;
                let (header, shown) = new_header(enc, &data_key)?;
                let image = image_of(&conn)?;
                (
                    Zeroizing::new(crypt::encode(&header, &data_key, &image)?),
                    shown,
                )
            }
        };
        drop(conn);
        let path = write_new(&target, &bytes)?;
        let secret = new
            .encryption
            .as_ref()
            .map(|e| Secret::Password(e.password.clone()));
        let store = Store::open(&path, secret.as_ref())?;
        Ok(Created {
            store,
            path,
            recovery_key,
        })
    }

    /// Open a book. An encrypted book needs `secret`. A Hyperbook's whole history is
    /// verified first.
    pub fn open(path: &Path, secret: Option<&Secret>) -> StoreResult<Store> {
        let store = match book_kind(path)? {
            BookKind::Plain => Store::open_plain(path, true)?,
            BookKind::Encrypted => {
                Store::open_encrypted(path, secret.ok_or(StoreError::NeedsPassword)?, true)?
            }
        };
        if store.info.hyperbook {
            store.verify()?;
        }
        Ok(store)
    }

    /// Open a book without checking its history, only to read it: a Hyperbook that won't open
    /// (to list what a restore would lose, or export the broken history), or a backup. The
    /// file is left exactly as it is: an older book isn't brought up to this version. Never
    /// used for a book that's in use.
    pub fn open_for_recovery(path: &Path, secret: Option<&Secret>) -> StoreResult<Store> {
        match book_kind(path)? {
            BookKind::Plain => Store::open_plain(path, false),
            BookKind::Encrypted => {
                Store::open_encrypted(path, secret.ok_or(StoreError::NeedsPassword)?, false)
            }
        }
    }

    /// `upgrade`: bring an older book up to this version (false only for recovery, which
    /// must not change the file).
    fn open_plain(path: &Path, upgrade: bool) -> StoreResult<Store> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // One household service per book: the exclusive lock is taken now and held
        // until the book closes; a second opener gets "busy" at once.
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_millis(0))?;
        conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
        // Never add tables to some other program's database.
        if schema_version(&conn)? == 0 {
            return Err(StoreError::NotABook);
        }
        if upgrade {
            migrate(&mut conn, MIGRATIONS)?;
        } else {
            refuse_newer(&conn)?;
        }
        let info = read_info(&conn, false)?;
        let head = last_record(&conn)?;
        Ok(Store {
            conn,
            backing: Backing::Plain {
                path: path.to_path_buf(),
            },
            info,
            head,
            fail_next_save: false,
        })
    }

    fn open_encrypted(path: &Path, secret: &Secret, upgrade: bool) -> StoreResult<Store> {
        let mut file = options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| io_err("open", path, &e))?;
        lock(&file, path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io_err("read", path, &e))?;
        let (header, header_len) = crypt::parse(&bytes)?;
        let data_key = crypt::unlock(&header, secret)?;
        let image = crypt::decrypt(&bytes, header_len, &data_key)?;
        if !image.starts_with(crypt::SQLITE_MAGIC) {
            return Err(StoreError::BookDamaged(String::new()));
        }
        let mut conn = memory_connection()?;
        load_image(&mut conn, &image)?;
        if schema_version(&conn)? == 0 {
            return Err(StoreError::NotABook);
        }
        let mut store = Store {
            info: read_info(&conn, true)?,
            head: last_record(&conn)?,
            conn,
            backing: Backing::Encrypted(Box::new(Encrypted {
                path: path.to_path_buf(),
                file,
                header,
                data_key,
                last: bytes,
            })),
            fail_next_save: false,
        };
        if !upgrade {
            refuse_newer(&store.conn)?;
        } else if schema_version(&store.conn)? < SCHEMA_VERSION {
            migrate(&mut store.conn, MIGRATIONS)?;
            store.save()?;
        } else {
            migrate(&mut store.conn, MIGRATIONS)?;
        }
        Ok(store)
    }

    /// Save an encrypted book's in-memory image; on failure the in-memory book goes back
    /// to the last saved file, so memory and disk never disagree.
    fn save(&mut self) -> StoreResult<()> {
        let fail = std::mem::take(&mut self.fail_next_save);
        let Backing::Encrypted(enc) = &mut self.backing else {
            return Ok(());
        };
        let attempt = (|| -> StoreResult<(Vec<u8>, File)> {
            let image = image_of(&self.conn)?;
            let bytes = crypt::encode(&enc.header, &enc.data_key, &image)?;
            let file = write_atomic(&enc.path, &bytes, fail)?;
            Ok((bytes, file))
        })();
        match attempt {
            Ok((bytes, file)) => {
                enc.file = file;
                enc.last = bytes;
                Ok(())
            }
            Err(e) => {
                let (_, len) = crypt::parse(&enc.last)?;
                let image = crypt::decrypt(&enc.last, len, &enc.data_key)?;
                load_image(&mut self.conn, &image)?;
                self.head = last_record(&self.conn)?;
                Err(e)
            }
        }
    }

    pub fn info(&self) -> &BookInfo {
        &self.info
    }

    pub fn path(&self) -> &Path {
        match &self.backing {
            Backing::Plain { path } => path,
            Backing::Encrypted(e) => &e.path,
        }
    }

    /// The newest record's number and hash.
    pub fn head(&self) -> Option<Head> {
        self.head.as_ref().map(Record::head)
    }

    /// Add a record to the history. The household service is the only writer, so the
    /// order it appends in is the book's order.
    pub fn append(
        &mut self,
        ts: i64,
        author: Author,
        kind: &str,
        body: Value,
    ) -> StoreResult<Record> {
        if kind == sk_chain::GENESIS {
            return Err(StoreError::Invalid(
                "a book has only one first record".into(),
            ));
        }
        let rec = sk_chain::next(self.head.as_ref(), ts, author, kind, body)?;
        {
            let tx = self.conn.transaction()?;
            insert_record(&tx, &rec)?;
            tx.commit()?;
        }
        let before = self.head.replace(rec.clone());
        if let Err(e) = self.save() {
            self.head = before;
            return Err(e);
        }
        Ok(rec)
    }

    /// Append once per `client_id` (a device's id for one request): a resent request
    /// returns the record the first one made, and `false`. The author is whoever the hub
    /// says sent it, never what the request claims.
    pub fn append_once(
        &mut self,
        client_id: &str,
        ts: i64,
        author: Author,
        kind: &str,
        body: Value,
    ) -> StoreResult<(Record, bool)> {
        let ok_id = (1..=128).contains(&client_id.len())
            && client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b));
        if !ok_id {
            return Err(StoreError::Invalid("that request id isn't valid".into()));
        }
        if let Some(rec) = self.made_by(client_id)? {
            return Ok((rec, false));
        }
        if kind == sk_chain::GENESIS {
            return Err(StoreError::Invalid(
                "a book has only one first record".into(),
            ));
        }
        let rec = sk_chain::next(self.head.as_ref(), ts, author, kind, body)?;
        {
            let tx = self.conn.transaction()?;
            insert_record(&tx, &rec)?;
            tx.execute(
                "INSERT INTO client_ids (client_id, seq) VALUES (?1, ?2)",
                rusqlite::params![client_id, rec.seq as i64],
            )?;
            tx.commit()?;
        }
        let before = self.head.replace(rec.clone());
        if let Err(e) = self.save() {
            self.head = before;
            return Err(e);
        }
        Ok((rec, true))
    }

    /// The record a request id already made, if any.
    pub fn made_by(&self, client_id: &str) -> StoreResult<Option<Record>> {
        let seen: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM client_ids WHERE client_id = ?1",
                [client_id],
                |r| r.get(0),
            )
            .optional()?;
        match seen {
            Some(seq) => Ok(Some(
                self.records(seq as u64, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| StoreError::Corrupt("a request's record is missing".into()))?,
            )),
            None => Ok(None),
        }
    }

    /// The record that would be appended next (for checking it first).
    pub fn candidate(
        &self,
        ts: i64,
        author: Author,
        kind: &str,
        body: Value,
    ) -> StoreResult<Record> {
        Ok(sk_chain::next(self.head.as_ref(), ts, author, kind, body)?)
    }

    /// Whether the history has a record of `kind` whose body's `field` equals `value`
    /// (e.g. a `member-add` with that member id).
    pub fn has_record_with(&self, kind: &str, field: &str, value: i64) -> StoreResult<bool> {
        let path = format!("$.{field}");
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM records WHERE kind = ?1 AND json_extract(body, ?2) = ?3",
            rusqlite::params![kind, path, value],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Change a book setting (see [`SETTINGS`]); the change is a history record.
    pub fn set_setting(
        &mut self,
        ts: i64,
        author: Author,
        key: &str,
        value: Value,
    ) -> StoreResult<Record> {
        if !SETTINGS.contains(&key) {
            return Err(StoreError::Invalid(format!(
                "\"{key}\" isn't a book setting"
            )));
        }
        self.append(
            ts,
            author,
            SETTING,
            serde_json::json!({ "key": key, "value": value }),
        )
    }

    /// The current value of a setting: its last change, if any.
    pub fn setting(&self, key: &str) -> StoreResult<Option<Value>> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM records WHERE kind = ?1 AND json_extract(body, '$.key') = ?2
                 ORDER BY seq DESC LIMIT 1",
                [SETTING, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(body
            .and_then(|b| serde_json::from_str::<Value>(&b).ok())
            .and_then(|v| v.get("value").cloned()))
    }

    /// Seal a closed period into the history: the head at closing and the statement's hash.
    pub fn seal(
        &mut self,
        ts: i64,
        agreement: u32,
        first: &str,
        last: &str,
        statement: Option<Hash>,
    ) -> StoreResult<Record> {
        let head = self
            .head()
            .ok_or_else(|| StoreError::Corrupt("the book has no history".into()))?;
        let body = sk_chain::seal_body(agreement, first, last, head, statement);
        self.append(ts, Author::hub(), sk_chain::SEAL, body)
    }

    /// Records from `from`, at most `limit` of them, oldest first.
    pub fn records(&self, from: u64, limit: u64) -> StoreResult<Vec<Record>> {
        load_records(&self.conn, from, limit)
    }

    /// Check the whole history (always done on opening a Hyperbook).
    pub fn verify(&self) -> StoreResult<Head> {
        const PAGE: u64 = 10_000;
        let mut head: Option<Head> = None;
        let mut from = 0;
        loop {
            let page = load_records(&self.conn, from, PAGE)?;
            if page.is_empty() {
                break;
            }
            head = Some(
                if head.is_none() {
                    sk_chain::verify(&page)
                } else {
                    sk_chain::verify_after(head, &page)
                }
                .map_err(StoreError::HistoryBroken)?,
            );
            from += page.len() as u64;
        }
        let h = head.ok_or(StoreError::HistoryBroken(Broken {
            at: 0,
            problem: sk_chain::Problem::Empty,
        }))?;
        if Some(h) != self.head() {
            return Err(StoreError::HistoryBroken(Broken {
                at: h.seq,
                problem: sk_chain::Problem::WrongHash,
            }));
        }
        Ok(h)
    }

    /// The whole history in the export format (one JSON record per line), which
    /// `sk-verify` checks.
    pub fn export_history(&self) -> StoreResult<String> {
        Ok(sk_chain::export_lines(&self.records(0, u64::MAX)?))
    }

    /// The book's bytes as they would be on disk (encrypted books stay encrypted).
    fn file_bytes(&self) -> StoreResult<Vec<u8>> {
        match &self.backing {
            Backing::Plain { .. } => Ok(image_of(&self.conn)?.to_vec()),
            Backing::Encrypted(e) => Ok(e.last.clone()),
        }
    }

    /// Back the book up into `dir` as `<name> backup <label>.sharekeep` (the next free
    /// numbered name if that exists). An encrypted book's backup stays encrypted.
    pub fn backup(&self, dir: &Path, label: &str) -> StoreResult<PathBuf> {
        std::fs::create_dir_all(dir).map_err(|e| io_err("create", dir, &e))?;
        let stem = file_name_for(&format!("{} backup {label}", self.info.name));
        write_new(&dir.join(stem), &self.file_bytes()?)
    }

    /// Tests only: make the next encrypted save fail, as a full disk would.
    #[doc(hidden)]
    pub fn fail_next_save_for_tests(&mut self) {
        self.fail_next_save = true;
    }
}

/// A backup as recovery lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackupInfo {
    pub file: String,
    /// When the file was last written (ms since 1970).
    pub modified: i64,
    /// Its last record.
    pub head: Option<Head>,
    /// "ok", "broken" or "unreadable".
    pub status: String,
    /// The broken record, for "Also broken at record N".
    pub broken_at: Option<u64>,
    /// Why it can't be read.
    pub problem: Option<String>,
}

/// The backups in `dir` of the book `book_id`, newest first, each checked.
pub fn scan_backups(dir: &Path, book_id: &str, secret: Option<&Secret>) -> Vec<BackupInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.extension().is_some_and(|x| x == EXTENSION) {
            continue;
        }
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let modified = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        let info = match Store::open_for_recovery(&path, secret) {
            Ok(s) => {
                if s.info().book_id != book_id {
                    continue;
                }
                let head = s.head();
                match s.verify() {
                    Ok(_) => BackupInfo {
                        file,
                        modified,
                        head,
                        status: "ok".into(),
                        broken_at: None,
                        problem: None,
                    },
                    Err(StoreError::HistoryBroken(b)) => BackupInfo {
                        file,
                        modified,
                        head,
                        status: "broken".into(),
                        broken_at: Some(b.at),
                        problem: Some(b.to_string()),
                    },
                    Err(e) => BackupInfo {
                        file,
                        modified,
                        head,
                        status: "unreadable".into(),
                        broken_at: None,
                        problem: Some(e.to_string()),
                    },
                }
            }
            // another book's backup (or one we can't open with this password): not listed,
            // unless it can't be told apart
            Err(StoreError::NotABook) | Err(StoreError::WrongPassword) => continue,
            Err(e) => BackupInfo {
                file,
                modified,
                head: None,
                status: "unreadable".into(),
                broken_at: None,
                problem: Some(e.to_string()),
            },
        };
        out.push(info);
    }
    out.sort_by(|a, b| {
        b.head
            .map(|h| h.seq)
            .cmp(&a.head.map(|h| h.seq))
            .then(b.modified.cmp(&a.modified))
    });
    out
}

/// Move a broken book aside so it's clearly the broken one and no longer loads as a book:
/// `<file> (broken <label>).sharekeep-broken`. Returns the new path.
pub fn set_aside_broken(path: &Path, label: &str) -> StoreResult<PathBuf> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Book".into());
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem} (broken {label}).sharekeep-broken")
        } else {
            format!("{stem} (broken {label}) ({n}).sharekeep-broken")
        };
        let to = dir.join(name);
        if !to.exists() {
            std::fs::rename(path, &to).map_err(|e| io_err("rename", path, &e))?;
            return Ok(to);
        }
        n += 1;
    }
}

/// Copy a backup into `dir` as a separate book (`<name> restored <label>.sharekeep`). The
/// original and every existing file stay untouched; the copy isn't opened.
pub fn restore_copy(backup: &Path, dir: &Path, label: &str) -> StoreResult<PathBuf> {
    let bytes = std::fs::read(backup).map_err(|e| io_err("read", backup, &e))?;
    kind_of(&bytes)?;
    let stem = backup
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Book".into());
    let stem = stem.split(" backup ").next().unwrap_or(&stem).to_string();
    let name = file_name_for(&format!("{stem} restored {label}"));
    write_new(&dir.join(name), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name_for("Home"), "Home.sharekeep");
        assert_eq!(file_name_for("  Apt 4/B: 2027 "), "Apt 4_B_ 2027.sharekeep");
        assert_eq!(file_name_for(""), "Book.sharekeep");
        assert_eq!(file_name_for("..."), "Book.sharekeep");
        assert_eq!(file_name_for("con"), "Book con.sharekeep");
        assert_eq!(file_name_for("COM1"), "Book COM1.sharekeep");
        assert_eq!(file_name_for("Comfort"), "Comfort.sharekeep");
        assert_eq!(file_name_for(&"x".repeat(200)).len(), 80 + 10);
    }

    #[test]
    fn a_newer_book_is_refused() {
        let mut conn = memory_connection().unwrap();
        migrate(&mut conn, MIGRATIONS).unwrap();
        conn.execute(
            "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        assert!(matches!(
            migrate(&mut conn, MIGRATIONS),
            Err(StoreError::NewerVersion { found: 99 })
        ));
    }

    #[test]
    fn migrations_run_in_order_from_any_version() {
        let mut conn = memory_connection().unwrap();
        migrate(&mut conn, &MIGRATIONS[..1]).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 1);
        let mut later = MIGRATIONS.to_vec();
        later.push("CREATE TABLE extra (x INTEGER);");
        migrate(&mut conn, &later).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION + 1);
        conn.execute("INSERT INTO extra (x) VALUES (1)", [])
            .unwrap();
    }
}
