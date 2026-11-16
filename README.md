# ShareKeep

Keep a household's shared money: partners, roommates, or a parent and an adult child
sharing a home. Part of the MoneyKeep family (Rust + Slint).

**Status: in development, release-ready pieces in place.** Milestones 1–4d: the split engine,
the Hyperbook chain, the book files, the household service and its client, the agreement
engine and statement PDFs, the desktop app for Windows and macOS, the web app for phones and PC
browsers, the hub as a Windows service or macOS daemon, installers and the release workflow.

## Install

- **Windows:** run `sharekeep-<version>-windows-x64-setup.exe`. On the one computer that keeps
  the household's books, tick **Household hub on this computer**.
- **Mac:** open the DMG and copy ShareKeep to Applications. On the hub Mac, open **Install the
  household hub**.
- **Phones and other computers' browsers:** on the hub computer, ShareKeep > Hub > pick a person:
  scan the QR code.

Give the hub computer a fixed address on the router (a DHCP reservation).

## Build & test

```sh
cargo test --workspace      # the split engine and its property tests
cargo xtask ci              # fmt, clippy -D warnings, tests, repository rules
cargo run -p sk-chain --bin sk-verify -- history.jsonl   # check an exported Hyperbook history
cargo run -p sharekeep-app --example screens -- shots    # headless screenshots of every screen
```

Trying it on one computer from a checkout:

```sh
cargo run -p sk-hub -- setup --data hubdata --address 192.168.1.20   # this computer's address
cargo run -p sk-hub -- run --data hubdata                            # leave it running
cargo run -p sharekeep-app -- --hub-data hubdata                     # the app, with the Hub tab
```

On the Hub tab: set the PIN, create a book, add people and an agreement, then make a pairing
code. Other computers run `sharekeep` and type the hub's address, the code and the PIN.

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
| `crates/sk-view/` | What every screen shows, worked out from a book's state; a sample household. |
| `crates/sk-ui/` | The desktop app's screens (Slint, the Capture look). |
| `crates/sk-webapp/` | The web app for phones and PC browsers (HTML, CSS, JavaScript), built into the hub. |
| `apps/sharekeep-app/` | The desktop app (`sharekeep`) for Windows and macOS: a background worker for the hub, file dialogs, settings. |
| `installer/`, `packaging/` | The Windows setup (NSIS), the Mac DMG, the scripts MoneyKeep Launcher's local mirror runs. |
| `assets/` | The icon (SVG source, PNG, ICO, ICNS). |
| `xtask/` | `cargo xtask ci`, `workflows`, `guards`. |
| `docs/specs/` | What the implemented parts must do. |

## Planned

| Milestone | Crates |
|---|---|
| 1 (done) | `sk-core`, `sk-split` |
| 2 (done) | `sk-chain` (Hyperbook hash chain), `sk-store` (SQLite book files) |
| 3 (done) | `sk-hub` (household service, pairing), `sk-client` (API client, outbox) |
| 4a (done) | `sk-ledger` (agreement engine), `sk-report` (statement PDFs), hub checks and seals |
| 4b (done) | `sk-ui`, `sharekeep-app` (Windows, macOS) |
| 4c (done) | `sk-view`, `sk-webapp` (phones and PC browsers) |
| 4d (done) | the hub as a service, OS-sealed keys (Windows), installers, release workflow, keep-launcher entry |

## Licence

MIT OR Apache-2.0, at your option. See `NOTICE`.
