//! The household service end to end, over real TLS on the loopback address: setup,
//! pairing (code + PIN), records, the outbox, sync with chain checks, revoking, restarts,
//! locked books, and a rewritten history caught by a device.

use serde_json::{json, Value};
use sk_client::{CallError, Client, HubAddress, Outbox, Replica, SyncError};
use sk_hub::{pin, wifi, HubConfig, HubOptions, KeyStore, Running};
use sk_store::crypt::KdfParams;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PIN: &str = "482916";

struct Hub {
    data: PathBuf,
    running: Option<Running>,
    clock: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn options(data: &Path, clock: &Arc<AtomicI64>) -> HubOptions {
    let c = clock.clone();
    HubOptions {
        data: data.to_path_buf(),
        keys: KeyStore::files(data.to_path_buf()),
        clock: Arc::new(move || c.load(Ordering::SeqCst)),
        log: Arc::new(|_| {}),
        pin_cost: pin::Cost { m: 64, t: 1, p: 1 },
        kdf: KdfParams::TEST,
    }
}

fn hub() -> Hub {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("hub");
    let mut cfg = HubConfig::new(Ipv4Addr::LOCALHOST, None);
    cfg.https_port = 0;
    cfg.helper_port = 0;
    cfg.admin_port = 0;
    let clock = Arc::new(AtomicI64::new(1_794_700_000_000));
    let keys = KeyStore::files(data.clone());
    sk_hub::setup(&data, &cfg, &keys, "test-pc", clock.load(Ordering::SeqCst)).unwrap();
    let running = sk_hub::start(options(&data, &clock)).unwrap();
    Hub {
        data,
        running: Some(running),
        clock,
        _dir: dir,
    }
}

impl Hub {
    fn r(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn restart(&mut self) {
        self.running.take().unwrap().stop();
        self.running = Some(sk_hub::start(options(&self.data, &self.clock)).unwrap());
    }

    fn address(&self) -> HubAddress {
        HubAddress::new(self.r().https, "127.0.0.1", &self.r().ca_der)
    }

    /// A call to the loopback port the hub computer's desktop app uses.
    fn admin(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let token = sk_hub::admin_token(&self.data).unwrap();
        http(self.r().admin, method, path, Some(&token), body)
    }

    fn advance_minutes(&self, m: i64) {
        self.clock.fetch_add(m * 60_000, Ordering::SeqCst);
    }
}

fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, bytes) = raw_http(addr, method, path, token, body, "test");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn raw_http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    user_agent: &str,
) -> (u16, Vec<u8>) {
    let mut tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let body = body
        .map(|b| serde_json::to_vec(&b).unwrap())
        .unwrap_or_default();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: {user_agent}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    req.push_str("\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.write_all(&body).unwrap();
    let mut answer = Vec::new();
    let _ = tcp.read_to_end(&mut answer);
    sk_client::tls::parse_answer(&answer).unwrap()
}

/// A household book with two members, and the hub's PIN set.
fn household(h: &Hub, hyperbook: bool, password: Option<&str>) -> (String, String) {
    assert_eq!(
        h.admin("POST", "/admin/v1/pin", Some(json!({"pin": PIN})))
            .0,
        200
    );
    let (s, b) = h.admin(
        "POST",
        "/admin/v1/books",
        Some(json!({"name": "Home", "hyperbook": hyperbook, "password": password, "recovery_key": password.is_some()})),
    );
    assert_eq!(s, 201, "{b}");
    let id = b["book"]["id"].as_str().unwrap().to_string();
    let file = b["file"].as_str().unwrap().to_string();
    for (n, name) in [(1, "Jerry"), (2, "Sam")] {
        let (s, _) = h.admin(
            "POST",
            &format!("/admin/v1/books/{id}/records"),
            Some(json!({"client_id": format!("m{n}"), "kind": "member-add", "body": {"id": n, "name": name}})),
        );
        assert_eq!(s, 201);
    }
    (id, file)
}

fn pairing_code(h: &Hub, book: &str, member: u32) -> String {
    let (s, b) = h.admin(
        "POST",
        "/admin/v1/pairing",
        Some(json!({"book_id": book, "member": member})),
    );
    assert_eq!(s, 201, "{b}");
    assert!(b["url"].as_str().unwrap().contains("#pair="));
    b["code"].as_str().unwrap().to_string()
}

fn status(e: &CallError) -> u16 {
    match e {
        CallError::Http { status, .. } => *status,
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}

#[test]
fn a_device_pairs_queues_offline_work_and_syncs_a_verified_history() {
    let h = hub();
    let (book, _) = household(&h, true, None);
    let code = pairing_code(&h, &book, 2);

    // the wrong PIN is refused; the code survives a few tries
    let e = Client::pair(h.address(), &code, "000000", "Sam's phone").unwrap_err();
    assert_eq!(status(&e), 403);
    let (sam, answer) =
        Client::pair(h.address(), &code.to_lowercase(), PIN, "Sam's phone").unwrap();
    assert_eq!(answer.member, 2);
    assert_eq!(answer.book.name, "Home");
    assert!(answer.book.hyperbook);
    // a code works once
    let e = Client::pair(h.address(), &code, PIN, "Someone else").unwrap_err();
    assert_eq!(status(&e), 403);

    let me = sam.me().unwrap();
    assert_eq!((me.member, me.device.as_str()), (2, "Sam's phone"));

    // made offline, sent later, in order
    let dir = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::open(&dir.path().join("outbox.json")).unwrap();
    outbox
        .push(
            "expense",
            json!({"amount": 8347, "category": "Groceries"}),
            1,
        )
        .unwrap();
    outbox
        .push("payment", json!({"to": 1, "amount": 2000}), 2)
        .unwrap();
    let report = outbox.flush(&sam);
    assert_eq!(report.sent.len(), 2);
    assert!(report.stopped.is_none());
    assert_eq!(report.remaining, 0);
    // the hub stamps who sent it, whatever the body says
    assert_eq!(report.sent[0].author.member, Some(sk_core::MemberId(2)));
    assert_eq!(report.sent[0].author.device, "Sam's phone");
    assert_eq!(report.sent[0].kind, "expense");

    // a resend (a crash after sending, before saving) adds nothing
    let again = sam
        .submit("same-id", "expense", &json!({"amount": 1}))
        .unwrap();
    let dup = sam
        .submit("same-id", "expense", &json!({"amount": 1}))
        .unwrap();
    assert_eq!(again, dup);

    // devices can't add what only the hub computer adds
    let e = sam.submit_now("member-add", &json!({"id": 9})).unwrap_err();
    assert_eq!(status(&e), 403);
    assert_eq!(
        status(
            &sam.submit_now("expense", &json!("not an object"))
                .unwrap_err()
        ),
        400
    );

    // the device's copy matches the hub's history, record for record
    let mut replica = Replica::open(&dir.path().join("home")).unwrap();
    assert_eq!(replica.sync(&sam, 100).unwrap(), 6);
    let (_, hub_records) = h.admin("GET", &format!("/admin/v1/books/{book}/records"), None);
    assert_eq!(
        replica.head().unwrap().hash.to_hex(),
        hub_records["head"]["hash"].as_str().unwrap()
    );
    assert_eq!(replica.last_synced(), Some(100));
    assert_eq!(replica.sync(&sam, 200).unwrap(), 0);
    drop(replica);
    assert_eq!(
        Replica::open(&dir.path().join("home"))
            .unwrap()
            .records()
            .len(),
        6
    );

    // removed devices are refused at once
    let (s, devices) = h.admin("GET", "/admin/v1/devices", None);
    assert_eq!(s, 200);
    let id = devices["devices"][0]["id"].as_u64().unwrap();
    assert_eq!(
        h.admin("POST", &format!("/admin/v1/devices/{id}/revoke"), None)
            .0,
        200
    );
    assert_eq!(status(&sam.me().unwrap_err()), 401);
}

#[test]
fn only_the_hubs_own_certificate_authority_is_trusted() {
    let h = hub();
    // a client trusting some other authority can't connect
    let other = sk_hub::wifi::new_ca(
        "Someone else",
        &wifi::Target {
            ip: Ipv4Addr::LOCALHOST,
            name: None,
        },
        1_794_700_000,
    )
    .unwrap()
    .0;
    let c = Client::new(HubAddress::new(h.r().https, "127.0.0.1", &other), "x");
    assert!(matches!(c.me(), Err(CallError::Offline(_))));
    // and the hub's own works
    let c = Client::new(h.address(), "not-a-token");
    assert_eq!(status(&c.me().unwrap_err()), 401);
}

#[test]
fn the_certificate_page_serves_the_authority_to_phones_and_computers() {
    let h = hub();
    let helper = h.r().helper;
    let (s, page) = raw_http(helper, "GET", "/", None, None, "Mozilla/5.0 (iPhone)");
    assert_eq!(s, 200);
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains(&h.r().info.fingerprint));
    assert!(page.contains("ShareKeep"));
    assert!(!page.contains("MoneyKeep"));
    let (s, crt) = raw_http(helper, "GET", "/sharekeep-ca.crt", None, None, "x");
    assert_eq!(s, 200);
    assert_eq!(
        wifi::from_pem(&String::from_utf8(crt).unwrap()).unwrap(),
        h.r().ca_der
    );
    let (s, mc) = raw_http(helper, "GET", "/sharekeep-ca.mobileconfig", None, None, "x");
    assert_eq!(s, 200);
    assert!(String::from_utf8(mc)
        .unwrap()
        .contains("com.apple.security.root"));
    assert_eq!(raw_http(helper, "GET", "/anything", None, None, "x").0, 302);
}

#[test]
fn the_hub_computers_port_needs_its_token() {
    let h = hub();
    assert_eq!(
        http(h.r().admin, "GET", "/admin/v1/status", None, None).0,
        401
    );
    assert_eq!(
        http(h.r().admin, "GET", "/admin/v1/status", Some("wrong"), None).0,
        401
    );
    let (s, st) = h.admin("GET", "/admin/v1/status", None);
    assert_eq!(s, 200);
    assert_eq!(st["pin_set"], false);
    assert!(st["fingerprint"].as_str().unwrap().len() > 30);
    // pairing before a PIN is set is refused with a clear message
    assert_eq!(
        h.admin("POST", "/admin/v1/pin", Some(json!({"pin": "12ab"})))
            .0,
        400
    );
}

#[test]
fn pairings_survive_a_restart_and_a_locked_book_waits_for_its_password() {
    let mut h = hub();
    let (book, file) = household(&h, true, Some("correct horse battery"));
    let code = pairing_code(&h, &book, 1);
    let (jerry, _) = Client::pair(h.address(), &code, PIN, "Jerry's laptop").unwrap();
    jerry
        .submit_now("expense", &json!({"amount": 500}))
        .unwrap();

    // a restart (an update installed over the old version, a reboot)
    h.restart();
    // the same device, same token: no re-pairing
    let jerry = Client::new(h.address(), jerry.token().unwrap());
    // the encrypted book is locked until someone at the hub computer unlocks it
    assert_eq!(status(&jerry.me().unwrap_err()), 423);
    let (s, _) = h.admin(
        "POST",
        &format!("/admin/v1/books/{}/unlock", file.replace(' ', "%20")),
        Some(json!({"password": "wrong password"})),
    );
    assert_eq!(s, 403);
    let (s, b) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/unlock"),
        Some(json!({"password": "correct horse battery"})),
    );
    assert_eq!(s, 200, "{b}");
    assert_eq!(jerry.me().unwrap().member, 1);
    let (_, recs) = h.admin("GET", &format!("/admin/v1/books/{book}/records"), None);
    assert_eq!(recs["records"].as_array().unwrap().len(), 4);
}

#[test]
fn wrong_guesses_pause_pairing_and_codes_expire() {
    let h = hub();
    let (book, _) = household(&h, false, None);
    let code = pairing_code(&h, &book, 2);
    h.advance_minutes(11);
    assert_eq!(
        status(&Client::pair(h.address(), &code, PIN, "late").unwrap_err()),
        403
    );
    let mut last = 0;
    for _ in 0..12 {
        last = status(&Client::pair(h.address(), "ZZZZ-ZZZZ", PIN, "guess").unwrap_err());
    }
    assert_eq!(last, 429);
    // a fresh, right code is still paused until the window passes
    let code = pairing_code(&h, &book, 2);
    assert_eq!(
        status(&Client::pair(h.address(), &code, PIN, "x").unwrap_err()),
        429
    );
    h.advance_minutes(11);
    let code = pairing_code(&h, &book, 2);
    assert!(Client::pair(h.address(), &code, PIN, "x").is_ok());
}

#[test]
fn a_device_notices_a_rewritten_history() {
    let mut h = hub();
    let (book, file) = household(&h, false, None);
    let code = pairing_code(&h, &book, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    for i in 0..3 {
        sam.submit_now("expense", &json!({"amount": 100 + i}))
            .unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let mut replica = Replica::open(dir.path()).unwrap();
    replica.sync(&sam, 1).unwrap();
    let seen = replica.head().unwrap();

    // someone with the hub computer rewrites the book: drops the last records and
    // appends others with a valid chain
    h.running.take().unwrap().stop();
    let path = h.data.join("books").join(&file);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("DROP TRIGGER records_no_delete; DELETE FROM records WHERE seq >= 4;")
            .unwrap();
    }
    {
        let mut s = sk_store::Store::open(&path, None).unwrap();
        for i in 0..3 {
            s.append(
                5,
                sk_chain::Author::hub(),
                "expense",
                json!({"amount": 999 + i}),
            )
            .unwrap();
        }
    }
    h.running = Some(sk_hub::start(options(&h.data, &h.clock)).unwrap());
    let sam = Client::new(h.address(), sam.token().unwrap());
    match replica.sync(&sam, 2) {
        Err(SyncError::Rewritten(b)) => assert_eq!(b.at, seen.seq),
        other => panic!("expected a rewritten history, got {other:?}"),
    }
    // the device's copy is kept as it was
    assert_eq!(replica.head(), Some(seen));
}
