//! The UI thread's side: the book's state, the open sheets, and filling the screens.
//! Everything slow goes to the [`crate::worker`] as a [`Job`].

use crate::config::AppConfig;
use crate::worker::{AdminThen, Job, Outcome};
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};
use sk_client::{Client, HubAddress};
use sk_core::{AgreementId, Currency, MemberId};
use sk_ledger::Ledger;
use sk_ui::phone as view_phone;
use sk_ui::view::{self, RuleDraft, EDITOR_METHODS};
use sk_ui::*;
use slint::{Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// A new agreement's schedule from the options: how (0 weekly, 1 every 2 weeks, 2 monthly,
/// 3 every N days), the day (of the month, or N), the weekday (0 = Monday) and the start (empty:
/// the start of the period today is in). Returns the schedule's JSON and its start.
pub fn agreement_schedule(
    how: i32,
    day: &str,
    weekday: i32,
    start: &str,
    today: NaiveDate,
) -> Result<(Value, NaiveDate), String> {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let typed = if start.is_empty() {
        None
    } else {
        Some(
            NaiveDate::parse_from_str(start, "%Y-%m-%d")
                .map_err(|_| "Type the first period's start as YYYY-MM-DD, or leave it empty.")?,
        )
    };
    let n = || -> Result<u32, String> {
        day.parse::<u32>()
            .map_err(|_| "Type a number of days.".to_string())
    };
    Ok(match how {
        0 | 1 => {
            let w = weekday.clamp(0, 6) as u32;
            // the latest such weekday on or before today
            let back = (7 + today.weekday().num_days_from_monday() - w) % 7;
            let start = typed.unwrap_or(today - chrono::Days::new(u64::from(back)));
            (
                json!({"Weekly": {"start": start, "weekday": DAYS[w as usize], "every": if how == 0 { 1 } else { 2 }}}),
                start,
            )
        }
        3 => {
            let n = n()?;
            if !(1..=365).contains(&n) {
                return Err("A period is 1 to 365 days.".into());
            }
            let start = typed.unwrap_or(today);
            (json!({"EveryNDays": {"start": start, "days": n}}), start)
        }
        _ => {
            let d = n().map_err(|_| "Type the day of the month, like 1.".to_string())?;
            if !(1..=31).contains(&d) {
                return Err("The day is 1 to 28, or 31 for the last day.".into());
            }
            let last = d > 28;
            // the latest such day on or before today
            let this_month = |t: NaiveDate| {
                if last {
                    t.with_day(1)
                        .and_then(|f| f.checked_add_months(chrono::Months::new(1)))
                        .and_then(|f| f.pred_opt())
                } else {
                    t.with_day(d)
                }
            };
            let start = match typed {
                Some(s) => s,
                None => match this_month(today) {
                    Some(s) if s <= today => s,
                    _ => {
                        let prev = today
                            .with_day(1)
                            .and_then(|f| f.pred_opt())
                            .unwrap_or(today);
                        this_month(prev).unwrap_or(today)
                    }
                },
            };
            let day = if last {
                json!("Last")
            } else {
                json!({"Day": d})
            };
            (
                json!({"Monthly": {"start": start, "day": day, "every": 1}}),
                start,
            )
        }
    })
}

/// One person's days or usage as typed: (member, name, text).
pub type NeedRow = (MemberId, String, String);

pub struct State {
    pub dir: PathBuf,
    pub cfg: AppConfig,
    pub jobs: Sender<Job>,
    pub today: NaiveDate,
    pub ledger: Ledger,
    pub cur: Currency,
    pub agreement: Option<AgreementId>,
    pub found: Option<HubAddress>,
    pub exp_payer: Option<MemberId>,
    pub pay_from: Option<MemberId>,
    pub pay_to: Option<MemberId>,
    pub draft: Option<RuleDraft>,
    /// The due items on screen, and the one the expense sheet records.
    pub due: Vec<view::ExpectedRow>,
    pub exp_due: Option<view::ExpectedRow>,
    /// A rule by days or usage: its kind and each person's value as typed.
    pub exp_needs: Option<(String, Vec<NeedRow>)>,
    /// An itemized rule: the receipt lines as typed (amount, who shares the line).
    pub exp_items: Vec<(String, BTreeSet<MemberId>)>,
    /// The entry the expense sheet edits, and its body as it was.
    pub exp_edit: Option<(u64, sk_ledger::bodies::Expense)>,
    pub pending: usize,
    pub refused: Vec<String>,
    pub problem: Option<String>,
    pub offline: bool,
    // the hub computer
    pub is_hub: bool,
    pub hub_book: Option<String>,
    pub hub_ledger: Ledger,
    pub hub_pick: BTreeSet<MemberId>,
    /// The open book's file.
    pub hub_file: Option<String>,
    /// Keep the message on the Hub tab through the next status (a recovery key).
    keep_info: bool,
    pub hub_pin_set: bool,
    /// EZ Start: the people step is done; a step chosen with Back.
    ez_people_done: bool,
    ez_manual: Option<i32>,
    seq: u64,
    /// This device's copy of the history (Verify, checkpoints, CSV).
    pub records: Vec<sk_chain::Record>,
    /// A restored backup waiting to be accepted; until then, no changes.
    pub restore: Option<sk_client::RestoreOffer>,
    /// The last record this device has and when it last synced (ms).
    pub seen: Option<(u64, i64)>,
    /// Recovering a broken book on the hub: its file and the backup picked.
    rec_file: Option<String>,
    rec_backup: Option<String>,
    /// After pairing or closing a period: suggest a checkpoint once the history arrives.
    offer_checkpoint: bool,
    /// The form sheet's state.
    pub form: Option<crate::forms::Form>,
    /// A checkpoint typed in when pairing, handed to the worker with the connection.
    pair_expect: Option<sk_chain::Head>,
    /// The hub's history didn't contain that checkpoint.
    pub checkpoint_failed: bool,
}

pub(crate) fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// The same list with new rows: changed in place when the length is the same, so a row's
/// text field isn't recreated (and doesn't lose the cursor) while someone types in it.
fn same_rows<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>) -> ModelRc<T> {
    if current.row_count() == rows.len() && !rows.is_empty() {
        for (i, r) in rows.into_iter().enumerate() {
            if current.row_data(i).as_ref() != Some(&r) {
                current.set_row_data(i, r);
            }
        }
        current
    } else {
        model(rows)
    }
}

pub(crate) fn s(t: impl AsRef<str>) -> SharedString {
    SharedString::from(t.as_ref())
}

pub fn currency(code: &str) -> Currency {
    Currency::ALL
        .into_iter()
        .find(|c| c.code() == code)
        .unwrap_or(Currency::ALL[0])
}

pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

impl State {
    pub fn new(dir: PathBuf, cfg: AppConfig, jobs: Sender<Job>, is_hub: bool) -> Self {
        let cur = cfg
            .book
            .as_ref()
            .map(|b| currency(&b.currency))
            .unwrap_or(Currency::ALL[0]);
        State {
            dir,
            cfg,
            jobs,
            today: today(),
            ledger: Ledger::new(),
            cur,
            agreement: None,
            found: None,
            exp_payer: None,
            pay_from: None,
            pay_to: None,
            draft: None,
            due: Vec::new(),
            exp_due: None,
            exp_edit: None,
            exp_needs: None,
            exp_items: Vec::new(),
            pending: 0,
            refused: Vec::new(),
            problem: None,
            offline: false,
            is_hub,
            hub_book: None,
            hub_ledger: Ledger::new(),
            hub_pick: BTreeSet::new(),
            hub_file: None,
            keep_info: false,
            hub_pin_set: false,
            ez_people_done: false,
            ez_manual: None,
            seq: 0,
            records: Vec::new(),
            restore: None,
            seen: None,
            rec_file: None,
            rec_backup: None,
            offer_checkpoint: false,
            form: None,
            pair_expect: None,
            checkpoint_failed: false,
        }
    }

    pub fn me(&self) -> MemberId {
        MemberId(self.cfg.member.unwrap_or(0))
    }

    pub(crate) fn send(&self, job: Job) {
        // a restored backup waits for the person: nothing new is sent before they accept it
        if self.restore.is_some() && matches!(job, Job::Send { .. }) {
            return;
        }
        let _ = self.jobs.send(job);
    }

    fn hyperbook(&self) -> bool {
        self.cfg.book.as_ref().is_some_and(|b| b.hyperbook)
    }

    fn client_id(&mut self) -> String {
        self.seq += 1;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("app-{ms}-{}", self.seq)
    }

    /// The local folder for the paired book's copy and outbox.
    pub fn book_dir(&self) -> PathBuf {
        let id = self.cfg.book.as_ref().map_or("book", |b| b.id.as_str());
        self.dir.join("books").join(id)
    }

    /// Start using the saved pairing.
    pub fn connect(&mut self) {
        if let (Some(hub), Some(token)) = (self.cfg.hub.clone(), self.cfg.token.clone()) {
            let expect = self.pair_expect.take();
            self.send(Job::Connect {
                client: Client::new(hub, token),
                book_dir: self.book_dir(),
                expect,
            });
        }
    }

    // ----- answers from the worker ---------------------------------------------------------

    pub fn on_outcome(&mut self, ui: &AppWindow, o: Outcome) {
        ui.set_busy(false);
        match o {
            Outcome::Found(Ok((hub, fp))) => {
                self.found = Some(hub);
                ui.set_hub_fingerprint(s(fp));
                ui.set_connect_error(s(""));
                ui.set_join_step(2);
            }
            Outcome::Found(Err(e)) => {
                self.found = None;
                ui.set_hub_fingerprint(s(""));
                ui.set_connect_error(s(e));
            }
            Outcome::Paired(Ok((client, answer))) => {
                self.cfg.hub = Some(client.hub.clone());
                self.cfg.token = client.token().map(str::to_string);
                self.cfg.member = Some(answer.member);
                self.cur = currency(&answer.book.currency);
                self.cfg.book = Some(answer.book);
                self.cfg.device_name = Some(ui.get_device_name().to_string());
                if let Err(e) = self.cfg.save(&self.dir) {
                    ui.set_connect_error(s(e));
                    return;
                }
                ui.set_connect_pin(s(""));
                ui.set_connect_code(s(""));
                ui.set_page(1);
                self.offer_checkpoint = true;
                self.connect();
            }
            Outcome::Paired(Err(e)) => ui.set_connect_error(s(e)),
            Outcome::Synced(sync) => {
                if let Some(records) = sync.records {
                    self.ledger = Ledger::replay(&records).0;
                    self.records = records;
                }
                self.restore = sync.restore;
                self.seen = sync.seen;
                self.checkpoint_failed = sync.checkpoint_failed;
                if self.offer_checkpoint && !self.records.is_empty() {
                    self.offer_checkpoint = false;
                    if self.hyperbook() {
                        toast(ui, "Optional: keep a checkpoint outside the house. Settings > Hyperbook > Save a checkpoint.");
                    }
                }
                self.pending = sync.pending;
                self.refused = sync.refused;
                self.problem = sync.problem;
                self.offline = sync.offline;
            }
            Outcome::Sent(Ok(m)) | Outcome::Admin(AdminThen::Done(m), Ok(_)) => {
                self.ez_manual = None;
                if !m.is_empty() {
                    toast(ui, &m);
                }
                if self.is_hub {
                    self.hub_status();
                }
            }
            Outcome::Sent(Err(e)) => toast(ui, &e),
            Outcome::Saved(Ok(p)) => toast(ui, &format!("Saved {}", p.display())),
            Outcome::Saved(Err(e)) => toast(ui, &e),
            Outcome::Admin(then, Ok(v)) => self.on_admin(ui, then, v),
            Outcome::Checked(ok, text) => {
                ui.set_check_ok(ok);
                ui.set_check_answer(s(text));
            }
            Outcome::Hashed(Ok(sha)) => {
                let (ok, text) = view::check_statement(&self.ledger, &sha);
                ui.set_check_ok(ok);
                ui.set_check_answer(s(text));
            }
            Outcome::Hashed(Err(e)) => {
                ui.set_check_ok(false);
                ui.set_check_answer(s(e));
            }
            Outcome::Admin(_, Err(e)) => {
                ui.set_hub_error(s(e));
            }
        }
        self.refresh(ui);
    }

    fn on_admin(&mut self, ui: &AppWindow, then: AdminThen, v: Value) {
        ui.set_hub_error(s(""));
        match then {
            AdminThen::Status => {
                let all = v["books"].as_array().cloned().unwrap_or_default();
                let detail = |b: &Value| {
                    let mut d = Vec::new();
                    if b["hyperbook"].as_bool() == Some(true) {
                        d.push("Hyperbook".to_string());
                    }
                    if b["encrypted"].as_bool() == Some(true) {
                        d.push("encrypted".into());
                    }
                    if let Some(h) = b["head"].as_u64() {
                        d.push(format!("{h} records"));
                    }
                    match b["problem"].as_str() {
                        Some("closed") | None => {}
                        Some(p) => d.push(p.to_string()),
                    }
                    d.join(" · ")
                };
                let current = all.iter().find(|b| b["open"] == true);
                self.hub_book = current.and_then(|b| b["id"].as_str()).map(str::to_string);
                self.hub_file = current.and_then(|b| b["file"].as_str()).map(str::to_string);
                ui.set_hub_current_name(s(current.and_then(|b| b["name"].as_str()).unwrap_or("")));
                ui.set_hub_current_detail(s(current.map(detail).unwrap_or_default()));
                if current.is_none() {
                    self.hub_ledger = Ledger::new();
                    ui.set_hub_code(s(""));
                }
                let books: Vec<BookS> = all
                    .iter()
                    .filter(|b| b["open"] != true)
                    .map(|b| BookS {
                        file: s(b["file"].as_str().unwrap_or("")),
                        name: s(b["name"].as_str().or(b["file"].as_str()).unwrap_or("")),
                        detail: s(detail(b)),
                        encrypted: b["encrypted"].as_bool().unwrap_or(false),
                        broken: !b["broken"].is_null(),
                    })
                    .collect();
                ui.set_hub_books(model(books));
                ui.set_hub_pin_set(v["pin_set"] == true);
                ui.set_hub_own_fingerprint(s(v["fingerprint"].as_str().unwrap_or("")));
                self.hub_pin_set = v["pin_set"] == true;
                if !self.keep_info {
                    ui.set_hub_info(s(format!(
                        "Hub at {} · certificate {}",
                        v["address"].as_str().unwrap_or("?"),
                        v["fingerprint"].as_str().unwrap_or("?"),
                    )));
                }
                self.keep_info = false;
                if let Some(id) = self.hub_book.clone() {
                    self.send(Job::Admin {
                        method: "GET",
                        path: format!("/admin/v1/books/{id}/records"),
                        body: None,
                        then: AdminThen::Records,
                    });
                }
                self.send(Job::Admin {
                    method: "GET",
                    path: "/admin/v1/devices".into(),
                    body: None,
                    then: AdminThen::Devices,
                });
            }
            AdminThen::Records => {
                let records: Vec<sk_chain::Record> =
                    serde_json::from_value(v["records"].clone()).unwrap_or_default();
                self.hub_ledger = Ledger::replay(&records).0;
            }
            AdminThen::Devices => {
                let list: Vec<DeviceS> = v["devices"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|d| {
                        let member = d["member"].as_u64().unwrap_or(0) as u32;
                        let who = self
                            .hub_ledger
                            .members
                            .get(&MemberId(member))
                            .cloned()
                            .unwrap_or_else(|| format!("member {member}"));
                        let revoked = !d["revoked"].is_null();
                        DeviceS {
                            id: d["id"].as_i64().unwrap_or(0) as i32,
                            name: s(d["name"].as_str().unwrap_or("")),
                            detail: s(if revoked {
                                format!("{who} · removed")
                            } else {
                                who
                            }),
                            revoked,
                        }
                    })
                    .collect();
                ui.set_hub_devices(model(list));
            }
            AdminThen::Code => {
                // the QR code opens the certificate page first, which links on to the household
                let url = v["qr"]
                    .as_str()
                    .or(v["url"].as_str())
                    .unwrap_or("")
                    .to_string();
                ui.set_hub_code(s(v["code"].as_str().unwrap_or("")));
                ui.set_hub_code_detail(s(format!(
                    "Works once, for 10 minutes. Without the QR code, a phone opens {} first.",
                    v["certificate_page"].as_str().unwrap_or("")
                )));
                ui.set_hub_qr(qr_image(&url));
                ui.set_hub_cert_page(s(v["certificate_page"].as_str().unwrap_or("")));
            }
            AdminThen::BookMade => {
                let name = v["book"]["name"].as_str().unwrap_or("The book");
                let msg = match v["recovery_key"].as_str() {
                    Some(k) => format!(
                        "{name} was created. Its recovery key is {k} — write it down and keep it somewhere safe: it is shown only now, and it opens the book if the password is lost."
                    ),
                    None => format!("{name} was created."),
                };
                self.hub_book = v["book"]["id"].as_str().map(str::to_string);
                ui.set_new_book_name(s(""));
                ui.set_new_book_password(s(""));
                ui.set_new_book_hyperbook(false);
                ui.set_hub_info(s(msg));
                self.keep_info = true;
                self.hub_status();
            }
            AdminThen::Recovery(file) => {
                let backups: Vec<BackupS> = v["backups"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|b| {
                        let ok = b["status"] == "ok";
                        let when = chrono::DateTime::from_timestamp_millis(
                            b["modified"].as_i64().unwrap_or(0),
                        )
                        .map(|d| {
                            d.with_timezone(&chrono::Local)
                                .format("%b %-d, %Y %H:%M")
                                .to_string()
                        })
                        .unwrap_or_default();
                        let head = b["head"]["seq"].as_u64();
                        let mut detail = vec![when];
                        if let Some(h) = head {
                            detail.push(format!("up to record {h}"));
                        }
                        match b["status"].as_str() {
                            Some("broken") => detail.push(format!(
                                "also broken, at record {}",
                                b["broken_at"].as_u64().unwrap_or(0)
                            )),
                            Some("unreadable") => detail.push(format!(
                                "can't be read: {}",
                                b["problem"].as_str().unwrap_or("")
                            )),
                            _ => detail.push("checks out".into()),
                        }
                        BackupS {
                            file: s(b["file"].as_str().unwrap_or("")),
                            title: s(b["file"].as_str().unwrap_or("")),
                            detail: s(detail.join(" · ")),
                            ok,
                        }
                    })
                    .collect();
                self.rec_file = Some(file.clone());
                self.rec_backup = None;
                ui.set_rec_file(s(&file));
                ui.set_rec_name(s(v["name"].as_str().unwrap_or(&file)));
                ui.set_rec_problem(s(v["problem"].as_str().unwrap_or("")));
                ui.set_rec_backups(model(backups));
                ui.set_rec_lost(s(""));
                ui.set_rec_backup(s(""));
            }
            AdminThen::Lost(_, backup) => {
                let after = v["after"].as_u64().unwrap_or(0);
                let lost = v["records"].as_array().cloned().unwrap_or_default();
                let text = if lost.is_empty() {
                    format!("{backup} has every record (up to {after}); nothing is lost.")
                } else {
                    let list: Vec<String> = lost
                        .iter()
                        .take(12)
                        .map(|r| {
                            format!(
                                "record {} ({}, {})",
                                r["seq"],
                                r["kind"].as_str().unwrap_or(""),
                                r["when"].as_str().unwrap_or("")
                            )
                        })
                        .collect();
                    let more = if lost.len() > 12 {
                        format!(" and {} more", lost.len() - 12)
                    } else {
                        String::new()
                    };
                    format!(
                        "Carrying on from {backup} keeps records up to {after}. Not in it, to be entered again: {}{more}. These are as the broken book has them, which can't be checked.",
                        list.join(", ")
                    )
                };
                self.rec_backup = Some(backup.clone());
                ui.set_rec_backup(s(backup));
                ui.set_rec_lost(s(text));
            }
            AdminThen::Done(_) | AdminThen::SaveText(_) => {}
        }
    }

    // ----- Hyperbook, Verify, recovery ------------------------------------------------------

    fn refresh_hyperbook(&self, ui: &AppWindow) {
        ui.set_is_hyperbook(self.hyperbook());
        ui.set_checkpoint_failed(self.checkpoint_failed);
        ui.set_restore_text(s(self
            .restore
            .as_ref()
            .map(|o| o.text())
            .unwrap_or_default()));
        let head = self.records.last().map(sk_chain::Record::head);
        ui.set_hb_head(s(
            head.map_or("no records yet".into(), |h| format!("record {}", h.seq))
        ));
        ui.set_hb_fingerprint(s(head.map(|h| h.hash.to_hex()).unwrap_or_default()));
        match self.seen {
            Some((seq, at)) if at > 0 => {
                let day = chrono::DateTime::from_timestamp_millis(at)
                    .map(|d| d.with_timezone(&chrono::Local).date_naive())
                    .unwrap_or(self.today);
                ui.set_hb_seen(s(format!(
                    "This device last saw record {seq} on {}.",
                    day.format("%b %-d, %Y")
                )));
                let days = (self.today - day).num_days();
                ui.set_hb_stale(s(if days > 30 {
                    format!("It has been {days} days since this device last checked with the hub.")
                } else {
                    String::new()
                }));
            }
            _ => {
                ui.set_hb_seen(s(""));
                ui.set_hb_stale(s(""));
            }
        }
    }

    pub fn save_checkpoint(&self, ui: &AppWindow) {
        let Some(head) = self.records.last().map(sk_chain::Record::head) else {
            return toast(ui, "Sync first.");
        };
        let book = self.cfg.book.as_ref().map_or("Book", |b| b.name.as_str());
        let text = sk_chain::checkpoint::text(book, head, &self.today.to_string());
        ui.set_checkpoint_qr(qr_image(&sk_chain::checkpoint::for_qr(&text)));
        ui.set_checkpoint_text(s(text));
    }

    pub fn save_checkpoint_pdf(&self, ui: &AppWindow) {
        let Some(head) = self.records.last().map(sk_chain::Record::head) else {
            return;
        };
        let book = self
            .cfg
            .book
            .as_ref()
            .map_or("Book", |b| b.name.as_str())
            .to_string();
        let line = sk_chain::checkpoint::text(&book, head, &self.today.to_string());
        let bytes = match sk_report::checkpoint_pdf(&book, &line, head.seq, &head.hash.to_hex()) {
            Ok(b) => b,
            Err(e) => return toast(ui, &e),
        };
        let jobs = self.jobs.clone();
        crate::dialogs::save_file(
            ui.window(),
            "Save the checkpoint to print",
            &format!("ShareKeep checkpoint record {}.pdf", head.seq),
            ("PDF", &["pdf"]),
            move |path| {
                if let Some(path) = path {
                    let _ = jobs.send(Job::SaveBytes { path, bytes });
                }
            },
        );
    }

    pub fn forget_checkpoint(&self) {
        let _ = self.jobs.send(Job::ForgetCheckpoint);
    }

    pub fn hub_save_lost(&self, ui: &AppWindow) {
        let (Some(file), Some(backup)) = (self.rec_file.clone(), self.rec_backup.clone()) else {
            return;
        };
        let mut body = Self::unlock_body(ui);
        body["backup"] = json!(backup);
        let jobs = self.jobs.clone();
        crate::dialogs::save_file(
            ui.window(),
            "Save the list to enter again",
            "ShareKeep records to enter again.txt",
            ("Text", &["txt"]),
            move |path| {
                if let Some(path) = path {
                    let _ = jobs.send(Job::Admin {
                        method: "POST",
                        path: format!("/admin/v1/books/{file}/lost"),
                        body: Some(body),
                        then: AdminThen::SaveText(path),
                    });
                }
            },
        );
    }

    pub fn save_checkpoint_file(&self, ui: &AppWindow) {
        let text = ui.get_checkpoint_text().to_string();
        if text.is_empty() {
            return;
        }
        let seq = self.records.last().map_or(0, |r| r.seq);
        let jobs = self.jobs.clone();
        crate::dialogs::save_file(
            ui.window(),
            "Save the checkpoint",
            &format!("ShareKeep checkpoint record {seq}.txt"),
            ("Text", &["txt"]),
            move |path| {
                if let Some(path) = path {
                    let _ = jobs.send(Job::SaveText {
                        path,
                        text: text + "\r\n",
                    });
                }
            },
        );
    }

    pub fn check_checkpoint(&self, ui: &AppWindow) {
        match sk_chain::checkpoint::parse(&ui.get_checkpoint_input()) {
            Some(head) => {
                ui.set_check_answer(s("Checking…"));
                ui.set_check_ok(true);
                let _ = self.jobs.send(Job::CheckCheckpoint(head));
            }
            None => {
                ui.set_check_ok(false);
                ui.set_check_answer(s("That doesn't read as a checkpoint: it needs the record number and the 64-character fingerprint."));
            }
        }
    }

    pub fn check_statement(&self, ui: &AppWindow) {
        let jobs = self.jobs.clone();
        crate::dialogs::open_file(
            ui.window(),
            "Check a statement",
            ("PDF", &["pdf"]),
            move |p| {
                if let Some(p) = p {
                    let _ = jobs.send(Job::HashFile(p));
                }
            },
        );
    }

    pub fn verify(&self, ui: &AppWindow) {
        if self.records.is_empty() {
            return toast(ui, "Sync first.");
        }
        let a = sk_ledger::audit::audit(&self.records, self.hyperbook());
        let mut text = Vec::new();
        if let Some(h) = &a.history {
            text.push("Has the history been changed?".to_string());
            text.extend(h.lines.iter().cloned());
            text.push(String::new());
        }
        text.push("Do the numbers add up?".into());
        text.extend(a.numbers.lines.iter().cloned());
        ui.set_verify_ok(a.numbers.ok && a.history.as_ref().is_none_or(|h| h.ok));
        ui.set_verify_answer(s(text.join("\n")));
    }

    pub fn accept_restore(&self) {
        if let Some(o) = self.restore.clone() {
            let _ = self.jobs.send(Job::AcceptRestore(o));
        }
    }

    pub fn export_csv(&self, ui: &AppWindow, first: &str) {
        let Some(a) = self.agreement else { return };
        let Ok(first) = first.parse::<NaiveDate>() else {
            return;
        };
        let head = self
            .hyperbook()
            .then(|| self.records.last().map(sk_chain::Record::head))
            .flatten();
        match view::csv(&self.ledger, a, first, head) {
            Ok((name, text)) => {
                let jobs = self.jobs.clone();
                crate::dialogs::save_file(
                    ui.window(),
                    "Export to a spreadsheet",
                    &name,
                    ("CSV", &["csv"]),
                    move |path| {
                        if let Some(path) = path {
                            let _ = jobs.send(Job::SaveText { path, text });
                        }
                    },
                );
            }
            Err(e) => toast(ui, &e),
        }
    }

    fn unlock_body(ui: &AppWindow) -> Value {
        let p = ui.get_unlock_password().to_string();
        if p.is_empty() {
            json!({})
        } else {
            json!({"password": p})
        }
    }

    pub fn hub_recover(&self, ui: &AppWindow, file: &str) {
        self.admin(
            "POST",
            format!("/admin/v1/books/{file}/recovery"),
            Some(Self::unlock_body(ui)),
            AdminThen::Recovery(file.to_string()),
        );
    }

    pub fn hub_recover_pick(&self, ui: &AppWindow, backup: &str) {
        let Some(file) = self.rec_file.clone() else {
            return;
        };
        let mut body = Self::unlock_body(ui);
        body["backup"] = json!(backup);
        self.admin(
            "POST",
            format!("/admin/v1/books/{file}/lost"),
            Some(body),
            AdminThen::Lost(file, backup.to_string()),
        );
    }

    pub fn hub_restore(&mut self, ui: &AppWindow) {
        let (Some(file), Some(backup)) = (self.rec_file.clone(), self.rec_backup.clone()) else {
            return;
        };
        let mut body = Self::unlock_body(ui);
        body["backup"] = json!(backup);
        self.admin(
            "POST",
            format!("/admin/v1/books/{file}/restore"),
            Some(body),
            AdminThen::Done(format!(
                "Restored from {backup}. The broken book was kept, renamed. Each device is asked to accept the restore."
            )),
        );
        self.hub_recover_cancel(ui);
    }

    pub fn hub_recover_cancel(&mut self, ui: &AppWindow) {
        self.rec_file = None;
        self.rec_backup = None;
        ui.set_rec_file(s(""));
        ui.set_rec_backups(model(Vec::new()));
        ui.set_rec_lost(s(""));
        ui.set_rec_backup(s(""));
    }

    pub fn hub_export_broken(&self, ui: &AppWindow) {
        let Some(file) = self.rec_file.clone() else {
            return;
        };
        let body = Self::unlock_body(ui);
        let jobs = self.jobs.clone();
        let stem = file.trim_end_matches(".sharekeep").to_string();
        crate::dialogs::save_file(
            ui.window(),
            "Save the broken history",
            &format!("{stem} broken history.txt"),
            ("Text", &["txt"]),
            move |path| {
                if let Some(path) = path {
                    let _ = jobs.send(Job::Admin {
                        method: "POST",
                        path: format!("/admin/v1/books/{file}/export-broken"),
                        body: Some(body),
                        then: AdminThen::SaveText(path),
                    });
                }
            },
        );
    }

    pub fn hub_status(&self) {
        if self.is_hub {
            self.send(Job::Admin {
                method: "GET",
                path: "/admin/v1/status".into(),
                body: None,
                then: AdminThen::Status,
            });
        }
    }

    // ----- filling the screens ---------------------------------------------------------------

    pub fn refresh(&mut self, ui: &AppWindow) {
        let l = &self.ledger;
        let me = self.me();
        let cur = self.cur;
        ui.set_is_hub(self.is_hub);
        ui.set_book_name(s(self.cfg.book.as_ref().map_or("", |b| b.name.as_str())));
        ui.set_me_name(s(l.members.get(&me).cloned().unwrap_or_default()));
        ui.set_pending(self.pending as i32);
        ui.set_offline(self.offline);
        ui.set_banner(s(match (&self.problem, self.offline) {
            (Some(_), true) => "Offline: the household hub can't be reached. New expenses, payments and approvals wait here and are sent when you're home.".to_string(),
            (Some(p), false) => p.clone(),
            (None, _) => String::new(),
        }));
        ui.set_refused(model(self.refused.iter().map(s).collect()));

        let cards = view::agreements(l, me, cur, self.today)
            .into_iter()
            .map(|c| CardS {
                id: c.id.0 as i32,
                name: s(c.name),
                headline: s(c.headline),
                sign: c.sign,
                members: model(c.members.into_iter().map(member_s).collect()),
                period: s(c.period),
                waiting: c.waiting as i32,
            })
            .collect();
        ui.set_cards(model(cards));
        ui.set_approvals(model(
            view::approvals(l, me)
                .into_iter()
                .map(|a| ApprovalS {
                    seq: a.seq as i32,
                    title: s(a.title),
                    detail: s(a.detail),
                })
                .collect(),
        ));

        if let Some(a) = self.agreement {
            let ag = l.agreements.get(&a);
            ui.set_agreement_name(s(ag.map_or("", |g| g.name.as_str())));
            ui.set_agreement_period(s(ag
                .and_then(|g| g.schedule.period_containing(self.today))
                .map(|p| format!("{} – {}", p.first.format("%b %-d"), p.last.format("%b %-d")))
                .unwrap_or_default()));
            ui.set_activity(model(
                view::activity(l, a, me, cur)
                    .into_iter()
                    .map(|r| ActivityS {
                        seq: r.seq as i32,
                        date: s(r.date),
                        title: s(r.title),
                        detail: s(r.detail),
                        amount: s(r.amount),
                        disputed: r.disputed,
                        editable: r.editable,
                    })
                    .collect(),
            ));
            ui.set_rules(model(
                view::rules(l, a, self.today)
                    .into_iter()
                    .map(|r| RuleS {
                        id: r.id.0 as i32,
                        name: s(r.name),
                        method: s(r.method),
                        detail: s(r.detail),
                    })
                    .collect(),
            ));
            self.due = view::expected(l, a, me, cur, self.today);
            ui.set_due(model(
                self.due
                    .iter()
                    .map(|d| DueS {
                        name: s(&d.name),
                        date: s(&d.date),
                        detail: s(&d.detail),
                        bill: d.kind == "bill",
                        mine: d.mine,
                        overdue: d.overdue,
                    })
                    .collect(),
            ));
            ui.set_deposits(model(
                view::deposits(l, a, me, cur)
                    .into_iter()
                    .map(|d| DepositS {
                        label: s(d.label),
                        kind: s(d.kind),
                        balance: s(d.balance),
                        detail: s(d.detail),
                    })
                    .collect(),
            ));
            ui.set_recurring(model(
                view::recurring(l, a, cur)
                    .into_iter()
                    .map(|r| RecurringS {
                        id: r.id as i32,
                        name: s(r.name),
                        detail: s(r.detail),
                        stopped: r.stopped,
                    })
                    .collect(),
            ));
            let (members, settle) = view::members(l, a, cur);
            ui.set_members(model(members.into_iter().map(member_s).collect()));
            ui.set_settle(model(settle.into_iter().map(s).collect()));
            ui.set_schedule_text(s(view::schedule_text(l, a, self.today)));
            ui.set_events(model(
                view::events(l, a)
                    .into_iter()
                    .map(|e| EventS {
                        name: s(e.name),
                        detail: s(e.detail),
                    })
                    .collect(),
            ));
            ui.set_can_join(l.members.keys().any(|m| {
                l.agreements
                    .get(&a)
                    .and_then(|g| g.members.get(m))
                    .is_none_or(|ms| ms.left.is_some())
            }));
            ui.set_periods(model(
                view::periods(l, a, self.today, 5)
                    .into_iter()
                    .map(|p| PeriodS {
                        key: s(p.first.to_string()),
                        label: s(p.label),
                        state: s(p.state),
                        closed: p.closed,
                        can_close: p.can_close,
                    })
                    .collect(),
            ));
        }
        self.refresh_hyperbook(ui);
        self.refresh_form(ui);
        self.refresh_expense(ui);
        self.refresh_payment(ui);
        self.refresh_rule(ui);
        self.refresh_hub(ui);
    }

    fn agreement_members(&self) -> Vec<(MemberId, String)> {
        self.agreement
            .and_then(|a| self.ledger.agreements.get(&a))
            .map(|g| {
                g.members
                    .iter()
                    .filter(|(_, m)| m.left.is_none())
                    .map(|(id, _)| {
                        (
                            *id,
                            self.ledger.members.get(id).cloned().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn chips(&self, chosen: Option<MemberId>) -> ModelRc<ChoiceS> {
        model(
            self.agreement_members()
                .into_iter()
                .map(|(id, name)| ChoiceS {
                    id: id.0 as i32,
                    label: s(name),
                    on: chosen == Some(id),
                })
                .collect(),
        )
    }

    // ----- the expense sheet -----

    pub fn open_expense(&mut self, ui: &AppWindow) {
        self.exp_payer = Some(self.me());
        self.exp_due = None;
        self.exp_edit = None;
        self.exp_needs = None;
        self.exp_items.clear();
        ui.set_exp_title(s("New expense"));
        ui.set_exp_amount(s(""));
        ui.set_exp_category(s(""));
        ui.set_exp_note(s(""));
        ui.set_exp_date(s(self.today.to_string()));
        ui.set_exp_open(true);
        self.refresh_expense(ui);
    }

    /// Edit an expense: the sheet filled in from it; saving sends an edit (needs the hub).
    pub fn edit_entry(&mut self, ui: &AppWindow, seq: i32) {
        let Some(a) = self.agreement else { return };
        let Some(e) = view::activity(&self.ledger, a, self.me(), self.cur)
            .into_iter()
            .find(|r| r.seq == seq as u64)
            .and_then(|r| r.expense)
        else {
            return;
        };
        self.exp_payer = match e.payer {
            sk_ledger::bodies::Payer::Member(m) => Some(m),
            _ => Some(self.me()),
        };
        ui.set_exp_amount(s(self.cur.format_number(e.amount)));
        ui.set_exp_category(s(&e.category));
        ui.set_exp_note(s(e.note.clone().unwrap_or_default()));
        ui.set_exp_date(s(e.date.to_string()));
        ui.set_exp_title(s("Edit expense"));
        self.exp_due = None;
        self.exp_edit = Some((seq as u64, e));
        ui.set_exp_open(true);
        self.refresh_expense(ui);
    }

    /// Record a due recurring bill: the expense sheet, filled in from it.
    pub fn record_due(&mut self, ui: &AppWindow, i: i32) {
        let Some(d) = self.due.get(i as usize).cloned() else {
            return;
        };
        self.exp_payer = Some(d.member.unwrap_or(self.me()));
        ui.set_exp_amount(s(self.cur.format_number(d.amount_cents)));
        ui.set_exp_category(s(&d.category));
        ui.set_exp_note(s(&d.name));
        ui.set_exp_date(s(d.date_iso.to_string()));
        ui.set_exp_title(s(format!("Record {}", d.name)));
        self.exp_edit = None;
        self.exp_due = Some(d);
        ui.set_exp_open(true);
        self.refresh_expense(ui);
    }

    fn expense_body(&self, ui: &AppWindow) -> Result<sk_ledger::bodies::Expense, String> {
        let a = self.agreement.ok_or("Open an agreement first.")?;
        let amount = self
            .cur
            .parse(ui.get_exp_amount().trim())
            .filter(|v| *v > 0)
            .ok_or("Type the amount, like 12.50.")?;
        let date = NaiveDate::parse_from_str(ui.get_exp_date().trim(), "%Y-%m-%d")
            .map_err(|_| "Type the date as YYYY-MM-DD.")?;
        let category = ui.get_exp_category().trim().to_string();
        if category.is_empty() {
            return Err("Type a category, like Groceries.".into());
        }
        let mut e = view::expense_body(
            a,
            date,
            amount,
            self.exp_payer.unwrap_or(self.me()),
            &category,
            &ui.get_exp_note(),
        );
        e.due = self.exp_due.as_ref().map(|d| sk_ledger::bodies::Due {
            recurring: d.recurring,
            date: d.date_iso,
        });
        if self.exp_needs.as_ref().is_some_and(|(k, _)| k == "items") {
            for (i, (text, who)) in self.exp_items.iter().enumerate() {
                if text.trim().is_empty() && who.is_empty() {
                    continue;
                }
                let amount = self
                    .cur
                    .parse(text.trim())
                    .ok_or(format!("Line {}: type its amount, like 4.99.", i + 1))?;
                if who.is_empty() {
                    return Err(format!("Line {}: pick who shares it.", i + 1));
                }
                e.items.push(sk_split::method::Item {
                    amount,
                    members: who.iter().copied().collect(),
                });
            }
        }
        if let Some((kind, values)) = self.exp_needs.as_ref().filter(|(k, _)| k != "items") {
            for (m, _, v) in values {
                let Ok(n) = v.trim().parse::<u64>() else {
                    continue;
                };
                if kind == "days" {
                    e.days.insert(*m, n.min(366) as u32);
                } else {
                    e.usage.insert(*m, n);
                }
            }
        }
        if let Some((_, old)) = &self.exp_edit {
            // an edit keeps what the sheet doesn't show
            e.payer = match (&old.payer, self.exp_payer) {
                (sk_ledger::bodies::Payer::Pot(p), _) => sk_ledger::bodies::Payer::Pot(*p),
                (_, Some(m)) => sk_ledger::bodies::Payer::Member(m),
                (p, None) => *p,
            };
            e.rule = old.rule;
            e.event = old.event;
            // the sheet shows days, usage or lines when the rule asks for them; otherwise
            // what the entry had stays
            if self.exp_needs.is_none() {
                e.items = old.items.clone();
                e.days = old.days.clone();
                e.usage = old.usage.clone();
            }
            e.due = old.due;
        }
        Ok(e)
    }

    fn refresh_expense(&mut self, ui: &AppWindow) {
        if !ui.get_exp_open() {
            return;
        }
        // no rule divides it yet: say why, and offer to propose one
        let missing = self
            .expense_body(ui)
            .ok()
            .and_then(|e| view::rule_missing(&self.ledger, &e));
        ui.set_exp_no_rule(s(missing.unwrap_or_default()));
        ui.set_exp_payers(self.chips(self.exp_payer));
        // ask for days or usage when the category's rule divides by them
        let needs = self
            .expense_body(ui)
            .ok()
            .and_then(|e| view::needs(&self.ledger, &e));
        match needs {
            Some(n) => {
                let keep = self.exp_needs.as_ref().is_some_and(|(k, _)| *k == n.kind);
                if !keep {
                    // an edit starts from what the entry had
                    let old = self.exp_edit.as_ref().map(|(_, e)| e);
                    self.exp_needs = Some((
                        n.kind.clone(),
                        n.who
                            .iter()
                            .map(|p| {
                                let had = old.and_then(|e| match n.kind.as_str() {
                                    "days" => e.days.get(&p.id).map(|v| v.to_string()),
                                    "usage" => e.usage.get(&p.id).map(|v| v.to_string()),
                                    _ => None,
                                });
                                (p.id, p.name.clone(), had.unwrap_or_default())
                            })
                            .collect(),
                    ));
                    self.exp_items = old
                        .map(|e| {
                            e.items
                                .iter()
                                .map(|i| {
                                    (
                                        self.cur.format_number(i.amount),
                                        i.members.iter().copied().collect(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if n.kind == "items" && self.exp_items.is_empty() {
                        self.exp_items.push((String::new(), BTreeSet::new()));
                    }
                }
                ui.set_exp_needs_title(s(match n.kind.as_str() {
                    "days" => format!("{}: days each person was home", n.rule),
                    "items" => format!(
                        "{}: the receipt's lines. Tax, tip and fees (the amount minus the lines) are shared like the lines.",
                        n.rule
                    ),
                    _ => format!("{}: each person's usage or meter reading", n.rule),
                }));
            }
            None => self.exp_needs = None,
        }
        let itemized = self.exp_needs.as_ref().is_some_and(|(k, _)| k == "items");
        ui.set_exp_itemized(itemized);
        let who: Vec<(MemberId, String)> = self
            .exp_needs
            .as_ref()
            .map(|(_, v)| v.iter().map(|(m, n, _)| (*m, n.clone())).collect())
            .unwrap_or_default();
        ui.set_exp_items(same_rows(
            ui.get_exp_items(),
            if itemized {
                self.exp_items
                    .iter()
                    .map(|(text, on)| ItemS {
                        amount: s(text),
                        members: model(
                            who.iter()
                                .map(|(m, n)| ChoiceS {
                                    id: m.0 as i32,
                                    label: s(n),
                                    on: on.contains(m),
                                })
                                .collect(),
                        ),
                    })
                    .collect()
            } else {
                Vec::new()
            },
        ));
        ui.set_exp_needs(same_rows(
            ui.get_exp_needs(),
            self.exp_needs
                .as_ref()
                .filter(|(k, _)| k != "items")
                .map(|(_, v)| {
                    v.iter()
                        .map(|(_, name, value)| NeedS {
                            name: s(name),
                            value: s(value),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ));
        // an edit's own recurring-bill mark would read as "already recorded"
        let r = self
            .expense_body(ui)
            .map(|mut e| {
                e.due = e.due.filter(|_| self.exp_edit.is_none());
                e
            })
            .and_then(|e| view::preview(&self.ledger, self.me(), self.cur, e));
        match r {
            Ok(rows) => {
                ui.set_exp_preview(model(rows.into_iter().map(split_s).collect()));
                ui.set_exp_error(s(""));
            }
            Err(e) => {
                ui.set_exp_preview(model(Vec::new()));
                ui.set_exp_error(s(e));
            }
        }
    }

    pub fn item_add(&mut self, ui: &AppWindow) {
        self.exp_items.push((String::new(), BTreeSet::new()));
        self.refresh_expense(ui);
    }

    pub fn item_amount(&mut self, ui: &AppWindow, i: i32, text: &str) {
        if let Some(l) = self.exp_items.get_mut(i as usize) {
            l.0 = text.to_string();
        }
        self.refresh_expense(ui);
    }

    pub fn item_member(&mut self, ui: &AppWindow, i: i32, m: i32) {
        if let Some(l) = self.exp_items.get_mut(i as usize) {
            let m = MemberId(m as u32);
            if !l.1.remove(&m) {
                l.1.insert(m);
            }
        }
        self.refresh_expense(ui);
    }

    pub fn item_remove(&mut self, ui: &AppWindow, i: i32) {
        if (i as usize) < self.exp_items.len() {
            self.exp_items.remove(i as usize);
        }
        self.refresh_expense(ui);
    }

    pub fn need_edited(&mut self, ui: &AppWindow, i: i32, text: &str) {
        if let Some((_, v)) = self.exp_needs.as_mut() {
            if let Some(row) = v.get_mut(i as usize) {
                row.2 = text.to_string();
            }
        }
        self.refresh_expense(ui);
    }

    pub fn payer_picked(&mut self, ui: &AppWindow, id: i32) {
        self.exp_payer = Some(MemberId(id as u32));
        self.refresh_expense(ui);
    }

    pub fn save_expense(&mut self, ui: &AppWindow) {
        match self.expense_body(ui) {
            Ok(e) => {
                ui.set_exp_open(false);
                let job = match self.exp_edit.take() {
                    Some((seq, _)) => Job::Send {
                        kind: "entry-edit".into(),
                        body: json!({"target": seq, "expense": e}),
                    },
                    None => Job::Send {
                        kind: "expense".into(),
                        body: serde_json::to_value(e).unwrap_or_default(),
                    },
                };
                self.send(job);
            }
            Err(e) => ui.set_exp_error(s(e)),
        }
    }

    // ----- the payment sheet -----

    pub fn open_payment(&mut self, ui: &AppWindow) {
        let me = self.me();
        self.pay_from = Some(me);
        self.pay_to = self
            .agreement_members()
            .into_iter()
            .map(|(m, _)| m)
            .find(|m| *m != me);
        // suggest what settles me up with them
        let suggestion = self.agreement.and_then(|a| {
            let b = self.ledger.balances(a);
            sk_split::ledger::settle_up(&b)
                .into_iter()
                .find(|t| t.from == me)
        });
        if let Some(t) = &suggestion {
            self.pay_to = Some(t.to);
        }
        ui.set_pay_amount(s(suggestion
            .map(|t| self.cur.format_number(t.amount))
            .unwrap_or_default()));
        ui.set_pay_error(s(""));
        ui.set_pay_open(true);
        self.refresh_payment(ui);
    }

    fn refresh_payment(&self, ui: &AppWindow) {
        if ui.get_pay_open() {
            ui.set_pay_from(self.chips(self.pay_from));
            ui.set_pay_to(self.chips(self.pay_to));
        }
    }

    pub fn pay_picked(&mut self, ui: &AppWindow, from: bool, id: i32) {
        if from {
            self.pay_from = Some(MemberId(id as u32));
        } else {
            self.pay_to = Some(MemberId(id as u32));
        }
        self.refresh_payment(ui);
    }

    pub fn save_payment(&mut self, ui: &AppWindow) {
        let (Some(a), Some(from), Some(to)) = (self.agreement, self.pay_from, self.pay_to) else {
            return ui.set_pay_error(s("Pick who paid whom."));
        };
        if from == to {
            return ui.set_pay_error(s("Pick two different people."));
        }
        let Some(amount) = self
            .cur
            .parse(ui.get_pay_amount().trim())
            .filter(|v| *v > 0)
        else {
            return ui.set_pay_error(s("Type the amount, like 25.00."));
        };
        ui.set_pay_open(false);
        self.send(Job::Send {
            kind: "payment".into(),
            body: json!({"agreement": a, "date": self.today, "from": from, "to": to, "amount": amount}),
        });
    }

    // ----- the rule editor -----

    /// Exact amounts: a member's fixed amount as typed (blank or unreadable: none).
    pub fn rule_amount(&mut self, ui: &AppWindow, i: i32, text: &str) {
        let cents = self.cur.parse(text.trim());
        self.rule_edit(ui, |d| d.set_amount_text(i as usize, text, cents));
    }

    pub fn open_rule(&mut self, ui: &AppWindow) {
        let Some(a) = self.agreement else { return };
        self.draft = Some(RuleDraft::new(&self.ledger, a));
        ui.set_rule_name(s(""));
        ui.set_rule_categories(s(""));
        let (_, from, now) = view::proposal_target(&self.ledger, a, self.today);
        ui.set_rule_when(s(if now {
            format!(
                "No rule is in force yet, so this one applies from this period ({}), once everyone else in the agreement approves it.",
                from.format("%b %-d, %Y")
            )
        } else {
            format!(
                "Everyone else in the agreement approves it before it takes effect. It starts with the next period ({}).",
                from.format("%b %-d, %Y")
            )
        }));
        ui.set_rule_open(true);
        self.refresh_rule(ui);
    }

    /// From the expense sheet, when no rule divides the expense: the rule editor, with the
    /// expense's category as one the rule is the default for.
    pub fn propose_for_expense(&mut self, ui: &AppWindow) {
        let category = ui.get_exp_category().trim().to_string();
        ui.set_exp_open(false);
        self.open_rule(ui);
        ui.set_rule_categories(s(category));
    }

    pub fn rule_edit(&mut self, ui: &AppWindow, f: impl FnOnce(&mut RuleDraft)) {
        if let Some(d) = self.draft.as_mut() {
            f(d);
        }
        self.refresh_rule(ui);
    }

    fn refresh_rule(&mut self, ui: &AppWindow) {
        if !ui.get_rule_open() {
            return;
        }
        let Some(d) = self.draft.as_mut() else { return };
        d.name = ui.get_rule_name().to_string();
        ui.set_rule_methods(model(
            EDITOR_METHODS
                .iter()
                .enumerate()
                .map(|(i, m)| ChoiceS {
                    id: i as i32,
                    label: s(m),
                    on: d.method == i,
                })
                .collect(),
        ));
        ui.set_rule_percent(d.method == 1);
        ui.set_rule_exact(d.method == view::EXACT);
        ui.set_rule_weighted(view::method_weighted(d.method));
        ui.set_rule_max(view::method_max(d.method));
        ui.set_rule_unit(s(match d.method {
            2 => " share(s)",
            3 => " part(s)",
            4 => " (room size)",
            _ => "",
        }));
        ui.set_rule_hint(s(match d.method {
            3 => "Income as parts, no salaries needed: 64 and 36 split like the incomes.",
            4 => "Each person's room: its size, or what it would rent for alone.",
            5 => "Each expense asks how many days each person was home in the period.",
            6 => "Each expense asks for each person's meter reading or usage count.",
            view::EXACT => "Fixed amounts each time, like rent. One person takes whatever is left of each expense.",
            view::ITEMIZED => "Each expense asks for the receipt's lines and who shares each one. Tax and tip follow the lines.",
            _ => "",
        }));
        ui.set_rule_weights(same_rows(
            ui.get_rule_weights(),
            d.rows
                .iter()
                .map(|r| WeightS {
                    name: s(&r.name),
                    value: r.value,
                    included: r.included,
                    locked: r.locked,
                    // as typed: never reformatted while someone types it
                    amount: s(&r.amount_text),
                    remainder: r.remainder,
                })
                .collect(),
        ));
        match view::examples(&d.rule(), &self.ledger, self.cur) {
            Ok(ex) => {
                ui.set_rule_error(s(""));
                ui.set_rule_examples(model(
                    ex.into_iter()
                        .map(|(amount, rows)| ExampleS {
                            amount: s(amount),
                            rows: model(rows.into_iter().map(split_s).collect()),
                        })
                        .collect(),
                ));
            }
            Err(e) => {
                ui.set_rule_error(s(e));
                ui.set_rule_examples(model(Vec::new()));
            }
        }
    }

    pub fn propose_rule(&mut self, ui: &AppWindow) {
        let (Some(a), Some(d)) = (self.agreement, self.draft.as_ref()) else {
            return;
        };
        // the next period, or this one while no rule is in force yet (view::proposal_target)
        let (rule_id, effective, _) = view::proposal_target(&self.ledger, a, self.today);
        let categories: Vec<String> = ui
            .get_rule_categories()
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        let body = d.proposal(a, rule_id, effective, &categories);
        ui.set_rule_open(false);
        self.send(Job::Send {
            kind: "rule-proposal".into(),
            body,
        });
    }

    // ----- other actions -----

    pub fn approve(&self, seq: i32, yes: bool) {
        self.send(Job::Send {
            kind: "approval".into(),
            body: json!({"proposal": seq, "approve": yes}),
        });
    }

    pub fn close_period(&mut self, first: &str) {
        self.offer_checkpoint = true;
        if let Some(a) = self.agreement {
            self.send(Job::Send {
                kind: "period-close".into(),
                body: json!({"agreement": a, "date": first}),
            });
        }
    }

    pub fn statement(&self, ui: &AppWindow, first: String) {
        let Some(a) = self.agreement else { return };
        let name = self
            .ledger
            .agreements
            .get(&a)
            .map_or("Statement".to_string(), |g| g.name.clone());
        let suggested = format!("{name} statement {first}.pdf");
        let jobs = self.jobs.clone();
        crate::dialogs::save_file(
            ui.window(),
            "Save the statement",
            &suggested,
            ("PDF", &["pdf"]),
            move |path| {
                if let Some(path) = path {
                    let _ = jobs.send(Job::Statement {
                        agreement: a.0,
                        first,
                        path,
                    });
                }
            },
        );
    }

    /// My top-up is paid: a pay-in into the pot, for that due date.
    pub fn paid_due(&self, i: i32) {
        let (Some(a), Some(d)) = (self.agreement, self.due.get(i as usize)) else {
            return;
        };
        let Some(dep) = d.deposit else { return };
        self.send(Job::Send {
            kind: "deposit".into(),
            body: json!({"op": "pay-in", "agreement": a, "id": dep, "member": self.me(),
                         "amount": d.amount_cents, "due": {"recurring": d.recurring, "date": d.date_iso}}),
        });
    }

    pub fn stop_recurring(&self, id: i32) {
        if let Some(a) = self.agreement {
            self.send(Job::Send {
                kind: "recurring".into(),
                body: json!({"op": "stop", "agreement": a, "id": id}),
            });
        }
    }

    pub fn delete_entry(&self, seq: i32) {
        self.send(Job::Send {
            kind: "entry-delete".into(),
            body: json!({"target": seq}),
        });
    }

    pub fn dispute_entry(&self, seq: i32) {
        let disputed = self
            .ledger
            .entries
            .get(&(seq as u64))
            .is_some_and(|e| e.disputed);
        self.send(Job::Send {
            kind: "dispute".into(),
            body: json!({"target": seq, "resolved": disputed}),
        });
    }

    pub fn sync(&self) {
        self.send(Job::Sync);
    }

    pub fn dismiss_refused(&self) {
        self.send(Job::DismissRefused);
    }

    pub fn find_hub(&self, ui: &AppWindow) {
        ui.set_busy(true);
        ui.set_connect_error(s(""));
        self.send(Job::FindHub(ui.get_hub_address().to_string()));
    }

    pub fn pair(&mut self, ui: &AppWindow) {
        let Some(hub) = self.found.clone() else {
            return ui.set_connect_error(s("Find the hub first."));
        };
        let device = ui.get_device_name().trim().to_string();
        if device.is_empty() {
            return ui.set_connect_error(s("Name this device, like Sam's laptop."));
        }
        // optional: a checkpoint the household kept, which the hub's history must contain
        let cp = ui.get_connect_checkpoint().trim().to_string();
        self.pair_expect = None;
        if !cp.is_empty() {
            match sk_chain::checkpoint::parse(&cp) {
                Some(h) => self.pair_expect = Some(h),
                None => {
                    return ui.set_connect_error(s(
                        "That checkpoint doesn't read: it needs the record number and the 64-character fingerprint. Leave it empty to pair without one.",
                    ))
                }
            }
        }
        ui.set_busy(true);
        self.send(Job::Pair {
            hub,
            code: ui.get_connect_code().replace(['-', ' '], "").to_uppercase(),
            pin: ui.get_connect_pin().to_string(),
            device,
        });
    }

    // ----- the hub computer -----

    fn refresh_hub(&self, ui: &AppWindow) {
        if !self.is_hub {
            return;
        }
        let members: Vec<(MemberId, String)> = self
            .hub_ledger
            .members
            .iter()
            .map(|(id, n)| (*id, n.clone()))
            .collect();
        ui.set_hub_members(model(
            members
                .iter()
                .map(|(id, n)| ChoiceS {
                    id: id.0 as i32,
                    label: s(n),
                    on: false,
                })
                .collect(),
        ));
        ui.set_hub_agreement_members(model(
            members
                .iter()
                .map(|(id, n)| ChoiceS {
                    id: id.0 as i32,
                    label: s(n),
                    on: self.hub_pick.contains(id),
                })
                .collect(),
        ));
        ui.set_hub_pick_count(self.hub_pick.len() as i32);
        ui.set_ez_people(self.hub_ledger.members.len() as i32);
        ui.set_ez_has_agreement(!self.hub_ledger.agreements.is_empty());
        ui.set_new_agreement_start_hint(s(agreement_schedule(
            ui.get_new_agreement_how(),
            ui.get_new_agreement_day().trim(),
            ui.get_new_agreement_weekday(),
            "",
            self.today,
        )
        .map(|(_, d)| d.to_string())
        .unwrap_or_default()));
        ui.set_hub_pin_set(self.hub_pin_set);
        // numbered, one paragraph per step
        let plain = |l: &[&str]| {
            s(l.iter()
                .enumerate()
                .map(|(i, x)| format!("{}.  {}", i + 1, view_phone::plain(x)))
                .collect::<Vec<_>>()
                .join("\n\n"))
        };
        ui.set_ez_apple_steps(plain(&view_phone::APPLE));
        ui.set_ez_android_steps(plain(&view_phone::ANDROID));
        ui.set_ez_step(self.ez_step(ui));
    }

    fn admin(&self, method: &'static str, path: String, body: Option<Value>, then: AdminThen) {
        self.send(Job::Admin {
            method,
            path,
            body,
            then,
        });
    }

    fn hub_record(&mut self, ui: &AppWindow, kind: &str, body: Value, done: &str) {
        let Some(id) = self.hub_book.clone() else {
            return ui.set_hub_error(s("Create or unlock a book first."));
        };
        let client_id = self.client_id();
        self.admin(
            "POST",
            format!("/admin/v1/books/{id}/records"),
            Some(json!({"client_id": client_id, "kind": kind, "body": body})),
            AdminThen::Done(done.into()),
        );
    }

    pub fn hub_set_pin(&self, ui: &AppWindow) {
        let pin = ui.get_hub_pin().trim().to_string();
        ui.set_hub_pin(s(""));
        self.admin(
            "POST",
            "/admin/v1/pin".into(),
            Some(json!({ "pin": pin })),
            AdminThen::Done("The household PIN is set.".into()),
        );
    }

    pub fn hub_new_book(&self, ui: &AppWindow) {
        let name = ui.get_new_book_name().trim().to_string();
        if name.is_empty() {
            return ui.set_hub_error(s("Name the book, like Home."));
        }
        let password = ui.get_new_book_password().to_string();
        let password = (!password.is_empty()).then_some(password);
        self.admin(
            "POST",
            "/admin/v1/books".into(),
            Some(json!({
                "name": name,
                "hyperbook": ui.get_new_book_hyperbook(),
                "recovery_key": password.is_some(),
                "password": password,
            })),
            AdminThen::BookMade,
        );
    }

    pub fn hub_unlock(&self, ui: &AppWindow, file: &str) {
        let password = ui.get_unlock_password().to_string();
        ui.set_unlock_password(s(""));
        self.admin(
            "POST",
            format!("/admin/v1/books/{file}/unlock"),
            Some(json!({ "password": password })),
            AdminThen::Done("The book is open.".into()),
        );
    }

    /// EZ Start's step on the hub computer, worked out from where the setup stands (or the
    /// earlier step chosen with Back).
    fn ez_step(&self, ui: &AppWindow) -> i32 {
        let auto = if !self.hub_pin_set {
            1
        } else if ui.get_hub_current_name().is_empty() {
            2
        } else if self.hub_ledger.agreements.is_empty() && !self.ez_people_done {
            3
        } else if self.hub_ledger.agreements.is_empty() {
            4
        } else {
            5
        };
        self.ez_manual.map_or(auto, |m| m.min(auto))
    }

    /// Back one step, to any step: a step already done shows what was done, to change it or
    /// go on.
    pub fn ez_back(&mut self, ui: &AppWindow) {
        let now = self.ez_step(ui);
        self.ez_manual = Some((now - 1).max(1));
        if now == 4 {
            self.ez_people_done = false;
        }
        self.refresh_hub(ui);
    }

    /// On to the next step, keeping what this one has.
    pub fn ez_next(&mut self, ui: &AppWindow) {
        let now = self.ez_step(ui);
        if now >= 3 {
            self.ez_people_done = true;
        }
        // the step after this one, but never past where setup stands
        self.ez_manual = Some(now + 1);
        self.refresh_hub(ui);
    }

    /// Step 5: pair the next device, from 5a.
    pub fn ez_pair_another(&self, ui: &AppWindow) {
        ui.set_hub_code(s(""));
        ui.set_ez_sub(0);
        self.hub_status();
    }

    /// EZ Start is done (or skipped): the full Hub tab from now on.
    pub fn ez_finish(&mut self, ui: &AppWindow) {
        self.cfg.ez_done = true;
        if let Err(e) = self.cfg.save(&self.dir) {
            toast(ui, &e);
        }
        ui.set_hub_code(s(""));
        ui.set_page(5);
    }

    pub fn hub_close_book(&self, ui: &AppWindow) {
        let Some(file) = self.hub_file.clone() else {
            return;
        };
        ui.set_hub_code(s(""));
        self.admin(
            "POST",
            format!("/admin/v1/books/{file}/close"),
            None,
            AdminThen::Done(String::new()),
        );
    }

    pub fn hub_add_member(&mut self, ui: &AppWindow) {
        let name = ui.get_new_member_name().trim().to_string();
        if name.is_empty() {
            return ui.set_hub_error(s("Type the person's name."));
        }
        let id = self
            .hub_ledger
            .members
            .keys()
            .map(|m| m.0)
            .max()
            .unwrap_or(0)
            + 1;
        ui.set_new_member_name(s(""));
        self.hub_record(
            ui,
            "member-add",
            json!({"id": id, "name": name}),
            &format!("{name} was added."),
        );
    }

    pub fn hub_member_picked(&mut self, ui: &AppWindow, id: i32) {
        let m = MemberId(id as u32);
        if !self.hub_pick.remove(&m) {
            self.hub_pick.insert(m);
        }
        self.refresh_hub(ui);
    }

    pub fn hub_new_agreement(&mut self, ui: &AppWindow) {
        let name = ui.get_new_agreement_name().trim().to_string();
        if name.is_empty() || self.hub_pick.is_empty() {
            return ui.set_hub_error(s("Name the agreement and pick who shares in it."));
        }
        let id = self
            .hub_ledger
            .agreements
            .keys()
            .map(|a| a.0)
            .max()
            .unwrap_or(0)
            + 1;
        let (schedule, start) = match agreement_schedule(
            ui.get_new_agreement_how(),
            ui.get_new_agreement_day().trim(),
            ui.get_new_agreement_weekday(),
            ui.get_new_agreement_start().trim(),
            self.today,
        ) {
            Ok(x) => x,
            Err(e) => return ui.set_hub_error(s(e)),
        };
        let members: serde_json::Map<String, Value> = self
            .hub_pick
            .iter()
            .map(|m| (m.0.to_string(), json!(start)))
            .collect();
        let people: Vec<MemberId> = self.hub_pick.iter().copied().collect();
        let equal = ui.get_new_agreement_split() == 0;
        self.hub_pick.clear();
        ui.set_new_agreement_name(s(""));
        ui.set_new_agreement_start(s(""));
        self.hub_record(
            ui,
            "agreement-add",
            json!({"id": id, "name": name, "members": members, "schedule": schedule}),
            &format!("{name} was created."),
        );
        // the first rule, everyone the same, in the name of the first person picked; the
        // others approve it on their devices
        if equal {
            if let Some(by) = people.first() {
                let client_id = self.client_id();
                if let Some(book) = self.hub_book.clone() {
                    self.admin(
                        "POST",
                        format!("/admin/v1/books/{book}/records"),
                        Some(json!({
                            "client_id": client_id,
                            "kind": "rule-proposal",
                            "member": by,
                            "body": {
                                "agreement": id, "rule_id": 1, "effective": start,
                                "rule": {"name": "Shared equally", "method": "Equal", "members": people},
                            },
                        })),
                        AdminThen::Done(String::new()),
                    );
                }
            }
        }
    }

    pub fn hub_make_code(&self, ui: &AppWindow, member: i32) {
        let Some(book) = self.hub_book.clone() else {
            return ui.set_hub_error(s("Create or unlock a book first."));
        };
        self.admin(
            "POST",
            "/admin/v1/pairing".into(),
            Some(json!({"book_id": book, "member": member})),
            AdminThen::Code,
        );
    }

    pub fn hub_remove_device(&self, id: i32) {
        self.admin(
            "POST",
            format!("/admin/v1/devices/{id}/revoke"),
            None,
            AdminThen::Done("The device was removed: it can't reach the hub any more.".into()),
        );
    }
}

fn member_s(m: view::MemberBalance) -> MemberS {
    MemberS {
        id: m.member.0 as i32,
        name: s(m.name),
        balance: s(m.balance),
        sign: m.sign,
    }
}

fn split_s(r: view::SplitRow) -> SplitS {
    SplitS {
        name: s(r.name),
        amount: s(r.amount),
    }
}

pub(crate) fn toast(ui: &AppWindow, msg: &str) {
    ui.set_toast(s(msg));
    let w = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
        if let Some(ui) = w.upgrade() {
            ui.set_toast(s(""));
        }
    });
}

/// The pairing link as a QR code (black on white, 6 px a module, 4-module border).
pub fn qr_image(text: &str) -> Image {
    use qrcodegen::{QrCode, QrCodeEcc};
    let Ok(qr) = QrCode::encode_text(text, QrCodeEcc::Medium) else {
        return Image::default();
    };
    let (scale, border) = (6, 4);
    let n = qr.size();
    let px = ((n + 2 * border) * scale) as u32;
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(px, px);
    let w = px as usize;
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (
            (i % w) as i32 / scale - border,
            (i / w) as i32 / scale - border,
        );
        let dark = qr.get_module(x, y);
        let v = if dark { 0 } else { 255 };
        *p = Rgb8Pixel { r: v, g: v, b: v };
    }
    Image::from_rgb8(buf)
}
