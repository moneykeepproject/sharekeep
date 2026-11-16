//! The Hyperbook: a book whose whole history is a hash chain.
//!
//! Every record holds its sequence number, a timestamp, its author (member and device),
//! its kind, its body and the SHA-256 of the previous record; its own hash covers all of
//! these in a fixed byte encoding ([`encode`]). Changing any byte of any record, removing
//! one, inserting one or reordering them breaks the chain at that record, and
//! [`verify`] says where.
//!
//! What a chain does not do: stop someone who controls the hub PC from rewriting the whole
//! book and every hash after the change. Members' apps keep the last head they saw and
//! check that the hub's chain still extends it ([`extends`]); that is what catches a
//! rewrite.

pub mod checkpoint;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sk_core::MemberId;
use std::fmt;

/// A SHA-256 hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// The `prev` of the first record.
    pub const ZERO: Hash = Hash([0; 32]);

    pub fn of(bytes: &[u8]) -> Hash {
        Hash(Sha256::digest(bytes).into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Hash> {
        let s = s.trim();
        if s.len() != 64 || !s.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Hash(out))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.to_hex())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Hash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Hash::from_hex(&s).ok_or_else(|| serde::de::Error::custom("not a 64-digit hex hash"))
    }
}

/// Who made a record: the member (none for the hub itself) and the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub member: Option<MemberId>,
    pub device: String,
}

impl Author {
    /// The household service itself (genesis, seals, scheduled work).
    pub fn hub() -> Self {
        Author {
            member: None,
            device: "hub".into(),
        }
    }
}

/// One record of a book's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    /// Milliseconds since 1970-01-01 UTC, from the hub's clock.
    pub ts: i64,
    #[serde(flatten)]
    pub author: Author,
    pub kind: String,
    pub body: Value,
    pub prev: Hash,
    pub hash: Hash,
}

/// The newest record a reader has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub seq: u64,
    pub hash: Hash,
}

impl Record {
    pub fn head(&self) -> Head {
        Head {
            seq: self.seq,
            hash: self.hash,
        }
    }
}

/// Why a record can't be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// Kinds are 1–64 characters of a–z, 0–9, `_`, `.` and `-`.
    BadKind(String),
    /// A device name is at most 128 bytes.
    DeviceTooLong,
    /// Bodies are at most 1 MiB once encoded.
    BodyTooLarge,
    /// Bodies hold no floating-point numbers: money is whole cents, and a float's text
    /// form is not stable enough to hash.
    BodyHasFloat,
    /// The chain has reached the end of its sequence numbers.
    Exhausted,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordError::BadKind(k) => write!(f, "\"{k}\" isn't a valid record kind"),
            RecordError::DeviceTooLong => write!(f, "the device name is too long"),
            RecordError::BodyTooLarge => write!(f, "the record is too large"),
            RecordError::BodyHasFloat => write!(f, "a record can't hold fractional numbers"),
            RecordError::Exhausted => write!(f, "the history is full"),
        }
    }
}

impl std::error::Error for RecordError {}

const MAGIC: &[u8; 8] = b"SKCHAIN1";
const MAX_BODY: usize = 1 << 20;
const MAX_DEVICE: usize = 128;

fn check_kind(kind: &str) -> Result<(), RecordError> {
    let ok = (1..=64).contains(&kind.len())
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(RecordError::BadKind(kind.to_string()))
    }
}

/// JSON with object keys in byte order, no whitespace, and no floats.
pub fn canonical_json(v: &Value) -> Result<Vec<u8>, RecordError> {
    let mut out = Vec::new();
    write_canonical(v, &mut out)?;
    if out.len() > MAX_BODY {
        return Err(RecordError::BodyTooLarge);
    }
    Ok(out)
}

fn write_canonical(v: &Value, out: &mut Vec<u8>) -> Result<(), RecordError> {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            if n.is_f64() {
                return Err(RecordError::BodyHasFloat);
            }
            out.extend_from_slice(n.to_string().as_bytes());
        }
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(x, out)?;
            }
            out.push(b']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(k, out);
                out.push(b':');
                if let Some(x) = o.get(k) {
                    write_canonical(x, out)?;
                }
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    // serde_json's escaping is fixed (only what JSON requires, \u for control
    // characters), so it is stable to hash.
    match serde_json::to_vec(s) {
        Ok(b) => out.extend_from_slice(&b),
        Err(_) => out.extend_from_slice(b"\"\""),
    }
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

/// The bytes a record's hash covers: everything but the hash itself.
pub fn encode(
    seq: u64,
    ts: i64,
    author: &Author,
    kind: &str,
    body: &Value,
    prev: &Hash,
) -> Result<Vec<u8>, RecordError> {
    check_kind(kind)?;
    if author.device.len() > MAX_DEVICE {
        return Err(RecordError::DeviceTooLong);
    }
    let body = canonical_json(body)?;
    let mut out = Vec::with_capacity(96 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ts.to_be_bytes());
    match author.member {
        Some(m) => {
            out.push(1);
            out.extend_from_slice(&m.0.to_be_bytes());
        }
        None => {
            out.push(0);
            out.extend_from_slice(&[0; 4]);
        }
    }
    put_bytes(&mut out, author.device.as_bytes());
    put_bytes(&mut out, kind.as_bytes());
    put_bytes(&mut out, &body);
    out.extend_from_slice(&prev.0);
    Ok(out)
}

/// The record that follows `prev` (or the first record, `seq` 0, when there is none).
pub fn next(
    prev: Option<&Record>,
    ts: i64,
    author: Author,
    kind: &str,
    body: Value,
) -> Result<Record, RecordError> {
    let (seq, prev_hash) = match prev {
        Some(p) => (p.seq.checked_add(1).ok_or(RecordError::Exhausted)?, p.hash),
        None => (0, Hash::ZERO),
    };
    let hash = Hash::of(&encode(seq, ts, &author, kind, &body, &prev_hash)?);
    Ok(Record {
        seq,
        ts,
        author,
        kind: kind.to_string(),
        body,
        prev: prev_hash,
        hash,
    })
}

/// The kind of the first record of every book.
pub const GENESIS: &str = "genesis";
/// The kind of a period seal.
pub const SEAL: &str = "seal";

/// Where and why a chain is broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The first record that doesn't check out (its position, when its own sequence
    /// number can't be trusted).
    pub at: u64,
    pub problem: Problem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Empty,
    NotGenesis,
    /// A record is missing, repeated or out of order.
    WrongSequence {
        expected: u64,
        found: u64,
    },
    /// The record doesn't point at the record before it.
    WrongPrev,
    /// The record's contents don't match its hash: it was changed.
    WrongHash,
    Unreadable(String),
    /// The chain doesn't contain a head this reader saw before: the history was rewritten.
    Rewritten,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match &self.problem {
            Problem::Empty => "the history is empty".to_string(),
            Problem::NotGenesis => "the history doesn't start with its first record".into(),
            Problem::WrongSequence { expected, found } => {
                format!("record {expected} was expected but {found} was found")
            }
            Problem::WrongPrev => "the record doesn't follow the one before it".into(),
            Problem::WrongHash => "the record was changed after it was written".into(),
            Problem::Unreadable(why) => format!("the record can't be read ({why})"),
            Problem::Rewritten => "the history was rewritten since it was last seen".into(),
        };
        write!(f, "history broken at record {}: {what}", self.at)
    }
}

impl std::error::Error for Broken {}

/// Check a whole chain from its first record. Returns its head.
pub fn verify(records: &[Record]) -> Result<Head, Broken> {
    let first = records.first().ok_or(Broken {
        at: 0,
        problem: Problem::Empty,
    })?;
    if first.seq != 0 || first.kind != GENESIS || first.prev != Hash::ZERO {
        return Err(Broken {
            at: 0,
            problem: Problem::NotGenesis,
        });
    }
    verify_after(None, records)
}

/// Check `records` as the continuation of a chain whose last checked record was `after`
/// (`None`: they start the chain). Returns the new head.
pub fn verify_after(after: Option<Head>, records: &[Record]) -> Result<Head, Broken> {
    let mut expect_seq = after.map_or(0, |h| h.seq + 1);
    let mut expect_prev = after.map_or(Hash::ZERO, |h| h.hash);
    let mut head = after;
    for r in records {
        if r.seq != expect_seq {
            return Err(Broken {
                at: expect_seq,
                problem: Problem::WrongSequence {
                    expected: expect_seq,
                    found: r.seq,
                },
            });
        }
        if r.prev != expect_prev {
            return Err(Broken {
                at: r.seq,
                problem: Problem::WrongPrev,
            });
        }
        let bytes =
            encode(r.seq, r.ts, &r.author, &r.kind, &r.body, &r.prev).map_err(|e| Broken {
                at: r.seq,
                problem: Problem::Unreadable(e.to_string()),
            })?;
        if Hash::of(&bytes) != r.hash {
            return Err(Broken {
                at: r.seq,
                problem: Problem::WrongHash,
            });
        }
        expect_seq = r.seq + 1;
        expect_prev = r.hash;
        head = Some(r.head());
    }
    head.ok_or(Broken {
        at: 0,
        problem: Problem::Empty,
    })
}

/// Whether a verified chain still contains the head a member saw before.
pub fn extends(records: &[Record], seen: Head) -> Result<(), Broken> {
    let found = usize::try_from(seen.seq)
        .ok()
        .and_then(|i| records.get(i))
        .is_some_and(|r| r.seq == seen.seq && r.hash == seen.hash);
    if found {
        Ok(())
    } else {
        Err(Broken {
            at: seen.seq,
            problem: Problem::Rewritten,
        })
    }
}

/// The body of a period seal: the period, the chain head at closing and the statement
/// PDF's hash.
pub fn seal_body(
    agreement: u32,
    first: &str,
    last: &str,
    head: Head,
    statement: Option<Hash>,
) -> Value {
    serde_json::json!({
        "agreement": agreement,
        "period": { "first": first, "last": last },
        "head": { "seq": head.seq, "hash": head.hash.to_hex() },
        "statement_sha256": statement.map(|h| h.to_hex()),
    })
}

/// The export format: one record per line as JSON.
pub fn export_lines(records: &[Record]) -> String {
    let mut out = String::new();
    for r in records {
        if let Ok(line) = serde_json::to_string(r) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Read an export. A line that isn't a record breaks the chain at that line.
pub fn import_lines(text: &str) -> Result<Vec<Record>, Broken> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str::<Record>(l).map_err(|e| Broken {
                at: i as u64,
                problem: Problem::Unreadable(e.to_string()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chain(n: usize) -> Vec<Record> {
        let mut out: Vec<Record> = Vec::new();
        out.push(next(None, 1_000, Author::hub(), GENESIS, json!({"name": "Home"})).unwrap());
        for i in 1..n {
            let author = Author {
                member: Some(MemberId(1 + (i % 3) as u32)),
                device: "phone".into(),
            };
            let body = json!({"amount": 1000 + i, "note": format!("entry {i}"), "z": [1, {"b": 2, "a": 1}]});
            let r = next(out.last(), 1_000 + i as i64, author, "expense", body).unwrap();
            out.push(r);
        }
        out
    }

    #[test]
    fn a_fresh_chain_verifies() {
        let c = chain(20);
        assert_eq!(verify(&c).unwrap(), c[19].head());
    }

    #[test]
    fn changing_any_field_of_any_record_breaks_the_chain_there() {
        let c = chain(10);
        for i in 0..10 {
            let mut t = c.clone();
            t[i].ts += 1;
            assert_eq!(verify(&t).unwrap_err().at, i as u64);

            let mut t = c.clone();
            t[i].body = json!({"amount": 1});
            assert_eq!(verify(&t).unwrap_err().at, i as u64);

            let mut t = c.clone();
            t[i].author.device.push('x');
            assert_eq!(verify(&t).unwrap_err().at, i as u64);

            if i > 0 {
                let mut t = c.clone();
                t[i].author.member = None;
                assert_eq!(verify(&t).unwrap_err().at, i as u64);
            }
        }
    }

    #[test]
    fn removing_inserting_or_reordering_is_caught() {
        let c = chain(6);
        let mut removed = c.clone();
        removed.remove(3);
        assert_eq!(verify(&removed).unwrap_err().at, 3);

        let mut swapped = c.clone();
        swapped.swap(2, 3);
        assert_eq!(verify(&swapped).unwrap_err().at, 2);

        // a rewritten tail that re-hashes everything still verifies on its own...
        let mut rewritten: Vec<Record> = c[..3].to_vec();
        for i in 3..6 {
            let r = next(
                rewritten.last(),
                9,
                Author::hub(),
                "expense",
                json!({"i": i}),
            )
            .unwrap();
            rewritten.push(r);
        }
        assert!(verify(&rewritten).is_ok());
        // ...but a member who saw the old head notices
        let seen = c[4].head();
        assert_eq!(
            extends(&rewritten, seen).unwrap_err().problem,
            Problem::Rewritten
        );
        assert!(extends(&c, seen).is_ok());
    }

    #[test]
    fn key_order_doesnt_change_the_hash() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[3,"é"]}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":[3,"é"],"y":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a).unwrap(), canonical_json(&b).unwrap());
        assert_eq!(
            canonical_json(&a).unwrap(),
            r#"{"a":{"x":[3,"é"],"y":2},"b":1}"#.as_bytes().to_vec()
        );
    }

    #[test]
    fn floats_bad_kinds_and_long_devices_are_refused() {
        assert_eq!(
            next(None, 0, Author::hub(), GENESIS, json!({"x": 1.5})).unwrap_err(),
            RecordError::BodyHasFloat
        );
        assert!(matches!(
            next(None, 0, Author::hub(), "Expense!", json!({})),
            Err(RecordError::BadKind(_))
        ));
        let long = Author {
            member: None,
            device: "x".repeat(129),
        };
        assert_eq!(
            next(None, 0, long, GENESIS, json!({})).unwrap_err(),
            RecordError::DeviceTooLong
        );
    }

    #[test]
    fn the_first_record_must_be_genesis() {
        let r = next(None, 0, Author::hub(), "expense", json!({})).unwrap();
        assert_eq!(verify(&[r]).unwrap_err().problem, Problem::NotGenesis);
        assert_eq!(verify(&[]).unwrap_err().problem, Problem::Empty);
    }

    #[test]
    fn export_round_trip_and_incremental_verify() {
        let c = chain(8);
        let text = export_lines(&c);
        assert_eq!(text.lines().count(), 8);
        let back = import_lines(&text).unwrap();
        assert_eq!(back, c);
        let head = verify(&c[..5]).unwrap();
        assert_eq!(verify_after(Some(head), &c[5..]).unwrap(), c[7].head());
        assert!(import_lines("{not json}\n").is_err());
    }

    #[test]
    fn hex_round_trip() {
        let h = Hash::of(b"abc");
        assert_eq!(
            h.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(Hash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(Hash::from_hex("zz"), None);
    }

    #[test]
    fn seals_record_the_head_and_statement() {
        let c = chain(3);
        let body = seal_body(
            1,
            "2027-03-01",
            "2027-03-31",
            c[2].head(),
            Some(Hash::of(b"pdf")),
        );
        let s = next(c.last(), 5, Author::hub(), SEAL, body).unwrap();
        let mut all = c.clone();
        all.push(s);
        assert!(verify(&all).is_ok());
    }
}
