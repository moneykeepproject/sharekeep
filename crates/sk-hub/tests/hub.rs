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
        admin_record(h, &id, None, "member-add", json!({"id": n, "name": name}));
    }
    admin_record(
        h,
        &id,
        None,
        "agreement-add",
        json!({
            "id": 1, "name": "Home",
            "members": {"1": "2026-11-01", "2": "2026-11-01"},
            "schedule": {"Monthly": {"start": "2026-11-01", "day": {"Day": 1}, "every": 1}},
        }),
    );
    // a one-keeper household, so the keeper's rule is in force at once
    let (s, _) = h.admin(
        "POST",
        &format!("/admin/v1/books/{id}/settings"),
        Some(json!({"key": "one_keeper", "value": 1})),
    );
    assert_eq!(s, 201);
    admin_record(
        h,
        &id,
        Some(1),
        "rule-proposal",
        json!({
            "agreement": 1, "rule_id": 1, "effective": "2026-11-01",
            "rule": {"name": "Shared", "method": "Equal", "members": [1, 2]},
        }),
    );
    (id, file)
}

fn admin_record(h: &Hub, book: &str, member: Option<u32>, kind: &str, body: Value) -> Value {
    let n = h.clock.fetch_add(1, Ordering::SeqCst);
    let (s, b) = h.admin(
        "POST",
        &format!("/admin/v1/books/{book}/records"),
        Some(json!({"client_id": format!("t{n}"), "kind": kind, "body": body, "member": member})),
    );
    assert_eq!(s, 201, "{kind}: {b}");
    b
}

fn expense(amount: i64, payer: u32) -> Value {
    json!({"agreement": 1, "date": "2026-11-14", "amount": amount, "payer": {"member": payer}, "category": "Groceries"})
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
    outbox.push("expense", expense(8347, 2), 1).unwrap();
    outbox
        .push(
            "payment",
            json!({"agreement": 1, "date": "2026-11-14", "from": 2, "to": 1, "amount": 2000}),
            2,
        )
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
    let again = sam.submit("same-id", "expense", &expense(100, 2)).unwrap();
    let dup = sam.submit("same-id", "expense", &expense(100, 2)).unwrap();
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
    // the agreement engine refuses what would make the book wrong
    let mut wrong = expense(100, 2);
    wrong["agreement"] = json!(9);
    let e = sam.submit_now("expense", &wrong).unwrap_err();
    assert_eq!(status(&e), 422);
    assert!(e.to_string().contains("no such agreement"), "{e}");

    assert_eq!(replica.sync(&sam, 100).unwrap(), 9);
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
        9
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
    // the desktop app's fetch, and the fingerprint it shows, match the hub computer's
    let der = sk_client::fetch_ca(helper).unwrap();
    assert_eq!(der, h.r().ca_der);
    assert_eq!(sk_client::fingerprint(&der), h.r().info.fingerprint);
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
    jerry.submit_now("expense", &expense(500, 1)).unwrap();

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
    assert_eq!(recs["records"].as_array().unwrap().len(), 7);
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
        sam.submit_now("expense", &expense(100 + i, 2)).unwrap();
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
        c.execute_batch("DROP TRIGGER records_no_delete; DELETE FROM records WHERE seq >= 7;")
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

#[test]
fn closing_a_period_saves_the_statement_and_seals_it_into_the_hyperbook() {
    let h = hub();
    let (book, _) = household(&h, true, None);
    let code = pairing_code(&h, &book, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    sam.submit_now("expense", &expense(9000, 2)).unwrap();
    let close = json!({"agreement": 1, "date": "2026-11-01"});
    // November isn't over yet
    assert_eq!(
        status(&sam.submit_now("period-close", &close).unwrap_err()),
        422
    );
    h.advance_minutes(20 * 24 * 60);
    sam.submit_now("period-close", &close).unwrap();

    let (_, recs) = h.admin("GET", &format!("/admin/v1/books/{book}/records"), None);
    let records = recs["records"].as_array().unwrap();
    let seal = records.last().unwrap();
    assert_eq!(seal["kind"], "seal");
    let sealed = seal["body"]["statement_sha256"]
        .as_str()
        .unwrap()
        .to_string();

    // a device downloads the statement; it is the one sealed
    let (s, pdf) = sk_client::tls::https_call(
        h.r().https,
        "127.0.0.1",
        &h.r().ca_der,
        "GET",
        "/api/v1/statements/1/2026-11-01",
        &[("Authorization", &format!("Bearer {}", sam.token().unwrap()))],
        None,
    )
    .unwrap();
    assert_eq!(s, 200);
    assert!(pdf.starts_with(b"%PDF"));
    assert_eq!(sk_chain::Hash::of(&pdf).to_hex(), sealed);
    // the closed period takes nothing more
    assert_eq!(
        status(&sam.submit_now("expense", &expense(100, 2)).unwrap_err()),
        422
    );
}

#[test]
fn the_hub_works_in_one_book_at_a_time() {
    let mut h = hub();
    let (id, file) = household(&h, false, None);
    // another book can't be made or opened while Home is open
    let (s, b) = h.admin(
        "POST",
        "/admin/v1/books",
        Some(json!({"name": "Cabin", "hyperbook": false})),
    );
    assert_eq!(s, 409, "{b}");
    assert!(b["error"].as_str().unwrap().contains("Close it first"));
    let (_, st) = h.admin("GET", "/admin/v1/status", None);
    assert_eq!(st["current"], "Home");
    // a pairing code stops working when its book closes
    let code = pairing_code(&h, &id, 2);
    let (s, closed) = h.admin("POST", &format!("/admin/v1/books/{file}/close"), None);
    assert_eq!(s, 200, "{closed}");
    assert_eq!(closed["open"], false);
    assert_eq!(
        closed["name"], "Home",
        "a closed book keeps its name in the list"
    );
    let e = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap_err();
    assert!(matches!(e, CallError::Http { .. }), "{e:?}");
    let (_, st) = h.admin("GET", "/admin/v1/status", None);
    assert!(st["current"].is_null());
    // now a second book can be made; it becomes the one the hub works in
    let (s, b) = h.admin(
        "POST",
        "/admin/v1/books",
        Some(json!({"name": "Cabin", "hyperbook": false})),
    );
    assert_eq!(s, 201, "{b}");
    let cabin = b["file"].as_str().unwrap().to_string();
    // opening Home again needs Cabin closed first
    let (s, _) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/unlock"),
        Some(json!({})),
    );
    assert_eq!(s, 409);
    h.admin("POST", &format!("/admin/v1/books/{cabin}/close"), None);
    let (s, b) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/unlock"),
        Some(json!({})),
    );
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["open"], true);
    // after a restart the hub opens the book it was working in, and only that one
    h.restart();
    let (_, st) = h.admin("GET", "/admin/v1/status", None);
    assert_eq!(st["current"], "Home");
    let open: Vec<&Value> = st["books"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["open"] == true)
        .collect();
    assert_eq!(open.len(), 1);
}

#[test]
fn the_pairing_qr_code_opens_the_certificate_page_with_step_two() {
    let h = hub();
    let (id, _) = household(&h, false, None);
    let (s, b) = h.admin(
        "POST",
        "/admin/v1/pairing",
        Some(json!({"book_id": id, "member": 2})),
    );
    assert_eq!(s, 201);
    let qr = b["qr"].as_str().unwrap();
    let port = h.r().helper.port();
    assert!(
        qr.starts_with(&format!("http://127.0.0.1:{port}/?pair=")),
        "{qr}"
    );
    let path = qr.split_once(&format!(":{port}")).unwrap().1;
    let (s, page) = raw_http(
        h.r().helper,
        "GET",
        path,
        None,
        None,
        "Mozilla/5.0 (iPhone)",
    );
    assert_eq!(s, 200);
    let page = String::from_utf8(page).unwrap();
    let code = b["code"].as_str().unwrap();
    assert!(page.contains("Step 2"));
    assert!(
        page.contains(code),
        "the code is shown for typing into the desktop app"
    );
    assert!(page.contains(&format!("/#pair={}", code.replace('-', ""))));
    // anything that isn't a code is ignored
    let (_, page) = raw_http(h.r().helper, "GET", "/?pair=<script>", None, None, "x");
    let page = String::from_utf8(page).unwrap();
    assert!(!page.contains("Step 2") && !page.contains("<script>"));
}

fn https(
    h: &Hub,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Vec<u8>) {
    let auth = token.map(|t| format!("Bearer {t}"));
    let mut headers = vec![("Content-Type", "application/json")];
    if let Some(a) = &auth {
        headers.push(("Authorization", a));
    }
    let bytes = body.map(|b| serde_json::to_vec(&b).unwrap());
    sk_client::tls::https_call(
        h.r().https,
        "127.0.0.1",
        &h.r().ca_der,
        method,
        path,
        &headers,
        bytes.as_deref(),
    )
    .unwrap()
}

#[test]
fn the_web_app_is_served_and_its_screens_come_from_the_hub() {
    let h = hub();
    let (id, _) = household(&h, false, None);
    // the app's files need no pairing
    let (s, page) = https(&h, "GET", "/", None, None);
    assert_eq!(s, 200);
    assert!(String::from_utf8(page).unwrap().contains("/app.js"));
    assert_eq!(https(&h, "GET", "/app.js", None, None).0, 200);
    assert_eq!(https(&h, "GET", "/secret.txt", None, None).0, 404);
    // its data does
    assert_eq!(https(&h, "GET", "/api/v1/view", None, None).0, 401);
    let code = pairing_code(&h, &id, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    let t = sam.token();
    sam.submit("e1", "expense", &expense(9000, 2)).unwrap();
    let (s, v) = https(&h, "GET", "/api/v1/view", t, None);
    assert_eq!(s, 200);
    let v: Value = serde_json::from_slice(&v).unwrap();
    assert_eq!(v["me_name"], "Sam");
    assert_eq!(v["cards"][0]["headline"], "You are owed $45.00");
    assert_eq!(v["agreements"][0]["activity"][0]["amount"], "$90.00");
    // the live split while typing, and its reason when it can't divide
    let (s, p) = https(
        &h,
        "POST",
        "/api/v1/preview",
        t,
        Some(json!({"expense": expense(1001, 2)})),
    );
    assert_eq!(s, 200);
    let p: Value = serde_json::from_slice(&p).unwrap();
    assert_eq!(p["rows"].as_array().unwrap().len(), 2);
    let mut bad = expense(1001, 2);
    bad["agreement"] = json!(7);
    assert_eq!(
        https(
            &h,
            "POST",
            "/api/v1/preview",
            t,
            Some(json!({"expense": bad}))
        )
        .0,
        422
    );
    // the rule editor's examples
    let rule =
        json!({"name": "Rent", "method": {"Percent": {"1": 6000, "2": 4000}}, "members": [1, 2]});
    let (s, x) = https(
        &h,
        "POST",
        "/api/v1/examples",
        t,
        Some(json!({ "rule": rule })),
    );
    assert_eq!(s, 200);
    let x: Value = serde_json::from_slice(&x).unwrap();
    assert_eq!(x["examples"][1]["rows"][0]["amount"], "$60.00");
    let wrong =
        json!({"name": "Rent", "method": {"Percent": {"1": 6000, "2": 3000}}, "members": [1, 2]});
    assert_eq!(
        https(
            &h,
            "POST",
            "/api/v1/examples",
            t,
            Some(json!({ "rule": wrong }))
        )
        .0,
        422
    );
}

#[test]
fn the_preview_asks_for_days_when_the_rule_divides_by_them() {
    let h = hub();
    let (id, _) = household(&h, false, None);
    admin_record(
        &h,
        &id,
        Some(1),
        "rule-proposal",
        json!({
            "agreement": 1, "rule_id": 2, "effective": "2026-11-01", "categories": ["Heating"],
            "rule": {"name": "Heating", "method": "Days", "members": [1, 2]},
        }),
    );
    let code = pairing_code(&h, &id, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    let mut e = expense(10000, 2);
    e["category"] = json!("Heating");
    let (s, p) = https(
        &h,
        "POST",
        "/api/v1/preview",
        sam.token(),
        Some(json!({"expense": e.clone()})),
    );
    assert_eq!(s, 200);
    let p: Value = serde_json::from_slice(&p).unwrap();
    assert_eq!(p["needs"]["kind"], "days");
    assert_eq!(p["needs"]["who"].as_array().unwrap().len(), 2);
    assert!(p["error"].is_string());
    e["days"] = json!({"1": 30, "2": 10});
    let (_, p) = https(
        &h,
        "POST",
        "/api/v1/preview",
        sam.token(),
        Some(json!({"expense": e})),
    );
    let p: Value = serde_json::from_slice(&p).unwrap();
    assert_eq!(p["rows"][0]["amount"], "$75.00");
    assert!(p["error"].is_null());
}

#[test]
fn a_broken_hyperbook_is_restored_from_a_backup_and_devices_accept_it() {
    let mut h = hub();
    let (book, file) = household(&h, true, None);
    let code = pairing_code(&h, &book, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    // the next day: the day's first change makes that day's backup
    h.advance_minutes(24 * 60);
    for i in 0..3 {
        sam.submit_now("expense", &expense(100 + i, 2)).unwrap();
    }
    let backups: Vec<_> = std::fs::read_dir(h.data.join("backups"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(backups.len(), 2, "one backup a day");
    let dir = tempfile::tempdir().unwrap();
    let mut replica = Replica::open(dir.path()).unwrap();
    replica.sync(&sam, 1).unwrap();
    let seen = replica.head().unwrap();

    // someone edits the last record in the file: the Hyperbook no longer opens
    h.running.take().unwrap().stop();
    let path = h.data.join("books").join(&file);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(&format!(
            "DROP TRIGGER records_no_update; UPDATE records SET body = replace(body, '102', '902') WHERE seq = {};",
            seen.seq
        ))
        .unwrap();
    }
    h.running = Some(sk_hub::start(options(&h.data, &h.clock)).unwrap());
    let (s, b) = h.admin("GET", "/admin/v1/status", None);
    assert_eq!(s, 200);
    let listed = b["books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["file"] == file.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["broken"], seen.seq);

    // the recovery screen lists both backups, newest first, both fine
    let (s, rec) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/recovery"),
        Some(json!({})),
    );
    assert_eq!(s, 200, "{rec}");
    let list = rec["backups"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|x| x["status"] == "ok"));
    let newest = list[0]["file"].as_str().unwrap().to_string();
    let backup_head = list[0]["head"]["seq"].as_u64().unwrap();
    assert!(backup_head < seen.seq);

    // what restoring it would lose
    let (s, lost) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/lost"),
        Some(json!({"backup": newest})),
    );
    assert_eq!(s, 200, "{lost}");
    assert_eq!(
        lost["records"].as_array().unwrap().len() as u64,
        seen.seq - backup_head
    );

    // the broken history can be exported as it is
    let (s, ex) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/export-broken"),
        Some(json!({})),
    );
    assert_eq!(s, 200);
    assert_eq!(ex["broken_at"], seen.seq);
    assert!(ex["text"].as_str().unwrap().contains("902"));

    // restore
    let (s, done) = h.admin(
        "POST",
        &format!("/admin/v1/books/{file}/restore"),
        Some(json!({"backup": newest})),
    );
    assert_eq!(s, 201, "{done}");
    assert!(!path.exists(), "the broken file is set aside");
    assert!(done["set_aside"]
        .as_str()
        .unwrap()
        .ends_with(".sharekeep-broken"));
    assert!(h
        .data
        .join("books")
        .join(done["set_aside"].as_str().unwrap())
        .exists());

    // the device sees the restore, keeps its copy until it accepts, then carries on
    let sam = Client::new(h.address(), sam.token().unwrap());
    let offer = match replica.sync(&sam, 2) {
        Err(SyncError::Restored(o)) => o,
        other => panic!("expected a restore offer, got {other:?}"),
    };
    assert_eq!(offer.to_seq, backup_head);
    assert_eq!(
        (offer.lost_from, offer.lost_to),
        (backup_head + 1, seen.seq)
    );
    assert!(offer.text().contains("not in it"));
    assert_eq!(replica.head(), Some(seen));
    replica.accept_restore(&sam, &offer, 3).unwrap();
    assert_eq!(replica.head().unwrap().seq, offer.seq);
    sam.submit_now("restore-accept", &json!({"restore": offer.seq}))
        .unwrap();
    replica.sync(&sam, 4).unwrap();
    // and the book takes new records again
    sam.submit_now("expense", &expense(500, 2)).unwrap();
    replica.sync(&sam, 5).unwrap();
    assert_eq!(replica.head().unwrap().seq, offer.seq + 2);
}

#[test]
fn the_web_app_can_verify_the_book_and_export_a_period() {
    let h = hub();
    let (book, _) = household(&h, true, None);
    let code = pairing_code(&h, &book, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    sam.submit_now("expense", &expense(1001, 2)).unwrap();
    let token = sam.token().unwrap();
    let (s, b) = https(&h, "GET", "/api/v1/verify", Some(token), None);
    assert_eq!(s, 200);
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["history"]["ok"], true);
    assert_eq!(v["numbers"]["ok"], true, "{v}");
    let (s, b) = https(&h, "GET", "/api/v1/csv/1/2026-11-01", Some(token), None);
    assert_eq!(s, 200);
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["file"], "Home 2026-11-01 to 2026-11-30.csv");
    let text = v["text"].as_str().unwrap();
    // an odd cent: split 5.01 / 5.00, never a fraction
    assert!(text.contains(",10.01,"), "{text}");
    assert!(text.contains("# History at export: record "));
    assert_eq!(
        https(&h, "GET", "/api/v1/csv/1/nope", Some(token), None).0,
        404
    );
}

#[test]
fn a_device_stops_at_a_record_kind_it_does_not_know() {
    let mut h = hub();
    let (book, file) = household(&h, true, None);
    let code = pairing_code(&h, &book, 2);
    let (sam, _) = Client::pair(h.address(), &code, PIN, "Sam's phone").unwrap();
    sam.submit_now("expense", &expense(100, 2)).unwrap();
    // a newer ShareKeep on the hub writes a kind this version doesn't have
    h.running.take().unwrap().stop();
    let path = h.data.join("books").join(&file);
    let before = {
        let mut s = sk_store::Store::open(&path, None).unwrap();
        let before = s.head().unwrap();
        s.append(
            5,
            sk_chain::Author::hub(),
            "teleport",
            json!({"to": "Mars"}),
        )
        .unwrap();
        before
    };
    h.running = Some(sk_hub::start(options(&h.data, &h.clock)).unwrap());
    let sam = Client::new(h.address(), sam.token().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut replica = Replica::open(dir.path()).unwrap();
    match replica.sync(&sam, 1) {
        Err(e @ SyncError::NeedsUpdate { .. }) => {
            assert!(
                e.to_string().contains("Update ShareKeep on this device"),
                "{e}"
            );
        }
        other => panic!("expected NeedsUpdate, got {other:?}"),
    }
    // everything before it is kept, and the next sync stops at the same place
    assert_eq!(replica.head(), Some(before));
    assert!(matches!(
        replica.sync(&sam, 2),
        Err(SyncError::NeedsUpdate { .. })
    ));
    assert_eq!(Replica::open(dir.path()).unwrap().head(), Some(before));
}
