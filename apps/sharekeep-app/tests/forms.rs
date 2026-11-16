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
    slint::platform::set_platform(Box::new(Headless(win))).unwrap();
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
