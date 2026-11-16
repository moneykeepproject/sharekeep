//! The background worker: every call to the hub and every disk write happens here, never
//! on the UI thread. The UI sends [`Job`]s; each comes back as an [`Outcome`].

use serde_json::{json, Value};
use sk_chain::Record;
use sk_client::{api::PairAnswer, Client, HubAddress, Outbox, Replica, SyncError, OUTBOX_KINDS};
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
}

struct Paired {
    client: Client,
    replica: Replica,
    outbox: Outbox,
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
            Job::Connect { client, book_dir } => match open_local(&book_dir) {
                Ok((replica, outbox)) => {
                    self.last_head = None;
                    self.paired = Some(Paired {
                        client,
                        replica,
                        outbox,
                    });
                    Outcome::Synced(self.sync())
                }
                Err(e) => Outcome::Synced(Synced {
                    problem: Some(e),
                    ..Default::default()
                }),
            },
            Job::Sync => Outcome::Synced(self.sync()),
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
        }
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
        match p.replica.sync(&p.client, now_ms()) {
            Ok(_) => {}
            Err(SyncError::Offline(e)) => {
                offline = matches!(e, sk_client::CallError::Offline(_));
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
pub fn admin_call(
    addr: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
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
