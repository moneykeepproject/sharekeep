# ShareKeep

Keep a household's shared money: partners, roommates, or a parent and an adult child
sharing a home. Part of the MoneyKeep family (Rust + Slint).

**Status: in development.** Milestones 1–3 and 4a: the split engine, the Hyperbook chain, the
book files, the household service and its client, the agreement engine and statement PDFs.
There is no desktop app yet.

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
| `crates/sk-ledger/` | The agreement engine: a book's history replayed into its state; every new record checked against it; closing periods. |
| `crates/sk-report/` | Statement PDFs. |
| `crates/sk-hub/` | The household service (`sharekeep-hub`): HTTPS on the home Wi-Fi with its own certificate authority, pairing (code + PIN), devices, records. |
| `crates/sk-client/` | The desktop app's side: HTTPS to the hub, the offline outbox, the checked local copy of a book's history. |
| `xtask/` | `cargo xtask ci`, `workflows`, `guards`. |
| `docs/specs/` | What the implemented parts must do. |

## Planned

| Milestone | Crates |
|---|---|
| 1 (done) | `sk-core`, `sk-split` |
| 2 (done) | `sk-chain` (Hyperbook hash chain), `sk-store` (SQLite book files) |
| 3 (done) | `sk-hub` (household service, pairing), `sk-client` (API client, outbox) |
| 4a (done) | `sk-ledger` (agreement engine), `sk-report` (statement PDFs), hub checks and seals |
| 4b | `sk-ui`, `sharekeep-app` (Windows, macOS) |
| 4c | `sk-webapp` (phones and PC browsers) |
| 4d | the hub as a service, OS-sealed keys, installers, release workflow, keep-launcher |

## Licence

MIT OR Apache-2.0, at your option. See `NOTICE`.
