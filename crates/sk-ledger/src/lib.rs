//! ShareKeep's agreement engine.
//!
//! A book's history ([`sk_chain::Record`]s) replays into its state: members, agreements,
//! rules (in force once every other member approves), entries with their splits worked out
//! when posted, deposits, events and closed periods. The household service checks every
//! new record by applying it to a copy first ([`Ledger::check`]); a record that would make
//! the state wrong is refused with a message for the person who sent it. Devices replay
//! their own copy of the history with the same code, so everyone sees the same numbers.
//!
//! Older records that no longer pass (made before a check existed) are skipped on replay
//! and listed, never fatal: a book always opens.

pub mod audit;
pub mod bodies;
pub mod statement;

use bodies::*;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sk_chain::Record;
use sk_core::{AgreementId, Cents, DateSpan, DepositId, MemberId, NaiveDate, RuleId};
use sk_split::deposits::{
    interest, split_shortfall, Deposit, DepositKind, DepositState, Shortfall,
};
use sk_split::ledger::{Balances, Paid, Posting};
use sk_split::method::{period_cap_adjustment, split, Rule, SplitInput};
use sk_split::periods::{place, Event, Placement, Schedule, ScheduleBook};
use sk_split::prorate::Membership;
use sk_split::rules::RuleBook;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use statement::Statement;

/// Largest single amount: one billion (in major units).
pub const MAX_AMOUNT: Cents = 100_000_000_000;

/// Why a record is refused. The text is for the person who sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject(pub String);

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reject {}

fn no(msg: impl Into<String>) -> Reject {
    Reject(msg.into())
}

impl From<sk_split::SplitError> for Reject {
    fn from(e: sk_split::SplitError) -> Self {
        Reject(e.to_string())
    }
}

/// What a disputed entry does until it's resolved (book setting `dispute_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DisputeMode {
    #[default]
    InBalances,
    HeldAside,
}

/// An entry as posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Expense {
        expense: Expense,
        rule: RuleId,
        rule_effective: NaiveDate,
        paid: Paid,
        /// What each member owes of it.
        shares: BTreeMap<MemberId, Cents>,
        /// Under a shortfall the pot couldn't cover: what the author paid on top.
        member_part: Option<(MemberId, BTreeMap<MemberId, Cents>)>,
    },
    Payment(Payment),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub agreement: AgreementId,
    pub date: NaiveDate,
    pub author: Option<MemberId>,
    pub kind: EntryKind,
    /// The event it belongs to, if any.
    pub event: Option<u32>,
    pub disputed: bool,
    pub deleted: bool,
    /// The record that last edited it.
    pub edited_by: Option<u64>,
    /// Pictures and PDFs attached to it, oldest first.
    #[serde(default)]
    pub attachments: Vec<FileRef>,
    /// Whether it counted when its period closed: a closed period's numbers never change
    /// after that, whatever is disputed or set later (review M5).
    #[serde(default)]
    pub counted_at_close: Option<bool>,
}

/// A file attached to an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// The record that attached it, and who.
    pub seq: u64,
    pub by: Option<MemberId>,
}

/// The kinds of file that can be attached.
pub const ATTACH_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "application/pdf"];
/// The largest file that can be attached.
pub const ATTACH_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many files one entry can carry.
pub const ATTACH_MAX: usize = 5;

/// What a file is, from its first bytes: one of [`ATTACH_TYPES`], or `None`.
pub fn sniff_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// Whether `s` is a lower-case hex SHA-256.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Entry {
    /// The postings this entry makes on the balances.
    pub fn postings(&self) -> Vec<Posting> {
        match &self.kind {
            EntryKind::Expense {
                paid,
                shares,
                member_part,
                ..
            } => {
                let mut v = Vec::new();
                match member_part {
                    Some((author, part)) => {
                        let pot: BTreeMap<MemberId, Cents> = shares
                            .iter()
                            .map(|(m, s)| (*m, s - part.get(m).copied().unwrap_or(0)))
                            .collect();
                        v.push(Posting::Expense {
                            paid: *paid,
                            shares: pot,
                        });
                        v.push(Posting::Expense {
                            paid: Paid::Member(*author),
                            shares: part.clone(),
                        });
                    }
                    None => v.push(Posting::Expense {
                        paid: *paid,
                        shares: shares.clone(),
                    }),
                }
                v
            }
            EntryKind::Payment(p) => vec![Posting::Payment {
                from: p.from,
                to: p.to,
                amount: p.amount,
            }],
        }
    }

    pub fn amount(&self) -> Cents {
        match &self.kind {
            EntryKind::Expense { expense, .. } => expense.amount,
            EntryKind::Payment(p) => p.amount,
        }
    }
}

/// A proposal waiting for approvals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    Rule(RuleProposal),
    Leave(MemberLeave),
    /// A buy-out (the member giving up part of their stake approves) or a deduction (approved
    /// like a rule): review M4.
    Deposit(DepositBody),
}

impl ProposalKind {
    /// The agreement it belongs to.
    pub fn agreement(&self) -> AgreementId {
        match self {
            ProposalKind::Rule(r) => r.agreement,
            ProposalKind::Leave(x) => x.agreement,
            ProposalKind::Deposit(d) => d.agreement(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    Waiting,
    Approved,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub seq: u64,
    pub by: MemberId,
    pub kind: ProposalKind,
    /// Who must approve.
    pub approvers: BTreeSet<MemberId>,
    pub approved: BTreeSet<MemberId>,
    pub status: ProposalStatus,
}

/// A closed period and what closing it posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub span: DateSpan,
    pub seq: u64,
    /// Per-period cap adjustments, one per rule that had any.
    pub adjustments: Vec<BTreeMap<MemberId, Cents>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agreement {
    pub id: AgreementId,
    pub name: String,
    pub members: BTreeMap<MemberId, Membership>,
    pub rules: RuleBook,
    pub schedule: ScheduleBook,
    pub events: BTreeMap<u32, Event>,
    pub deposits: BTreeMap<DepositId, (Deposit, DepositState)>,
    pub closed: Vec<Closed>,
    /// Recurring bills and top-ups, by id.
    #[serde(default)]
    pub recurring: BTreeMap<u32, Recurring>,
    /// Occurrences already recorded: (recurring id, due date, the member for a top-up).
    #[serde(default)]
    pub fulfilled: BTreeSet<(u32, NaiveDate, Option<MemberId>)>,
}

/// A recurring bill or top-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recurring {
    pub id: u32,
    pub name: String,
    pub schedule: Schedule,
    pub what: Repeats,
    pub added: NaiveDate,
    pub stopped: Option<NaiveDate>,
}

/// An occurrence that is due (or due within a week) and not recorded yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    pub recurring: u32,
    pub name: String,
    pub date: NaiveDate,
    pub what: Repeats,
    /// For a top-up: whose payment this is.
    pub member: Option<MemberId>,
    pub amount: Cents,
}

/// How far back unrecorded occurrences are still listed.
pub const EXPECTED_LOOKBACK_DAYS: i64 = 92;
/// How far ahead upcoming ones are listed.
pub const EXPECTED_LOOKAHEAD_DAYS: i64 = 7;

impl Agreement {
    /// Members in the agreement on `date`.
    pub fn active_on(&self, date: NaiveDate) -> BTreeSet<MemberId> {
        self.members
            .iter()
            .filter(|(_, s)| s.joined <= date && s.left.is_none_or(|l| date <= l))
            .map(|(m, _)| *m)
            .collect()
    }

    fn is_closed(&self, date: NaiveDate) -> bool {
        self.closed.iter().any(|c| c.span.contains(date))
    }
}

/// The device name the hub computer's own records carry (a device can't be paired under it).
pub const HUB_DEVICE: &str = "hub";

/// Every record kind this version of ShareKeep understands. A device that meets any other
/// kind stops syncing and asks to be updated rather than leave the record out and show wrong
/// balances (book files carry a version for the same reason: `sk_store::SCHEMA_VERSION`).
pub const KINDS: &[&str] = &[
    sk_chain::GENESIS,
    sk_chain::SEAL,
    "setting",
    "member-add",
    "agreement-add",
    "schedule",
    "rule-proposal",
    "member-leave",
    "approval",
    "member-join",
    "expense",
    "recurring",
    "restore",
    "restore-accept",
    "payment",
    "entry-edit",
    "entry-delete",
    "dispute",
    "deposit",
    "period-close",
    "attachment",
];

/// A book's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub members: BTreeMap<MemberId, String>,
    pub agreements: BTreeMap<AgreementId, Agreement>,
    pub entries: BTreeMap<u64, Entry>,
    pub proposals: BTreeMap<u64, Proposal>,
    pub dispute_mode: DisputeMode,
    /// A one-keeper household: this member's approval alone is enough.
    pub keeper: Option<MemberId>,
    /// Interest on held deposits, basis points a year.
    pub interest_bp: u32,
    pub grace_days: u16,
    /// Period seals (Hyperbooks): which statement was sealed at which head.
    pub seals: Vec<Seal>,
    /// Backups the household restored after a broken Hyperbook, and who accepted each.
    pub restores: Vec<Restore>,
    /// The book's time zone (IANA name): its days are counted there. None: UTC.
    #[serde(default)]
    pub time_zone: Option<String>,
}

/// A `restore` record: the hub carried on from a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restore {
    pub seq: u64,
    pub date: NaiveDate,
    pub backup: String,
    /// The backup's last record: the history continues from there.
    pub to_seq: u64,
    pub to_hash: String,
    /// Where the broken book was broken.
    pub broken_at: Option<u64>,
    /// Members who accepted carrying on from the backup.
    pub accepted: BTreeSet<MemberId>,
}

/// A `seal` record: a closed period's statement fingerprint and the head at closing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    /// The seal record itself.
    pub seq: u64,
    pub agreement: AgreementId,
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub head_seq: u64,
    pub head_hash: String,
    pub statement_sha256: Option<String>,
}

/// What applying a record produced beyond the state.
#[derive(Debug, Clone, Default)]
pub struct Effects {
    /// A period was closed: its statement (the hub saves it and seals it).
    pub statement: Option<Statement>,
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Value) -> Result<T, Reject> {
    serde_json::from_value(body.clone()).map_err(|e| no(format!("The record isn't valid ({e}).")))
}

/// The UTC day of a record's timestamp.
pub fn day_of(ts_ms: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(ts_ms)
        .map(|d| d.date_naive())
        .unwrap_or_default()
}

/// The day of a timestamp in a time zone (an IANA name); UTC if the name is unknown.
pub fn day_in(zone: &str, ts_ms: i64) -> NaiveDate {
    let Ok(tz) = jiff::tz::db().get(zone) else {
        return day_of(ts_ms);
    };
    jiff::Timestamp::from_millisecond(ts_ms)
        .ok()
        .map(|t| t.to_zoned_datetime(tz).date())
        .and_then(|d| NaiveDate::from_ymd_opt(d.year().into(), d.month() as u32, d.day() as u32))
        .unwrap_or_else(|| day_of(ts_ms))
}

/// Whether a time zone name is known (`America/New_York`).
pub fn known_zone(zone: &str) -> bool {
    jiff::tz::db().get(zone).is_ok()
}

/// This computer's time zone, if it has an IANA name.
pub fn local_zone() -> Option<String> {
    jiff::tz::TimeZone::system()
        .iana_name()
        .map(str::to_string)
        .filter(|z| known_zone(z))
}

fn check_amount(a: Cents) -> Result<(), Reject> {
    if a == 0 || a.abs() > MAX_AMOUNT {
        return Err(no(
            "The amount must be more than zero and at most one billion.",
        ));
    }
    Ok(())
}

fn check_text(what: &str, s: &str, max: usize) -> Result<(), Reject> {
    if s.trim().is_empty() || s.chars().count() > max {
        return Err(no(format!("The {what} must be 1 to {max} characters.")));
    }
    Ok(())
}

impl Ledger {
    pub fn new() -> Self {
        Ledger {
            grace_days: sk_split::periods::DEFAULT_GRACE_DAYS,
            ..Default::default()
        }
    }

    /// Replay a whole history. Records that don't apply are skipped and listed.
    pub fn replay(records: &[Record]) -> (Ledger, Vec<(u64, Reject)>) {
        let mut l = Ledger::new();
        let mut skipped = Vec::new();
        for r in records {
            if let Err(e) = l.apply(r) {
                skipped.push((r.seq, e));
            }
        }
        (l, skipped)
    }

    /// Would `record` apply? Nothing changes.
    pub fn check(&self, record: &Record) -> Result<Effects, Reject> {
        self.clone().apply(record)
    }

    /// Apply one record. On error nothing changes.
    pub fn apply(&mut self, r: &Record) -> Result<Effects, Reject> {
        let mut next = self.clone();
        let effects = next.apply_inner(r)?;
        *self = next;
        Ok(effects)
    }

    fn agreement(&mut self, id: AgreementId) -> Result<&mut Agreement, Reject> {
        self.agreements
            .get_mut(&id)
            .ok_or_else(|| no("There's no such agreement."))
    }

    fn author_in(
        &self,
        a: AgreementId,
        author: Option<MemberId>,
        date: NaiveDate,
    ) -> Result<(), Reject> {
        let Some(m) = author else {
            return Ok(()); // the hub computer
        };
        let ag = self
            .agreements
            .get(&a)
            .ok_or_else(|| no("There's no such agreement."))?;
        if ag.members.contains_key(&m) && ag.active_on(date).contains(&m) {
            Ok(())
        } else if ag.members.contains_key(&m) {
            // a member who left can still see and settle old things
            Ok(())
        } else {
            Err(no("You aren't in this agreement."))
        }
    }

    fn apply_inner(&mut self, r: &Record) -> Result<Effects, Reject> {
        let author = r.author.member;
        let today = self.day(r.ts);
        let mut effects = Effects::default();
        match r.kind.as_str() {
            sk_chain::GENESIS => {}
            sk_chain::SEAL => {
                let b = &r.body;
                let date = |v: &Value| {
                    v.as_str()
                        .and_then(|t| NaiveDate::parse_from_str(t, "%Y-%m-%d").ok())
                };
                if let (Some(a), Some(first), Some(last), Some(seq), Some(hash)) = (
                    b["agreement"].as_u64(),
                    date(&b["period"]["first"]),
                    date(&b["period"]["last"]),
                    b["head"]["seq"].as_u64(),
                    b["head"]["hash"].as_str(),
                ) {
                    self.seals.push(Seal {
                        seq: r.seq,
                        agreement: AgreementId(a as u32),
                        first,
                        last,
                        head_seq: seq,
                        head_hash: hash.to_string(),
                        statement_sha256: b["statement_sha256"].as_str().map(str::to_string),
                    });
                }
            }
            "setting" => {
                let s: Setting = parse(&r.body)?;
                match s.key.as_str() {
                    "dispute_mode" => {
                        self.dispute_mode = match s.value.as_str() {
                            Some("held_aside") => DisputeMode::HeldAside,
                            _ => DisputeMode::InBalances,
                        }
                    }
                    "one_keeper" => {
                        // chosen before the household's first rule, never switched on later
                        // to put one person's proposals in force (review M3)
                        if !self.proposals.is_empty() {
                            return Err(no(
                                "A one-keeper household is chosen before the first rule is proposed.",
                            ));
                        }
                        self.keeper = s.value.as_u64().map(|v| MemberId(v as u32));
                    }
                    "deposit_interest_bp" => {
                        self.interest_bp = s.value.as_u64().unwrap_or(0).min(10_000) as u32;
                    }
                    "grace_days" => {
                        self.grace_days = s.value.as_u64().unwrap_or(7).min(60) as u16;
                    }
                    "time_zone" => {
                        // chosen when the book is made: moving it later would move entries'
                        // days and closed periods' edges
                        if !self.entries.is_empty() || !self.proposals.is_empty() {
                            return Err(no(
                                "A book's time zone is chosen when it is made, before anything is recorded.",
                            ));
                        }
                        let z = s.value.as_str().unwrap_or("");
                        if !known_zone(z) {
                            return Err(no(format!("\"{z}\" isn't a time zone.")));
                        }
                        self.time_zone = Some(z.to_string());
                    }
                    _ => {}
                }
            }
            "member-add" => {
                if author.is_some() {
                    return Err(no("Members are added on the hub computer."));
                }
                let b: MemberAdd = parse(&r.body)?;
                check_text("name", &b.name, 64)?;
                if self.members.contains_key(&b.id) {
                    return Err(no("That member already exists."));
                }
                self.members.insert(b.id, b.name.trim().to_string());
            }
            "agreement-add" => {
                if author.is_some() {
                    return Err(no("Agreements are added on the hub computer."));
                }
                let b: AgreementAdd = parse(&r.body)?;
                check_text("name", &b.name, 64)?;
                if self.agreements.contains_key(&b.id) {
                    return Err(no("That agreement already exists."));
                }
                if b.members.is_empty() {
                    return Err(no("An agreement needs at least one member."));
                }
                for m in b.members.keys() {
                    if !self.members.contains_key(m) {
                        return Err(no(format!("{m} isn't a member of this book.")));
                    }
                }
                let schedule = ScheduleBook::new(b.schedule)?;
                self.agreements.insert(
                    b.id,
                    Agreement {
                        id: b.id,
                        name: b.name.trim().to_string(),
                        members: b
                            .members
                            .iter()
                            .map(|(m, d)| {
                                (
                                    *m,
                                    Membership {
                                        joined: *d,
                                        left: None,
                                    },
                                )
                            })
                            .collect(),
                        rules: RuleBook::new(),
                        schedule,
                        events: BTreeMap::new(),
                        deposits: BTreeMap::new(),
                        closed: Vec::new(),
                        recurring: BTreeMap::new(),
                        fulfilled: BTreeSet::new(),
                    },
                );
            }
            "schedule" => {
                let b: ScheduleBody = parse(&r.body)?;
                match b {
                    ScheduleBody::Change {
                        agreement,
                        schedule,
                    } => {
                        self.author_in(agreement, author, today)?;
                        let ag = self.agreement(agreement)?;
                        ag.schedule.change(today, |start| {
                            let mut s = schedule.clone();
                            set_start(&mut s, start);
                            s
                        })?;
                    }
                    ScheduleBody::Event {
                        agreement,
                        id,
                        name,
                        first,
                        last,
                        members,
                    } => {
                        self.author_in(agreement, author, today)?;
                        check_text("event name", &name, 64)?;
                        let span = DateSpan::new(first, last)
                            .ok_or_else(|| no("An event ends on or after it starts."))?;
                        let ag = self.agreement(agreement)?;
                        if ag.events.contains_key(&id) {
                            return Err(no("That event already exists."));
                        }
                        let members: BTreeSet<MemberId> = if members.is_empty() {
                            ag.members.keys().copied().collect()
                        } else {
                            members.into_iter().collect()
                        };
                        if members.iter().any(|m| !ag.members.contains_key(m)) {
                            return Err(no("Everyone in an event must be in the agreement."));
                        }
                        ag.events.insert(
                            id,
                            Event {
                                name: name.trim().to_string(),
                                span,
                                members,
                            },
                        );
                    }
                }
            }
            "rule-proposal" => {
                let b: RuleProposal = parse(&r.body)?;
                let by = author.ok_or_else(|| no("A rule is proposed by a member."))?;
                self.author_in(b.agreement, author, today)?;
                let ag = self
                    .agreements
                    .get(&b.agreement)
                    .ok_or_else(|| no("There's no such agreement."))?;
                for m in &b.rule.members {
                    if !ag.members.contains_key(m) {
                        return Err(no(format!("{m} isn't in this agreement.")));
                    }
                }
                b.rule.validate()?;
                check_text("rule name", &b.rule.name, 64)?;
                if ag
                    .rules
                    .versions(b.rule_id)
                    .iter()
                    .any(|v| v.effective == b.effective)
                {
                    return Err(no("That rule already has a version from that date."));
                }
                let approvers = self.approvers(b.agreement, by, today);
                self.propose(r.seq, by, ProposalKind::Rule(b), approvers)?;
            }
            "member-leave" => {
                let b: MemberLeave = parse(&r.body)?;
                let by = author.ok_or_else(|| no("A leave is proposed by a member."))?;
                self.author_in(b.agreement, author, today)?;
                let ag = self
                    .agreements
                    .get(&b.agreement)
                    .ok_or_else(|| no("There's no such agreement."))?;
                let stay = ag
                    .members
                    .get(&b.member)
                    .ok_or_else(|| no("That member isn't in this agreement."))?;
                if stay.left.is_some() {
                    return Err(no("That member has already left."));
                }
                if b.date < stay.joined {
                    return Err(no("The leave date is before they joined."));
                }
                if ag.is_closed(b.date) {
                    return Err(no("That date is in a closed period."));
                }
                // the leaving member and everyone staying must approve
                let mut approvers = self.approvers(b.agreement, by, today);
                if self.keeper.is_none() && b.member != by {
                    approvers.insert(b.member);
                }
                self.propose(r.seq, by, ProposalKind::Leave(b), approvers)?;
            }
            "approval" => {
                let b: Approval = parse(&r.body)?;
                let who = author.ok_or_else(|| no("Approvals come from members."))?;
                if r.author.device == HUB_DEVICE {
                    return Err(no("Approvals come from the person's own device."));
                }
                let p = self
                    .proposals
                    .get_mut(&b.proposal)
                    .ok_or_else(|| no("There's no such proposal."))?;
                if p.status != ProposalStatus::Waiting {
                    return Err(no("That proposal was already decided."));
                }
                if !p.approvers.contains(&who) {
                    return Err(no("Your approval isn't needed for this proposal."));
                }
                if b.approve {
                    p.approved.insert(who);
                    let keeper_ok = self.keeper == Some(who);
                    if keeper_ok || p.approved.is_superset(&p.approvers) {
                        let seq = b.proposal;
                        self.complete(seq)?;
                    }
                } else {
                    p.status = ProposalStatus::Declined;
                }
            }
            "member-join" => {
                let b: MemberJoin = parse(&r.body)?;
                self.author_in(b.agreement, author, today)?;
                if !self.members.contains_key(&b.member) {
                    return Err(no("Add that person to the book first."));
                }
                let ag = self.agreement(b.agreement)?;
                if ag.members.get(&b.member).is_some_and(|s| s.left.is_none()) {
                    return Err(no("That member is already in the agreement."));
                }
                if ag.is_closed(b.date) {
                    return Err(no("That date is in a closed period."));
                }
                ag.members.insert(
                    b.member,
                    Membership {
                        joined: b.date,
                        left: None,
                    },
                );
            }
            "expense" => {
                let e: Expense = parse(&r.body)?;
                let entry = self.post_expense(r.seq, author, e, None)?;
                self.fulfil_entry(&entry, true);
                self.entries.insert(r.seq, entry);
            }
            "recurring" => {
                let b: RecurringBody = parse(&r.body)?;
                self.recurring(author, today, b)?;
            }
            "restore" => {
                if author.is_some() {
                    return Err(no("A restore is made by the hub computer."));
                }
                let b = &r.body;
                let to_seq = b["restored_to"]["seq"]
                    .as_u64()
                    .ok_or_else(|| no("A restore names the record it continues from."))?;
                if to_seq + 1 != r.seq {
                    return Err(no("A restore comes right after the backup's last record."));
                }
                self.restores.push(Restore {
                    seq: r.seq,
                    date: today,
                    backup: b["backup"].as_str().unwrap_or("").to_string(),
                    to_seq,
                    to_hash: b["restored_to"]["hash"].as_str().unwrap_or("").to_string(),
                    broken_at: b["broken_at"].as_u64(),
                    accepted: BTreeSet::new(),
                });
            }
            "restore-accept" => {
                let m = author.ok_or_else(|| no("A member accepts a restore."))?;
                if r.author.device == HUB_DEVICE {
                    return Err(no("A restore is accepted on the person's own device."));
                }
                let target = r.body["restore"]
                    .as_u64()
                    .ok_or_else(|| no("Say which restore is accepted."))?;
                let rs = self
                    .restores
                    .iter_mut()
                    .find(|x| x.seq == target)
                    .ok_or_else(|| no("There's no such restore."))?;
                rs.accepted.insert(m);
            }
            "payment" => {
                let p: Payment = parse(&r.body)?;
                check_amount(p.amount)?;
                if p.amount < 0 {
                    return Err(no("A payment is more than zero."));
                }
                if p.from == p.to {
                    return Err(no("A payment is between two different members."));
                }
                if let Some(a) = author {
                    if a != p.from && a != p.to {
                        return Err(no(
                            "Only the payer or the person paid can record a payment.",
                        ));
                    }
                }
                let ag = self
                    .agreements
                    .get(&p.agreement)
                    .ok_or_else(|| no("There's no such agreement."))?;
                if !ag.members.contains_key(&p.from) || !ag.members.contains_key(&p.to) {
                    return Err(no("Both people must be in the agreement."));
                }
                if ag.is_closed(p.date) {
                    return Err(no("That date is in a closed period: date it today."));
                }
                place(p.date, None, &ag.schedule)?;
                self.entries.insert(
                    r.seq,
                    Entry {
                        seq: r.seq,
                        agreement: p.agreement,
                        date: p.date,
                        author,
                        event: None,
                        kind: EntryKind::Payment(p),
                        disputed: false,
                        deleted: false,
                        edited_by: None,
                        attachments: Vec::new(),
                        counted_at_close: None,
                    },
                );
            }
            "entry-edit" => {
                let b: EntryEdit = parse(&r.body)?;
                let old = self.open_entry(b.target)?;
                if b.expense.agreement != old.agreement {
                    return Err(no("An entry stays in its agreement."));
                }
                if !matches!(old.kind, EntryKind::Expense { .. }) {
                    return Err(no(
                        "Only expenses can be edited; record another payment instead.",
                    ));
                }
                self.undo_pot(&old)?;
                self.fulfil_entry(&old, false);
                let mut entry = self.post_expense(old.seq, author, b.expense, Some(&old))?;
                self.fulfil_entry(&entry, true);
                entry.edited_by = Some(r.seq);
                entry.author = old.author;
                entry.disputed = old.disputed;
                entry.attachments = old.attachments.clone();
                self.entries.insert(old.seq, entry);
            }
            "entry-delete" => {
                let b: EntryDelete = parse(&r.body)?;
                let old = self.open_entry(b.target)?;
                self.author_in(old.agreement, author, today)?;
                self.undo_pot(&old)?;
                // a deleted recurring bill is due again
                self.fulfil_entry(&old, false);
                if let Some(e) = self.entries.get_mut(&b.target) {
                    e.deleted = true;
                    e.edited_by = Some(r.seq);
                }
            }
            "dispute" => {
                let b: Dispute = parse(&r.body)?;
                let e = self
                    .entries
                    .get(&b.target)
                    .ok_or_else(|| no("There's no such entry."))?
                    .clone();
                self.author_in(e.agreement, author, today)?;
                if let Some(e) = self.entries.get_mut(&b.target) {
                    e.disputed = !b.resolved;
                }
            }
            "attachment" => {
                let b: AttachmentBody = parse(&r.body)?;
                let e = self
                    .entries
                    .get(&b.target)
                    .ok_or_else(|| no("There's no such entry."))?
                    .clone();
                if e.deleted {
                    return Err(no("That entry was deleted."));
                }
                if !matches!(e.kind, EntryKind::Expense { .. }) {
                    return Err(no("Pictures and PDFs go with expenses."));
                }
                self.author_in(e.agreement, author, today)?;
                if !is_sha256_hex(&b.sha256) {
                    return Err(no("That file's fingerprint isn't valid."));
                }
                let at = e.attachments.iter().position(|f| f.sha256 == b.sha256);
                let entry = self.entries.get_mut(&b.target).expect("checked above");
                if b.remove {
                    let i = at.ok_or_else(|| no("That file isn't attached to this entry."))?;
                    entry.attachments.remove(i);
                } else {
                    if at.is_some() {
                        return Err(no("That file is already attached."));
                    }
                    if entry.attachments.len() >= ATTACH_MAX {
                        return Err(no(format!(
                            "An expense can carry {ATTACH_MAX} pictures or PDFs."
                        )));
                    }
                    if !ATTACH_TYPES.contains(&b.mime.as_str()) {
                        return Err(no(
                            "Only pictures (JPEG, PNG, WebP) and PDFs can be attached.",
                        ));
                    }
                    if b.size == 0 || b.size > ATTACH_MAX_BYTES {
                        return Err(no("A file can be at most 10 MB."));
                    }
                    let name: String = b
                        .name
                        .trim()
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(80)
                        .collect();
                    entry.attachments.push(FileRef {
                        sha256: b.sha256,
                        name: if name.is_empty() { "file".into() } else { name },
                        mime: b.mime,
                        size: b.size,
                        seq: r.seq,
                        by: author,
                    });
                }
            }
            "deposit" => {
                let b: DepositBody = parse(&r.body)?;
                self.deposit(r.seq, author, today, b)?;
            }
            "period-close" => {
                let b: PeriodClose = parse(&r.body)?;
                self.author_in(b.agreement, author, today)?;
                effects.statement = Some(self.close(r.seq, &r.hash.to_hex(), b, today)?);
            }
            other => return Err(no(format!("ShareKeep doesn't know \"{other}\" records."))),
        }
        Ok(effects)
    }

    /// Who must approve a proposal by `by`: everyone else in the agreement, or the keeper.
    fn approvers(&self, a: AgreementId, by: MemberId, today: NaiveDate) -> BTreeSet<MemberId> {
        if let Some(k) = self.keeper {
            if k == by {
                return BTreeSet::new();
            }
            return [k].into_iter().collect();
        }
        self.agreements
            .get(&a)
            .map(|ag| ag.active_on(today))
            .unwrap_or_default()
            .into_iter()
            .filter(|m| *m != by)
            .collect()
    }

    fn propose(
        &mut self,
        seq: u64,
        by: MemberId,
        kind: ProposalKind,
        approvers: BTreeSet<MemberId>,
    ) -> Result<(), Reject> {
        let done = approvers.is_empty();
        self.proposals.insert(
            seq,
            Proposal {
                seq,
                by,
                kind,
                approvers,
                approved: BTreeSet::new(),
                status: ProposalStatus::Waiting,
            },
        );
        if done {
            self.complete(seq)?;
        }
        Ok(())
    }

    fn complete(&mut self, seq: u64) -> Result<(), Reject> {
        let p = self
            .proposals
            .get(&seq)
            .ok_or_else(|| no("There's no such proposal."))?
            .clone();
        match &p.kind {
            ProposalKind::Rule(b) => {
                let ag = self.agreement(b.agreement)?;
                ag.rules
                    .add_version(b.rule_id, b.effective, b.rule.clone())?;
                for c in &b.categories {
                    ag.rules.set_default(c.clone(), b.rule_id);
                }
                // the agreement's first rule is its fallback until another is chosen
                if b.fallback || ag.rules.fallback().is_none() {
                    ag.rules.set_fallback(b.rule_id);
                }
            }
            ProposalKind::Leave(b) => {
                let ag = self.agreement(b.agreement)?;
                if let Some(s) = ag.members.get_mut(&b.member) {
                    s.left = Some(b.date);
                }
            }
            ProposalKind::Deposit(b) => self.apply_deposit(b.clone())?,
        }
        if let Some(p) = self.proposals.get_mut(&seq) {
            p.status = ProposalStatus::Approved;
        }
        Ok(())
    }

    fn open_entry(&self, seq: u64) -> Result<Entry, Reject> {
        let e = self
            .entries
            .get(&seq)
            .ok_or_else(|| no("There's no such entry."))?;
        if e.deleted {
            return Err(no("That entry was deleted."));
        }
        let ag = self
            .agreements
            .get(&e.agreement)
            .ok_or_else(|| no("There's no such agreement."))?;
        if ag.is_closed(e.date) {
            return Err(no(
                "That entry is in a closed period: record a correction in the current period instead.",
            ));
        }
        Ok(e.clone())
    }

    /// Give a pot back what an entry took from it (before an edit or a delete).
    fn undo_pot(&mut self, e: &Entry) -> Result<(), Reject> {
        if let EntryKind::Expense {
            paid: Paid::Pot(d),
            shares,
            member_part,
            ..
        } = &e.kind
        {
            let ag = self.agreement(e.agreement)?;
            let (_, state) = ag
                .deposits
                .get_mut(d)
                .ok_or_else(|| no("That deposit is missing."))?;
            for (m, s) in shares {
                let part = member_part
                    .as_ref()
                    .and_then(|(_, p)| p.get(m).copied())
                    .unwrap_or(0);
                let from_pot = s - part;
                if from_pot > 0 {
                    state.pay_in(*m, from_pot)?;
                }
            }
        }
        Ok(())
    }

    fn post_expense(
        &mut self,
        seq: u64,
        author: Option<MemberId>,
        e: Expense,
        _old: Option<&Entry>,
    ) -> Result<Entry, Reject> {
        check_amount(e.amount)?;
        check_text("category", &e.category, 64)?;
        if let Some(n) = &e.note {
            if n.chars().count() > 500 {
                return Err(no("A note is at most 500 characters."));
            }
        }
        let ag = self
            .agreements
            .get(&e.agreement)
            .ok_or_else(|| no("There's no such agreement."))?;
        if let Some(a) = author {
            if !ag.members.contains_key(&a) {
                return Err(no("You aren't in this agreement."));
            }
        }
        if ag.is_closed(e.date) {
            return Err(no(
                "That date is in a closed period: record it as a correction in the current period.",
            ));
        }
        if let Some(d) = e.due {
            match ag.recurring.get(&d.recurring) {
                Some(r) if matches!(r.what, Repeats::Bill { .. }) => {}
                _ => return Err(no("That isn't one of this agreement's recurring bills.")),
            }
            if ag.fulfilled.contains(&(d.recurring, d.date, None)) {
                return Err(no("That bill is already recorded."));
            }
        }
        let event = match e.event {
            Some(id) => Some(
                ag.events
                    .get(&id)
                    .ok_or_else(|| no("There's no such event."))?,
            ),
            None => None,
        };
        let placement = place(e.date, event, &ag.schedule)?;
        let active = ag.active_on(e.date);
        let resolved = ag.rules.resolve(&e.category, e.date, e.rule)?;
        let mut rule: Rule = resolved.rule.clone();
        // members not in the agreement that day share nothing; a percentage rule then
        // no longer adds up and must be updated (approved again)
        for m in rule.members.clone() {
            if !active.contains(&m) {
                rule.excluded.insert(m);
            }
        }
        if let Placement::Event(ev) = placement {
            for m in rule.members.clone() {
                if !ev.members.contains(&m) {
                    rule.excluded.insert(m);
                }
            }
        }
        let payer_member = match e.payer {
            Payer::Member(m) => Some(m),
            Payer::Pot(_) => None,
        };
        let shares = split(
            &rule,
            &SplitInput {
                amount: e.amount,
                seq,
                payer: payer_member,
                items: e.items.clone(),
                days: e.days.clone(),
                usage: e.usage.clone(),
            },
        )
        .map_err(|err| {
            if rule.excluded.len() > resolved.rule.excluded.len() {
                no(format!(
                    "The rule \"{}\" needs updating since someone joined or left ({err}).",
                    rule.name
                ))
            } else {
                Reject::from(err)
            }
        })?;
        let (rule_id, rule_effective) = (resolved.id, resolved.effective);
        let mut member_part = None;
        let paid = match e.payer {
            Payer::Member(m) => {
                if !ag.members.contains_key(&m) {
                    return Err(no("The payer must be in the agreement."));
                }
                Paid::Member(m)
            }
            Payer::Pot(d) => {
                let ag = self.agreement(e.agreement)?;
                let (dep, state) = ag
                    .deposits
                    .get_mut(&d)
                    .ok_or_else(|| no("There's no such deposit."))?;
                if dep.kind != DepositKind::Spending {
                    return Err(no("Only a spending deposit pays bills."));
                }
                if dep.rule.is_some_and(|r| r != rule_id) {
                    return Err(no("That pot only pays its own rule's bills."));
                }
                match state.spend(DepositKind::Spending, &shares) {
                    Ok(()) => {}
                    Err(err) => {
                        let who = author.ok_or_else(|| Reject::from(err.clone()))?;
                        if dep.shortfall != Shortfall::Debts {
                            return Err(err.into());
                        }
                        let (pot, mine) = split_shortfall(&shares, state.balance())?;
                        state.spend(DepositKind::Spending, &pot)?;
                        member_part = Some((who, mine));
                    }
                }
                Paid::Pot(d)
            }
        };
        Ok(Entry {
            seq,
            agreement: e.agreement,
            date: e.date,
            author,
            event: e.event,
            kind: EntryKind::Expense {
                expense: e,
                rule: rule_id,
                rule_effective,
                paid,
                shares,
                member_part,
            },
            disputed: false,
            deleted: false,
            edited_by: None,
            attachments: Vec::new(),
            counted_at_close: None,
        })
    }

    fn deposit(
        &mut self,
        seq: u64,
        author: Option<MemberId>,
        today: NaiveDate,
        b: DepositBody,
    ) -> Result<(), Reject> {
        let agreement = b.agreement();
        self.author_in(agreement, author, today)?;
        // moving or cutting someone's stake needs their say (review M4): a buy-out the member
        // giving up part of their stake approves, a deduction everyone (or the keeper) does
        let approvers = match (&b, author) {
            (DepositBody::BuyOut { from, .. }, Some(by)) if *from != by => [*from].into(),
            (DepositBody::Deduct { shares, .. }, Some(by)) => {
                if shares.values().any(|s| *s < 0) {
                    return Err(no("A deduction takes from the pot: no negative parts."));
                }
                self.approvers(agreement, by, today)
            }
            _ => BTreeSet::new(),
        };
        if approvers.is_empty() {
            return self.apply_deposit(b);
        }
        // it must work today; it is checked again when the last approval comes
        self.clone().apply_deposit(b.clone())?;
        let by = author.expect("approvers only for a member's request");
        self.propose(seq, by, ProposalKind::Deposit(b), approvers)
    }

    fn apply_deposit(&mut self, b: DepositBody) -> Result<(), Reject> {
        let agreement = b.agreement();
        let ag = self.agreement(agreement)?;
        let member_ok = |ag: &Agreement, m: &MemberId| -> Result<(), Reject> {
            if ag.members.contains_key(m) {
                Ok(())
            } else {
                Err(no(format!("{m} isn't in this agreement.")))
            }
        };
        match b {
            DepositBody::Create {
                id,
                kind,
                label,
                rule,
                holder,
                refundable,
                shortfall,
                ..
            } => {
                check_text("deposit name", &label, 64)?;
                if ag.deposits.contains_key(&id) {
                    return Err(no("That deposit already exists."));
                }
                ag.deposits.insert(
                    id,
                    (
                        Deposit {
                            id,
                            kind,
                            label: label.trim().to_string(),
                            rule,
                            holder,
                            refundable,
                            shortfall,
                        },
                        DepositState::new(),
                    ),
                );
            }
            DepositBody::PayIn {
                id,
                member,
                amount,
                due,
                ..
            } => {
                check_amount(amount)?;
                member_ok(ag, &member)?;
                if let Some(d) = due {
                    match ag.recurring.get(&d.recurring).map(|r| &r.what) {
                        Some(Repeats::TopUp { deposit, .. }) if *deposit == id => {}
                        _ => return Err(no("That isn't a top-up of this pot.")),
                    }
                    if !ag.fulfilled.insert((d.recurring, d.date, Some(member))) {
                        return Err(no("That top-up is already recorded."));
                    }
                }
                let (_, st) = ag
                    .deposits
                    .get_mut(&id)
                    .ok_or_else(|| no("There's no such deposit."))?;
                st.pay_in(member, amount)?;
            }
            DepositBody::BuyOut {
                id,
                from,
                to,
                amount,
                ..
            } => {
                check_amount(amount)?;
                member_ok(ag, &from)?;
                member_ok(ag, &to)?;
                let (dep, st) = ag
                    .deposits
                    .get_mut(&id)
                    .ok_or_else(|| no("There's no such deposit."))?;
                if dep.kind != DepositKind::Held {
                    return Err(no("Only a held deposit is bought out."));
                }
                st.buy_out(from, to, amount)?;
            }
            DepositBody::Deduct { id, shares, .. } => {
                for m in shares.keys() {
                    member_ok(ag, m)?;
                }
                let (_, st) = ag
                    .deposits
                    .get_mut(&id)
                    .ok_or_else(|| no("There's no such deposit."))?;
                st.deduct(&shares)?;
            }
            DepositBody::Refund { id, amount, .. } => {
                check_amount(amount)?;
                let (dep, st) = ag
                    .deposits
                    .get_mut(&id)
                    .ok_or_else(|| no("There's no such deposit."))?;
                let dep = dep.clone();
                st.refund(&dep, amount)?;
            }
        }
        Ok(())
    }

    /// Balances of an agreement: every live entry (disputed ones too unless the book holds
    /// them aside) and every close's adjustments.
    pub fn balances(&self, a: AgreementId) -> Balances {
        self.balances_until(a, None)
    }

    /// The book's day at a moment: in its time zone (UTC for an older book).
    pub fn day(&self, ts_ms: i64) -> NaiveDate {
        match &self.time_zone {
            Some(z) => day_in(z, ts_ms),
            None => day_of(ts_ms),
        }
    }

    fn counts(&self, e: &Entry) -> bool {
        if let Some(c) = e.counted_at_close {
            return c;
        }
        !e.deleted && !(e.disputed && self.dispute_mode == DisputeMode::HeldAside)
    }

    pub fn balances_until(&self, a: AgreementId, until: Option<NaiveDate>) -> Balances {
        let mut b = Balances::new();
        for e in self.entries.values() {
            if e.agreement != a || !self.counts(e) || until.is_some_and(|u| e.date > u) {
                continue;
            }
            for p in e.postings() {
                let _ = b.apply(&p);
            }
        }
        if let Some(ag) = self.agreements.get(&a) {
            for c in &ag.closed {
                if until.is_some_and(|u| c.span.last > u) {
                    continue;
                }
                for adj in &c.adjustments {
                    let _ = b.apply(&Posting::Adjustment {
                        shares: adj.clone(),
                    });
                }
            }
        }
        b
    }

    fn close(
        &mut self,
        seq: u64,
        hash: &str,
        b: PeriodClose,
        today: NaiveDate,
    ) -> Result<Statement, Reject> {
        let ag = self
            .agreements
            .get(&b.agreement)
            .ok_or_else(|| no("There's no such agreement."))?;
        let span = ag
            .schedule
            .period_containing(b.date)
            .ok_or_else(|| no("No period covers that date."))?;
        if ag.is_closed(span.first) {
            return Err(no("That period is already closed."));
        }
        if today <= span.last {
            return Err(no(format!(
                "That period runs until {}: it can be closed from the day after.",
                span.last
            )));
        }
        // per-period caps: each rule's totals in the period
        let mut totals: BTreeMap<RuleId, BTreeMap<MemberId, Cents>> = BTreeMap::new();
        for e in self.entries.values() {
            if e.agreement != b.agreement
                || !span.contains(e.date)
                || e.event.is_some()
                || !self.counts(e)
            {
                continue;
            }
            if let EntryKind::Expense { rule, shares, .. } = &e.kind {
                let t = totals.entry(*rule).or_default();
                for (m, s) in shares {
                    *t.entry(*m).or_default() += s;
                }
            }
        }
        let mut adjustments = Vec::new();
        for (rule_id, t) in &totals {
            if let Some(v) = ag.rules.rule_at(*rule_id, span.last) {
                let adj = period_cap_adjustment(v.rule, t, seq)?;
                if !adj.is_empty() {
                    adjustments.push(adj);
                }
            }
        }
        let bp = self.interest_bp;
        let ag = self.agreement(b.agreement)?;
        // interest on held deposits for the period, at the book's rate (none by default)
        if bp > 0 {
            for (dep, st) in ag.deposits.values_mut() {
                if dep.kind == DepositKind::Held {
                    let amount = interest(st.balance(), bp, span.days());
                    st.credit_interest(amount)?;
                }
            }
        }
        ag.closed.push(Closed {
            span,
            seq,
            adjustments,
        });
        // what counted is fixed now
        let fixed: Vec<(u64, bool)> = self
            .entries
            .values()
            .filter(|e| e.agreement == b.agreement && span.contains(e.date))
            .map(|e| (e.seq, self.counts(e)))
            .collect();
        for (seq, c) in fixed {
            if let Some(e) = self.entries.get_mut(&seq) {
                e.counted_at_close = Some(c);
            }
        }
        Ok(statement::build(
            self,
            b.agreement,
            span,
            seq,
            hash.to_string(),
        ))
    }

    fn recurring(
        &mut self,
        author: Option<MemberId>,
        today: NaiveDate,
        b: RecurringBody,
    ) -> Result<(), Reject> {
        match b {
            RecurringBody::Add {
                agreement,
                id,
                name,
                schedule,
                what,
            } => {
                self.author_in(agreement, author, today)?;
                check_text("name", &name, 64)?;
                ScheduleBook::new(schedule.clone())?;
                let ag = self.agreement(agreement)?;
                if ag.recurring.contains_key(&id) {
                    return Err(no("That recurring item already exists."));
                }
                match &what {
                    Repeats::Bill {
                        amount,
                        category,
                        payer,
                    } => {
                        check_amount(*amount)?;
                        check_text("category", category, 64)?;
                        if let Some(p) = payer {
                            if !ag.members.contains_key(p) {
                                return Err(no(format!("{p} isn't in this agreement.")));
                            }
                        }
                    }
                    Repeats::TopUp { deposit, amounts } => {
                        match ag.deposits.get(deposit) {
                            Some((d, _)) if d.kind == DepositKind::Spending => {}
                            _ => return Err(no("Top-ups go into a spending deposit (a pot).")),
                        }
                        if amounts.is_empty() {
                            return Err(no("Say how much each member puts in."));
                        }
                        for (m, a) in amounts {
                            check_amount(*a)?;
                            if !ag.members.contains_key(m) {
                                return Err(no(format!("{m} isn't in this agreement.")));
                            }
                        }
                    }
                }
                ag.recurring.insert(
                    id,
                    Recurring {
                        id,
                        name: name.trim().to_string(),
                        schedule,
                        what,
                        added: today,
                        stopped: None,
                    },
                );
            }
            RecurringBody::Stop { agreement, id } => {
                self.author_in(agreement, author, today)?;
                let ag = self.agreement(agreement)?;
                let r = ag
                    .recurring
                    .get_mut(&id)
                    .ok_or_else(|| no("There's no such recurring item."))?;
                r.stopped.get_or_insert(today);
            }
        }
        Ok(())
    }

    /// Mark (or unmark) the recurring bill an expense entry records.
    fn fulfil_entry(&mut self, e: &Entry, on: bool) {
        let EntryKind::Expense { expense, .. } = &e.kind else {
            return;
        };
        let Some(d) = expense.due else { return };
        if let Some(ag) = self.agreements.get_mut(&e.agreement) {
            if on {
                ag.fulfilled.insert((d.recurring, d.date, None));
            } else {
                ag.fulfilled.remove(&(d.recurring, d.date, None));
            }
        }
    }

    /// What's due in an agreement on `today` (and within a week), not recorded yet: bills for
    /// anyone to record, top-ups per member. Oldest first.
    pub fn expected(&self, a: AgreementId, today: NaiveDate) -> Vec<Expected> {
        let Some(ag) = self.agreements.get(&a) else {
            return Vec::new();
        };
        let from = today - chrono::Duration::days(EXPECTED_LOOKBACK_DAYS);
        let to = today + chrono::Duration::days(EXPECTED_LOOKAHEAD_DAYS);
        let mut out = Vec::new();
        for r in ag.recurring.values() {
            let first = from.max(r.added);
            let last = r.stopped.map_or(to, |s| s.min(to));
            let Some(span) = DateSpan::new(first, last) else {
                continue;
            };
            for date in r.schedule.dates_in(&span) {
                if ag.is_closed(date) {
                    continue;
                }
                match &r.what {
                    Repeats::Bill { amount, .. } => {
                        if !ag.fulfilled.contains(&(r.id, date, None)) {
                            out.push(Expected {
                                recurring: r.id,
                                name: r.name.clone(),
                                date,
                                what: r.what.clone(),
                                member: None,
                                amount: *amount,
                            });
                        }
                    }
                    Repeats::TopUp { amounts, .. } => {
                        for (m, amount) in amounts {
                            let active = ag.members.get(m).is_some_and(|s| {
                                s.joined <= date && s.left.is_none_or(|l| date <= l)
                            });
                            if active && !ag.fulfilled.contains(&(r.id, date, Some(*m))) {
                                out.push(Expected {
                                    recurring: r.id,
                                    name: r.name.clone(),
                                    date,
                                    what: r.what.clone(),
                                    member: Some(*m),
                                    amount: *amount,
                                });
                            }
                        }
                    }
                }
            }
        }
        out.sort_by_key(|e| (e.date, e.recurring, e.member));
        out
    }

    /// How an expense would be divided if posted now (the live preview while typing).
    pub fn preview_expense(&self, author: Option<MemberId>, e: Expense) -> Result<Entry, Reject> {
        let seq = self.entries.keys().next_back().map_or(1, |s| s + 1);
        self.clone().post_expense(seq, author, e, None)
    }

    /// Proposals waiting for `member`'s answer.
    pub fn waiting_for(&self, member: MemberId) -> Vec<&Proposal> {
        self.proposals
            .values()
            .filter(|p| {
                p.status == ProposalStatus::Waiting
                    && p.approvers.contains(&member)
                    && !p.approved.contains(&member)
            })
            .collect()
    }
}

fn set_start(s: &mut sk_split::periods::Schedule, start: NaiveDate) {
    use sk_split::periods::Schedule::*;
    match s {
        Weekly { start: x, .. }
        | Monthly { start: x, .. }
        | EveryNDays { start: x, .. }
        | Custom { start: x, .. } => *x = start,
    }
}
