//! Write a small sample history: `cargo run -p sk-chain --example sample -- out.jsonl`.

use serde_json::json;
use sk_chain::{export_lines, next, Author, GENESIS};

fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.jsonl".into());
    let mut records = vec![];
    if let Ok(g) = next(None, 0, Author::hub(), GENESIS, json!({"name": "Sample"})) {
        records.push(g);
    }
    for i in 1..5 {
        if let Ok(r) = next(
            records.last(),
            i,
            Author::hub(),
            "expense",
            json!({"amount": i * 100}),
        ) {
            records.push(r);
        }
    }
    std::fs::write(path, export_lines(&records))
}
