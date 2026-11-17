//! The desktop's form sheet makes records the book accepts: each kind of change, filled in
//! as a person would, checked by the ledger against the sample household.

use sharekeep_app::config::AppConfig;
use sharekeep_app::controller::State;
use sk_client::api::BookFacts;
use sk_core::{AgreementId, NaiveDate};
use sk_ui::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use std::rc::Rc;

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn every_form_makes_a_record_the_book_accepts() {
    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(Headless(win)));
    let ui = AppWindow::new().unwrap();
    let today = NaiveDate::from_ymd_opt(2027, 3, 14).unwrap();
    let (jobs, _rx) = std::sync::mpsc::channel();
    let cfg = AppConfig {
        member: Some(1),
        book: Some(BookFacts {
            id: "demo".into(),
            name: "Home".into(),
            currency: "USD".into(),
            hyperbook: true,
        }),
        ..Default::default()
    };
    let mut st = State::new(std::env::temp_dir(), cfg, jobs, false);
    st.today = today;
    let records = sk_ui::demo::records(today);
    st.ledger = sk_ledger::Ledger::replay(&records).0;
    st.agreement = Some(AgreementId(1));
    st.refresh(&ui);

    let check = |st: &State, ui: &AppWindow| -> String {
        let (kind, body) = st.form_record(ui).unwrap_or_else(|e| panic!("{e}"));
        let ts = today
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let author = sk_chain::Author {
            member: Some(sk_core::MemberId(1)),
            device: "t".into(),
        };
        let r = sk_chain::next(records.last(), ts, author, kind, body.clone()).unwrap();
        st.ledger
            .check(&r)
            .unwrap_or_else(|e| panic!("{kind} {body}: {}", e.0));
        kind.to_string()
    };

    st.open_form(&ui, "deposit");
    assert_eq!(ui.get_form_title(), "Deposit or pot");
    assert!(st.form_record(&ui).is_err(), "a deposit needs a name");
    ui.set_form_name("Utility deposit".into());
    st.form_kind(&ui, 0);
    st.form_sub(&ui, 1);
    assert_eq!(check(&st, &ui), "deposit");

    st.open_form(&ui, "recurring");
    ui.set_form_name("Internet".into());
    ui.set_form_amount("70.00".into());
    ui.set_form_date("31".into());
    assert_eq!(check(&st, &ui), "recurring");

    st.open_form(&ui, "event");
    ui.set_form_name("Beach weekend".into());
    ui.set_form_date("2027-03-20".into());
    ui.set_form_date2("2027-03-19".into());
    assert!(
        st.form_record(&ui).is_err(),
        "the last day is after the first"
    );
    ui.set_form_date2("2027-03-22".into());
    st.form_person(&ui, 3); // leave one person out
    assert_eq!(check(&st, &ui), "schedule");

    st.open_form(&ui, "schedule");
    st.form_kind(&ui, 1);
    st.form_sub(&ui, 4);
    assert_eq!(check(&st, &ui), "schedule");
    st.form_kind(&ui, 3);
    ui.set_form_text("14".into());
    assert_eq!(check(&st, &ui), "schedule");

    st.open_form(&ui, "leave");
    st.form_person(&ui, 3);
    ui.set_form_date("2027-03-31".into());
    assert_eq!(check(&st, &ui), "member-leave");

    // the sample household's deposits: money in, paid back, bought out, kept back
    let deposits = view::deposits(&st.ledger, AgreementId(1), sk_core::MemberId(1), st.cur);
    assert!(!deposits.is_empty(), "the sample household has a deposit");
    let held = deposits.iter().position(|d| d.kind == "Held").unwrap();
    st.open_form(&ui, &format!("pay-in:{held}"));
    ui.set_form_amount("100.00".into());
    assert_eq!(check(&st, &ui), "deposit");
    st.open_form(&ui, &format!("move:{held}"));
    ui.set_form_amount("10.00".into());
    for what in 0..3 {
        st.form_kind(&ui, what);
        if what == 1 {
            st.form_person(&ui, 1);
            st.form_person2(&ui, 1);
            assert!(
                st.form_record(&ui).is_err(),
                "a buy-out is between two people"
            );
            st.form_person2(&ui, 2);
        }
        assert_eq!(check(&st, &ui), "deposit");
    }
}

#[test]
fn ez_start_goes_back_to_any_step_and_on_again() {
    // the headless platform is set once per test binary
    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(Headless(win)));
    let ui = AppWindow::new().unwrap();
    let (jobs, _rx) = std::sync::mpsc::channel();
    let mut st = State::new(std::env::temp_dir(), AppConfig::default(), jobs, true);
    // where Jerry was: the PIN is set, Home is open, people are being added
    st.hub_pin_set = true;
    ui.set_hub_current_name("Home".into());
    let mut l = sk_ledger::Ledger::new();
    l.members.insert(sk_core::MemberId(1), "Jerry".into());
    st.hub_ledger = l;
    st.refresh(&ui);
    assert_eq!(ui.get_ez_step(), 3);
    st.ez_back(&ui);
    assert_eq!(ui.get_ez_step(), 2, "back to the book");
    st.ez_back(&ui);
    assert_eq!(ui.get_ez_step(), 1, "back to the PIN");
    st.ez_back(&ui);
    assert_eq!(ui.get_ez_step(), 1, "step 1 is the first");
    st.ez_next(&ui);
    assert_eq!(ui.get_ez_step(), 2, "keep the PIN");
    st.ez_next(&ui);
    assert_eq!(ui.get_ez_step(), 3, "keep the book");
    // Next can't go past where setup stands: there's no agreement yet
    st.ez_next(&ui);
    assert_eq!(ui.get_ez_step(), 4);
    st.ez_next(&ui);
    assert_eq!(
        ui.get_ez_step(),
        4,
        "the agreement step waits for an agreement"
    );
    st.ez_back(&ui);
    assert_eq!(ui.get_ez_step(), 3);
}

#[test]
fn a_new_agreements_schedule_from_the_options() {
    use sharekeep_app::controller::agreement_schedule as sched;
    // Wednesday 2026-11-18
    let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
    let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    // monthly on the 1st: the period started on Nov 1
    let (v, start) = sched(2, "1", 0, "", today).unwrap();
    assert_eq!(start, d("2026-11-01"));
    assert_eq!(v["Monthly"]["day"]["Day"], 1);
    // on the 25th: last month's 25th
    assert_eq!(sched(2, "25", 0, "", today).unwrap().1, d("2026-10-25"));
    // 31: the last day of the month
    let (v, start) = sched(2, "31", 0, "", today).unwrap();
    assert_eq!(v["Monthly"]["day"], "Last");
    assert_eq!(start, d("2026-10-31"));
    // weekly on Friday: last Friday; every 2 weeks on Wednesday: today
    let (v, start) = sched(0, "", 4, "", today).unwrap();
    assert_eq!(
        (v["Weekly"]["weekday"].as_str(), start),
        (Some("Fri"), d("2026-11-13"))
    );
    let (v, start) = sched(1, "", 2, "", today).unwrap();
    assert_eq!((v["Weekly"]["every"].as_u64(), start), (Some(2), today));
    // every 10 days from a typed start
    let (v, start) = sched(3, "10", 0, "2026-11-05", today).unwrap();
    assert_eq!(
        (v["EveryNDays"]["days"].as_u64(), start),
        (Some(10), d("2026-11-05"))
    );
    assert!(sched(3, "0", 0, "", today).is_err());
    assert!(sched(2, "40", 0, "", today).is_err());
    assert!(sched(2, "1", 0, "next week", today).is_err());
    // every schedule made here is one the engine accepts
    for (how, day) in [(0, ""), (1, ""), (2, "1"), (2, "31"), (3, "14")] {
        let (v, _) = sched(how, day, 3, "", today).unwrap();
        let s: sk_split::periods::Schedule = serde_json::from_value(v).unwrap();
        s.validate().unwrap();
        assert!(s.period_containing(today).is_some());
    }
}

#[test]
fn exact_amounts_stay_as_typed() {
    use slint::Model;
    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(Headless(win)));
    let ui = AppWindow::new().unwrap();
    let today = NaiveDate::from_ymd_opt(2027, 3, 14).unwrap();
    let (jobs, _rx) = std::sync::mpsc::channel();
    let cfg = AppConfig {
        member: Some(1),
        book: Some(BookFacts {
            id: "demo".into(),
            name: "Home".into(),
            currency: "USD".into(),
            hyperbook: false,
        }),
        ..Default::default()
    };
    let mut st = State::new(std::env::temp_dir(), cfg, jobs, false);
    st.today = today;
    st.ledger = sk_ui::demo::ledger(today);
    st.agreement = Some(AgreementId(1));
    st.refresh(&ui);
    st.open_rule(&ui);
    st.rule_edit(&ui, |d| d.set_method(view::EXACT));
    let shown = |ui: &AppWindow, i: usize| {
        ui.get_rule_weights()
            .row_data(i)
            .unwrap()
            .amount
            .to_string()
    };
    // typed one key at a time, as a person does: every step stays on screen
    let mut typed = String::new();
    for k in "800.50".chars() {
        typed.push(k);
        st.rule_amount(&ui, 0, &typed);
        assert_eq!(shown(&ui, 0), typed, "after typing {typed:?}");
    }
    // and the rule uses it: $800.50 fixed for the first person
    let first = ui.get_rule_examples().row_data(0).unwrap();
    assert_eq!(first.rows.row_data(0).unwrap().amount, "$800.50");
    // the amount stays when someone else is given what's left, and after a sync's refresh
    st.rule_edit(&ui, |d| d.set_remainder(1));
    st.refresh(&ui);
    assert_eq!(shown(&ui, 0), "800.50");
    // something that isn't an amount stays on screen too, and counts as none
    st.rule_amount(&ui, 0, "8x");
    assert_eq!(shown(&ui, 0), "8x");
}
