//! View models: what each screen shows, worked out from a book's state. Plain Rust (no
//! window needed), so it is tested on its own and shared by the desktop and web apps.

use serde::Serialize;
use serde_json::json;
use sk_core::{AgreementId, Cents, Currency, DateSpan, MemberId, NaiveDate, RuleId};
use sk_ledger::bodies::{Expense, Payer, Repeats};
use sk_ledger::{EntryKind, Ledger, ProposalKind, ProposalStatus};
use sk_split::deposits::DepositKind;
use sk_split::ledger::{settle_up, Paid};
use sk_split::method::{split, Method, Rule, SplitInput, FULL_BP};
use sk_split::periods::status;
use std::collections::BTreeMap;

/// "$1,234.50", "-$3.00".
pub fn money(cur: Currency, v: Cents) -> String {
    cur.format(v)
}

fn name(l: &Ledger, m: MemberId) -> String {
    l.members
        .get(&m)
        .cloned()
        .unwrap_or_else(|| format!("Member {}", m.0))
}

/// Where a member stands: owed (+1), owes (−1), settled (0).
pub fn sign(v: Cents) -> i32 {
    v.signum() as i32
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberBalance {
    pub member: MemberId,
    pub name: String,
    pub balance: String,
    pub sign: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgreementCard {
    pub id: AgreementId,
    pub name: String,
    /// "You are owed $60.00", "You owe $30.00", "You're settled up".
    pub headline: String,
    pub sign: i32,
    pub members: Vec<MemberBalance>,
    pub period: String,
    pub waiting: usize,
}

fn headline(cur: Currency, v: Cents) -> String {
    match v.signum() {
        1 => format!("You are owed {}", money(cur, v)),
        -1 => format!("You owe {}", money(cur, -v)),
        _ => "You're settled up".into(),
    }
}

fn period_text(span: &DateSpan) -> String {
    format!(
        "{} – {}",
        span.first.format("%b %-d"),
        span.last.format("%b %-d, %Y")
    )
}

pub fn agreements(l: &Ledger, me: MemberId, cur: Currency, today: NaiveDate) -> Vec<AgreementCard> {
    l.agreements
        .values()
        .filter(|a| a.members.contains_key(&me))
        .map(|a| {
            let b = l.balances(a.id);
            let waiting = l
                .waiting_for(me)
                .iter()
                .filter(|p| match &p.kind {
                    ProposalKind::Rule(r) => r.agreement == a.id,
                    ProposalKind::Leave(x) => x.agreement == a.id,
                })
                .count();
            AgreementCard {
                id: a.id,
                name: a.name.clone(),
                headline: headline(cur, b.get(me)),
                sign: sign(b.get(me)),
                members: a
                    .members
                    .keys()
                    .map(|m| MemberBalance {
                        member: *m,
                        name: name(l, *m),
                        balance: money(cur, b.get(*m)),
                        sign: sign(b.get(*m)),
                    })
                    .collect(),
                period: a
                    .schedule
                    .period_containing(today)
                    .map(|p| period_text(&p))
                    .unwrap_or_default(),
                waiting,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityRow {
    pub seq: u64,
    pub date: String,
    pub title: String,
    pub detail: String,
    pub amount: String,
    pub disputed: bool,
    pub editable: bool,
}

/// An agreement's entries, newest first.
pub fn activity(l: &Ledger, a: AgreementId, me: MemberId, cur: Currency) -> Vec<ActivityRow> {
    let closed = |d: NaiveDate| {
        l.agreements
            .get(&a)
            .is_some_and(|g| g.closed.iter().any(|c| c.span.contains(d)))
    };
    let mut rows: Vec<ActivityRow> = l
        .entries
        .values()
        .filter(|e| e.agreement == a && !e.deleted)
        .map(|e| match &e.kind {
            EntryKind::Expense {
                expense,
                paid,
                shares,
                ..
            } => {
                let by = match paid {
                    Paid::Member(m) => name(l, *m),
                    Paid::Pot(d) => l
                        .agreements
                        .get(&a)
                        .and_then(|g| g.deposits.get(d))
                        .map(|(dep, _)| dep.label.clone())
                        .unwrap_or_else(|| "the pot".into()),
                };
                let mine = shares.get(&me).copied().unwrap_or(0);
                ActivityRow {
                    seq: e.seq,
                    date: e.date.format("%b %-d").to_string(),
                    title: expense
                        .note
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| expense.category.clone()),
                    detail: format!("Paid by {by} · your share {}", money(cur, mine)),
                    amount: money(cur, expense.amount),
                    disputed: e.disputed,
                    editable: !closed(e.date),
                }
            }
            EntryKind::Payment(p) => ActivityRow {
                seq: e.seq,
                date: e.date.format("%b %-d").to_string(),
                title: "Payment".into(),
                detail: format!("{} paid {}", name(l, p.from), name(l, p.to)),
                amount: money(cur, p.amount),
                disputed: e.disputed,
                editable: false,
            },
        })
        .collect();
    rows.reverse();
    rows
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleRow {
    pub id: RuleId,
    pub name: String,
    pub method: String,
    pub detail: String,
}

/// The rules in force today, with their method and who they cover.
pub fn rules(l: &Ledger, a: AgreementId, today: NaiveDate) -> Vec<RuleRow> {
    let Some(ag) = l.agreements.get(&a) else {
        return Vec::new();
    };
    let mut ids: Vec<RuleId> = Vec::new();
    for p in l.proposals.values() {
        if let ProposalKind::Rule(r) = &p.kind {
            if r.agreement == a && p.status == ProposalStatus::Approved && !ids.contains(&r.rule_id)
            {
                ids.push(r.rule_id);
            }
        }
    }
    ids.into_iter()
        .filter_map(|id| {
            let v = ag.rules.rule_at(id, today)?;
            let who: Vec<String> = v
                .rule
                .participants()
                .into_iter()
                .map(|m| name(l, m))
                .collect();
            let fallback = if ag.rules.fallback() == Some(id) {
                " · default"
            } else {
                ""
            };
            Some(RuleRow {
                id,
                name: v.rule.name.clone(),
                method: v.rule.method.label().into(),
                detail: format!(
                    "{} · since {}{fallback}",
                    who.join(", "),
                    v.effective.format("%b %-d, %Y")
                ),
            })
        })
        .collect()
}

/// Members' balances and the payments that would settle everyone up.
pub fn members(l: &Ledger, a: AgreementId, cur: Currency) -> (Vec<MemberBalance>, Vec<String>) {
    let b = l.balances(a);
    let list = l
        .agreements
        .get(&a)
        .map(|g| {
            g.members
                .iter()
                .map(|(m, s)| MemberBalance {
                    member: *m,
                    name: if s.left.is_some() {
                        format!("{} (left)", name(l, *m))
                    } else {
                        name(l, *m)
                    },
                    balance: money(cur, b.get(*m)),
                    sign: sign(b.get(*m)),
                })
                .collect()
        })
        .unwrap_or_default();
    let settle = settle_up(&b)
        .into_iter()
        .map(|t| {
            format!(
                "{} pays {} {}",
                name(l, t.from),
                name(l, t.to),
                money(cur, t.amount)
            )
        })
        .collect();
    (list, settle)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeriodRow {
    pub first: NaiveDate,
    pub label: String,
    pub state: String,
    pub closed: bool,
    pub can_close: bool,
}

/// The current period and the `n` before it, newest first.
pub fn periods(l: &Ledger, a: AgreementId, today: NaiveDate, n: usize) -> Vec<PeriodRow> {
    let Some(ag) = l.agreements.get(&a) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut day = today;
    for _ in 0..=n {
        let Some(p) = ag.schedule.period_containing(day) else {
            break;
        };
        let closed = ag.closed.iter().any(|c| c.span == p);
        let st = status(&p, today, l.grace_days);
        let (state, can_close) = if closed {
            ("Closed".to_string(), false)
        } else {
            match st {
                sk_split::periods::PeriodStatus::Open => ("Open".into(), false),
                sk_split::periods::PeriodStatus::ReadyToClose { late_entries_until } => (
                    format!(
                        "Ready to close · late entries until {}",
                        late_entries_until.format("%b %-d")
                    ),
                    true,
                ),
                sk_split::periods::PeriodStatus::GraceOver => ("Ready to close".into(), true),
            }
        };
        out.push(PeriodRow {
            first: p.first,
            label: period_text(&p),
            state,
            closed,
            can_close,
        });
        match p.first.pred_opt() {
            Some(d) => day = d,
            None => break,
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalRow {
    pub seq: u64,
    pub title: String,
    pub detail: String,
}

/// What waits for `me` to approve.
pub fn approvals(l: &Ledger, me: MemberId) -> Vec<ApprovalRow> {
    l.waiting_for(me)
        .into_iter()
        .map(|p| match &p.kind {
            ProposalKind::Rule(r) => ApprovalRow {
                seq: p.seq,
                title: format!("New rule: {}", r.rule.name),
                detail: format!(
                    "{} proposed {} from {}",
                    name(l, p.by),
                    r.rule.method.label(),
                    r.effective.format("%b %-d, %Y")
                ),
            },
            ProposalKind::Leave(x) => ApprovalRow {
                seq: p.seq,
                title: format!("{} leaves", name(l, x.member)),
                detail: format!(
                    "From {}, proposed by {}",
                    x.date.format("%b %-d, %Y"),
                    name(l, p.by)
                ),
            },
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SplitRow {
    pub name: String,
    pub amount: String,
}

/// The live split while an expense is typed in: each member's share, or why it can't post.
pub fn preview(
    l: &Ledger,
    me: MemberId,
    cur: Currency,
    e: Expense,
) -> Result<Vec<SplitRow>, String> {
    let entry = l.preview_expense(Some(me), e).map_err(|r| r.0)?;
    match entry.kind {
        EntryKind::Expense { shares, .. } => Ok(shares
            .into_iter()
            .map(|(m, v)| SplitRow {
                name: name(l, m),
                amount: money(cur, v),
            })
            .collect()),
        EntryKind::Payment(_) => Ok(Vec::new()),
    }
}

/// An expense's record body.
pub fn expense_body(
    a: AgreementId,
    date: NaiveDate,
    amount: Cents,
    payer: MemberId,
    category: &str,
    note: &str,
) -> Expense {
    Expense {
        agreement: a,
        date,
        amount,
        payer: Payer::Member(payer),
        category: category.trim().to_string(),
        note: Some(note.trim().to_string()).filter(|n| !n.is_empty()),
        rule: None,
        event: None,
        items: Vec::new(),
        days: BTreeMap::new(),
        usage: BTreeMap::new(),
        due: None,
    }
}

// ----- the rule editor ---------------------------------------------------------------------------

/// The methods the editor's first version offers (the sliders' meaning per method).
pub const EDITOR_METHODS: [&str; 4] = ["Equal", "Percentages", "Shares", "By income"];

/// One member's row in the rule editor.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightRow {
    pub member: MemberId,
    pub name: String,
    /// Percent (0–100) for percentages; a weight for shares and income.
    pub value: f32,
    pub included: bool,
    pub locked: bool,
}

/// The rule being edited.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleDraft {
    pub name: String,
    pub method: usize,
    pub rows: Vec<WeightRow>,
}

impl RuleDraft {
    /// A new equal rule over an agreement's members.
    pub fn new(l: &Ledger, a: AgreementId) -> Self {
        let rows: Vec<WeightRow> = l
            .agreements
            .get(&a)
            .map(|g| {
                g.members
                    .iter()
                    .filter(|(_, s)| s.left.is_none())
                    .map(|(m, _)| *m)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|m| WeightRow {
                member: m,
                name: name(l, m),
                value: 1.0,
                included: true,
                locked: false,
            })
            .collect();
        let mut d = RuleDraft {
            name: String::new(),
            method: 0,
            rows,
        };
        d.set_method(0);
        d
    }

    /// Switch method; percentages start even.
    pub fn set_method(&mut self, m: usize) {
        self.method = m.min(EDITOR_METHODS.len() - 1);
        let n = self.rows.iter().filter(|r| r.included).count().max(1) as f32;
        for r in &mut self.rows {
            r.locked = false;
            r.value = match self.method {
                1 if r.included => 100.0 / n,
                1 => 0.0,
                _ => 1.0,
            };
        }
        if self.method == 1 {
            self.round_percentages();
        }
    }

    /// Whole percentages that total exactly 100 (the last unlocked included row takes the
    /// remainder).
    fn round_percentages(&mut self) {
        for r in &mut self.rows {
            r.value = if r.included {
                r.value.round().max(0.0)
            } else {
                0.0
            };
        }
        let total: f32 = self.rows.iter().map(|r| r.value).sum();
        let fix = self
            .rows
            .iter()
            .rposition(|r| r.included && !r.locked)
            .or_else(|| self.rows.iter().rposition(|r| r.included));
        if let Some(i) = fix {
            self.rows[i].value = (self.rows[i].value + 100.0 - total).max(0.0);
        }
    }

    /// A slider moved. For percentages the other unlocked sliders move the other way so
    /// the total stays 100%.
    pub fn set_value(&mut self, i: usize, v: f32) {
        if i >= self.rows.len() {
            return;
        }
        if self.method != 1 {
            self.rows[i].value = v.max(0.0).round();
            return;
        }
        let locked: f32 = self
            .rows
            .iter()
            .enumerate()
            .filter(|(j, r)| *j != i && r.included && r.locked)
            .map(|(_, r)| r.value)
            .sum();
        let v = v.clamp(0.0, 100.0 - locked);
        self.rows[i].value = v;
        let others: Vec<usize> = (0..self.rows.len())
            .filter(|j| *j != i && self.rows[*j].included && !self.rows[*j].locked)
            .collect();
        let left = 100.0 - locked - v;
        let current: f32 = others.iter().map(|j| self.rows[*j].value).sum();
        for j in &others {
            self.rows[*j].value = if current > 0.0 {
                self.rows[*j].value / current * left
            } else {
                left / others.len() as f32
            };
        }
        if others.is_empty() {
            self.rows[i].value = 100.0 - locked;
        }
        self.round_percentages();
    }

    pub fn toggle_included(&mut self, i: usize) {
        if let Some(r) = self.rows.get_mut(i) {
            r.included = !r.included;
        }
        if self.method == 1 {
            self.set_method(1);
        }
    }

    pub fn toggle_locked(&mut self, i: usize) {
        if let Some(r) = self.rows.get_mut(i) {
            r.locked = !r.locked;
        }
    }

    /// The rule this draft describes.
    pub fn rule(&self) -> Rule {
        let members: Vec<MemberId> = self.rows.iter().map(|r| r.member).collect();
        let weights = |scale: f32| -> BTreeMap<MemberId, u32> {
            self.rows
                .iter()
                .map(|r| (r.member, (r.value * scale).round().max(0.0) as u32))
                .collect()
        };
        let method = match self.method {
            1 => Method::Percent(weights(FULL_BP as f32 / 100.0)),
            2 => Method::Shares(weights(1.0)),
            3 => Method::Income(weights(1.0)),
            _ => Method::Equal,
        };
        let mut rule = Rule::new(self.name.trim(), method, &members);
        rule.excluded = self
            .rows
            .iter()
            .filter(|r| !r.included)
            .map(|r| r.member)
            .collect();
        rule
    }

    /// The proposal's record body.
    pub fn proposal(
        &self,
        a: AgreementId,
        id: RuleId,
        effective: NaiveDate,
        categories: &[String],
    ) -> serde_json::Value {
        json!({
            "agreement": a,
            "rule_id": id,
            "effective": effective,
            "rule": self.rule(),
            "categories": categories,
        })
    }
}

/// Sample amounts that show odd cents.
pub const EXAMPLE_AMOUNTS: [Cents; 3] = [8347, 10000, 123456];

/// The rule's live examples: each sample amount and how it divides, or why it can't.
pub fn examples(
    rule: &Rule,
    l: &Ledger,
    cur: Currency,
) -> Result<Vec<(String, Vec<SplitRow>)>, String> {
    rule.validate().map_err(|e| e.to_string())?;
    EXAMPLE_AMOUNTS
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let shares = split(
                rule,
                &SplitInput {
                    amount: *a,
                    seq: i as u64,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
            Ok((
                money(cur, *a),
                shares
                    .into_iter()
                    .map(|(m, v)| SplitRow {
                        name: name(l, m),
                        amount: money(cur, v),
                    })
                    .collect(),
            ))
        })
        .collect()
}

// ----- everything at once, for the web app ------------------------------------------------------

/// A person in an agreement, for pickers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: MemberId,
    pub name: String,
}

/// Something due: a recurring bill to record, or a member's pot top-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectedRow {
    pub recurring: u32,
    pub name: String,
    /// "Mar 5"
    pub date: String,
    pub date_iso: NaiveDate,
    /// "bill" or "top-up"
    pub kind: String,
    pub amount: String,
    pub amount_cents: Cents,
    /// A top-up's member; a bill's usual payer.
    pub member: Option<MemberId>,
    pub detail: String,
    /// A top-up this person pays, or any bill (anyone records it).
    pub mine: bool,
    pub category: String,
    /// A top-up's pot.
    pub deposit: Option<sk_core::DepositId>,
    pub overdue: bool,
}

/// A deposit or pot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepositRow {
    pub id: sk_core::DepositId,
    pub label: String,
    /// "Held" or "Pot"
    pub kind: String,
    pub balance: String,
    /// "Your part: $500.00"
    pub detail: String,
}

/// A recurring item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecurringRow {
    pub id: u32,
    pub name: String,
    pub detail: String,
    pub stopped: bool,
}

/// What's due in an agreement for `me`.
pub fn expected(
    l: &Ledger,
    a: AgreementId,
    me: MemberId,
    cur: Currency,
    today: NaiveDate,
) -> Vec<ExpectedRow> {
    l.expected(a, today)
        .into_iter()
        .map(|e| {
            let (kind, member, category, deposit) = match &e.what {
                Repeats::Bill {
                    category, payer, ..
                } => ("bill", *payer, category.clone(), None),
                Repeats::TopUp { deposit, .. } => {
                    ("top-up", e.member, String::new(), Some(*deposit))
                }
            };
            let detail = match (kind, member) {
                ("top-up", Some(m)) => format!("{} puts in {}", name(l, m), money(cur, e.amount)),
                (_, Some(m)) => format!(
                    "About {} · usually paid by {}",
                    money(cur, e.amount),
                    name(l, m)
                ),
                _ => format!("About {}", money(cur, e.amount)),
            };
            ExpectedRow {
                recurring: e.recurring,
                name: e.name,
                date: e.date.format("%b %-d").to_string(),
                date_iso: e.date,
                kind: kind.into(),
                amount: money(cur, e.amount),
                amount_cents: e.amount,
                member,
                detail,
                mine: kind == "bill" || member == Some(me),
                category,
                deposit,
                overdue: e.date < today,
            }
        })
        .collect()
}

/// An agreement's deposits and pots, with my part of each.
pub fn deposits(l: &Ledger, a: AgreementId, me: MemberId, cur: Currency) -> Vec<DepositRow> {
    l.agreements
        .get(&a)
        .map(|g| {
            g.deposits
                .values()
                .map(|(d, st)| DepositRow {
                    id: d.id,
                    label: d.label.clone(),
                    kind: match d.kind {
                        DepositKind::Held => "Held".into(),
                        DepositKind::Spending => "Pot".into(),
                    },
                    balance: money(cur, st.balance()),
                    detail: format!(
                        "Your part: {}",
                        money(cur, st.stakes().get(&me).copied().unwrap_or(0))
                    ),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An agreement's recurring bills and top-ups.
pub fn recurring(l: &Ledger, a: AgreementId, cur: Currency) -> Vec<RecurringRow> {
    l.agreements
        .get(&a)
        .map(|g| {
            g.recurring
                .values()
                .map(|r| RecurringRow {
                    id: r.id,
                    name: r.name.clone(),
                    detail: match (&r.what, r.stopped) {
                        (_, Some(s)) => format!("Stopped {}", s.format("%b %-d, %Y")),
                        (
                            Repeats::Bill {
                                amount, category, ..
                            },
                            None,
                        ) => format!(
                            "{category} · about {} · from {}",
                            money(cur, *amount),
                            r.added.format("%b %-d, %Y")
                        ),
                        (Repeats::TopUp { amounts, .. }, None) => {
                            format!("Top-up · {} in all", money(cur, amounts.values().sum()))
                        }
                    },
                    stopped: r.stopped.is_some(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One agreement's screens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgreementView {
    pub id: AgreementId,
    pub name: String,
    pub period: String,
    /// Active members today, for "paid by" and payments.
    pub people: Vec<Person>,
    pub activity: Vec<ActivityRow>,
    pub rules: Vec<RuleRow>,
    pub members: Vec<MemberBalance>,
    pub settle: Vec<String>,
    pub periods: Vec<PeriodRow>,
    /// The payment that would settle me up, if I owe: (to, amount in cents).
    pub i_owe: Option<(MemberId, Cents)>,
    /// The next rule id and when a new rule would start (the next period).
    pub next_rule: RuleId,
    pub next_period: NaiveDate,
    pub expected: Vec<ExpectedRow>,
    pub deposits: Vec<DepositRow>,
    pub recurring: Vec<RecurringRow>,
    pub next_recurring: u32,
    /// People in the book but not (or no longer) in this agreement, who could join.
    pub can_join: Vec<Person>,
    pub next_deposit: sk_core::DepositId,
}

/// Everything a device's screens show, for `me` on `today`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub me: MemberId,
    pub me_name: String,
    pub today: NaiveDate,
    pub currency: String,
    pub cards: Vec<AgreementCard>,
    pub approvals: Vec<ApprovalRow>,
    pub agreements: Vec<AgreementView>,
}

pub fn snapshot(l: &Ledger, me: MemberId, cur: Currency, today: NaiveDate) -> Snapshot {
    let views: Vec<AgreementView> = l
        .agreements
        .values()
        .filter(|a| a.members.contains_key(&me))
        .map(|a| {
            let (members, settle) = members(l, a.id, cur);
            let b = l.balances(a.id);
            let i_owe = settle_up(&b)
                .into_iter()
                .find(|t| t.from == me)
                .map(|t| (t.to, t.amount));
            let next_rule = l
                .proposals
                .values()
                .filter_map(|p| match &p.kind {
                    ProposalKind::Rule(r) if r.agreement == a.id => Some(r.rule_id.0),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
                + 1;
            let current = a.schedule.period_containing(today);
            AgreementView {
                id: a.id,
                name: a.name.clone(),
                period: current.as_ref().map(period_text).unwrap_or_default(),
                people: a
                    .active_on(today)
                    .into_iter()
                    .map(|m| Person {
                        id: m,
                        name: name(l, m),
                    })
                    .collect(),
                activity: activity(l, a.id, me, cur),
                rules: rules(l, a.id, today),
                members,
                settle,
                periods: periods(l, a.id, today, 5),
                i_owe,
                next_rule: RuleId(next_rule),
                next_period: current.and_then(|p| p.last.succ_opt()).unwrap_or(today),
                expected: expected(l, a.id, me, cur, today),
                deposits: deposits(l, a.id, me, cur),
                recurring: recurring(l, a.id, cur),
                next_recurring: a.recurring.keys().next_back().map_or(1, |k| k + 1),
                can_join: l
                    .members
                    .iter()
                    .filter(|(m, _)| a.members.get(m).is_none_or(|s| s.left.is_some()))
                    .map(|(m, n)| Person {
                        id: *m,
                        name: n.clone(),
                    })
                    .collect(),
                next_deposit: sk_core::DepositId(
                    a.deposits.keys().next_back().map_or(1, |k| k.0 + 1),
                ),
            }
        })
        .collect();
    Snapshot {
        me,
        me_name: name(l, me),
        today,
        currency: cur.code().to_string(),
        cards: agreements(l, me, cur, today),
        approvals: approvals(l, me),
        agreements: views,
    }
}
