//! Verify: the two questions a member can ask of a book, answered from the records alone.
//!
//! 1. Has the history been changed? (Hyperbooks: every record's fingerprint and link.)
//! 2. Do the numbers add up? (Replay every record under the rules; every expense split
//!    exactly, to the cent; every seal points at the record it names.)

use crate::{EntryKind, Ledger};
use serde::Serialize;
use sk_chain::Record;
use sk_core::Cents;

/// One answer: yes or no, and the lines that say why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub ok: bool,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Audit {
    /// "Has the history been changed?" (`None` for a book that isn't a Hyperbook.)
    pub history: Option<Answer>,
    /// "Do the numbers add up?"
    pub numbers: Answer,
}

fn money(c: Cents) -> String {
    let sign = if c < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", c.abs() / 100, c.abs() % 100)
}

pub fn audit(records: &[Record], hyperbook: bool) -> Audit {
    let history = hyperbook.then(|| match sk_chain::verify(records) {
        Ok(h) => Answer {
            ok: true,
            lines: vec![format!(
                "No. All {} records check out, each linked to the one before. The last is record {} ({}).",
                records.len(),
                h.seq,
                h.hash.to_hex()
            )],
        },
        Err(b) => Answer {
            ok: false,
            lines: vec![format!("Yes: {b}.")],
        },
    });

    let (l, rejects) = Ledger::replay(records);
    let mut lines = Vec::new();
    for (seq, r) in &rejects {
        lines.push(format!(
            "Record {seq} doesn't follow the book's rules: {}",
            r.0
        ));
    }
    let mut expenses = 0;
    for e in l.entries.values() {
        if let EntryKind::Expense {
            expense, shares, ..
        } = &e.kind
        {
            expenses += 1;
            let split: Cents = shares.values().sum();
            if split != expense.amount {
                lines.push(format!(
                    "Record {}: the shares add up to {}, not {}.",
                    e.seq,
                    money(split),
                    money(expense.amount)
                ));
            }
        }
    }
    for s in &l.seals {
        let ok = records
            .iter()
            .find(|r| r.seq == s.head_seq)
            .is_some_and(|r| r.hash.to_hex() == s.head_hash);
        if !ok {
            lines.push(format!(
                "The seal at record {} names record {}, which doesn't match.",
                s.seq, s.head_seq
            ));
        }
    }
    let ok = lines.is_empty();
    if ok {
        let mut summary = format!(
            "Yes. All {} records replay under the rules; every one of {expenses} expenses is split exactly, to the cent",
            records.len()
        );
        if !l.seals.is_empty() {
            summary.push_str(&format!(
                "; all {} seals match their records",
                l.seals.len()
            ));
        }
        summary.push('.');
        lines.push(summary);
    }
    Audit {
        history,
        numbers: Answer { ok, lines },
    }
}
