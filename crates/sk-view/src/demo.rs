//! A sample household (Jerry, Sam and Alex sharing an apartment), for the screenshots,
//! the tests, and a demo mode.

use serde_json::{json, Value};
use sk_chain::{Author, Record};
use sk_core::{MemberId, NaiveDate};
use sk_ledger::Ledger;

fn ts(d: NaiveDate) -> i64 {
    d.and_hms_opt(12, 0, 0)
        .map(|t| t.and_utc().timestamp_millis())
        .unwrap_or(0)
}

/// The sample history, with this month's activity dated around `today`.
pub fn records(today: NaiveDate) -> Vec<Record> {
    use chrono::Datelike;
    let month = today.with_day(1).unwrap_or(today);
    let last = month
        .checked_sub_months(chrono::Months::new(1))
        .unwrap_or(month);
    let day = |n: u32| month.with_day(n.min(today.day())).unwrap_or(month);
    let mut out: Vec<Record> = Vec::new();
    let mut add = |d: NaiveDate, member: Option<u32>, kind: &str, body: Value| -> u64 {
        let author = Author {
            member: member.map(MemberId),
            device: "demo".into(),
        };
        let r = match sk_chain::next(out.last(), ts(d), author, kind, body) {
            Ok(r) => r,
            Err(_) => return 0,
        };
        let seq = r.seq;
        out.push(r);
        seq
    };
    add(last, None, sk_chain::GENESIS, json!({}));
    for (id, name) in [(1, "Jerry"), (2, "Sam"), (3, "Alex")] {
        add(last, None, "member-add", json!({"id": id, "name": name}));
    }
    add(
        last,
        None,
        "agreement-add",
        json!({
            "id": 1, "name": "Apartment",
            "members": {"1": last, "2": last, "3": last},
            "schedule": {"Monthly": {"start": last, "day": {"Day": 1}, "every": 1}},
        }),
    );
    let rule = |id: u32, name: &str, method: Value, cats: &[&str]| {
        json!({
            "agreement": 1, "rule_id": id, "effective": last, "categories": cats,
            "rule": {"name": name, "method": method, "members": [1, 2, 3]},
        })
    };
    for r in [
        rule(1, "Shared", json!("Equal"), &["Groceries", "Household"]),
        rule(
            2,
            "Rent",
            json!({"Percent": {"1": 4000, "2": 3500, "3": 2500}}),
            &["Rent", "Utilities"],
        ),
    ] {
        let p = add(last, Some(1), "rule-proposal", r);
        for m in [2, 3] {
            add(
                last,
                Some(m),
                "approval",
                json!({"proposal": p, "approve": true}),
            );
        }
    }
    let exp = |d: NaiveDate, amount: i64, payer: u32, cat: &str, note: &str| json!({"agreement": 1, "date": d, "amount": amount, "payer": {"member": payer}, "category": cat, "note": note});
    add(
        last,
        Some(1),
        "expense",
        exp(last, 180000, 1, "Rent", "Rent"),
    );
    add(
        last,
        Some(2),
        "expense",
        exp(last, 12450, 2, "Groceries", "Costco"),
    );
    add(
        month,
        Some(1),
        "expense",
        exp(month, 180000, 1, "Rent", "Rent"),
    );
    add(
        day(3),
        Some(2),
        "expense",
        exp(day(3), 8347, 2, "Groceries", "Trader Joe's"),
    );
    // a pot everyone tops up monthly, a security deposit, and the monthly bills
    add(
        last,
        Some(1),
        "deposit",
        json!({"op": "create", "agreement": 1, "id": 1, "kind": "Spending", "label": "Household pot",
               "holder": "HouseholdPot", "refundable": "Yes"}),
    );
    add(
        last,
        Some(1),
        "deposit",
        json!({"op": "create", "agreement": 1, "id": 2, "kind": "Held", "label": "Rental security",
               "holder": "Landlord", "refundable": "Yes"}),
    );
    for (m, amount) in [(1, 60000), (2, 52500), (3, 37500)] {
        add(
            last,
            Some(m),
            "deposit",
            json!({"op": "pay-in", "agreement": 1, "id": 2, "member": m, "amount": amount}),
        );
    }
    // a monthly schedule's start is its first due date
    let monthly = |d: u32| json!({"Monthly": {"start": last.with_day(d).unwrap_or(last), "day": {"Day": d}, "every": 1}});
    add(
        last,
        Some(3),
        "recurring",
        json!({"op": "add", "agreement": 1, "id": 1, "name": "Electric", "schedule": monthly(5),
               "what": {"type": "bill", "amount": 9500, "category": "Utilities", "payer": 3}}),
    );
    add(
        last,
        Some(1),
        "recurring",
        json!({"op": "add", "agreement": 1, "id": 2, "name": "Internet",
               "schedule": monthly(today.day().min(28)),
               "what": {"type": "bill", "amount": 6999, "category": "Utilities", "payer": 1}}),
    );
    add(
        last,
        Some(1),
        "recurring",
        json!({"op": "add", "agreement": 1, "id": 3, "name": "Pot top-up", "schedule": monthly(1),
               "what": {"type": "top-up", "deposit": 1, "amounts": {"1": 5000, "2": 5000, "3": 5000}}}),
    );
    add(
        month,
        Some(1),
        "deposit",
        json!({"op": "pay-in", "agreement": 1, "id": 1, "member": 1, "amount": 5000,
               "due": {"recurring": 3, "date": month}}),
    );
    let mut electric = exp(day(5), 9620, 3, "Utilities", "Electric bill");
    electric["due"] = json!({"recurring": 1, "date": month.with_day(5).unwrap_or(month)});
    add(day(5), Some(3), "expense", electric);
    let d = add(
        day(6),
        Some(3),
        "expense",
        exp(day(6), 2599, 3, "Household", "Paper towels, soap"),
    );
    add(
        day(7),
        Some(1),
        "dispute",
        json!({"target": d, "note": "Wasn't that from the last trip?"}),
    );
    add(
        day(8),
        Some(2),
        "payment",
        json!({"agreement": 1, "date": day(8), "from": 2, "to": 1, "amount": 50000}),
    );
    // a proposal waiting for Jerry
    add(
        day(8),
        Some(2),
        "rule-proposal",
        rule(
            3,
            "Streaming",
            json!({"Shares": {"1": 1, "2": 2, "3": 1}}),
            &["Subscriptions"],
        ),
    );
    out
}

pub fn ledger(today: NaiveDate) -> Ledger {
    Ledger::replay(&records(today)).0
}
