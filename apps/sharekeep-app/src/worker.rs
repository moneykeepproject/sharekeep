//! The background worker: every call to the hub and every disk write happens here, never
//! on the UI thread. The UI sends [`Job`]s; each comes back as an [`Outcome`].

use serde_json::{json, Value};
use sk_chain::Head;
use sk_chain::Record;
use sk_client::{
    api::PairAnswer, Client, HubAddress, Outbox, Replica, RestoreOffer, SyncError, OUTBOX_KINDS,
};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Work for the worker.
#[derive(Debug)]
pub enum Job {
    /// Fetch the hub's certificate from its helper page.
    FindHub(String),
    Pair {
        hub: HubAddress,
        code: String,
        pin: String,
        device: String,
    },
    /// Use this paired connection (after pairing, or at start).
    Connect {
        client: Client,
        book_dir: PathBuf,
        /// A checkpoint given when pairing: the history must contain it.
        expect: Option<Head>,
    },
    /// Give up the checkpoint given when pairing.
    ForgetCheckpoint,
    /// Attach a picture or PDF from this computer to an expense (a photo is shrunk first).
    Attach {
        seq: u64,
        path: PathBuf,
    },
    /// Fetch an attached file, check it against the book, and open it in the computer's viewer.
    OpenFile {
        sha256: String,
        name: String,
    },
    /// Send what waits, then fetch what's new.
    Sync,
    /// A change: queued if it may wait offline, otherwise sent now or refused.
    Send {
        kind: String,
        body: Value,
    },
    /// Save a closed period's statement to `path`.
    Statement {
        agreement: u32,
        first: String,
        path: PathBuf,
    },
    DismissRefused,
    /// Carry on from a backup the household restored, and say so to the hub.
    AcceptRestore(RestoreOffer),
    /// Does the history still contain this checkpoint? (This device's copy, then the hub.)
    CheckCheckpoint(Head),
    /// A file's SHA-256 (Check a statement).
    HashFile(PathBuf),
    /// Write text to a file (a checkpoint, a spreadsheet).
    SaveText {
        path: PathBuf,
        text: String,
    },
    /// Write bytes to a file (a checkpoint as PDF).
    SaveBytes {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// A call to the hub computer's own admin port.
    Admin {
        method: &'static str,
        path: String,
        body: Option<Value>,
        then: AdminThen,
    },
}

/// What to do with an admin answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminThen {
    Status,
    /// A book's whole history (the hub computer's view of members and agreements).
    Records,
    BookMade,
    Devices,
    Code,
    Done(String),
    /// The recovery screen for a broken book.
    Recovery(String),
    /// What restoring a backup would lose: (book file, backup).
    Lost(String, String),
    /// Save the answer's `text` to this file.
    SaveText(PathBuf),
}

#[derive(Debug)]
pub enum Outcome {
    Found(Result<(HubAddress, String), String>),
    Paired(Result<(Client, PairAnswer), String>),
    Synced(Synced),
    /// A change was sent (or queued); the message is for the person.
    Sent(Result<String, String>),
    Saved(Result<PathBuf, String>),
    Admin(AdminThen, Result<Value, String>),
    /// A checkpoint's answer: (kept everywhere, the words).
    Checked(bool, String),
    Hashed(Result<String, String>),
}

/// The local copy after a sync.
#[derive(Debug, Default)]
pub struct Synced {
    /// The whole history, when it changed.
    pub records: Option<Vec<Record>>,
    pub pending: usize,
    pub refused: Vec<String>,
    /// Why the hub couldn't be reached, or what's wrong with its history.
    pub problem: Option<String>,
    pub offline: bool,
    /// A restored backup waiting to be accepted.
    pub restore: Option<RestoreOffer>,
    /// The last record this device has, and when it last synced (ms).
    pub seen: Option<(u64, i64)>,
    /// A checkpoint given when pairing that the hub's history doesn't contain.
    pub checkpoint_failed: bool,
}

struct Paired {
    client: Client,
    replica: Replica,
    outbox: Outbox,
    /// Where opened attachments are kept (`<book folder>/files`).
    files: PathBuf,
}

pub struct Worker {
    paired: Option<Paired>,
    admin: Option<(SocketAddr, String)>,
    last_head: Option<u64>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse "192.168.1.20" (or with a port, which is ignored: the hub's ports are fixed).
pub fn parse_ip(text: &str) -> Result<IpAddr, String> {
    let t = text.trim();
    let host = t.rsplit_once(':').map_or(t, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            t
        }
    });
    host.parse()
        .map_err(|_| format!("\"{t}\" isn't an address like 192.168.1.20."))
}

impl Worker {
    /// `admin`: the hub computer's loopback port and token, when this is the hub computer.
    pub fn new(admin: Option<(SocketAddr, String)>) -> Self {
        Worker {
            paired: None,
            admin,
            last_head: None,
        }
    }

    /// Run until the UI goes away.
    pub fn run(mut self, jobs: mpsc::Receiver<Job>, out: impl Fn(Outcome)) {
        while let Ok(job) = jobs.recv() {
            if let Some(o) = self.handle(job) {
                out(o);
            }
        }
    }

    pub fn handle(&mut self, job: Job) -> Option<Outcome> {
        Some(match job {
            Job::FindHub(text) => Outcome::Found(find_hub(&text)),
            Job::Pair {
                hub,
                code,
                pin,
                device,
            } => Outcome::Paired(
                Client::pair(hub, code.trim(), pin.trim(), device.trim())
                    .map_err(|e| e.to_string()),
            ),
            Job::Connect {
                client,
                book_dir,
                expect,
            } => match open_local(&book_dir) {
                Ok((mut replica, outbox)) => {
                    if let Some(h) = expect {
                        if let Err(e) = replica.expect(h) {
                            return Some(Outcome::Synced(Synced {
                                problem: Some(e),
                                ..Default::default()
                            }));
                        }
                    }
                    self.last_head = None;
                    self.paired = Some(Paired {
                        client,
                        replica,
                        outbox,
                        files: book_dir.join("files"),
                    });
                    Outcome::Synced(self.sync())
                }
                Err(e) => Outcome::Synced(Synced {
                    problem: Some(e),
                    ..Default::default()
                }),
            },
            Job::Sync => Outcome::Synced(self.sync()),
            Job::Attach { seq, path } => {
                let Some(p) = self.paired.as_ref() else {
                    return Some(Outcome::Sent(Err("This device isn't paired yet.".into())));
                };
                let offline = |e: sk_client::CallError| match e {
                    sk_client::CallError::Offline(_) => {
                        format!("{e}. Pictures need the hub: try again when you're home.")
                    }
                    _ => e.to_string(),
                };
                let r = prepare_file(&path).and_then(|(bytes, mime, name)| {
                    let f = p.client.upload(&bytes, mime).map_err(offline)?;
                    p.client
                        .submit_now(
                            "attachment",
                            &json!({"target": seq, "sha256": f.sha256, "name": name, "mime": f.mime, "size": f.size}),
                        )
                        .map_err(offline)
                });
                match r {
                    Ok(_) => Outcome::Synced(self.sync()),
                    Err(e) => Outcome::Sent(Err(e)),
                }
            }
            Job::OpenFile { sha256, name } => {
                let Some(p) = self.paired.as_ref() else {
                    return Some(Outcome::Sent(Err("This device isn't paired yet.".into())));
                };
                Outcome::Sent(open_attachment(p, &sha256, &name).map(|_| String::new()))
            }
            Job::Send { kind, body } => {
                let Some(p) = self.paired.as_mut() else {
                    return Some(Outcome::Sent(Err("This device isn't paired yet.".into())));
                };
                let r = if OUTBOX_KINDS.contains(&kind.as_str()) {
                    p.outbox.push(&kind, body, now_ms()).map(|_| String::new())
                } else {
                    p.client
                        .submit_now(&kind, &body)
                        .map(|_| String::new())
                        .map_err(|e| match e {
                            sk_client::CallError::Offline(_) => format!(
                                "{e}. This change needs the hub: try again when you're home."
                            ),
                            _ => e.to_string(),
                        })
                };
                match r {
                    // send it (if it waits in the outbox) and bring the result back at once
                    Ok(_) => Outcome::Synced(self.sync()),
                    Err(e) => Outcome::Sent(Err(e)),
                }
            }
            Job::Statement {
                agreement,
                first,
                path,
            } => Outcome::Saved(self.statement(agreement, &first, &path)),
            Job::DismissRefused => {
                if let Some(p) = self.paired.as_mut() {
                    let _ = p.outbox.dismiss_refused();
                }
                Outcome::Synced(self.status_only())
            }
            Job::AcceptRestore(offer) => {
                let Some(p) = self.paired.as_mut() else {
                    return Some(Outcome::Synced(Synced::default()));
                };
                let r = p
                    .replica
                    .accept_restore(&p.client, &offer, now_ms())
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        p.client
                            .submit_now("restore-accept", &json!({"restore": offer.seq}))
                            .map_err(|e| e.to_string())
                    });
                match r {
                    Ok(_) => {
                        self.last_head = None;
                        Outcome::Synced(self.sync())
                    }
                    Err(e) => Outcome::Sent(Err(e)),
                }
            }
            Job::CheckCheckpoint(head) => self.check_checkpoint(head),
            Job::HashFile(path) => Outcome::Hashed(
                std::fs::read(&path)
                    .map(|b| sk_chain::Hash::of(&b).to_hex())
                    .map_err(|e| format!("The file can't be read ({e}).")),
            ),
            Job::ForgetCheckpoint => {
                if let Some(p) = self.paired.as_mut() {
                    let _ = p.replica.forget_expected();
                }
                Outcome::Synced(self.sync())
            }
            Job::SaveBytes { path, bytes } => Outcome::Saved(
                std::fs::write(&path, bytes)
                    .map(|_| path.clone())
                    .map_err(|e| format!("the file can't be saved ({e})")),
            ),
            Job::SaveText { path, text } => Outcome::Saved(
                std::fs::write(&path, text)
                    .map(|_| path.clone())
                    .map_err(|e| format!("the file can't be saved ({e})")),
            ),
            Job::Admin {
                method,
                path,
                body,
                then: AdminThen::SaveText(to),
            } => Outcome::Saved(self.admin(method, &path, body).and_then(|v| {
                std::fs::write(&to, v["text"].as_str().unwrap_or(""))
                    .map(|_| to.clone())
                    .map_err(|e| format!("the file can't be saved ({e})"))
            })),
            Job::Admin {
                method,
                path,
                body,
                then,
            } => Outcome::Admin(then, self.admin(method, &path, body)),
        })
    }

    fn status_only(&self) -> Synced {
        let Some(p) = &self.paired else {
            return Synced::default();
        };
        Synced {
            records: None,
            pending: p.outbox.pending().len(),
            refused: p
                .outbox
                .refused()
                .iter()
                .map(|r| format!("{}: {}", r.item.kind, r.reason))
                .collect(),
            problem: None,
            offline: false,
            restore: None,
            seen: p
                .replica
                .head()
                .map(|h| (h.seq, p.replica.last_synced().unwrap_or(0))),
            // still waiting for the pairing checkpoint (a sync says why)
            checkpoint_failed: p.replica.expected().is_some() && p.replica.head().is_none(),
        }
    }

    fn check_checkpoint(&self, head: Head) -> Outcome {
        use sk_chain::checkpoint::{check, explain, Found};
        let Some(p) = &self.paired else {
            return Outcome::Checked(false, "This device isn't paired yet.".into());
        };
        let here = check(p.replica.records(), head);
        let mut lines = vec![format!("On this device: {}", explain(&here, head))];
        let hub = match p.client.records(head.seq, 1) {
            Ok(page) => {
                let found = match page.records.first() {
                    Some(r) if r.seq == head.seq && r.hash == head.hash => Found::Kept,
                    Some(r) if r.seq == head.seq => Found::Rewritten,
                    _ => Found::Missing {
                        have: page.head.map(|h| h.seq),
                    },
                };
                lines.push(format!("On the hub: {}", explain(&found, head)));
                Some(found)
            }
            Err(e) => {
                lines.push(format!("On the hub: not checked ({e})."));
                None
            }
        };
        let ok = here == Found::Kept && hub.is_none_or(|f| f == Found::Kept);
        Outcome::Checked(ok, lines.join("\n"))
    }

    fn sync(&mut self) -> Synced {
        let Some(p) = self.paired.as_mut() else {
            return Synced::default();
        };
        let flush = p.outbox.flush(&p.client);
        let mut problem = flush.stopped.as_ref().map(|e| e.to_string());
        let mut offline = flush
            .stopped
            .as_ref()
            .is_some_and(|e| matches!(e, sk_client::CallError::Offline(_)));
        let mut restore = None;
        let mut checkpoint_failed = false;
        match p.replica.sync(&p.client, now_ms()) {
            Ok(_) => {}
            Err(SyncError::Offline(e)) => {
                offline = matches!(e, sk_client::CallError::Offline(_));
                problem = Some(e.to_string());
            }
            Err(SyncError::Restored(o)) => restore = Some(o),
            Err(e @ SyncError::CheckpointNotKept(_)) => {
                checkpoint_failed = true;
                problem = Some(e.to_string());
            }
            Err(e) => problem = Some(e.to_string()),
        }
        let head = p.replica.head().map(|h| h.seq);
        let records = if head != self.last_head {
            self.last_head = head;
            Some(p.replica.records().to_vec())
        } else {
            None
        };
        let mut s = self.status_only();
        s.restore = restore;
        s.checkpoint_failed = checkpoint_failed;
        s.records = records;
        s.problem = problem;
        s.offline = offline;
        s
    }

    fn statement(&self, agreement: u32, first: &str, path: &Path) -> Result<PathBuf, String> {
        let p = self
            .paired
            .as_ref()
            .ok_or("This device isn't paired yet.")?;
        let pdf = p
            .client
            .statement(agreement, first)
            .map_err(|e| e.to_string())?;
        std::fs::write(path, pdf).map_err(|e| format!("the file can't be saved ({e})"))?;
        Ok(path.to_path_buf())
    }

    fn admin(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let (addr, token) = self
            .admin
            .as_ref()
            .ok_or("This computer isn't the hub computer.")?;
        admin_call(*addr, token, method, path, body)
    }
}

/// A call to the hub's loopback admin port.
/// A path with every byte that can't appear as-is in a request line escaped: a book's file
/// name can have spaces ("Our Home.sharekeep"), which would otherwise break the request (the
/// hub answered 400). Slashes and the query's `?`, `=` and `&` are kept.
pub fn escape_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'/'
            | b'?'
            | b'='
            | b'&' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn admin_call(
    addr: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let path = &escape_path(path);
    let auth = format!("Bearer {token}");
    let bytes = body.map(|b| b.to_string().into_bytes());
    let mut headers = vec![
        ("Authorization", auth.as_str()),
        ("Accept", "application/json"),
    ];
    if bytes.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    let (status, answer) =
        sk_client::tls::http_call(addr, method, path, &headers, bytes.as_deref()).map_err(|e| {
            format!("The hub isn't running on this computer ({e}). Start it, then try again.")
        })?;
    let v: Value = serde_json::from_slice(&answer).unwrap_or(json!({}));
    if (200..300).contains(&status) {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("The hub answered {status}.")))
    }
}

fn find_hub(text: &str) -> Result<(HubAddress, String), String> {
    let ip = parse_ip(text)?;
    let der = sk_client::fetch_ca(SocketAddr::new(ip, sk_client::HELPER_PORT))
        .map_err(|e| format!("No ShareKeep hub answered at {ip} ({e}). Check the address and that both are on the home Wi-Fi."))?;
    let fp = sk_client::fingerprint(&der);
    Ok((
        HubAddress::new(
            SocketAddr::new(ip, sk_client::HTTPS_PORT),
            ip.to_string(),
            &der,
        ),
        fp,
    ))
}

fn open_local(dir: &Path) -> Result<(Replica, Outbox), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("the book's folder can't be made ({e})"))?;
    Ok((Replica::open(dir)?, Outbox::open(&dir.join("outbox.json"))?))
}

/// The longest side a photo keeps when it's attached.
const PHOTO_EDGE: u32 = 1600;

/// A file from this computer, ready to attach: (bytes, kind, name). A photo larger than
/// [`PHOTO_EDGE`] or 600 KB is turned upright and shrunk to a JPEG; a PDF goes as it is.
pub fn prepare_file(path: &Path) -> Result<(Vec<u8>, &'static str, String), String> {
    use std::io::Read;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(64 * 1024 * 1024).read_to_end(&mut bytes))
        .map_err(|e| format!("{name} can't be read ({e})"))?;
    let Some(mime) = sk_ledger::sniff_type(&bytes) else {
        return Err("Only pictures (JPEG, PNG, WebP) and PDFs can be attached.".into());
    };
    let picture = mime == "image/jpeg" || mime == "image/png";
    if picture {
        let (img, small) = decode_upright(&bytes)?;
        if !small || bytes.len() > 600 * 1024 {
            let img = if img.width().max(img.height()) > PHOTO_EDGE {
                img.resize(
                    PHOTO_EDGE,
                    PHOTO_EDGE,
                    image::imageops::FilterType::Triangle,
                )
            } else {
                img
            };
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
                .encode_image(&img.to_rgb8())
                .map_err(|e| format!("{name} can't be shrunk ({e})"))?;
            let stem = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "photo".into());
            return Ok((out, "image/jpeg", format!("{stem}.jpg")));
        }
    }
    if bytes.len() as u64 > sk_ledger::ATTACH_MAX_BYTES {
        return Err(format!("{name} is larger than 10 MB."));
    }
    Ok((bytes, mime, name))
}

/// A picture turned the way the camera held it, and whether it was already small.
fn decode_upright(bytes: &[u8]) -> Result<(image::DynamicImage, bool), String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| "That picture can't be read.".to_string())?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| "That picture can't be read.".to_string())?;
    img.apply_orientation(orientation);
    let small = img.width().max(img.height()) <= PHOTO_EDGE
        && orientation == image::metadata::Orientation::NoTransforms;
    Ok((img, small))
}

/// Fetch an attached file, check it is the one the book names, keep it in the book's folder
/// and open it with the computer's own viewer.
fn open_attachment(p: &Paired, sha256: &str, name: &str) -> Result<(), String> {
    if !sk_ledger::is_sha256_hex(sha256) {
        return Err("That file's fingerprint isn't valid.".into());
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let dir = p.files.join(&sha256[..16]);
    let path = dir.join(if safe.trim_matches('.').is_empty() {
        "file".to_string()
    } else {
        safe
    });
    let cached = std::fs::read(&path)
        .ok()
        .filter(|b| sk_chain::Hash::of(b).to_hex() == sha256);
    if cached.is_none() {
        let bytes = p.file(sha256)?;
        if sk_chain::Hash::of(&bytes).to_hex() != sha256 {
            return Err("The hub's copy of this file isn't the one that was attached (its fingerprint doesn't match the book). Ask the household what happened.".into());
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, &bytes).map_err(|e| format!("the file can't be kept ({e})"))?;
    }
    open_with_viewer(&path)
}

impl Paired {
    fn file(&self, sha256: &str) -> Result<Vec<u8>, String> {
        self.client.file(sha256).map_err(|e| match e {
            sk_client::CallError::Offline(_) => {
                format!("{e}. Attached files are kept by the hub: try again when you're home.")
            }
            _ => e.to_string(),
        })
    }
}

/// Open a file in the computer's own viewer (no shell, no PowerShell).
fn open_with_viewer(path: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::process::Command::new(PathBuf::from(root).join("explorer.exe"))
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).spawn().map(|_| ()).map_err(|e| {
        format!(
            "the file couldn't be opened ({e}); it is in {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn a_big_photo_is_shrunk_and_a_pdf_goes_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("receipt.png");
        image::RgbImage::from_pixel(3200, 2400, image::Rgb([200, 120, 40]))
            .save(&big)
            .unwrap();
        let (bytes, mime, name) = prepare_file(&big).unwrap();
        assert_eq!((mime, name.as_str()), ("image/jpeg", "receipt.jpg"));
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (1600, 1200));
        assert!(bytes.len() < 600 * 1024);

        let pdf = dir.path().join("bill.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").unwrap();
        let (bytes, mime, name) = prepare_file(&pdf).unwrap();
        assert_eq!(
            (mime, name.as_str(), bytes.len()),
            ("application/pdf", "bill.pdf", 15)
        );

        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(prepare_file(&txt).unwrap_err().contains("Only pictures"));
    }
}
