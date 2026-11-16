//! The screens' view models, on the sample household.

use sk_core::{AgreementId, Currency, MemberId, NaiveDate};
use sk_view::view::{self, RuleDraft};

const A: AgreementId = AgreementId(1);
const JERRY: MemberId = MemberId(1);

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2027, 3, 14).unwrap()
}

fn usd() -> Currency {
    Currency::ALL
        .into_iter()
        .find(|c| c.code() == "USD")
        .unwrap()
}

#[test]
fn the_sample_household_replays_cleanly() {
    let (_, skipped) = sk_ledger::Ledger::replay(&sk_view::demo::records(today()));
    assert!(skipped.is_empty(), "{skipped:?}");
}

#[test]
fn home_shows_where_i_stand_in_each_agreement() {
    let l = sk_view::demo::ledger(today());
    let cards = view::agreements(&l, JERRY, usd(), today());
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!(c.name, "Apartment");
    assert_eq!(c.sign, 1, "{}", c.headline);
    assert!(c.headline.starts_with("You are owed $"), "{}", c.headline);
    assert_eq!(c.waiting, 1);
    assert_eq!(c.period, "Mar 1 – Mar 31, 2027");
    // balances add up to zero
    let b = l.balances(A);
    assert_eq!(c.members.iter().map(|m| b.get(m.member)).sum::<i64>(), 0);
}

#[test]
fn activity_is_newest_first_and_marks_disputes() {
    let l = sk_view::demo::ledger(today());
    let rows = view::activity(&l, A, JERRY, usd());
    assert_eq!(rows[0].title, "Payment");
    assert_eq!(rows[0].detail, "Sam paid Jerry");
    let towels = rows.iter().find(|r| r.title.starts_with("Paper")).unwrap();
    assert!(towels.disputed);
    assert!(towels.detail.contains("Paid by Alex"));
    // last month isn't closed yet, so its entries can still change
    assert!(rows.iter().all(|r| r.editable || r.title == "Payment"));
}

#[test]
fn rules_members_periods_and_approvals() {
    let l = sk_view::demo::ledger(today());
    let rules = view::rules(&l, A, today());
    assert_eq!(rules.len(), 2, "the waiting proposal isn't in force");
    assert!(rules
        .iter()
        .any(|r| r.name == "Rent" && r.method == "Percentages"));
    let (members, settle) = view::members(&l, A, usd());
    assert_eq!(members.len(), 3);
    assert!(!settle.is_empty());
    let periods = view::periods(&l, A, today(), 2);
    assert_eq!(periods.len(), 2, "the agreement began last month");
    assert!(!periods[0].can_close);
    assert!(periods[1].can_close, "{}", periods[1].state);
    let a = view::approvals(&l, JERRY);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].title, "New rule: Streaming");
    assert!(
        view::approvals(&l, MemberId(2)).is_empty(),
        "Sam proposed it"
    );
}

#[test]
fn the_expense_preview_divides_by_the_categorys_rule() {
    let l = sk_view::demo::ledger(today());
    let e = view::expense_body(A, today(), 10000, JERRY, "Rent", "");
    let rows = view::preview(&l, JERRY, usd(), e).unwrap();
    let amounts: Vec<&str> = rows.iter().map(|r| r.amount.as_str()).collect();
    assert_eq!(amounts, ["$40.00", "$35.00", "$25.00"]);
    // a category no rule names goes to the default rule (the first approved: Shared)
    let e = view::expense_body(A, today(), 10000, JERRY, "Plants", "");
    let rows = view::preview(&l, JERRY, usd(), e).unwrap();
    let cents: Vec<&str> = rows.iter().map(|r| r.amount.as_str()).collect();
    assert_eq!(cents.iter().filter(|a| **a == "$33.33").count(), 2);
    // a zero amount can't post
    let e = view::expense_body(A, today(), 0, JERRY, "Rent", "");
    assert!(view::preview(&l, JERRY, usd(), e).is_err());
}

#[test]
fn percentage_sliders_always_total_one_hundred() {
    let l = sk_view::demo::ledger(today());
    let mut d = RuleDraft::new(&l, A);
    d.set_method(1);
    let total = |d: &RuleDraft| d.rows.iter().map(|r| r.value).sum::<f32>();
    assert_eq!(total(&d), 100.0);
    assert_eq!(
        d.rows.iter().map(|r| r.value).collect::<Vec<_>>(),
        [33.0, 33.0, 34.0]
    );
    d.set_value(0, 50.0);
    assert_eq!(d.rows[0].value, 50.0);
    assert_eq!(total(&d), 100.0);
    // a locked slider stays put while the others move
    d.toggle_locked(1);
    let held = d.rows[1].value;
    d.set_value(0, 80.0);
    assert_eq!(d.rows[1].value, held);
    assert_eq!(total(&d), 100.0);
    // one can't take more than the locked ones leave
    d.set_value(0, 100.0);
    assert_eq!(d.rows[0].value, 100.0 - held);
    assert_eq!(d.rows[2].value, 0.0);
    // opting someone out starts the percentages even again over the rest
    d.toggle_included(2);
    assert_eq!(d.rows[2].value, 0.0);
    assert_eq!(total(&d), 100.0);
    assert_eq!(d.rows[0].value, 50.0);
}

#[test]
fn the_rule_editor_shows_live_examples_and_makes_a_proposal() {
    let l = sk_view::demo::ledger(today());
    let mut d = RuleDraft::new(&l, A);
    d.name = "Internet".into();
    let ex = view::examples(&d.rule(), &l, usd()).unwrap();
    assert_eq!(ex.len(), 3);
    assert_eq!(ex[0].0, "$83.47");
    let cents: i64 = ex[0]
        .1
        .iter()
        .map(|r| usd().parse(r.amount.trim_start_matches('$')).unwrap())
        .sum();
    assert_eq!(cents, 8347, "examples never lose a cent");
    d.set_method(2);
    d.set_value(1, 3.0);
    let ex = view::examples(&d.rule(), &l, usd()).unwrap();
    assert_eq!(ex[1].1[1].amount, "$60.00", "Sam has 3 of 5 shares of $100");
    let body = d.proposal(A, sk_core::RuleId(9), today(), &["Internet".into()]);
    assert_eq!(body["rule"]["name"], "Internet");
    assert_eq!(body["categories"][0], "Internet");
    let p: sk_ledger::bodies::RuleProposal = serde_json::from_value(body).unwrap();
    assert_eq!(p.rule_id, sk_core::RuleId(9));
    // nobody included: the examples say why instead
    for i in 0..3 {
        d.toggle_included(i);
    }
    assert!(view::examples(&d.rule(), &l, usd()).is_err());
}

#[test]
fn the_web_apps_snapshot_has_every_screen() {
    let l = sk_view::demo::ledger(today());
    let s = view::snapshot(&l, JERRY, usd(), today());
    assert_eq!(s.me_name, "Jerry");
    assert_eq!(s.cards.len(), 1);
    assert_eq!(s.approvals.len(), 1);
    let a = &s.agreements[0];
    assert_eq!(a.people.len(), 3);
    assert_eq!(a.next_rule, sk_core::RuleId(4));
    assert_eq!(a.next_period.to_string(), "2027-04-01");
    assert!(a.i_owe.is_none(), "Jerry is owed");
    let sam = view::snapshot(&l, MemberId(2), usd(), today());
    assert_eq!(sam.agreements[0].i_owe.unwrap().0, JERRY);
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["agreements"][0]["people"][0]["name"], "Jerry");
    assert_eq!(v["today"], "2027-03-14");
}

#[test]
fn whats_due_deposits_and_recurring_items() {
    let l = sk_view::demo::ledger(today());
    let a = &view::snapshot(&l, JERRY, usd(), today()).agreements[0];
    // last month's Electric and Internet were never recorded; this month's Electric was;
    // Internet is due today; Sam's and Alex's top-ups are due, Jerry's is paid
    let names: Vec<(&str, &str)> = a
        .expected
        .iter()
        .map(|e| (e.name.as_str(), e.date.as_str()))
        .collect();
    assert!(names.contains(&("Internet", "Mar 14")), "{names:?}");
    assert!(names.contains(&("Electric", "Feb 5")), "{names:?}");
    assert!(!names.contains(&("Electric", "Mar 5")), "{names:?}");
    let topups: Vec<_> = a
        .expected
        .iter()
        .filter(|e| e.kind == "top-up" && e.date == "Mar 1")
        .collect();
    assert_eq!(topups.len(), 2);
    assert!(topups.iter().all(|e| !e.mine), "they're Sam's and Alex's");
    assert!(a.expected.iter().any(|e| e.overdue));
    assert_eq!(a.deposits.len(), 2);
    assert_eq!(a.deposits[1].balance, "$1,500.00");
    assert_eq!(a.deposits[1].detail, "Your part: $600.00");
    assert_eq!(a.recurring.len(), 3);
    assert_eq!(a.next_recurring, 4);
}

#[test]
fn a_recurring_bill_is_due_only_on_its_day() {
    let l = sk_view::demo::ledger(today());
    let a = &view::snapshot(&l, JERRY, usd(), today()).agreements[0];
    for e in a.expected.iter().filter(|e| e.name == "Electric") {
        assert!(e.date.ends_with(" 5"), "{}", e.date);
    }
}

#[test]
fn the_schedule_in_words_and_expenses_for_editing() {
    let l = sk_view::demo::ledger(today());
    let a = &view::snapshot(&l, JERRY, usd(), today()).agreements[0];
    assert_eq!(a.schedule, "Every month, on the 1st");
    assert!(a.events.is_empty());
    assert_eq!(a.next_event, 1);
    let groceries = a
        .activity
        .iter()
        .find(|r| r.title == "Trader Joe's")
        .unwrap();
    assert_eq!(groceries.expense.as_ref().unwrap().amount, 8347);
    assert!(a
        .activity
        .iter()
        .find(|r| r.title == "Payment")
        .unwrap()
        .expense
        .is_none());
}

#[test]
fn a_waiting_schedule_change_is_shown() {
    use serde_json::json;
    let mut records = sk_view::demo::records(today());
    let ts = today()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let author = sk_chain::Author {
        member: Some(JERRY),
        device: "t".into(),
    };
    let r = sk_chain::next(
        records.last(),
        ts,
        author,
        "schedule",
        json!({
            "op": "change", "agreement": 1,
            "schedule": {"Weekly": {"start": today(), "weekday": "Fri", "every": 2}},
        }),
    )
    .unwrap();
    records.push(r);
    let (l, skipped) = sk_ledger::Ledger::replay(&records);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(
        view::schedule_text(&l, A, today()),
        "Every month, on the 1st. From Apr 1: every 2 weeks, on Friday"
    );
}
