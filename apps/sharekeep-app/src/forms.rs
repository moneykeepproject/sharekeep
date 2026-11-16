//! The form sheet: the agreement's other changes on the desktop, sending the same records
//! as the web app's sheets. One sheet, set up per kind of change: a deposit or pot, money
//! put into one, a held deposit paid back, bought out or kept back, a recurring bill, a
//! special event, how often the agreement settles, and someone joining or leaving.

use crate::controller::{model, s, toast, State};
use crate::worker::Job;
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};
use sk_core::MemberId;
use sk_ui::view::{self, AgreementView, DepositRow};
use sk_ui::{AppWindow, ChoiceS};
use std::collections::BTreeSet;

/// Which change the sheet makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    Deposit,
    PayIn(DepositRow),
    Move(DepositRow),
    Recurring,
    Event,
    Schedule,
    Join,
    Leave,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub mode: FormMode,
    /// The chosen option of the first chip row (a deposit's kind, how often, what happened).
    pub kind: usize,
    /// The second chip row (who has the deposit's money, a weekday).
    pub sub: usize,
    pub people: BTreeSet<MemberId>,
    pub person2: Option<MemberId>,
}

const WEEKDAYS: [(&str, &str); 7] = [
    ("Mon", "Monday"),
    ("Tue", "Tuesday"),
    ("Wed", "Wednesday"),
    ("Thu", "Thursday"),
    ("Fri", "Friday"),
    ("Sat", "Saturday"),
    ("Sun", "Sunday"),
];
const HOLDERS: [(&str, &str); 3] = [
    ("Landlord", "Landlord"),
    ("Utility", "Utility"),
    ("HouseholdPot", "Household"),
];

fn chips(labels: &[&str], on: usize) -> Vec<ChoiceS> {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| ChoiceS {
            id: i as i32,
            label: s(l),
            on: i == on,
        })
        .collect()
}

impl State {
    fn agreement_view(&self) -> Option<AgreementView> {
        let a = self.agreement?;
        view::snapshot(&self.ledger, self.me(), self.cur, self.today)
            .agreements
            .into_iter()
            .find(|v| v.id == a)
    }

    /// Open the sheet: "deposit", "pay-in:<row>", "move:<row>", "recurring", "event",
    /// "schedule", "join" or "leave".
    pub fn open_form(&mut self, ui: &AppWindow, what: &str) {
        let Some(v) = self.agreement_view() else {
            return;
        };
        let row = |i: &str| {
            i.parse::<usize>()
                .ok()
                .and_then(|i| v.deposits.get(i).cloned())
        };
        let mode = match what.split_once(':') {
            Some(("pay-in", i)) => FormMode::PayIn(match row(i) {
                Some(d) => d,
                None => return,
            }),
            Some(("move", i)) => FormMode::Move(match row(i) {
                Some(d) => d,
                None => return,
            }),
            _ => match what {
                "deposit" => FormMode::Deposit,
                "recurring" => FormMode::Recurring,
                "event" => FormMode::Event,
                "schedule" => FormMode::Schedule,
                "join" => FormMode::Join,
                "leave" => FormMode::Leave,
                _ => return,
            },
        };
        let me = self.me();
        let others: Vec<MemberId> = v.people.iter().map(|p| p.id).filter(|m| *m != me).collect();
        let (people, person2, kind, sub) = match &mode {
            FormMode::Event => (v.people.iter().map(|p| p.id).collect(), None, 0, 0),
            FormMode::Join => (
                v.can_join.first().map(|p| p.id).into_iter().collect(),
                None,
                0,
                0,
            ),
            FormMode::Move(_) => ([me].into_iter().collect(), others.first().copied(), 0, 0),
            FormMode::Schedule => (BTreeSet::new(), None, 2, 4),
            _ => ([me].into_iter().collect(), None, 0, 0),
        };
        self.form = Some(Form {
            mode,
            kind,
            sub,
            people,
            person2,
        });
        let today = self.today.to_string();
        ui.set_form_name(s(""));
        ui.set_form_amount(s(""));
        ui.set_form_text(s(match what {
            "recurring" => "Utilities".to_string(),
            "schedule" => "1".into(),
            _ => String::new(),
        }));
        ui.set_form_date(s(match what {
            "recurring" => self.today.day().to_string(),
            _ => today.clone(),
        }));
        ui.set_form_date2(s(&today));
        ui.set_form_error(s(""));
        ui.set_form_open(true);
        self.refresh_form(ui);
    }

    pub fn form_kind(&mut self, ui: &AppWindow, i: i32) {
        if let Some(f) = self.form.as_mut() {
            f.kind = i.max(0) as usize;
            if f.mode == FormMode::Deposit {
                // a pot is the household's; a held deposit is with the landlord
                f.sub = if f.kind == 1 { 2 } else { 0 };
            }
        }
        self.refresh_form(ui);
    }

    pub fn form_sub(&mut self, ui: &AppWindow, i: i32) {
        if let Some(f) = self.form.as_mut() {
            f.sub = i.max(0) as usize;
        }
        self.refresh_form(ui);
    }

    pub fn form_person(&mut self, ui: &AppWindow, id: i32) {
        if let Some(f) = self.form.as_mut() {
            let m = MemberId(id as u32);
            if f.mode == FormMode::Event {
                if !f.people.remove(&m) {
                    f.people.insert(m);
                }
            } else {
                f.people = [m].into_iter().collect();
            }
        }
        self.refresh_form(ui);
    }

    pub fn form_person2(&mut self, ui: &AppWindow, id: i32) {
        if let Some(f) = self.form.as_mut() {
            f.person2 = Some(MemberId(id as u32));
        }
        self.refresh_form(ui);
    }

    pub(crate) fn refresh_form(&self, ui: &AppWindow) {
        let Some(f) = &self.form else { return };
        if !ui.get_form_open() {
            return;
        }
        let Some(v) = self.agreement_view() else {
            return;
        };
        let people = |list: &[view::Person], on: &dyn Fn(MemberId) -> bool| {
            model(
                list.iter()
                    .map(|p| ChoiceS {
                        id: p.id.0 as i32,
                        label: s(&p.name),
                        on: on(p.id),
                    })
                    .collect(),
            )
        };
        // everything hidden, then what this kind of change shows
        let title: String;
        let mut hint = String::new();
        let mut save = "Save";
        let mut kinds: Option<(&str, Vec<ChoiceS>)> = None;
        let mut sub: Option<(&str, Vec<ChoiceS>)> = None;
        let (mut name, mut name_ph, mut amount, mut text, mut text_ph) = ("", "", "", "", "");
        let (mut date, mut date2, mut ppl, mut ppl2) = ("", "", "", "");
        let chosen = |m: MemberId| f.people.contains(&m);
        let second = |m: MemberId| f.person2 == Some(m);
        let mut list = &v.people;
        match &f.mode {
            FormMode::Deposit => {
                title = "Deposit or pot".into();
                name = "Name";
                name_ph = "Rental security";
                kinds = Some((
                    "Kind",
                    chips(&["Held (paid back later)", "Pot (pays bills)"], f.kind),
                ));
                sub = Some(("Who has the money", chips(&HOLDERS.map(|h| h.1), f.sub)));
                save = "Add";
            }
            FormMode::PayIn(d) => {
                title = format!("Put money into {}", d.label);
                amount = "Amount";
                hint = "What you paid in yourself. It counts as your part of it.".into();
            }
            FormMode::Move(d) => {
                title = d.label.clone();
                kinds = Some((
                    "What happened",
                    chips(&["Paid back", "Bought out", "Kept back"], f.kind),
                ));
                amount = "Amount";
                match f.kind {
                    1 => {
                        ppl = "Whose part";
                        ppl2 = "Bought by";
                    }
                    2 => {
                        ppl = "From whose part";
                        text = "Why (optional)";
                        text_ph = "Carpet cleaning";
                        hint = "Money the landlord or utility kept (damage, a final bill), taken from one person's part.".into();
                    }
                    _ => {
                        hint = "What the landlord or utility paid back: shared out by each person's part.".into();
                    }
                }
            }
            FormMode::Recurring => {
                title = "Recurring bill".into();
                name = "Name";
                name_ph = "Internet";
                amount = "About how much";
                text = "Category";
                date = "Day of the month it's due (1–31)";
                ppl = "Usually paid by";
                hint =
                    "Each month it shows under Due until someone records it with the real amount."
                        .into();
                save = "Add";
            }
            FormMode::Event => {
                title = "Special event".into();
                name = "Name";
                name_ph = "Beach weekend";
                date = "First day";
                date2 = "Last day";
                ppl = "Who is in it (tap to include or leave out)";
                save = "Add";
            }
            FormMode::Schedule => {
                title = "How often you settle up".into();
                kinds = Some((
                    "How often",
                    chips(
                        &["Weekly", "Every 2 weeks", "Monthly", "Every N days"],
                        f.kind,
                    ),
                ));
                match f.kind {
                    2 => text = "Day of the month (1–28, or 31 for the last day)",
                    3 => text = "Every how many days (1–365)",
                    _ => sub = Some(("On", chips(&WEEKDAYS.map(|w| w.1), f.sub))),
                }
                hint = "The change starts when the current period ends; the current period is never cut short. Everyone approves it first.".into();
            }
            FormMode::Join => {
                title = format!("Someone joins {}", v.name);
                list = &v.can_join;
                ppl = "Who";
                date = "Moving in on";
                hint = "Someone new to the household is added on the hub computer first.".into();
            }
            FormMode::Leave => {
                title = format!("Someone leaves {}", v.name);
                ppl = "Who";
                date = "Last day";
                hint = "Their share of bills stops after that day, and their settlement is worked out to the day once everyone approves.".into();
                save = "Propose";
            }
        }
        ui.set_form_title(s(title));
        ui.set_form_hint(s(hint));
        ui.set_form_save_text(s(save));
        let (kl, kc) = kinds.unwrap_or(("", Vec::new()));
        ui.set_form_kinds_label(s(kl));
        ui.set_form_kinds(model(kc));
        let (sl, sc) = sub.unwrap_or(("", Vec::new()));
        ui.set_form_sub_label(s(sl));
        ui.set_form_sub(model(sc));
        ui.set_form_name_label(s(name));
        ui.set_form_name_placeholder(s(name_ph));
        ui.set_form_amount_label(s(amount));
        ui.set_form_text_label(s(text));
        ui.set_form_text_placeholder(s(text_ph));
        ui.set_form_date_label(s(date));
        ui.set_form_date2_label(s(date2));
        ui.set_form_people_label(s(ppl));
        ui.set_form_people(people(list, &chosen));
        ui.set_form_people2_label(s(ppl2));
        ui.set_form_people2(people(&v.people, &second));
    }

    /// The record the sheet makes: (kind, body), or what's missing.
    pub fn form_record(&self, ui: &AppWindow) -> Result<(&'static str, Value), String> {
        let f = self.form.as_ref().ok_or("Nothing to save.")?;
        let v = self.agreement_view().ok_or("Open an agreement first.")?;
        let a = v.id;
        let name = ui.get_form_name().trim().to_string();
        let text = ui.get_form_text().trim().to_string();
        let amount = || {
            self.cur
                .parse(ui.get_form_amount().trim())
                .filter(|c| *c > 0)
                .ok_or_else(|| "Type the amount, like 200.00.".to_string())
        };
        let date = |t: slint::SharedString| {
            NaiveDate::parse_from_str(t.trim(), "%Y-%m-%d")
                .map_err(|_| "Type the date as YYYY-MM-DD.".to_string())
        };
        let one = || {
            f.people
                .iter()
                .next()
                .copied()
                .ok_or("Pick who.".to_string())
        };
        Ok(match &f.mode {
            FormMode::Deposit => {
                if name.is_empty() {
                    return Err("Name it, like Rental security.".into());
                }
                (
                    "deposit",
                    json!({"op": "create", "agreement": a, "id": v.next_deposit,
                           "kind": if f.kind == 1 { "Spending" } else { "Held" },
                           "label": name, "holder": HOLDERS[f.sub.min(2)].0, "refundable": "Yes"}),
                )
            }
            FormMode::PayIn(d) => (
                "deposit",
                json!({"op": "pay-in", "agreement": a, "id": d.id, "member": self.me(), "amount": amount()?}),
            ),
            FormMode::Move(d) => {
                let amount = amount()?;
                let from = one()?;
                match f.kind {
                    1 => {
                        let to = f.person2.ok_or("Pick who bought it.")?;
                        if to == from {
                            return Err("Pick two different people.".into());
                        }
                        (
                            "deposit",
                            json!({"op": "buy-out", "agreement": a, "id": d.id, "from": from, "to": to, "amount": amount}),
                        )
                    }
                    2 => (
                        "deposit",
                        json!({"op": "deduct", "agreement": a, "id": d.id,
                               "shares": {from.0.to_string(): amount},
                               "reason": if text.is_empty() { Value::Null } else { json!(text) }}),
                    ),
                    _ => (
                        "deposit",
                        json!({"op": "refund", "agreement": a, "id": d.id, "amount": amount}),
                    ),
                }
            }
            FormMode::Recurring => {
                if name.is_empty() {
                    return Err("Name it, like Internet.".into());
                }
                let amount = amount()?;
                let day: u32 = ui
                    .get_form_date()
                    .trim()
                    .parse()
                    .ok()
                    .filter(|d| (1..=31).contains(d))
                    .ok_or("The day is 1 to 31.")?;
                // a monthly schedule's start is its first due date: this month's
                let t = self.today;
                let last = NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
                    .and_then(|d| d.checked_add_months(chrono::Months::new(1)))
                    .and_then(|d| d.pred_opt())
                    .map_or(28, |d| d.day());
                let start = NaiveDate::from_ymd_opt(
                    t.year(),
                    t.month(),
                    if day >= 29 { last } else { day },
                )
                .ok_or("That day isn't in this month.")?;
                let due_day = if day >= 29 {
                    json!("Last")
                } else {
                    json!({"Day": day})
                };
                (
                    "recurring",
                    json!({"op": "add", "agreement": a, "id": v.next_recurring, "name": name,
                           "schedule": {"Monthly": {"start": start, "day": due_day, "every": 1}},
                           "what": {"type": "bill", "amount": amount,
                                    "category": if text.is_empty() { "Bills".to_string() } else { text },
                                    "payer": one()?}}),
                )
            }
            FormMode::Event => {
                if name.is_empty() {
                    return Err("Name it, like Beach weekend.".into());
                }
                let first = date(ui.get_form_date())?;
                let last = date(ui.get_form_date2())?;
                if last < first {
                    return Err("The last day is on or after the first.".into());
                }
                if f.people.is_empty() {
                    return Err("Pick who is in it.".into());
                }
                (
                    "schedule",
                    json!({"op": "event", "agreement": a, "id": v.next_event, "name": name,
                           "first": first, "last": last, "members": f.people}),
                )
            }
            FormMode::Schedule => {
                let start = self.today;
                let schedule = match f.kind {
                    2 => {
                        let d: u32 = text
                            .parse()
                            .ok()
                            .filter(|d| (1..=31).contains(d))
                            .ok_or("The day is 1 to 28, or 31 for the last day.")?;
                        let day = if d > 28 {
                            json!("Last")
                        } else {
                            json!({"Day": d})
                        };
                        json!({"Monthly": {"start": start, "day": day, "every": 1}})
                    }
                    3 => {
                        let n: u32 = text
                            .parse()
                            .ok()
                            .filter(|d| (1..=365).contains(d))
                            .ok_or("1 to 365 days.")?;
                        json!({"EveryNDays": {"start": start, "days": n}})
                    }
                    k => json!({"Weekly": {"start": start, "weekday": WEEKDAYS[f.sub.min(6)].0,
                                           "every": if k == 0 { 1 } else { 2 }}}),
                };
                (
                    "schedule",
                    json!({"op": "change", "agreement": a, "schedule": schedule}),
                )
            }
            FormMode::Join => (
                "member-join",
                json!({"agreement": a, "member": one()?, "date": date(ui.get_form_date())?}),
            ),
            FormMode::Leave => (
                "member-leave",
                json!({"agreement": a, "member": one()?, "date": date(ui.get_form_date())?}),
            ),
        })
    }

    pub fn form_save(&mut self, ui: &AppWindow) {
        if self.restore.is_some() {
            return toast(ui, "Accept the restored backup first (Home).");
        }
        match self.form_record(ui) {
            Ok((kind, body)) => {
                let leave = matches!(
                    self.form.as_ref().map(|f| &f.mode),
                    Some(FormMode::Leave | FormMode::Schedule)
                );
                ui.set_form_open(false);
                self.form = None;
                self.send(Job::Send {
                    kind: kind.into(),
                    body,
                });
                if leave {
                    toast(ui, "Proposed. It takes effect once everyone approves.");
                }
            }
            Err(e) => ui.set_form_error(s(e)),
        }
    }
}
