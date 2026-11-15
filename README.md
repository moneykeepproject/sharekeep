# ShareKeep

Keep a household's shared money: partners, roommates, or a parent and an adult child
sharing a home. Part of the MoneyKeep family (Rust + Slint).

**Status: in development.** Milestones 1 and 2 are the split engine, the Hyperbook chain and
the book files; there is no app to run yet.

## Build & test

```sh
cargo test --workspace      # the split engine and its property tests
cargo xtask ci              # fmt, clippy -D warnings, tests, repository rules
cargo run -p sk-chain --bin sk-verify -- history.jsonl   # check an exported Hyperbook history
```

## Layout

| Path | What |
|---|---|
| `crates/sk-core/` | Money (integer cents, currencies), ids, date spans. No I/O. |
| `crates/sk-split/` | The split engine: ten methods, caps and floors, rounding, rule versions, balances, settle-up, deposits, join/leave proration. No I/O. |
| `crates/sk-split/src/periods.rs` | Settle-up schedules, schedule changes, events, bill due dates, closing. |
| `crates/sk-split/tests/properties.rs` | Property tests: thousands of seeded random cases. |
| `crates/sk-chain/` | The Hyperbook hash chain; `sk-verify`, a tool that checks an exported history. |
| `crates/sk-store/` | Book files: SQLite, append-only history, optional encryption, backups and restored copies. |
| `xtask/` | `cargo xtask ci`, `workflows`, `guards`. |
| `docs/specs/` | What the implemented parts must do. |

## Planned

| Milestone | Crates |
|---|---|
| 1 (done) | `sk-core`, `sk-split` |
| 2 (done) | `sk-chain` (Hyperbook hash chain), `sk-store` (SQLite book files) |
| 3 | `sk-hub` (household service, pairing), `sk-client` (API client, outbox) |
| 4 | `sk-ui`, `sharekeep-app` (Windows, macOS), `sk-webapp` (phones and PC browsers), `sk-report` (PDFs), installers |

## Licence

MIT OR Apache-2.0, at your option. See `NOTICE`.
