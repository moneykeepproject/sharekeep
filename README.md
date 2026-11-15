# ShareKeep

Keep a household's shared money: partners, roommates, or a parent and an adult child
sharing a home. Part of the MoneyKeep family (Rust + Slint).

**Status: in development.** Milestone 1 (this commit) is the split engine; there is no app
to run yet.

## Build & test

```sh
cargo test --workspace      # the split engine and its property tests
cargo xtask ci              # fmt, clippy -D warnings, tests, repository rules
```

## Layout

| Path | What |
|---|---|
| `crates/sk-core/` | Money (integer cents, currencies), ids, date spans. No I/O. |
| `crates/sk-split/` | The split engine: ten methods, caps and floors, rounding, rule versions, balances, settle-up, deposits, join/leave proration. No I/O. |
| `crates/sk-split/tests/properties.rs` | Property tests: thousands of seeded random cases. |
| `xtask/` | `cargo xtask ci`, `workflows`, `guards`. |
| `docs/specs/` | What the implemented parts must do. |

## Planned

| Milestone | Crates |
|---|---|
| 1 (done) | `sk-core`, `sk-split` |
| 2 | `sk-chain` (Hyperbook hash chain), `sk-store` (SQLite book files) |
| 3 | `sk-hub` (household service, pairing), `sk-client` (API client, outbox) |
| 4 | `sk-ui`, `sharekeep-app` (Windows, macOS), `sk-webapp` (phones and PC browsers), `sk-report` (PDFs), installers |

## Licence

MIT OR Apache-2.0, at your option. See `NOTICE`.
