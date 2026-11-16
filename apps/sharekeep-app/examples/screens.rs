//! Headless screenshots of every screen with Slint's software renderer, on the sample
//! household. `cargo run -p sharekeep-app --example screens -- <out_dir> [width] [height]`

use sharekeep_app::config::AppConfig;
use sharekeep_app::controller::{qr_image, State};
use sk_client::api::BookFacts;
use sk_core::{AgreementId, NaiveDate};
use sk_ui::*;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const SCALE: f32 = 2.0;

fn shot(win: &MinimalSoftwareWindow, w: u32, h: u32, path: &str) {
    win.set_size(slint::PhysicalSize::new(
        (w as f32 * SCALE) as u32,
        (h as f32 * SCALE) as u32,
    ));
    slint::platform::update_timers_and_animations();
    win.request_redraw();
    let (pw, ph) = ((w as f32 * SCALE) as usize, (h as f32 * SCALE) as usize);
    let mut buf = vec![PremultipliedRgbaColor::default(); pw * ph];
    win.draw_if_needed(|r| {
        r.render(&mut buf, pw);
    });
    let mut img = image::RgbaImage::new(pw as u32, ph as u32);
    for (i, px) in buf.iter().enumerate() {
        img.put_pixel(
            (i % pw) as u32,
            (i / pw) as u32,
            image::Rgba([px.red, px.green, px.blue, 255]),
        );
    }
    img.save(path).unwrap();
    println!("wrote {path}");
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out).unwrap();
    let w: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(430);
    let h: u32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(860);
    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(win.clone()))).unwrap();
    win.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    win.set_size(slint::PhysicalSize::new(
        (w as f32 * SCALE) as u32,
        (h as f32 * SCALE) as u32,
    ));
    let ui = AppWindow::new().unwrap();
    ui.show().unwrap();

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
    let mut st = State::new(std::env::temp_dir(), cfg, jobs, true);
    st.today = today;
    st.ledger = sk_ui::demo::ledger(today);
    st.hub_ledger = st.ledger.clone();
    st.agreement = Some(AgreementId(1));
    st.refresh(&ui);
    let p = |n: &str| format!("{out}/{n}.png");

    // joining a hub
    ui.set_page(0);
    shot(&win, w, h, &p("00-join"));
    ui.set_hub_address("192.168.1.20".into());
    ui.set_hub_fingerprint("3F2A 91C4 0B7E 55D1 A8F0 6C2B 19E4 D7A3".into());
    ui.set_device_name("Jerry's MacBook".into());
    ui.set_join_step(2);
    shot(&win, w, h, &p("01-join-step2"));
    ui.set_join_step(3);
    ui.set_connect_code("K7QM-2XRP".into());
    ui.set_connect_error(
        "The pairing code was wrong or has expired. Make a new one on the hub computer.".into(),
    );
    shot(&win, w, h, &p("01-join-step3"));
    ui.set_connect_error("".into());
    ui.set_join_step(1);

    // EZ Start on the hub computer, steps 1 to 5
    ui.set_page(6);
    let saved = st.hub_ledger.clone();
    st.hub_ledger = sk_ledger::Ledger::new();
    st.hub_pin_set = false;
    ui.set_hub_current_name("".into());
    st.refresh(&ui);
    shot(&win, w, h, &p("05-ez1-pin"));
    st.hub_pin_set = true;
    st.refresh(&ui);
    ui.set_new_book_name("Home".into());
    shot(&win, w, h, &p("05-ez2-book"));
    ui.set_hub_current_name("Home".into());
    st.hub_ledger = saved.clone();
    st.hub_ledger.agreements.clear();
    st.refresh(&ui);
    shot(&win, w, h, &p("05-ez3-people"));
    st.ez_next(&ui);
    st.hub_member_picked(&ui, 1);
    st.hub_member_picked(&ui, 2);
    ui.set_new_agreement_name("Apartment".into());
    st.refresh(&ui);
    shot(&win, w, h, &p("05-ez4-agreement"));
    st.hub_ledger = saved;
    st.refresh(&ui);
    ui.set_hub_code("K7QM-2XRP".into());
    ui.set_hub_code_detail("Works once, for 10 minutes.".into());
    ui.set_hub_qr(qr_image("http://192.168.1.20:15080/?pair=K7QM2XRP"));
    shot(&win, w, h, &p("05-ez5-pair"));
    ui.set_hub_code("".into());

    ui.set_page(1);
    shot(&win, w, h, &p("02-home"));
    st.pending = 2;
    st.problem = Some("offline".into());
    st.offline = true;
    st.refresh(&ui);
    shot(&win, w, h, &p("03-home-offline"));
    st.pending = 0;
    st.problem = None;
    st.offline = false;
    st.refresh(&ui);

    ui.set_page(2);
    for (i, tab) in ["activity", "members", "rules", "periods"]
        .iter()
        .enumerate()
    {
        ui.set_tab(i as i32);
        shot(&win, w, h, &p(&format!("1{i}-agreement-{tab}")));
    }
    ui.set_page(3);
    shot(&win, w, h, &p("20-approvals"));
    st.refused = vec!["expense: Sam left the agreement on Mar 1, so it can't include them.".into()];
    st.refresh(&ui);
    ui.set_page(4);
    shot(&win, w, h, &p("21-settings"));

    // sheets
    ui.set_page(2);
    ui.set_tab(0);
    st.open_expense(&ui);
    ui.set_exp_amount("83.47".into());
    ui.set_exp_category("Rent".into());
    ui.set_exp_note("Parking".into());
    st.refresh(&ui);
    shot(&win, w, h, &p("30-expense"));
    ui.set_exp_category("".into());
    st.refresh(&ui);
    shot(&win, w, h, &p("31-expense-error"));
    ui.set_exp_open(false);
    st.open_payment(&ui);
    shot(&win, w, h, &p("32-payment"));
    ui.set_pay_open(false);
    st.open_rule(&ui);
    ui.set_rule_name("Internet".into());
    st.rule_edit(&ui, |d| {
        d.set_method(1);
        d.set_value(0, 50.0);
        d.toggle_locked(0);
    });
    shot(&win, w, h, &p("33-rule-percent"));
    st.rule_edit(&ui, |d| {
        d.set_method(2);
        d.set_value(1, 3.0);
        d.toggle_included(2);
    });
    shot(&win, w, h, &p("34-rule-shares"));
    ui.set_rule_open(false);

    // the hub computer
    ui.set_page(5);
    ui.set_hub_info(
        "Hub at 192.168.1.20 · certificate 3F2A 91C4 0B7E 55D1 A8F0 6C2B 19E4 D7A3 · PIN set"
            .into(),
    );
    ui.set_hub_current_name("Home".into());
    ui.set_hub_current_detail("Hyperbook · encrypted · 42 records".into());
    ui.set_hub_pin_set(true);
    ui.set_hub_books(ModelRc::new(VecModel::from(vec![BookS {
        file: "cabin.skbook".into(),
        name: "Cabin".into(),
        detail: "encrypted".into(),
        encrypted: true,
    }])));
    ui.set_hub_devices(ModelRc::new(VecModel::from(vec![
        DeviceS {
            id: 1,
            name: "Sam's phone".into(),
            detail: "Sam".into(),
            revoked: false,
        },
        DeviceS {
            id: 2,
            name: "Old iPad".into(),
            detail: "Alex · removed".into(),
            revoked: true,
        },
    ])));
    ui.set_hub_code("K7QM-2XRP".into());
    ui.set_hub_code_detail(
        "Works once, for 10 minutes. Certificate page: http://192.168.1.20:15080/".into(),
    );
    ui.set_hub_qr(qr_image("https://192.168.1.20:15443/#pair=K7QM2XRP"));
    shot(&win, w, h, &p("40-hub"));
    ui.set_hub_code("".into());
    st.hub_member_picked(&ui, 1);
    st.hub_member_picked(&ui, 2);
    ui.set_hub_info("Home was created. Its recovery key is SKRK-ABCD-EFGH-IJKL-MNOP — write it down and keep it somewhere safe: it is shown only now, and it opens the book if the password is lost.".into());
    shot(&win, w, h * 2, &p("41-hub-long"));
    // nothing open: the new-book form is available, and pairing waits
    ui.set_hub_current_name("".into());
    ui.set_hub_info("".into());
    ui.set_hub_pin_set(false);
    ui.set_hub_pin("4829".into());
    ui.set_new_book_name("Cabin".into());
    shot(&win, w, h * 2, &p("42-hub-no-book"));
}
