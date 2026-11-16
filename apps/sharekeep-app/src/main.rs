//! ShareKeep for Windows and macOS.
//!
//! `sharekeep` — the app. On the hub computer it also shows the Hub tab, which manages the
//! hub through its loopback port: found in the installer's place, or given once with
//! `--hub-data <folder>` (remembered).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sharekeep_app::config::{self, AppConfig};
use sharekeep_app::controller::{self, State};
use sharekeep_app::worker;
use sk_ui::AppWindow;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// How often the app syncs while it's open.
const SYNC_EVERY: Duration = Duration::from_secs(30);

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// The hub's admin port and token, when this computer runs the hub.
fn hub_admin(data: &Path) -> Option<(SocketAddr, String)> {
    let token = std::fs::read_to_string(data.join("admin.token")).ok()?;
    let port = std::fs::read(data.join("hub.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["admin_port"].as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(sk_client::ADMIN_PORT);
    Some((
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        token.trim().to_string(),
    ))
}

fn main() -> Result<(), slint::PlatformError> {
    let dir = arg("--data")
        .map(PathBuf::from)
        .unwrap_or_else(config::data_dir);
    let mut cfg = AppConfig::load(&dir);
    if let Some(h) = arg("--hub-data") {
        cfg.hub_data = Some(PathBuf::from(h));
        let _ = cfg.save(&dir);
    }
    // the hub installed on this computer is found without --hub-data
    let admin = cfg
        .hub_data
        .clone()
        .or_else(config::default_hub_data)
        .as_deref()
        .and_then(hub_admin);
    let is_hub = admin.is_some();

    let ui = AppWindow::new()?;
    let (jobs, rx) = std::sync::mpsc::channel();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("sharekeep-worker".into())
        .spawn(move || {
            worker::Worker::new(admin).run(rx, move |o| {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    STATE.with(|st| {
                        if let Some(st) = st.borrow_mut().as_mut() {
                            st.on_outcome(&ui, o);
                        }
                    });
                });
            })
        })
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;

    let paired = cfg.paired();
    let state = State::new(dir, cfg, jobs, is_hub);
    // the hub computer starts with EZ Start until it's finished or skipped
    ui.set_page(if is_hub && !state.cfg.ez_done {
        6
    } else if paired {
        1
    } else if is_hub {
        5
    } else {
        0
    });
    ui.set_device_name(state.cfg.device_name.clone().unwrap_or_default().into());
    if paired {
        state.connect();
    }
    state.hub_status();
    STATE.with(|s| *s.borrow_mut() = Some(state));
    wire(&ui);
    with(&ui, |st, ui| st.refresh(ui));

    let timer = slint::Timer::default();
    let w = ui.as_weak();
    timer.start(slint::TimerMode::Repeated, SYNC_EVERY, move || {
        if let Some(ui) = w.upgrade() {
            with(&ui, |st, ui| {
                st.today = controller::today();
                st.sync();
                st.refresh(ui);
            });
        }
    });
    let _keep = Rc::new(timer);
    ui.run()
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Run `f` with the state and the window, then refresh the screens.
fn with(ui: &AppWindow, f: impl FnOnce(&mut State, &AppWindow)) {
    STATE.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            f(st, ui);
        }
    });
}

/// Connect each callback to the state. Each closure holds a weak handle, so the window can
/// close.
fn wire(ui: &AppWindow) {
    macro_rules! on {
        ($cb:ident, |$st:ident, $u:ident $(, $a:ident : $t:ty)*| $body:expr) => {{
            let w = ui.as_weak();
            ui.$cb(move |$($a: $t),*| {
                if let Some(u) = w.upgrade() {
                    with(&u, |$st, $u| { $body; });
                }
            });
        }};
    }
    on!(on_check_hub, |st, u| st.find_hub(u));
    on!(on_pair, |st, u| st.pair(u));
    on!(on_open_agreement, |st, u, id: i32| {
        st.agreement = Some(sk_core::AgreementId(id as u32));
        u.set_tab(0);
        u.set_page(2);
        st.refresh(u)
    });
    on!(on_sync_now, |st, u| {
        u.set_busy(true);
        st.sync()
    });
    on!(on_add_expense, |st, u| st.open_expense(u));
    on!(on_expense_changed, |st, u| st.refresh(u));
    on!(on_payer_picked, |st, u, id: i32| st.payer_picked(u, id));
    on!(
        on_exp_need_edited,
        |st, u, i: i32, t: slint::SharedString| st.need_edited(u, i, &t)
    );
    on!(on_save_expense, |st, u| st.save_expense(u));
    on!(on_add_payment, |st, u| st.open_payment(u));
    on!(on_pay_from_picked, |st, u, id: i32| st
        .pay_picked(u, true, id));
    on!(on_pay_to_picked, |st, u, id: i32| st
        .pay_picked(u, false, id));
    on!(on_save_payment, |st, u| st.save_payment(u));
    on!(on_new_rule, |st, u| st.open_rule(u));
    on!(on_rule_method_picked, |st, u, m: i32| st
        .rule_edit(u, |d| d.set_method(m as usize)));
    on!(on_rule_weight, |st, u, i: i32, v: f32| st
        .rule_edit(u, |d| d.set_value(i as usize, v)));
    on!(on_rule_toggle, |st, u, i: i32| st
        .rule_edit(u, |d| d.toggle_included(i as usize)));
    on!(on_rule_lock, |st, u, i: i32| st
        .rule_edit(u, |d| d.toggle_locked(i as usize)));
    on!(on_rule_changed, |st, u| st.rule_edit(u, |_| {}));
    on!(on_propose_rule, |st, u| st.propose_rule(u));
    on!(on_approve, |st, _u, seq: i32, yes: bool| st
        .approve(seq, yes));
    on!(on_close_period, |st, _u, key: slint::SharedString| st
        .close_period(&key));
    on!(on_statement, |st, u, key: slint::SharedString| st
        .statement(u, key.to_string()));
    on!(on_delete_entry, |st, _u, seq: i32| st.delete_entry(seq));
    on!(on_edit_entry, |st, u, seq: i32| st.edit_entry(u, seq));
    on!(on_dispute_entry, |st, _u, seq: i32| st.dispute_entry(seq));
    on!(on_dismiss_refused, |st, _u| st.dismiss_refused());
    on!(on_record_due, |st, u, i: i32| st.record_due(u, i));
    on!(on_paid_due, |st, _u, i: i32| st.paid_due(i));
    on!(on_stop_recurring, |st, _u, id: i32| st.stop_recurring(id));
    on!(on_hub_set_pin, |st, u| st.hub_set_pin(u));
    on!(on_hub_new_book, |st, u| st.hub_new_book(u));
    on!(on_hub_unlock, |st, u, file: slint::SharedString| st
        .hub_unlock(u, &file));
    on!(on_hub_add_member, |st, u| st.hub_add_member(u));
    on!(on_hub_close_book, |st, u| st.hub_close_book(u));
    on!(on_ez_back, |st, u| st.ez_back(u));
    on!(on_ez_next, |st, u| st.ez_next(u));
    on!(on_ez_finish, |st, u| st.ez_finish(u));
    on!(on_ez_open, |st, u| {
        u.set_page(6);
        st.refresh(u)
    });
    on!(on_hub_member_picked, |st, u, id: i32| st
        .hub_member_picked(u, id));
    on!(on_hub_new_agreement, |st, u| st.hub_new_agreement(u));
    on!(on_hub_make_code, |st, u, id: i32| st.hub_make_code(u, id));
    on!(on_hub_remove_device, |st, _u, id: i32| st
        .hub_remove_device(id));
    on!(on_save_checkpoint, |st, u| st.save_checkpoint(u));
    on!(on_open_form, |st, u, what: slint::SharedString| st
        .open_form(u, &what));
    on!(on_form_kind, |st, u, i: i32| st.form_kind(u, i));
    on!(on_form_sub_picked, |st, u, i: i32| st.form_sub(u, i));
    on!(on_form_person, |st, u, i: i32| st.form_person(u, i));
    on!(on_form_person2, |st, u, i: i32| st.form_person2(u, i));
    on!(on_form_save, |st, u| st.form_save(u));
    on!(on_exp_item_add, |st, u| st.item_add(u));
    on!(
        on_exp_item_amount,
        |st, u, i: i32, t: slint::SharedString| st.item_amount(u, i, &t)
    );
    on!(on_exp_item_member, |st, u, i: i32, m: i32| st
        .item_member(u, i, m));
    on!(on_exp_item_remove, |st, u, i: i32| st.item_remove(u, i));
    on!(on_rule_amount, |st, u, i: i32, t: slint::SharedString| st
        .rule_amount(u, i, &t));
    on!(on_rule_remainder, |st, u, i: i32| st
        .rule_edit(u, |d| d.set_remainder(i as usize)));
    on!(on_save_checkpoint_file, |st, u| st.save_checkpoint_file(u));
    on!(on_check_checkpoint, |st, u| st.check_checkpoint(u));
    on!(on_check_statement, |st, u| st.check_statement(u));
    on!(on_verify, |st, u| st.verify(u));
    on!(on_accept_restore, |st, _u| st.accept_restore());
    on!(on_export_csv, |st, u, key: slint::SharedString| st
        .export_csv(u, &key));
    on!(on_hub_recover, |st, u, file: slint::SharedString| st
        .hub_recover(u, &file));
    on!(on_hub_recover_pick, |st, u, b: slint::SharedString| st
        .hub_recover_pick(u, &b));
    on!(on_hub_restore, |st, u| st.hub_restore(u));
    on!(on_hub_recover_cancel, |st, u| st.hub_recover_cancel(u));
    on!(on_hub_export_broken, |st, u| st.hub_export_broken(u));
}
