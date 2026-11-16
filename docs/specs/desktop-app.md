# The desktop app (milestone 4b)

`sk-ui` holds the screens (`ui/app.slint`, `ui/components.slint`) and the view models
(`src/view.rs`) that fill them from a book's [`Ledger`]. The view models are plain Rust and are
tested without a window. `apps/sharekeep-app` is the desktop app for Windows and macOS.

## Screens

One column of cards in the Capture look (white cards on a soft background, teal accent),
centred and at most 640 px wide on a desktop. Bottom tabs: Home, Approvals, Hub (only on the
hub computer), Settings.

| Screen | Shows | Does |
|---|---|---|
| Join | hub address, the certificate's fingerprint, code, PIN, device name | fetch the certificate (port 15080), pair |
| Home | one card per agreement I'm in: where I stand, everyone's balance, this period, proposals waiting | open an agreement |
| Agreement · Activity | entries newest first, disputes marked | + Expense, Record payment, dispute/resolve, delete (open periods only) |
| Agreement · Members | balances, the payments that settle everyone up | |
| Agreement · Rules | the rules in force, their method, since when, the default | Propose a rule |
| Agreement · Periods | the current period and the five before it | close a period after it ends, save a closed period's statement PDF |
| Approvals | rule proposals and leaves waiting for me | approve, decline |
| Settings | who I am, the book, what waits to send, what the hub refused | sync, dismiss refused |
| Hub | the book it's working in, the hub's address and fingerprint, people, other books, devices, a pairing code with its QR code | close the book; open another (password if encrypted) or create one, only while none is open; set or change the PIN; add a person, create an agreement (monthly on the 1st), make a code (book open and PIN set), remove a device |

Sheets: New expense (with the live split as it's typed: `Ledger::preview_expense`), Record a
payment (suggests what settles me up), Propose a rule.

## The rule editor

Methods in the editor: Equal, Percentages, Shares, By income (the other methods come with the
web app). Each member has a toggle (opted out when off) and, except for Equal, a slider.
Percentages are whole numbers and always total 100: moving one slider moves the other unlocked
ones the other way, in proportion; a locked slider stays put and caps how far the others go.
Under the sliders, live examples divide $83.47, $100.00 and $1,234.56 (odd cents on purpose),
or say why the rule can't divide anything. The proposal takes effect at the start of the next
period, once everyone else approves.

## Threads and the hub

All network and disk work runs on one worker thread (`worker.rs`): the UI sends a `Job` and
gets an `Outcome` back through `upgrade_in_event_loop`. Expenses, payments and approvals go
through the outbox, so they wait while the hub is away; other changes are sent at once or
refused with "this change needs the hub". The app syncs every 30 seconds while open.

The app's settings (`sharekeep.json` in the app-data folder, owner-only on Unix) hold the hub's
address and certificate, the device token, the member and the book. The token is never shown
or logged. On the hub computer, `--hub-data <folder>` (remembered) points at the hub's data
folder; the app reads `admin.token` there and calls the loopback admin port.

## File dialogs

Only `dialogs.rs` uses `rfd` (a unit test and `cargo xtask guards` check). On macOS it uses
`rfd::AsyncFileDialog` through `slint::spawn_local`, never the blocking dialog, which crashes
winit on the Mac (MoneyKeep 0.1.1). Windows and Linux use the blocking dialog.

## One book at a time on the hub

The hub works in one book at a time (the owner's rule, 2026-11-16). Creating or opening a book
while another is open is refused (409, "Close it first"); `POST /admin/v1/books/<file>/close`
closes it, its pairing codes stop working and its devices get "locked or unavailable" until it
opens again. The hub remembers the book it works in (`current_book` in `hub.sqlite`) and opens
only that one at start (unencrypted; an encrypted one waits for its password). Buttons on the
Hub tab are enabled only when their action can work.

The pairing QR code opens the certificate page (`http://<address>:15080/?pair=<code>`), whose
step 2 links to `https://<address>:15443/#pair=<code>` and shows the code for typing into the
desktop app. (Before, the QR code went straight to port 15443, which a phone can't open until
the certificate is installed.)
