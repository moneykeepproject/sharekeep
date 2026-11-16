//! Book files end to end: create, append, reopen, lock, tamper, encrypt, back up, restore.

use serde_json::json;
use sk_chain::{Author, Hash};
use sk_core::{Currency, MemberId};
use sk_store::crypt::KdfParams;
use sk_store::{book_kind, restore_copy, BookKind, Encryption, NewBook, Secret, Store, StoreError};
use std::path::Path;

fn jerry() -> Author {
    Author {
        member: Some(MemberId(1)),
        device: "desktop".into(),
    }
}

fn new_book(name: &str, hyperbook: bool, encryption: Option<Encryption>) -> NewBook {
    NewBook {
        name: name.into(),
        currency: Currency::USD,
        hyperbook,
        encryption,
        ts: 1_794_700_000_000,
        author: jerry(),
    }
}

fn enc(recovery: bool) -> Encryption {
    Encryption {
        password: "correct horse battery".into(),
        recovery_key: recovery,
        kdf: KdfParams::TEST,
    }
}

fn add_entries(store: &mut Store, n: usize) {
    for i in 0..n {
        store
            .append(
                1_794_700_000_000 + i as i64,
                jerry(),
                "expense",
                json!({"amount": 1000 + i, "category": "Groceries"}),
            )
            .unwrap();
    }
}

#[test]
fn a_hyperbook_keeps_and_verifies_its_history_across_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let created = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    assert_eq!(created.path, dir.path().join("Home.sharekeep"));
    assert!(created.recovery_key.is_none());
    let mut store = created.store;
    assert!(store.info().hyperbook);
    add_entries(&mut store, 25);
    let head = store.head().unwrap();
    assert_eq!(head.seq, 25);
    drop(store);

    let store = Store::open(&dir.path().join("Home.sharekeep"), None).unwrap();
    assert_eq!(store.head(), Some(head));
    assert_eq!(store.verify().unwrap(), head);
    assert_eq!(store.records(0, 3).unwrap()[0].kind, sk_chain::GENESIS);
    assert_eq!(store.info().name, "Home");
    assert_eq!(store.info().currency, Currency::USD);
}

#[test]
fn only_one_hub_can_have_a_book_open() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", false, None)).unwrap();
    assert!(matches!(
        Store::open(&c.path, None),
        Err(StoreError::BookInUse)
    ));
    drop(c.store);
    assert!(Store::open(&c.path, None).is_ok());
}

#[test]
fn a_second_book_with_the_same_name_gets_a_numbered_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = Store::create(dir.path(), new_book("Home", false, None)).unwrap();
    let b = Store::create(dir.path(), new_book("Home", false, None)).unwrap();
    assert_eq!(b.path, dir.path().join("Home (2).sharekeep"));
    assert_ne!(a.store.info().book_id, b.store.info().book_id);
}

fn tamper(path: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
}

#[test]
fn the_history_refuses_changes_and_a_hyperbook_catches_a_forced_one() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    let mut store = c.store;
    add_entries(&mut store, 5);
    drop(store);

    // the triggers refuse an ordinary change
    let conn = rusqlite::Connection::open(&c.path).unwrap();
    assert!(conn
        .execute(
            "UPDATE records SET body = '{\"amount\":1}' WHERE seq = 3",
            []
        )
        .is_err());
    assert!(conn
        .execute("DELETE FROM records WHERE seq = 3", [])
        .is_err());
    drop(conn);

    // someone removes the triggers and edits record 3: the Hyperbook won't open
    tamper(
        &c.path,
        "DROP TRIGGER records_no_update;
         UPDATE records SET body = '{\"amount\":1,\"category\":\"Groceries\"}' WHERE seq = 3;",
    );
    match Store::open(&c.path, None) {
        Err(StoreError::HistoryBroken(b)) => assert_eq!(b.at, 3),
        other => panic!("expected a broken history, got {other:?}"),
    }
}

#[test]
fn a_normal_book_opens_but_verify_reports_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Flat", false, None)).unwrap();
    let mut store = c.store;
    add_entries(&mut store, 3);
    drop(store);
    tamper(
        &c.path,
        "DROP TRIGGER records_no_delete; DELETE FROM records WHERE seq = 2;",
    );
    let store = Store::open(&c.path, None).unwrap();
    assert!(matches!(store.verify(), Err(StoreError::HistoryBroken(_))));
}

#[test]
fn other_files_are_not_books_and_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.sharekeep");
    std::fs::write(&junk, b"hello, this is not a book").unwrap();
    assert!(matches!(
        Store::open(&junk, None),
        Err(StoreError::NotABook)
    ));

    let foreign = dir.path().join("other.db");
    rusqlite::Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER);")
        .unwrap();
    let before = std::fs::read(&foreign).unwrap();
    assert!(matches!(
        Store::open(&foreign, None),
        Err(StoreError::NotABook)
    ));
    assert_eq!(std::fs::read(&foreign).unwrap(), before);
}

#[test]
fn an_encrypted_book_never_holds_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Secret Home", true, Some(enc(true)))).unwrap();
    let rk = c.recovery_key.clone().unwrap();
    assert!(rk.starts_with("SKRK-"));
    let mut store = c.store;
    add_entries(&mut store, 4);
    let head = store.head().unwrap();
    drop(store);

    let bytes = std::fs::read(&c.path).unwrap();
    assert_eq!(book_kind(&c.path).unwrap(), BookKind::Encrypted);
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!has(b"Secret Home") && !has(b"Groceries") && !has(b"SQLite format 3"));
    assert!(sk_store::prev_path(&c.path).exists());

    assert!(matches!(
        Store::open(&c.path, None),
        Err(StoreError::NeedsPassword)
    ));
    assert!(matches!(
        Store::open(&c.path, Some(&Secret::Password("wrong password".into()))),
        Err(StoreError::WrongPassword)
    ));
    let s = Store::open(
        &c.path,
        Some(&Secret::Password("correct horse battery".into())),
    )
    .unwrap();
    assert_eq!(s.head(), Some(head));
    assert!(s.info().encrypted);
    drop(s);
    let s = Store::open(&c.path, Some(&Secret::RecoveryKey(rk))).unwrap();
    assert_eq!(s.head(), Some(head));
}

#[test]
fn an_encrypted_book_is_locked_while_open() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", false, Some(enc(false)))).unwrap();
    let pw = Secret::Password("correct horse battery".into());
    assert!(matches!(
        Store::open(&c.path, Some(&pw)),
        Err(StoreError::BookInUse)
    ));
}

#[test]
fn a_failed_encrypted_save_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, Some(enc(false)))).unwrap();
    let mut store = c.store;
    add_entries(&mut store, 2);
    let head = store.head().unwrap();
    store.fail_next_save_for_tests();
    assert!(store
        .append(1, jerry(), "expense", json!({"amount": 5}))
        .is_err());
    assert_eq!(store.head(), Some(head));
    assert_eq!(store.records(0, 100).unwrap().len(), 3);
    // and the next append carries on from the same head
    let r = store
        .append(2, jerry(), "expense", json!({"amount": 6}))
        .unwrap();
    assert_eq!(r.seq, head.seq + 1);
    drop(store);
    let s = Store::open(
        &c.path,
        Some(&Secret::Password("correct horse battery".into())),
    )
    .unwrap();
    assert_eq!(s.head(), Some(r.head()));
}

#[test]
fn weak_passwords_are_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = enc(false);
    e.password = "short".into();
    assert!(matches!(
        Store::create(dir.path(), new_book("Home", false, Some(e))),
        Err(StoreError::InvalidPassword(_))
    ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn backups_and_restored_copies_never_replace_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("ShareKeep Backups");
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    let mut store = c.store;
    add_entries(&mut store, 3);
    let b1 = store.backup(&backups, "2027-03-01 0900").unwrap();
    let b2 = store.backup(&backups, "2027-03-01 0900").unwrap();
    assert_eq!(b1, backups.join("Home backup 2027-03-01 0900.sharekeep"));
    assert_eq!(
        b2,
        backups.join("Home backup 2027-03-01 0900 (2).sharekeep")
    );
    let head = store.head();
    drop(store);

    let r = restore_copy(&b1, dir.path(), "2027-03-02 1000").unwrap();
    assert_eq!(
        r,
        dir.path().join("Home restored 2027-03-02 1000.sharekeep")
    );
    let restored = Store::open(&r, None).unwrap();
    assert_eq!(restored.head(), head);
    // the original is untouched and still opens
    assert!(Store::open(&c.path, None).is_ok());

    let junk = dir.path().join("x.sharekeep");
    std::fs::write(&junk, b"nope").unwrap();
    assert!(restore_copy(&junk, dir.path(), "x").is_err());
}

#[test]
fn an_encrypted_backup_stays_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", false, Some(enc(false)))).unwrap();
    let b = c.store.backup(dir.path(), "now").unwrap();
    assert_eq!(book_kind(&b).unwrap(), BookKind::Encrypted);
}

#[test]
fn settings_seals_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    let mut store = c.store;
    assert_eq!(store.setting("deposit_interest_bp").unwrap(), None);
    store
        .set_setting(1, jerry(), "deposit_interest_bp", json!(150))
        .unwrap();
    store
        .set_setting(2, jerry(), "deposit_interest_bp", json!(0))
        .unwrap();
    assert_eq!(
        store.setting("deposit_interest_bp").unwrap(),
        Some(json!(0))
    );
    assert!(store
        .set_setting(3, jerry(), "colour", json!("red"))
        .is_err());
    assert!(store
        .append(4, jerry(), sk_chain::GENESIS, json!({}))
        .is_err());

    add_entries(&mut store, 2);
    let seal = store
        .seal(
            5,
            1,
            "2027-03-01",
            "2027-03-31",
            Some(Hash::of(b"statement")),
        )
        .unwrap();
    assert_eq!(seal.kind, sk_chain::SEAL);

    let text = store.export_history().unwrap();
    let records = sk_chain::import_lines(&text).unwrap();
    assert_eq!(sk_chain::verify(&records).unwrap(), store.head().unwrap());
}

#[test]
fn a_resent_request_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    let mut store = c.store;
    let (a, fresh) = store
        .append_once("dev3:abc-1", 10, jerry(), "expense", json!({"amount": 500}))
        .unwrap();
    assert!(fresh);
    let (b, fresh) = store
        .append_once("dev3:abc-1", 99, jerry(), "expense", json!({"amount": 999}))
        .unwrap();
    assert!(!fresh);
    assert_eq!(a, b);
    assert_eq!(store.head(), Some(a.head()));
    assert!(store
        .append_once("bad id!", 1, jerry(), "expense", json!({}))
        .is_err());
    store
        .append(11, jerry(), "member-add", json!({"id": 2, "name": "Sam"}))
        .unwrap();
    assert!(store.has_record_with("member-add", "id", 2).unwrap());
    assert!(!store.has_record_with("member-add", "id", 3).unwrap());
}

#[test]
fn a_version_1_book_is_upgraded_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    let head = c.store.head();
    drop(c.store);
    // make it look like a book from the first release
    tamper(
        &c.path,
        "DROP TABLE client_ids; UPDATE meta SET value = '1' WHERE key = 'schema_version';",
    );
    let mut store = Store::open(&c.path, None).unwrap();
    assert_eq!(store.head(), head);
    store
        .append_once("d:1", 1, jerry(), "expense", json!({"amount": 1}))
        .unwrap();
}

fn version(path: &Path) -> String {
    let c = rusqlite::Connection::open(path).unwrap();
    c.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_version_2_book_becomes_version_3_and_older_sharekeeps_refuse_it() {
    let dir = tempfile::tempdir().unwrap();
    let c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    drop(c.store);
    assert_eq!(version(&c.path), sk_store::SCHEMA_VERSION.to_string());
    assert_eq!(sk_store::SCHEMA_VERSION, 3);
    // a book saved by the version before restores and recurring bills
    tamper(
        &c.path,
        "DELETE FROM meta WHERE key = 'record_kinds'; UPDATE meta SET value = '2' WHERE key = 'schema_version';",
    );
    drop(Store::open(&c.path, None).unwrap());
    assert_eq!(version(&c.path), "3");
    // what a version-2 ShareKeep does with it: "made by a newer ShareKeep"
    tamper(
        &c.path,
        "UPDATE meta SET value = '4' WHERE key = 'schema_version';",
    );
    assert!(matches!(
        Store::open(&c.path, None),
        Err(StoreError::NewerVersion { found: 4 })
    ));
    assert!(matches!(
        Store::open_for_recovery(&c.path, None),
        Err(StoreError::NewerVersion { found: 4 })
    ));
}

#[test]
fn recovery_reads_an_older_book_or_backup_without_changing_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Store::create(dir.path(), new_book("Home", true, None)).unwrap();
    add_entries(&mut c.store, 3);
    let backups = dir.path().join("backups");
    let backup = c.store.backup(&backups, "2027-01-01").unwrap();
    let id = c.store.info().book_id.clone();
    drop(c.store);
    for p in [&c.path, &backup] {
        tamper(
            p,
            "DELETE FROM meta WHERE key = 'record_kinds'; UPDATE meta SET value = '2' WHERE key = 'schema_version';",
        );
    }
    let before = (
        std::fs::read(&c.path).unwrap(),
        std::fs::read(&backup).unwrap(),
    );
    let s = Store::open_for_recovery(&c.path, None).unwrap();
    assert!(s.head().is_some());
    drop(s);
    let listed = sk_store::scan_backups(&backups, &id, None);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "ok");
    assert_eq!(
        (
            std::fs::read(&c.path).unwrap(),
            std::fs::read(&backup).unwrap()
        ),
        before,
        "recovery changed a file"
    );
    assert_eq!(version(&backup), "2");
    // a restored copy is a book in use: it is brought up to date
    let copy = restore_copy(&backup, dir.path(), "2027-01-02").unwrap();
    drop(Store::open(&copy, None).unwrap());
    assert_eq!(version(&copy), "3");
}
