//! A household's life, record by record: members, an agreement, rules approved by
//! everyone, expenses, payments, a pot, disputes, a leave, a closed period with caps.

use serde_json::{json, Value};
use sk_chain::{Author, Record};
use sk_core::{AgreementId, MemberId};
use sk_ledger::{Ledger, ProposalStatus};

const JERRY: u32 = 1;
const SAM: u32 = 2;
const ALEX: u32 = 3;
const A: AgreementId = AgreementId(1);

/// Day `d` of 2027 at noon UTC, in ms.
fn day(month: u32, d: u32) -> i64 {
    let date = chrono::NaiveDate::from_ymd_opt(2027, month, d).unwrap();
    date.and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

struct Book {
    records: Vec<Record>,
    ledger: Ledger,
}

impl Book {
    fn new() -> Self {
        let g =
            sk_chain::next(None, day(1, 1), Author::hub(), sk_chain::GENESIS, json!({})).unwrap();
        let mut b = Book {
            records: vec![g.clone()],
            ledger: Ledger::new(),
        };
        b.ledger.apply(&g).unwrap();
        b
    }

    fn try_add(
        &mut self,
        ts: i64,
        member: Option<u32>,
        kind: &str,
        body: Value,
    ) -> Result<u64, String> {
        let author = Author {
            member: member.map(MemberId),
            device: "test".into(),
        };
        let r = sk_chain::next(self.records.last(), ts, author, kind, body).unwrap();
        self.ledger.check(&r).map_err(|e| e.0)?;
        self.ledger.apply(&r).map_err(|e| e.0)?;
        self.records.push(r.clone());
        Ok(r.seq)
    }

    fn add(&mut self, ts: i64, member: Option<u32>, kind: &str, body: Value) -> u64 {
        self.try_add(ts, member, kind, body.clone())
            .unwrap_or_else(|e| panic!("{kind} {body}: {e}"))
    }

    fn balance(&self, m: u32) -> i64 {
        self.ledger.balances(A).get(MemberId(m))
    }
}

fn household() -> Book {
    let mut b = Book::new();
    for (id, name) in [(JERRY, "Jerry"), (SAM, "Sam"), (ALEX, "Alex")] {
        b.add(
            day(1, 1),
            None,
            "member-add",
            json!({"id": id, "name": name}),
        );
    }
    b.add(
        day(1, 1),
        None,
        "agreement-add",
        json!({
            "id": 1, "name": "Apartment",
            "members": {"1": "2027-01-01", "2": "2027-01-01", "3": "2027-01-01"},
            "schedule": {"Monthly": {"start": "2027-01-01", "day": {"Day": 1}, "every": 1}},
        }),
    );
    b
}

fn equal_rule(id: u32, name: &str, effective: &str, categories: &[&str]) -> Value {
    json!({
        "agreement": 1, "rule_id": id, "effective": effective, "categories": categories,
        "rule": {"name": name, "method": "Equal", "members": [1, 2, 3]},
    })
}

fn approved_rule(b: &mut Book, rule: Value) -> u64 {
    let p = b.add(day(1, 2), Some(JERRY), "rule-proposal", rule);
    b.add(
        day(1, 2),
        Some(SAM),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    b.add(
        day(1, 2),
        Some(ALEX),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    p
}

fn expense(date: &str, amount: i64, payer: u32, category: &str) -> Value {
    json!({"agreement": 1, "date": date, "amount": amount, "payer": {"member": payer}, "category": category})
}

#[test]
fn a_rule_takes_effect_only_once_everyone_approves() {
    let mut b = household();
    let p = b.add(
        day(1, 2),
        Some(JERRY),
        "rule-proposal",
        equal_rule(1, "Shared", "2027-01-01", &["Groceries"]),
    );
    let e = b
        .try_add(
            day(1, 2),
            Some(SAM),
            "expense",
            expense("2027-01-02", 9000, SAM, "Groceries"),
        )
        .unwrap_err();
    assert!(e.contains("no rule"), "{e}");
    // the proposer can't approve their own proposal; outsiders' approvals aren't needed
    assert!(b
        .try_add(
            day(1, 2),
            Some(JERRY),
            "approval",
            json!({"proposal": p, "approve": true})
        )
        .is_err());
    b.add(
        day(1, 2),
        Some(SAM),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    assert_eq!(b.ledger.proposals[&p].status, ProposalStatus::Waiting);
    assert_eq!(b.ledger.waiting_for(MemberId(ALEX)).len(), 1);
    b.add(
        day(1, 2),
        Some(ALEX),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    assert_eq!(b.ledger.proposals[&p].status, ProposalStatus::Approved);

    b.add(
        day(1, 3),
        Some(SAM),
        "expense",
        expense("2027-01-03", 9000, SAM, "Groceries"),
    );
    assert_eq!(
        (b.balance(SAM), b.balance(JERRY), b.balance(ALEX)),
        (6000, -3000, -3000)
    );
}

#[test]
fn one_decline_returns_a_proposal() {
    let mut b = household();
    let p = b.add(
        day(1, 2),
        Some(JERRY),
        "rule-proposal",
        equal_rule(1, "Shared", "2027-01-01", &[]),
    );
    b.add(
        day(1, 2),
        Some(SAM),
        "approval",
        json!({"proposal": p, "approve": false}),
    );
    assert_eq!(b.ledger.proposals[&p].status, ProposalStatus::Declined);
    assert!(b
        .try_add(
            day(1, 2),
            Some(ALEX),
            "approval",
            json!({"proposal": p, "approve": true})
        )
        .is_err());
}

#[test]
fn a_one_keeper_household_needs_only_the_keeper() {
    let mut b = household();
    b.add(
        day(1, 1),
        None,
        "setting",
        json!({"key": "one_keeper", "value": JERRY}),
    );
    let p = b.add(
        day(1, 2),
        Some(JERRY),
        "rule-proposal",
        equal_rule(1, "Shared", "2027-01-01", &[]),
    );
    assert_eq!(b.ledger.proposals[&p].status, ProposalStatus::Approved);
    let q = b.add(
        day(1, 2),
        Some(SAM),
        "rule-proposal",
        equal_rule(2, "Other", "2027-01-01", &[]),
    );
    b.add(
        day(1, 2),
        Some(JERRY),
        "approval",
        json!({"proposal": q, "approve": true}),
    );
    assert_eq!(b.ledger.proposals[&q].status, ProposalStatus::Approved);
}

#[test]
fn devices_cant_do_hub_things_and_payments_are_checked() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    assert!(b
        .try_add(
            day(1, 2),
            Some(SAM),
            "member-add",
            json!({"id": 9, "name": "X"})
        )
        .is_err());
    // only the payer or payee records a payment
    let pay =
        json!({"agreement": 1, "date": "2027-01-05", "from": JERRY, "to": SAM, "amount": 3000});
    assert!(b
        .try_add(day(1, 5), Some(ALEX), "payment", pay.clone())
        .is_err());
    b.add(day(1, 5), Some(JERRY), "payment", pay);
    assert_eq!(b.balance(JERRY), 3000);
    assert!(b
        .try_add(
            day(1, 5),
            Some(SAM),
            "expense",
            expense("2027-01-05", 0, SAM, "x")
        )
        .is_err());
    assert!(b
        .try_add(day(1, 5), Some(SAM), "nonsense", json!({}))
        .is_err());
}

#[test]
fn closing_a_period_locks_it_applies_caps_and_states_who_owes_whom() {
    let mut b = household();
    let mut rule = equal_rule(1, "Utilities", "2027-01-01", &["Utilities"]);
    rule["rule"]["period_caps"] = json!({"3": 10000});
    approved_rule(&mut b, rule);
    b.add(
        day(1, 10),
        Some(JERRY),
        "expense",
        expense("2027-01-10", 45000, JERRY, "Utilities"),
    );
    assert_eq!(b.balance(ALEX), -15000);

    let close = json!({"agreement": 1, "date": "2027-01-01"});
    let e = b
        .try_add(day(1, 31), Some(SAM), "period-close", close.clone())
        .unwrap_err();
    assert!(e.contains("2027-01-31"), "{e}");
    let seq = b.add(day(2, 1), Some(SAM), "period-close", close.clone());
    // Alex's cap: 150.00 → 100.00; the 50.00 goes to Jerry and Sam
    assert_eq!(b.balance(ALEX), -10000);
    assert_eq!(b.balance(SAM), -17500);
    assert_eq!(b.ledger.balances(A).sum(), 0);

    // the statement is what applying the close returned
    let close_rec = b.records.last().unwrap().clone();
    assert_eq!(close_rec.seq, seq);
    let mut before = Ledger::replay(&b.records[..b.records.len() - 1]).0;
    let st = before.apply(&close_rec).unwrap().statement.unwrap();
    assert_eq!(st.total, 45000);
    assert_eq!(
        st.members
            .iter()
            .find(|m| m.member.0 == ALEX)
            .unwrap()
            .adjusted,
        -5000
    );
    assert_eq!(st.settle.len(), 2);

    assert!(b
        .try_add(day(2, 1), Some(SAM), "period-close", close)
        .is_err());
    // the closed period takes no more entries, edits or deletes
    assert!(b
        .try_add(
            day(2, 2),
            Some(SAM),
            "expense",
            expense("2027-01-20", 100, SAM, "Utilities")
        )
        .is_err());
    let first = b.records.iter().find(|r| r.kind == "expense").unwrap().seq;
    assert!(b
        .try_add(
            day(2, 2),
            Some(JERRY),
            "entry-delete",
            json!({"target": first})
        )
        .is_err());
}

#[test]
fn the_pot_pays_and_gives_back_on_delete() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    b.add(
        day(1, 2),
        Some(JERRY),
        "deposit",
        json!({
            "op": "create", "agreement": 1, "id": 1, "kind": "Spending", "label": "Household pot",
            "holder": "HouseholdPot", "refundable": "Yes",
        }),
    );
    for m in [JERRY, SAM, ALEX] {
        b.add(
            day(1, 2),
            Some(m),
            "deposit",
            json!({"op": "pay-in", "agreement": 1, "id": 1, "member": m, "amount": 10000}),
        );
    }
    let seq = b.add(day(1, 3), Some(SAM), "expense", json!({
        "agreement": 1, "date": "2027-01-03", "amount": 6000, "payer": {"pot": 1}, "category": "Supplies",
    }));
    assert_eq!(b.balance(SAM), 0);
    let pot = &b.ledger.agreements[&A].deposits[&sk_core::DepositId(1)].1;
    assert_eq!((pot.balance(), pot.stake(MemberId(SAM))), (24000, 8000));

    // more than the pot holds: refused under the default "block"
    assert!(b
        .try_add(day(1, 3), Some(SAM), "expense", json!({
            "agreement": 1, "date": "2027-01-03", "amount": 30000, "payer": {"pot": 1}, "category": "Big",
        }))
        .is_err());

    b.add(day(1, 4), Some(SAM), "entry-delete", json!({"target": seq}));
    let pot = &b.ledger.agreements[&A].deposits[&sk_core::DepositId(1)].1;
    assert_eq!(pot.balance(), 30000);
}

#[test]
fn leaving_needs_everyone_and_rules_follow() {
    let mut b = household();
    approved_rule(
        &mut b,
        equal_rule(1, "Shared", "2027-01-01", &["Groceries"]),
    );
    let p = b.add(
        day(1, 10),
        Some(ALEX),
        "member-leave",
        json!({"agreement": 1, "member": ALEX, "date": "2027-01-15"}),
    );
    b.add(
        day(1, 10),
        Some(JERRY),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    assert!(b.ledger.agreements[&A].members[&MemberId(ALEX)]
        .left
        .is_none());
    b.add(
        day(1, 10),
        Some(SAM),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    assert!(b.ledger.agreements[&A].members[&MemberId(ALEX)]
        .left
        .is_some());
    // an equal rule re-divides among those still here
    b.add(
        day(1, 20),
        Some(SAM),
        "expense",
        expense("2027-01-20", 1000, SAM, "Groceries"),
    );
    assert_eq!(b.balance(ALEX), 0);
    assert_eq!(b.balance(JERRY), -500);

    // a percentage rule naming the leaver must be updated first
    let pct = json!({
        "agreement": 1, "rule_id": 2, "effective": "2027-01-01", "categories": ["Rent"],
        "rule": {"name": "Rent", "method": {"Percent": {"1": 4000, "2": 4000, "3": 2000}}, "members": [1, 2, 3]},
    });
    let p = b.add(day(1, 21), Some(JERRY), "rule-proposal", pct);
    b.add(
        day(1, 21),
        Some(SAM),
        "approval",
        json!({"proposal": p, "approve": true}),
    );
    let e = b
        .try_add(
            day(1, 21),
            Some(SAM),
            "expense",
            expense("2027-01-21", 1000, SAM, "Rent"),
        )
        .unwrap_err();
    assert!(e.contains("needs updating"), "{e}");
    // before the leave date it still divides three ways
    b.add(
        day(1, 21),
        Some(SAM),
        "expense",
        expense("2027-01-12", 1000, SAM, "Rent"),
    );
}

#[test]
fn disputed_entries_can_be_held_aside() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    let seq = b.add(
        day(1, 3),
        Some(SAM),
        "expense",
        expense("2027-01-03", 3000, SAM, "Dinner"),
    );
    b.add(
        day(1, 3),
        Some(JERRY),
        "dispute",
        json!({"target": seq, "note": "I wasn't there"}),
    );
    assert_eq!(b.balance(SAM), 2000); // in balances by default
    b.add(
        day(1, 3),
        None,
        "setting",
        json!({"key": "dispute_mode", "value": "held_aside"}),
    );
    assert_eq!(b.balance(SAM), 0);
    b.add(
        day(1, 4),
        Some(JERRY),
        "dispute",
        json!({"target": seq, "resolved": true}),
    );
    assert_eq!(b.balance(SAM), 2000);
}

#[test]
fn edits_recompute_the_split() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    let seq = b.add(
        day(1, 3),
        Some(SAM),
        "expense",
        expense("2027-01-03", 3000, SAM, "Dinner"),
    );
    b.add(
        day(1, 3),
        Some(SAM),
        "entry-edit",
        json!({"target": seq, "expense": expense("2027-01-03", 6000, SAM, "Dinner")}),
    );
    assert_eq!(b.balance(SAM), 4000);
    assert_eq!(b.ledger.entries[&seq].author, Some(MemberId(SAM)));
}

#[test]
fn events_settle_on_their_own_with_their_own_members() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    b.add(
        day(1, 2),
        Some(JERRY),
        "schedule",
        json!({
            "op": "event", "agreement": 1, "id": 7, "name": "Beach week",
            "first": "2027-01-04", "last": "2027-01-11", "members": [1, 2],
        }),
    );
    let mut e = expense("2027-01-05", 1000, JERRY, "Beach");
    e["event"] = json!(7);
    b.add(day(1, 5), Some(JERRY), "expense", e);
    assert_eq!(b.balance(ALEX), 0);
    assert_eq!(b.balance(SAM), -500);
    let mut outside = expense("2027-01-20", 1000, JERRY, "Beach");
    outside["event"] = json!(7);
    assert!(b
        .try_add(day(1, 20), Some(JERRY), "expense", outside)
        .is_err());
}

#[test]
fn replay_gives_the_same_state_and_skips_what_no_longer_applies() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    b.add(
        day(1, 3),
        Some(SAM),
        "expense",
        expense("2027-01-03", 3000, SAM, "Dinner"),
    );
    // a record from before checks existed, which doesn't apply
    let bad = sk_chain::next(
        b.records.last(),
        day(1, 4),
        Author {
            member: Some(MemberId(SAM)),
            device: "old".into(),
        },
        "expense",
        json!({"amount": "lots"}),
    )
    .unwrap();
    let mut records = b.records.clone();
    records.push(bad.clone());
    let (replayed, skipped) = Ledger::replay(&records);
    assert_eq!(replayed, b.ledger);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, bad.seq);
}

#[test]
fn every_listed_kind_is_understood_and_no_other() {
    let l = sk_ledger::Ledger::new();
    let rec = |kind: &str| sk_chain::Record {
        seq: 1,
        ts: 0,
        author: sk_chain::Author::hub(),
        kind: kind.into(),
        body: serde_json::json!({}),
        prev: sk_chain::Hash::ZERO,
        hash: sk_chain::Hash::ZERO,
    };
    for k in sk_ledger::KINDS {
        if let Err(e) = l.check(&rec(k)) {
            assert!(!e.0.contains("doesn't know"), "{k}: {}", e.0);
        }
    }
    let e = l.check(&rec("teleport")).unwrap_err();
    assert!(e.0.contains("doesn't know"));
}

#[test]
fn pictures_and_pdfs_go_with_expenses() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Equal", "2027-01-01", &[]));
    let e = b.add(
        day(1, 10),
        Some(JERRY),
        "expense",
        expense("2027-01-10", 9000, JERRY, "Food"),
    );
    let file = |n: u8| json!({"target": e, "sha256": format!("{:064x}", n), "name": "receipt.jpg", "mime": "image/jpeg", "size": 300_000});
    // anyone in the agreement, after the expense was recorded
    b.add(day(1, 11), Some(SAM), "attachment", file(1));
    assert_eq!(b.ledger.entries[&e].attachments.len(), 1);
    assert_eq!(b.ledger.entries[&e].attachments[0].by, Some(MemberId(SAM)));
    assert!(b
        .try_add(day(1, 11), Some(SAM), "attachment", file(1))
        .unwrap_err()
        .contains("already"));
    // only pictures and PDFs, at most 10 MB, five to an expense
    let mut odd = file(9);
    odd["mime"] = json!("text/html");
    assert!(b
        .try_add(day(1, 11), Some(JERRY), "attachment", odd)
        .unwrap_err()
        .contains("Only pictures"));
    let mut big = file(9);
    big["size"] = json!(11 * 1024 * 1024);
    assert!(b
        .try_add(day(1, 11), Some(JERRY), "attachment", big)
        .is_err());
    let mut bad = file(9);
    bad["sha256"] = json!("../../etc");
    assert!(b
        .try_add(day(1, 11), Some(JERRY), "attachment", bad)
        .is_err());
    for n in 2..=5 {
        b.add(day(1, 11), Some(JERRY), "attachment", file(n));
    }
    assert!(b
        .try_add(day(1, 11), Some(JERRY), "attachment", file(6))
        .unwrap_err()
        .contains("5"));
    // an edit keeps them; a removal takes one off (the history keeps that it was there)
    b.add(
        day(1, 12),
        Some(JERRY),
        "entry-edit",
        json!({"target": e, "expense": expense("2027-01-10", 9300, JERRY, "Food")}),
    );
    assert_eq!(b.ledger.entries[&e].attachments.len(), 5);
    let mut off = file(3);
    off["remove"] = json!(true);
    b.add(day(1, 13), Some(ALEX), "attachment", off.clone());
    assert_eq!(b.ledger.entries[&e].attachments.len(), 4);
    assert!(b
        .try_add(day(1, 13), Some(ALEX), "attachment", off)
        .is_err());
    // payments don't carry files
    let p = b.add(
        day(1, 14),
        Some(SAM),
        "payment",
        json!({"agreement": 1, "date": "2027-01-14", "from": SAM, "to": JERRY, "amount": 100}),
    );
    let mut on_payment = file(7);
    on_payment["target"] = json!(p);
    assert!(b
        .try_add(day(1, 14), Some(SAM), "attachment", on_payment)
        .is_err());
    // money doesn't move
    assert_eq!(b.balance(JERRY) + b.balance(SAM) + b.balance(ALEX), 0);
}
