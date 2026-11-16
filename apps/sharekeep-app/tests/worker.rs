//! The desktop app's worker against a real hub on the loopback address: the hub computer's
//! admin calls, pairing, sync, sending, and going offline.

use serde_json::json;
use sharekeep_app::worker::{admin_call, parse_ip, AdminThen, Job, Outcome, Synced, Worker};
use sk_client::HubAddress;
use sk_core::{AgreementId, MemberId};
use sk_hub::{pin, HubConfig, HubOptions, KeyStore};
use sk_ledger::Ledger;
use sk_store::crypt::KdfParams;
use std::net::Ipv4Addr;
use std::sync::Arc;

const PIN: &str = "482916";

fn synced(o: Option<Outcome>) -> Synced {
    match o {
        Some(Outcome::Synced(s)) => s,
        other => panic!("expected a sync, got {other:?}"),
    }
}

#[test]
fn a_device_pairs_syncs_sends_and_queues_while_offline() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("hub");
    let mut cfg = HubConfig::new(Ipv4Addr::LOCALHOST, None);
    cfg.https_port = 0;
    cfg.helper_port = 0;
    cfg.admin_port = 0;
    let keys = KeyStore::files(data.clone());
    let now = sk_hub::util::now_ms();
    sk_hub::setup(&data, &cfg, &keys, "test-pc", now).unwrap();
    let running = sk_hub::start(HubOptions {
        data: data.clone(),
        keys,
        clock: Arc::new(sk_hub::util::now_ms),
        log: Arc::new(|_| {}),
        pin_cost: pin::Cost { m: 64, t: 1, p: 1 },
        kdf: KdfParams::TEST,
    })
    .unwrap();
    let token = sk_hub::admin_token(&data).unwrap();
    let admin =
        |m: &str, p: &str, b: Option<serde_json::Value>| admin_call(running.admin, &token, m, p, b);

    // the hub computer: a PIN, a book, two people, an agreement, a rule
    assert!(admin_call(running.admin, "wrong", "GET", "/admin/v1/status", None).is_err());
    admin("POST", "/admin/v1/pin", Some(json!({"pin": PIN}))).unwrap();
    let book = admin(
        "POST",
        "/admin/v1/books",
        Some(json!({"name": "Home", "hyperbook": true})),
    )
    .unwrap();
    let id = book["book"]["id"].as_str().unwrap().to_string();
    let today = chrono::Utc::now().date_naive();
    let records = format!("/admin/v1/books/{id}/records");
    let mut n = 0;
    let mut rec = |kind: &str, member: Option<u32>, body: serde_json::Value| {
        n += 1;
        admin(
            "POST",
            &records,
            Some(
                json!({"client_id": format!("c{n}"), "kind": kind, "body": body, "member": member}),
            ),
        )
        .unwrap_or_else(|e| panic!("{kind}: {e}"));
    };
    rec("member-add", None, json!({"id": 1, "name": "Jerry"}));
    rec("member-add", None, json!({"id": 2, "name": "Sam"}));
    rec(
        "agreement-add",
        None,
        json!({"id": 1, "name": "Apartment", "members": {"1": today, "2": today},
               "schedule": {"Monthly": {"start": today, "day": {"Day": 1}, "every": 1}}}),
    );
    admin(
        "POST",
        &format!("/admin/v1/books/{id}/settings"),
        Some(json!({"key": "one_keeper", "value": 1})),
    )
    .unwrap();
    rec(
        "rule-proposal",
        Some(1),
        json!({"agreement": 1, "rule_id": 1, "effective": today,
               "rule": {"name": "Shared", "method": "Equal", "members": [1, 2]}}),
    );
    let status = admin("GET", "/admin/v1/status", None).unwrap();
    assert_eq!(status["books"][0]["name"], "Home");

    // the worker's admin path is the same call
    let mut w = Worker::new(Some((running.admin, token.clone())));
    match w.handle(Job::Admin {
        method: "POST",
        path: "/admin/v1/pairing".into(),
        body: Some(json!({"book_id": id, "member": 2})),
        then: AdminThen::Code,
    }) {
        Some(Outcome::Admin(AdminThen::Code, Ok(v))) => {
            let code = v["code"].as_str().unwrap().replace('-', "");
            let hub = HubAddress::new(running.https, "127.0.0.1", &running.ca_der);
            // a wrong PIN is refused with a clear message
            let wrong = w.handle(Job::Pair {
                hub: hub.clone(),
                code: code.clone(),
                pin: "000000".into(),
                device: "Sam's laptop".into(),
            });
            assert!(matches!(wrong, Some(Outcome::Paired(Err(_)))));
            // a new code, since the wrong PIN used up a try
            let v = admin(
                "POST",
                "/admin/v1/pairing",
                Some(json!({"book_id": id, "member": 2})),
            )
            .unwrap();
            let code = v["code"].as_str().unwrap().to_string();
            let paired = w.handle(Job::Pair {
                hub,
                code,
                pin: PIN.into(),
                device: "Sam's laptop".into(),
            });
            let Some(Outcome::Paired(Ok((client, answer)))) = paired else {
                panic!("pairing failed: {paired:?}")
            };
            assert_eq!(answer.member, 2);
            assert_eq!(answer.book.name, "Home");
            let s = synced(w.handle(Job::Connect {
                client,
                book_dir: dir.path().join("device"),
            }));
            assert!(s.problem.is_none(), "{:?}", s.problem);
            let l = Ledger::replay(&s.records.unwrap()).0;
            assert_eq!(l.agreements[&AgreementId(1)].name, "Apartment");
        }
        other => panic!("{other:?}"),
    }

    // an expense goes out through the outbox and comes back in the history
    let s = synced(w.handle(Job::Send {
        kind: "expense".into(),
        body: json!({"agreement": 1, "date": today, "amount": 9000, "payer": {"member": 2}, "category": "Groceries"}),
    }));
    assert_eq!(s.pending, 0);
    assert!(s.refused.is_empty(), "{:?}", s.refused);
    let l = Ledger::replay(&s.records.unwrap()).0;
    assert_eq!(l.balances(AgreementId(1)).get(MemberId(2)), 4500);
    let seq = *l.entries.keys().next().unwrap();

    // one the hub refuses is kept for the person to see, not resent
    let s = synced(w.handle(Job::Send {
        kind: "expense".into(),
        body: json!({"agreement": 9, "date": today, "amount": 100, "payer": {"member": 2}, "category": "X"}),
    }));
    assert_eq!(s.refused.len(), 1);
    let s = synced(w.handle(Job::DismissRefused));
    assert!(s.refused.is_empty());

    // an online-only change goes now
    let s = synced(w.handle(Job::Send {
        kind: "dispute".into(),
        body: json!({"target": seq, "note": "check the receipt"}),
    }));
    let l = Ledger::replay(&s.records.unwrap()).0;
    assert!(l.entries[&seq].disputed);

    // a statement for a period that isn't closed: a clear answer, no file
    let out = dir.path().join("s.pdf");
    match w.handle(Job::Statement {
        agreement: 1,
        first: today.to_string(),
        path: out.clone(),
    }) {
        Some(Outcome::Saved(Err(_))) => assert!(!out.exists()),
        other => panic!("{other:?}"),
    }

    // the hub goes away: new things wait, online-only changes say why they can't
    running.stop();
    let s = synced(w.handle(Job::Send {
        kind: "payment".into(),
        body: json!({"agreement": 1, "date": today, "from": 1, "to": 2, "amount": 4500}),
    }));
    assert_eq!(s.pending, 1);
    assert!(s.offline);
    assert!(s.problem.is_some());
    match w.handle(Job::Send {
        kind: "entry-delete".into(),
        body: json!({"target": seq}),
    }) {
        Some(Outcome::Sent(Err(e))) => assert!(e.contains("needs the hub"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn addresses_are_read_forgivingly() {
    assert_eq!(
        parse_ip(" 192.168.1.20 ").unwrap().to_string(),
        "192.168.1.20"
    );
    assert_eq!(
        parse_ip("192.168.1.20:15443").unwrap().to_string(),
        "192.168.1.20"
    );
    assert!(parse_ip("my hub").is_err());
    // nothing answers here: a clear message rather than a hang
    let mut w = Worker::new(None);
    match w.handle(Job::FindHub("127.0.0.1".into())) {
        Some(Outcome::Found(Err(e))) => assert!(e.contains("No ShareKeep hub answered"), "{e}"),
        other => panic!("{other:?}"),
    }
    match w.handle(Job::Admin {
        method: "GET",
        path: "/admin/v1/status".into(),
        body: None,
        then: AdminThen::Status,
    }) {
        Some(Outcome::Admin(_, Err(e))) => assert!(e.contains("isn't the hub computer")),
        other => panic!("{other:?}"),
    }
}
