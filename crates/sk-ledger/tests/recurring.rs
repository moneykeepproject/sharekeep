//! Recurring bills and pot top-ups (listed as due until someone records them), and interest
//! on held deposits at period close.

use serde_json::{json, Value};
use sk_chain::{Author, Record};
use sk_core::{AgreementId, MemberId};
use sk_ledger::Ledger;

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

fn ts(date: &str) -> i64 {
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    day(d.month(), d.day())
}

use chrono::Datelike;

fn d(s: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn monthly(start: &str, day_of_month: u32) -> Value {
    json!({"Monthly": {"start": start, "day": {"Day": day_of_month}, "every": 1}})
}

#[test]
fn a_recurring_bill_is_due_until_someone_records_it() {
    let mut b = household();
    approved_rule(
        &mut b,
        equal_rule(1, "Shared", "2027-01-01", &["Utilities"]),
    );
    b.add(
        day(1, 3),
        Some(SAM),
        "recurring",
        json!({"op": "add", "agreement": 1, "id": 1, "name": "Electric",
               "schedule": monthly("2027-01-05", 5),
               "what": {"type": "bill", "amount": 9000, "category": "Utilities", "payer": SAM}}),
    );
    // on Jan 3 it's coming up (within a week); by Feb 6 two are due
    let e = b.ledger.expected(A, d("2027-01-03"));
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].date, d("2027-01-05"));
    assert_eq!(e[0].amount, 9000);
    assert_eq!(b.ledger.expected(A, d("2027-02-06")).len(), 2);
    // recording January's clears it, and it can't be recorded twice
    let bill = json!({"agreement": 1, "date": "2027-01-05", "amount": 9312, "payer": {"member": SAM},
                      "category": "Utilities", "due": {"recurring": 1, "date": "2027-01-05"}});
    let seq = b.add(ts("2027-01-06"), Some(SAM), "expense", bill.clone());
    let e = b.ledger.expected(A, d("2027-02-06"));
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].date, d("2027-02-05"));
    let again = b
        .try_add(ts("2027-01-07"), Some(SAM), "expense", bill)
        .unwrap_err();
    assert!(again.contains("already recorded"), "{again}");
    // deleting it makes it due again
    b.add(
        ts("2027-01-08"),
        Some(SAM),
        "entry-delete",
        json!({"target": seq}),
    );
    assert_eq!(b.ledger.expected(A, d("2027-02-06")).len(), 2);
    // a made-up reference is refused
    let bad = json!({"agreement": 1, "date": "2027-01-05", "amount": 100, "payer": {"member": SAM},
                     "category": "Utilities", "due": {"recurring": 7, "date": "2027-01-05"}});
    assert!(b
        .try_add(ts("2027-01-08"), Some(SAM), "expense", bad)
        .is_err());
    // stopping it ends what's due after the stop
    b.add(
        ts("2027-01-20"),
        Some(JERRY),
        "recurring",
        json!({"op": "stop", "agreement": 1, "id": 1}),
    );
    let e = b.ledger.expected(A, d("2027-03-01"));
    assert_eq!(
        e.iter().map(|x| x.date).collect::<Vec<_>>(),
        [d("2027-01-05")]
    );
}

#[test]
fn pot_top_ups_are_due_per_member_and_paid_in_with_a_reference() {
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
    b.add(
        day(1, 2),
        Some(JERRY),
        "recurring",
        json!({
            "op": "add", "agreement": 1, "id": 2, "name": "Pot top-up",
            "schedule": monthly("2027-01-05", 5),
            "what": {"type": "top-up", "deposit": 1, "amounts": {"1": 5000, "2": 5000, "3": 3000}},
        }),
    );
    let e = b.ledger.expected(A, d("2027-01-05"));
    assert_eq!(e.len(), 3);
    assert_eq!(
        e.iter()
            .find(|x| x.member == Some(MemberId(ALEX)))
            .unwrap()
            .amount,
        3000
    );
    b.add(
        day(1, 3),
        Some(ALEX),
        "deposit",
        json!({
            "op": "pay-in", "agreement": 1, "id": 1, "member": ALEX, "amount": 3000,
            "due": {"recurring": 2, "date": "2027-01-05"},
        }),
    );
    let e = b.ledger.expected(A, d("2027-01-05"));
    assert_eq!(e.len(), 2);
    assert!(e.iter().all(|x| x.member != Some(MemberId(ALEX))));
    // twice is refused; a top-up into the wrong deposit too
    assert!(b
        .try_add(
            day(1, 3),
            Some(ALEX),
            "deposit",
            json!({
                "op": "pay-in", "agreement": 1, "id": 1, "member": ALEX, "amount": 3000,
                "due": {"recurring": 2, "date": "2027-01-05"},
            })
        )
        .is_err());
    // a top-up needs a spending deposit
    b.add(
        day(1, 3),
        Some(JERRY),
        "deposit",
        json!({
            "op": "create", "agreement": 1, "id": 2, "kind": "Held", "label": "Security",
            "holder": "Landlord", "refundable": "Yes",
        }),
    );
    let e = b
        .try_add(
            day(1, 3),
            Some(JERRY),
            "recurring",
            json!({
                "op": "add", "agreement": 1, "id": 3, "name": "Wrong",
                "schedule": monthly("2027-01-01", 1),
                "what": {"type": "top-up", "deposit": 2, "amounts": {"1": 100}},
            }),
        )
        .unwrap_err();
    assert!(e.contains("spending deposit"), "{e}");
}

#[test]
fn held_deposits_earn_the_books_interest_when_a_period_closes() {
    let mut b = household();
    approved_rule(&mut b, equal_rule(1, "Shared", "2027-01-01", &[]));
    b.add(
        day(1, 2),
        None,
        "setting",
        json!({"key": "deposit_interest_bp", "value": 365}),
    );
    b.add(
        day(1, 2),
        Some(JERRY),
        "deposit",
        json!({
            "op": "create", "agreement": 1, "id": 1, "kind": "Held", "label": "Security",
            "holder": "Landlord", "refundable": "Yes",
        }),
    );
    for (m, amount) in [(JERRY, 100_000), (SAM, 100_000)] {
        b.add(
            day(1, 2),
            Some(m),
            "deposit",
            json!({"op": "pay-in", "agreement": 1, "id": 1, "member": m, "amount": amount}),
        );
    }
    b.add(
        day(2, 2),
        Some(JERRY),
        "period-close",
        json!({"agreement": 1, "date": "2027-01-01"}),
    );
    // 3.65% a year for 31 days on $2,000: $6.20, shared by the stakes
    let st = &b.ledger.agreements[&A].deposits[&sk_core::DepositId(1)].1;
    assert_eq!(st.balance(), 200_000 + 620);
    assert_eq!(st.stakes()[&MemberId(JERRY)], 100_310);
    // replaying the history gives the same
    let again = Ledger::replay(&b.records).0;
    assert_eq!(
        again.agreements[&A].deposits[&sk_core::DepositId(1)]
            .1
            .balance(),
        200_620
    );
}
