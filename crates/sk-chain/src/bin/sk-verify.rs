//! Check an exported Hyperbook history.
//!
//! ```text
//! sk-verify <history.jsonl> [<seq>:<hash>]
//! ```
//!
//! Prints the head (last record's number and hash) when the whole chain checks out, or the
//! first broken record. With `<seq>:<hash>` (a head you saw before, from your app's
//! Settings > Hyperbook), it also checks that the history still contains that head.
//! Exit code 0: verified; 1: broken; 2: usage or file error.

use sk_chain::{extends, import_lines, verify, Hash, Head};
use std::process::ExitCode;

fn parse_head(s: &str) -> Option<Head> {
    let (seq, hash) = s.split_once(':')?;
    Some(Head {
        seq: seq.trim().parse().ok()?,
        hash: Hash::from_hex(hash)?,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: sk-verify <history.jsonl> [<seq>:<hash>]");
        return ExitCode::from(2);
    };
    let seen = match args.get(1).map(|s| parse_head(s)) {
        Some(None) => {
            eprintln!("a head looks like 42:<64 hex digits>");
            return ExitCode::from(2);
        }
        Some(Some(h)) => Some(h),
        None => None,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("can't read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let result = import_lines(&text).and_then(|records| {
        let head = verify(&records)?;
        if let Some(s) = seen {
            extends(&records, s)?;
        }
        Ok((records.len(), head))
    });
    match result {
        Ok((n, head)) => {
            println!("OK: {n} records; head {}:{}", head.seq, head.hash);
            ExitCode::SUCCESS
        }
        Err(b) => {
            println!("BROKEN: {b}");
            ExitCode::from(1)
        }
    }
}
