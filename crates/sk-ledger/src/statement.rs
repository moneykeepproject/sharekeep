//! A closed period's statement: what was spent, who paid, each member's share, and the
//! payments that settle everyone up. The hub saves it as a PDF and seals its hash.

use crate::{EntryKind, Ledger};
use serde::{Deserialize, Serialize};
use sk_core::{AgreementId, Cents, DateSpan, MemberId};
use sk_split::ledger::{settle_up, Paid, Transfer};
use std::collections::BTreeMap;

/// One member's line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLine {
    pub member: MemberId,
    pub name: String,
    /// What they paid for shared expenses in the period.
    pub paid: Cents,
    /// Their share of the period's expenses.
    pub share: Cents,
    /// Payments they made (+) or received (−) in the period.
    pub settled: Cents,
    /// Per-period cap adjustments (− lowers what they owe).
    pub adjusted: Cents,
    /// Their running balance after the period: + they are owed, − they owe.
    pub balance: Cents,
}

/// One entry on the statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub seq: u64,
    pub date: String,
    pub what: String,
    pub by: String,
    pub amount: Cents,
    pub disputed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    pub agreement: AgreementId,
    pub agreement_name: String,
    pub period: DateSpan,
    /// The `period-close` record.
    pub closed_by: u64,
    pub members: Vec<MemberLine>,
    pub by_category: Vec<(String, Cents)>,
    pub total: Cents,
    pub lines: Vec<Line>,
    /// Payments that settle every balance.
    pub settle: Vec<(String, String, Cents)>,
}

pub(crate) fn build(l: &Ledger, a: AgreementId, span: DateSpan, seq: u64) -> Statement {
    let name = |m: &MemberId| l.members.get(m).cloned().unwrap_or_else(|| m.to_string());
    let ag = l.agreements.get(&a);
    let mut paid: BTreeMap<MemberId, Cents> = BTreeMap::new();
    let mut share: BTreeMap<MemberId, Cents> = BTreeMap::new();
    let mut settled: BTreeMap<MemberId, Cents> = BTreeMap::new();
    let mut cats: BTreeMap<String, Cents> = BTreeMap::new();
    let mut lines = Vec::new();
    for e in l.entries.values() {
        if e.agreement != a || !span.contains(e.date) || e.event.is_some() || e.deleted {
            continue;
        }
        let counted = l.counts(e);
        match &e.kind {
            EntryKind::Expense {
                expense,
                paid: by,
                shares,
                member_part,
                ..
            } => {
                let who = match by {
                    Paid::Member(m) => name(m),
                    Paid::Pot(d) => ag
                        .and_then(|g| g.deposits.get(d))
                        .map(|(dep, _)| dep.label.clone())
                        .unwrap_or_else(|| "the pot".into()),
                };
                lines.push(Line {
                    seq: e.seq,
                    date: e.date.to_string(),
                    what: expense.category.clone(),
                    by: who,
                    amount: expense.amount,
                    disputed: e.disputed,
                });
                if !counted {
                    continue;
                }
                *cats.entry(expense.category.clone()).or_default() += expense.amount;
                if let Paid::Member(m) = by {
                    *paid.entry(*m).or_default() += expense.amount;
                }
                if let Some((m, part)) = member_part {
                    *paid.entry(*m).or_default() += part.values().sum::<Cents>();
                }
                for (m, s) in shares {
                    *share.entry(*m).or_default() += s;
                }
            }
            EntryKind::Payment(p) => {
                lines.push(Line {
                    seq: e.seq,
                    date: e.date.to_string(),
                    what: "Payment".into(),
                    by: format!("{} → {}", name(&p.from), name(&p.to)),
                    amount: p.amount,
                    disputed: e.disputed,
                });
                if counted {
                    *settled.entry(p.from).or_default() += p.amount;
                    *settled.entry(p.to).or_default() -= p.amount;
                }
            }
        }
    }
    let mut adjusted: BTreeMap<MemberId, Cents> = BTreeMap::new();
    if let Some(c) = ag.and_then(|g| g.closed.iter().find(|c| c.span == span)) {
        for adj in &c.adjustments {
            for (m, v) in adj {
                *adjusted.entry(*m).or_default() += v;
            }
        }
    }
    let balances = l.balances_until(a, Some(span.last));
    let members: Vec<MemberLine> = ag
        .map(|g| g.members.keys().copied().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|m| MemberLine {
            member: m,
            name: name(&m),
            paid: paid.get(&m).copied().unwrap_or(0),
            share: share.get(&m).copied().unwrap_or(0),
            settled: settled.get(&m).copied().unwrap_or(0),
            adjusted: adjusted.get(&m).copied().unwrap_or(0),
            balance: balances.get(m),
        })
        .collect();
    let settle = settle_up(&balances)
        .into_iter()
        .map(|Transfer { from, to, amount }| (name(&from), name(&to), amount))
        .collect();
    let total = cats.values().sum();
    Statement {
        agreement: a,
        agreement_name: ag.map(|g| g.name.clone()).unwrap_or_default(),
        period: span,
        closed_by: seq,
        members,
        by_category: cats.into_iter().collect(),
        total,
        lines,
        settle,
    }
}
