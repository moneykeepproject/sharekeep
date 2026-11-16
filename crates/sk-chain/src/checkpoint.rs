//! Checkpoints: a book's head written as one line of text (and a QR code), for keeping
//! outside the house: printed, emailed to yourself, given to a relative. Later, anyone can
//! check whether the history still contains it. The hub doesn't store checkpoints or know
//! they exist: the hub is the party being checked.
//!
//! `ShareKeep checkpoint · Home · record 17 · 2027-02-03 · ab12…` (64 hex digits)

use crate::{Hash, Head, Record};

/// A checkpoint's text.
pub fn text(book: &str, head: Head, date: &str) -> String {
    // the book name can't break the line's shape
    let book: String = book
        .chars()
        .map(|c| if c == '·' || c.is_control() { ' ' } else { c })
        .collect();
    format!(
        "ShareKeep checkpoint · {} · record {} · {} · {}",
        book.trim(),
        head.seq,
        date,
        head.hash.to_hex()
    )
}

/// The text for a QR code: the same line with plain `|` separators, because scanners guess
/// the character encoding and can garble `·`. [`parse`] reads either.
pub fn for_qr(text: &str) -> String {
    text.replace('·', "|")
}

/// The head in a checkpoint's text: forgiving about spacing, line breaks and case, so a
/// checkpoint typed back from paper or pasted from an email still reads.
pub fn parse(text: &str) -> Option<Head> {
    let lower = text.to_ascii_lowercase();
    // "record" followed by a number (a book may be called "Records")
    let (seq, after) = lower.match_indices("record").find_map(|(i, _)| {
        let rest = lower.get(i + "record".len()..)?;
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        Some((digits.parse::<u64>().ok()?, rest))
    })?;
    // the fingerprint: the last 64 hex digits, read backwards (spaces and line breaks inside
    // it are allowed; the date's digits before it aren't taken)
    let mut rev = Vec::new();
    for c in after
        .trim_end_matches(|c: char| !c.is_ascii_hexdigit())
        .chars()
        .rev()
    {
        if c.is_ascii_hexdigit() {
            rev.push(c);
            if rev.len() == 64 {
                break;
            }
        } else if !c.is_whitespace() {
            break;
        }
    }
    let run: String = rev.into_iter().rev().collect();
    Some(Head {
        seq,
        hash: Hash::from_hex(&run)?,
    })
}

/// What checking a checkpoint against a history found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The history still contains the checkpoint.
    Kept,
    /// The history has a different record at that place: it was rewritten.
    Rewritten,
    /// The history is shorter than the checkpoint: it isn't here yet (a device that hasn't
    /// synced), or records were removed.
    Missing { have: Option<u64> },
}

/// Check a checkpoint against records (a whole history, from record 0).
pub fn check(records: &[Record], head: Head) -> Found {
    match records.iter().find(|r| r.seq == head.seq) {
        Some(r) if r.hash == head.hash => Found::Kept,
        Some(_) => Found::Rewritten,
        None => Found::Missing {
            have: records.last().map(|r| r.seq),
        },
    }
}

/// The plain-words answer.
pub fn explain(found: &Found, head: Head) -> String {
    match found {
        Found::Kept => format!(
            "Still in the history: record {} has the same fingerprint.",
            head.seq
        ),
        Found::Rewritten => format!(
            "The history was rewritten since this checkpoint was made: record {} is different now.",
            head.seq
        ),
        Found::Missing { have } => match have {
            Some(h) => format!(
                "This history ends at record {h}; the checkpoint is at record {}. Sync, then check again. If it still ends earlier, records were removed.",
                head.seq
            ),
            None => "There's no history here to check against yet.".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{next, Author, GENESIS};
    use serde_json::json;

    #[test]
    fn a_checkpoint_round_trips_and_reads_back_from_paper() {
        let g = next(None, 0, Author::hub(), GENESIS, json!({})).unwrap();
        let t = text("Home · Sweet", g.head(), "2027-02-03");
        assert!(t.starts_with("ShareKeep checkpoint · Home   Sweet · record 0 · 2027-02-03 · "));
        assert_eq!(parse(&t), Some(g.head()));
        // retyped: upper case, the fingerprint broken over lines in groups
        let hex = g.hash.to_hex().to_uppercase();
        let typed = format!(
            "sharekeep checkpoint home RECORD 0\n2027-02-03\n{} {}\n{} {}",
            &hex[..16],
            &hex[16..32],
            &hex[32..48],
            &hex[48..]
        );
        assert_eq!(parse(&typed), Some(g.head()));
        assert_eq!(parse("record 5 nothing else"), None);
        assert_eq!(parse("no record here"), None);
        let t = text("Records", g.head(), "2027-02-03");
        assert_eq!(parse(&t), Some(g.head()));
        assert!(for_qr(&t).is_ascii());
        assert_eq!(parse(&for_qr(&t)), Some(g.head()));
    }

    #[test]
    fn checking_finds_kept_rewritten_and_missing() {
        let g = next(None, 0, Author::hub(), GENESIS, json!({})).unwrap();
        let a = next(
            Some(&g),
            1,
            Author::hub(),
            "member-add",
            json!({"id": 1, "name": "A"}),
        )
        .unwrap();
        let b = next(
            Some(&g),
            1,
            Author::hub(),
            "member-add",
            json!({"id": 1, "name": "B"}),
        )
        .unwrap();
        let mine = vec![g.clone(), a.clone()];
        assert_eq!(check(&mine, a.head()), Found::Kept);
        assert_eq!(check(&[g.clone(), b], a.head()), Found::Rewritten);
        assert_eq!(check(&[g], a.head()), Found::Missing { have: Some(0) });
        assert!(explain(&Found::Rewritten, a.head()).contains("rewritten"));
    }
}
