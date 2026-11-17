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
    /// An expense's record body, for editing it.
    pub expense: Option<Expense>,
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
                let event = expense
                    .event
                    .and_then(|id| l.agreements.get(&a).and_then(|g| g.events.get(&id)))
                    .map(|ev| format!(" · {}", ev.name))
                    .unwrap_or_default();
                ActivityRow {
                    seq: e.seq,
                    date: e.date.format("%b %-d").to_string(),
                    title: expense
                        .note
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| expense.category.clone()),
                    detail: format!("Paid by {by} · your share {}{event}", money(cur, mine)),
                    amount: money(cur, expense.amount),
                    disputed: e.disputed,
                    editable: !closed(e.date),
                    expense: Some(expense.clone()),
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
                expense: None,
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
            // in force today, or else its first version, which starts later
            let v = ag.rules.rule_at(id, today).or_else(|| {
                let first = ag.rules.versions(id).first()?.effective;
                ag.rules.rule_at(id, first)
            })?;
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
                    "{} · {} {}{fallback}",
                    who.join(", "),
                    if v.effective > today {
                        "starts"
                    } else {
                        "since"
                    },
                    v.effective.format("%b %-d, %Y")
                ),
            })
        })
        .collect()
}

/// What a new rule proposal is and from when: (rule id, effective date, starts now). While
/// no rule is in force today (an agreement's first rule, or one that starts later), the
/// proposal applies from the current period, so today's expenses can be divided: a new
/// version of the default rule when there is one, else a new rule. Any other rule starts with
/// the next period, so a period is never divided two ways.
pub fn proposal_target(l: &Ledger, a: AgreementId, today: NaiveDate) -> (RuleId, NaiveDate, bool) {
    let next_id = l
        .proposals
        .values()
        .filter_map(|p| match &p.kind {
            ProposalKind::Rule(r) if r.agreement == a => Some(r.rule_id.0),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let Some(ag) = l.agreements.get(&a) else {
        return (RuleId(next_id), today, true);
    };
    let current = ag.schedule.period_containing(today);
    let now = current.as_ref().map_or(today, |p| p.first);
    let next = current.and_then(|p| p.last.succ_opt()).unwrap_or(today);
    match ag.rules.fallback() {
        None => (RuleId(next_id), now, true),
        Some(id) if ag.rules.rule_at(id, today).is_none() => {
            // a version from that date may exist already: then the day after the
            // period's start is still in this period
            let taken = ag.rules.versions(id).iter().any(|v| v.effective == now);
            (id, if taken { today } else { now }, true)
        }
        Some(_) => (RuleId(next_id), next, false),
    }
}

/// Why no rule divides this expense, in plain words, when that's what stops it (and the
/// screens then offer "Propose how it's split"); `None` when a rule applies or something
/// else is wrong.
pub fn rule_missing(l: &Ledger, e: &Expense) -> Option<String> {
    let ag = l.agreements.get(&e.agreement)?;
    match ag.rules.resolve(&e.category, e.date, e.rule) {
        Ok(_) => return None,
        Err(sk_split::SplitError::NoRuleFor(_)) | Err(sk_split::SplitError::RuleNotInForce(..)) => {
        }
        Err(_) => return None,
    }
    let day = e.date.format("%b %-d").to_string();
    // a rule proposal still waiting for approvals
    let waiting = l.proposals.values().find_map(|p| match &p.kind {
        ProposalKind::Rule(r)
            if r.agreement == e.agreement && p.status == ProposalStatus::Waiting =>
        {
            let who: Vec<String> = p
                .approvers
                .iter()
                .filter(|m| !p.approved.contains(m))
                .map(|m| name(l, *m))
                .collect();
            Some((r.rule.name.clone(), who))
        }
        _ => None,
    });
    if let Some((rule, who)) = waiting {
        return Some(format!(
            "No rule divides expenses on {day} yet. “{rule}” waits for approval from {}: they approve it under Approvals on their device.",
            who.join(" and ")
        ));
    }
    if let Some(id) = ag.rules.fallback() {
        if let Some(v) = ag.rules.versions(id).first() {
            if v.effective > e.date {
                return Some(format!(
                    "No rule divides expenses on {day} yet: “{}” starts on {}. Propose how expenses are split from this period on.",
                    v.rule.name,
                    v.effective.format("%b %-d, %Y")
                ));
            }
        }
    }
    Some(format!(
        "No rule divides expenses in {} yet. Propose how they're split; everyone else in the agreement approves it.",
        ag.name
    ))
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
pub const EDITOR_METHODS: [&str; 9] = [
    "Equal",
    "Percentages",
    "Shares",
    "By income",
    "By room",
    "By days present",
    "By usage",
    "Exact amounts",
    "Itemized",
];

/// The editor index of "Exact amounts" and "Itemized".
pub const EXACT: usize = 7;
pub const ITEMIZED: usize = 8;

/// Whether an editor method has a slider per member (equal, days and usage don't: days and
/// usage are entered with each expense).
pub fn method_weighted(m: usize) -> bool {
    matches!(m, 1..=4)
}

/// The slider's top for an editor method.
pub fn method_max(m: usize) -> f32 {
    match m {
        1 => 100.0,
        4 => 50.0,
        _ => 10.0,
    }
}

/// One member's row in the rule editor.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightRow {
    pub member: MemberId,
    pub name: String,
    /// Percent (0–100) for percentages; a weight for shares and income.
    pub value: f32,
    pub included: bool,
    pub locked: bool,
    /// Exact amounts: this member's fixed amount, in cents.
    pub cents: Cents,
    /// Exact amounts: the amount as typed, shown back unchanged (a half-typed "800." isn't
    /// an amount yet, and must not be cleared while typing).
    pub amount_text: String,
    /// Exact amounts: this member takes whatever is left.
    pub remainder: bool,
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
                cents: 0,
                amount_text: String::new(),
                remainder: false,
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
        let last = self.rows.iter().rposition(|r| r.included);
        for (i, r) in self.rows.iter_mut().enumerate() {
            r.cents = 0;
            r.amount_text.clear();
            r.remainder = self.method == EXACT && Some(i) == last;
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
        // the one who takes what's left must share in it
        if self.method == EXACT && !self.rows.iter().any(|r| r.included && r.remainder) {
            let last = self.rows.iter().rposition(|r| r.included);
            for (j, r) in self.rows.iter_mut().enumerate() {
                r.remainder = Some(j) == last;
            }
        }
    }

    /// Exact amounts: a member's fixed amount (whole cents).
    pub fn set_amount(&mut self, i: usize, cents: Cents) {
        if let Some(r) = self.rows.get_mut(i) {
            r.cents = cents.max(0);
        }
    }

    /// Exact amounts: the amount as the person types it, and what it reads as (`None` while
    /// it isn't an amount yet: no fixed amount, but the text stays).
    pub fn set_amount_text(&mut self, i: usize, text: &str, cents: Option<Cents>) {
        if let Some(r) = self.rows.get_mut(i) {
            r.amount_text = text.to_string();
            r.cents = cents.unwrap_or(0).max(0);
        }
    }

    /// Exact amounts: who takes whatever is left of each expense.
    pub fn set_remainder(&mut self, i: usize) {
        if self.rows.get(i).is_some_and(|r| r.included) {
            for (j, r) in self.rows.iter_mut().enumerate() {
                r.remainder = j == i;
            }
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
            4 => Method::Room(weights(1.0)),
            5 => Method::Days,
            6 => Method::Usage,
            EXACT => {
                let remainder = self
                    .rows
                    .iter()
                    .find(|r| r.included && r.remainder)
                    .or_else(|| self.rows.iter().rfind(|r| r.included))
                    .map_or(MemberId(0), |r| r.member);
                Method::Exact {
                    amounts: self
                        .rows
                        .iter()
                        .filter(|r| r.included && r.member != remainder && r.cents > 0)
                        .map(|r| (r.member, r.cents))
                        .collect(),
                    remainder,
                }
            }
            ITEMIZED => Method::Itemized,
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
    // days and usage are entered with each expense: the examples use sample ones, shown
    // beside each name
    let people = rule.participants();
    let sample_days: BTreeMap<MemberId, u32> = people
        .iter()
        .enumerate()
        .map(|(i, m)| (*m, [30, 20, 10, 25, 15][i % 5]))
        .collect();
    let sample_usage: BTreeMap<MemberId, u64> = people
        .iter()
        .enumerate()
        .map(|(i, m)| (*m, [120, 80, 200, 50, 100][i % 5]))
        .collect();
    // itemized: a sample receipt, one line for each person and one they all share; the
    // amounts are the lines plus some tax or tip
    let sample_items: Vec<sk_split::method::Item> = people
        .iter()
        .enumerate()
        .map(|(i, m)| sk_split::method::Item {
            amount: [1250, 899, 2075, 640, 1500][i % 5],
            members: vec![*m],
        })
        .chain(std::iter::once(sk_split::method::Item {
            amount: 1499,
            members: people.iter().copied().collect(),
        }))
        .collect();
    let items_total: Cents = sample_items.iter().map(|i| i.amount).sum();
    let note = |m: MemberId| match rule.method {
        Method::Days => format!(" ({} days)", sample_days.get(&m).copied().unwrap_or(0)),
        Method::Usage => format!(" ({} used)", sample_usage.get(&m).copied().unwrap_or(0)),
        Method::Itemized => {
            let own: Cents = sample_items
                .iter()
                .filter(|i| i.members == [m])
                .map(|i| i.amount)
                .sum();
            format!(" (own lines {})", money(cur, own))
        }
        _ => String::new(),
    };
    // exact amounts: the fixed amounts plus each sample, so something is left over
    let base: Cents = match &rule.method {
        Method::Exact { amounts, .. } => amounts.values().sum(),
        Method::Itemized => items_total,
        _ => 0,
    };
    let samples: Vec<Cents> = match rule.method {
        Method::Itemized => vec![base, base + 347, base + 1234],
        _ => EXAMPLE_AMOUNTS.iter().map(|a| base + a).collect(),
    };
    samples
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let shares = split(
                rule,
                &SplitInput {
                    amount: *a,
                    seq: i as u64,
                    days: sample_days.clone(),
                    usage: sample_usage.clone(),
                    items: sample_items.clone(),
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
            Ok((
                money(cur, *a),
                shares
                    .into_iter()
                    .map(|(m, v)| SplitRow {
                        name: format!("{}{}", name(l, m), note(m)),
                        amount: money(cur, v),
                    })
                    .collect(),
            ))
        })
        .collect()
}

/// What an expense must also give for the rule its category falls under: per-member days
/// present or usage, or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Needs {
    /// "days", "usage" or "items" (receipt lines)
    pub kind: String,
    pub rule: String,
    pub who: Vec<Person>,
}

pub fn needs(l: &Ledger, e: &Expense) -> Option<Needs> {
    let g = l.agreements.get(&e.agreement)?;
    let r = g.rules.resolve(&e.category, e.date, e.rule).ok()?;
    let kind = match r.rule.method {
        Method::Days => "days",
        Method::Usage => "usage",
        Method::Itemized => "items",
        _ => return None,
    };
    Some(Needs {
        kind: kind.into(),
        rule: r.rule.name.clone(),
        who: r
            .rule
            .participants()
            .into_iter()
            .map(|m| Person {
                id: m,
                name: name(l, m),
            })
            .collect(),
    })
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

/// A special event (a trip, a party): its expenses settle on their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRow {
    pub id: u32,
    pub name: String,
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub detail: String,
}

pub fn events(l: &Ledger, a: AgreementId) -> Vec<EventRow> {
    l.agreements
        .get(&a)
        .map(|g| {
            g.events
                .iter()
                .map(|(id, ev)| EventRow {
                    id: *id,
                    name: ev.name.clone(),
                    first: ev.span.first,
                    last: ev.span.last,
                    detail: format!(
                        "{} · {}",
                        period_text(&ev.span),
                        ev.members
                            .iter()
                            .map(|m| name(l, *m))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How often an agreement settles up, in words, with a change that's waiting for the
/// current period to end.
pub fn schedule_text(l: &Ledger, a: AgreementId, today: NaiveDate) -> String {
    let Some(g) = l.agreements.get(&a) else {
        return String::new();
    };
    let versions = g.schedule.versions();
    // the version in force today (a change waits for the current period to end)
    let now = versions
        .iter()
        .rposition(|v| v.start() <= today)
        .unwrap_or(0);
    let mut text = versions.get(now).map(describe).unwrap_or_default();
    if let Some(next) = versions.get(now + 1) {
        text.push_str(&format!(". From {}: {}", next.start().format("%b %-d"), {
            // "Every 2 weeks…" reads on after a colon as "every 2 weeks…"
            let d = describe(next);
            let mut c = d.chars();
            c.next()
                .map(|f| f.to_lowercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }));
    }
    text
}

fn describe(s: &sk_split::periods::Schedule) -> String {
    use sk_split::periods::{MonthDay, Schedule};
    match s {
        Schedule::Weekly { weekday, every, .. } => {
            let day = [
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ][weekday.num_days_from_monday() as usize];
            match every {
                1 => format!("Every week, on {day}"),
                n => format!("Every {n} weeks, on {day}"),
            }
        }
        Schedule::Monthly { day, every, .. } => {
            let d = match day {
                MonthDay::Day(d) => format!("the {}", ordinal(u32::from(*d))),
                MonthDay::Last => "the last day".into(),
            };
            match every {
                1 => format!("Every month, on {d}"),
                n => format!("Every {n} months, on {d}"),
            }
        }
        Schedule::EveryNDays { days, .. } => format!("Every {days} days"),
        Schedule::Custom { .. } => "On dates chosen by hand".into(),
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
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
    /// The next proposal applies from the current period (no rule is in force yet).
    pub rule_starts_now: bool,
    pub expected: Vec<ExpectedRow>,
    pub deposits: Vec<DepositRow>,
    pub recurring: Vec<RecurringRow>,
    pub next_recurring: u32,
    /// People in the book but not (or no longer) in this agreement, who could join.
    pub can_join: Vec<Person>,
    pub next_deposit: sk_core::DepositId,
    pub events: Vec<EventRow>,
    pub next_event: u32,
    /// "Every month, on the 1st"
    pub schedule: String,
}

/// A sealed statement, for "Check a statement".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SealRow {
    pub agreement: String,
    pub period: String,
    pub sha256: String,
    pub head_seq: u64,
    pub head_hash: String,
}

pub fn seals(l: &Ledger) -> Vec<SealRow> {
    l.seals
        .iter()
        .filter_map(|s| {
            Some(SealRow {
                agreement: l.agreements.get(&s.agreement)?.name.clone(),
                period: format!(
                    "{} – {}",
                    s.first.format("%b %-d"),
                    s.last.format("%b %-d, %Y")
                ),
                sha256: s.statement_sha256.clone()?,
                head_seq: s.head_seq,
                head_hash: s.head_hash.clone(),
            })
        })
        .collect()
}

/// Whether a file (by its SHA-256, hex) is a sealed statement: the answer in plain words, and
/// whether it is.
pub fn check_statement(l: &Ledger, sha256_hex: &str) -> (bool, String) {
    let want = sha256_hex.trim().to_ascii_lowercase();
    match seals(l).into_iter().find(|s| s.sha256 == want) {
        Some(s) => (
            true,
            format!(
                "This is the statement that was sealed for {}, {} (history at closing: record {}).",
                s.agreement, s.period, s.head_seq
            ),
        ),
        None => (
            false,
            "This file is not a statement that was sealed in this book. It may have been changed, or it belongs to another book.".into(),
        ),
    }
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
    pub seals: Vec<SealRow>,
    /// Backups the household restored (Hyperbooks), newest last.
    pub restores: Vec<RestoreRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreRow {
    pub seq: u64,
    pub date: NaiveDate,
    pub to_seq: u64,
    pub to_hash: String,
    pub accepted_by_me: bool,
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
            let target = proposal_target(l, a.id, today);
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
                next_rule: target.0,
                next_period: target.1,
                rule_starts_now: target.2,
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
                events: events(l, a.id),
                next_event: a.events.keys().next_back().map_or(1, |k| k + 1),
                schedule: schedule_text(l, a.id, today),
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
        seals: seals(l),
        restores: l
            .restores
            .iter()
            .map(|r| RestoreRow {
                seq: r.seq,
                date: r.date,
                to_seq: r.to_seq,
                to_hash: r.to_hash.clone(),
                accepted_by_me: r.accepted.contains(&me),
            })
            .collect(),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv_cents(v: Cents) -> String {
    let sign = if v < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", v.abs() / 100, v.abs() % 100)
}

/// A period's entries as a spreadsheet: one row per entry with what it moved for each member
/// and their balances after it, opening and closing rows, and (for a Hyperbook) the history's
/// head at export on the last line. `first`: the period's first day.
pub fn csv(
    l: &Ledger,
    a: AgreementId,
    first: NaiveDate,
    head: Option<sk_chain::Head>,
) -> Result<(String, String), String> {
    let ag = l.agreements.get(&a).ok_or("There's no such agreement.")?;
    let span = ag
        .schedule
        .period_containing(first)
        .ok_or("That period isn't in the agreement's schedule.")?;
    let people: Vec<MemberId> = ag.members.keys().copied().collect();
    let mut bal = match span.first.pred_opt() {
        Some(d) => l.balances_until(a, Some(d)),
        None => sk_split::ledger::Balances::new(),
    };
    let mut out = String::new();
    let mut head_row = vec![
        "Date".to_string(),
        "Record".into(),
        "What".into(),
        "Category".into(),
        "Paid by".into(),
        "Amount".into(),
        "Note".into(),
    ];
    for m in &people {
        head_row.push(format!("{} change", name(l, *m)));
    }
    for m in &people {
        head_row.push(format!("{} balance", name(l, *m)));
    }
    let line = |cols: Vec<String>| -> String {
        cols.iter()
            .map(|c| csv_field(c))
            .collect::<Vec<_>>()
            .join(",")
            + "\r\n"
    };
    out.push_str(&line(head_row));
    let row = |date: String,
               seq: String,
               what: String,
               cat: String,
               by: String,
               amount: String,
               note: String,
               change: &BTreeMap<MemberId, Cents>,
               bal: &sk_split::ledger::Balances| {
        let mut cols = vec![date, seq, what, cat, by, amount, note];
        for m in &people {
            cols.push(change.get(m).map(|v| csv_cents(*v)).unwrap_or_default());
        }
        for m in &people {
            cols.push(csv_cents(bal.get(*m)));
        }
        cols
    };
    out.push_str(&line(row(
        span.first.to_string(),
        String::new(),
        "Opening balances".into(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        &BTreeMap::new(),
        &bal,
    )));
    let mut entries: Vec<_> = l
        .entries
        .values()
        .filter(|e| e.agreement == a && span.contains(e.date))
        .collect();
    entries.sort_by_key(|e| (e.date, e.seq));
    for e in entries {
        let counts =
            !e.deleted && !(e.disputed && l.dispute_mode == sk_ledger::DisputeMode::HeldAside);
        let before = bal.clone();
        if counts {
            for p in e.postings() {
                let _ = bal.apply(&p);
            }
        }
        let change: BTreeMap<MemberId, Cents> = people
            .iter()
            .map(|m| (*m, bal.get(*m) - before.get(*m)))
            .filter(|(_, v)| *v != 0)
            .collect();
        let mut note = String::new();
        if e.deleted {
            note.push_str("deleted; ");
        }
        if e.disputed {
            note.push_str("disputed; ");
        }
        let (what, cat, by, extra) = match &e.kind {
            EntryKind::Expense { expense, paid, .. } => (
                "Expense".to_string(),
                expense.category.clone(),
                match paid {
                    Paid::Member(m) => name(l, *m),
                    Paid::Pot(_) => "the pot".into(),
                },
                expense.note.clone().unwrap_or_default(),
            ),
            EntryKind::Payment(p) => (
                "Payment".to_string(),
                String::new(),
                name(l, p.from),
                format!("to {}", name(l, p.to)),
            ),
        };
        note.push_str(&extra);
        out.push_str(&line(row(
            e.date.to_string(),
            e.seq.to_string(),
            what,
            cat,
            by,
            csv_cents(e.amount()),
            note.trim_end_matches("; ").to_string(),
            &change,
            &bal,
        )));
    }
    if let Some(c) = ag.closed.iter().find(|c| c.span == span) {
        let before = bal.clone();
        for adj in &c.adjustments {
            let _ = bal.apply(&sk_split::ledger::Posting::Adjustment {
                shares: adj.clone(),
            });
        }
        let change: BTreeMap<MemberId, Cents> = people
            .iter()
            .map(|m| (*m, bal.get(*m) - before.get(*m)))
            .filter(|(_, v)| *v != 0)
            .collect();
        out.push_str(&line(row(
            span.last.to_string(),
            c.seq.to_string(),
            "Period closed".into(),
            String::new(),
            String::new(),
            String::new(),
            "cap adjustments".into(),
            &change,
            &bal,
        )));
    }
    out.push_str(&line(row(
        span.last.to_string(),
        String::new(),
        "Closing balances".into(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        &BTreeMap::new(),
        &bal,
    )));
    if let Some(h) = head {
        out.push_str(&format!(
            "# History at export: record {} · fingerprint {}\r\n",
            h.seq,
            h.hash.to_hex()
        ));
    }
    let file = format!(
        "{} {} to {}.csv",
        crate::file_safe(&ag.name),
        span.first,
        span.last
    );
    Ok((file, out))
}
