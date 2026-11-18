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

## EZ Start

Setup shows only the immediate step: its number ("Step 2 of 5", with progress dots), a title,
a **Why:** box saying what the step is for, and only that step's fields and one main button.
No tabs or other screens while it runs. Back works from every step after the first; a step
that's already done shows what was done ("✓ The PIN is set", "✓ Working in Home") with Next
to keep it, or the way to change it (a new PIN; close the book to create another). Next never
goes past where setup stands.

Step 4 (and the Hub tab's new agreement) has the agreement's options: settle up weekly, every 2
weeks, monthly (a day 1–28, or 31 for the last day) or every N days; when the first period starts
(empty: the start of the period today is in, `controller::agreement_schedule`); and whether
expenses split equally from the start (a first rule, "Shared equally", proposed in the name of
the first person picked, for the others to approve) or a rule is set up later.

Step 5 walks through the phone's side too, one part at a time, so the person doing it reads at
the computer and acts on the phone: 5a whose device (code and QR code), 5b scan the code with the
phone's camera, 5c install the certificate (iPhone or Android steps, from the same copy as the
phone's certificate page, `sk_view::phone`, and the fingerprint to compare), 5d open the
household page, 5e join from the phone (PIN, device name, optional checkpoint), 5f check that it
joined (the devices list), then pair another device or finish.

Calls to the hub computer's port escape the path (`worker::escape_path`): a book's file name can
have spaces, which otherwise broke the request (the hub answered 400).

- **Joining** (a device that isn't paired; desktop and web app): 1 Find your household's hub
  (address), 2 Check it's really your hub (fingerprint: "They match" / "They don't match"),
  3 Pair this device (code, PIN, name). The web app's steps are 1 pairing code, 2 household
  PIN, 3 device name; a code that came with the QR code's link skips step 1.
- **The hub computer** (until finished or skipped; `ez_done` in the settings; reopened from the
  Hub tab): 1 Choose the household PIN, 2 Create the household's book, 3 Add the people (at
  least two), 4 Create your first agreement, 5 Pair the first device. The step follows where
  setup stands (a step already done is skipped), so EZ Start resumes where it was left.

## Hyperbook, Verify, recovery

- Settings > Hyperbook: the head (record and fingerprint), "This device last saw record N on
  <date>" (a quiet line after 30 days), Save a checkpoint (text, QR code, save as a file),
  Check a checkpoint (this device's copy, then the hub), Check a statement (a PDF from an
  open dialog, hashed by the worker). Offered as a toast after pairing and after closing a
  period.
- Pairing (EZ Start step 3) takes an optional checkpoint; if the hub's history doesn't
  contain it, Settings says so with "Forget the checkpoint". Checkpoints also save as a PDF to
  print. The recovery screen saves "what would be lost" as a text file.
- Settings > Verify runs `sk_ledger::audit` on this device's own copy.
- Periods: Export to a spreadsheet (CSV), written by the worker.
- A restored backup shows on Home and Settings with "Accept and carry on"; nothing is sent
  before.
- Hub tab: a broken book has "Recover": its backups (checked), what a backup loses, Restore
  it, and Save the broken history as it is.
- One form sheet (`forms.rs`) for deposits and pots, pay-ins, refunds, buy-outs and
  deductions, recurring bills, special events, schedule changes, joining and leaving.
- Lists with text fields (days, usage, receipt lines, exact amounts) are updated row by row,
  so a field keeps the cursor while someone types.

## The first question (0.1.4)

A computer that isn't paired and isn't the hub starts with a choice (page 7):
**Join my household's hub** (the joining steps; their step 1 has Back to the choice) or
**Make this computer the hub**. Nothing is remembered about a choice that wasn't finished; the
choice comes back at every start until the device is paired or the computer is the hub. The
start-up table (`hubhere::first_page`, unit-tested):

| Hub here? | State | First screen |
|---|---|---|
| yes | hub EZ Start not finished | hub EZ Start (page 6) |
| either | paired | the app (page 1) |
| yes | EZ Start finished, not paired | Hub tab (page 5) |
| no | not paired | the choice (page 7) |

"Make this computer the hub":
- **Windows:** offered only when the app runs from Program Files with `sharekeep-hub.exe` beside
  it. The screen says what happens (Windows asks for administrator permission; the hub runs in
  the background from now on; give the computer a fixed address first), then the worker runs
  `sharekeep-hub.exe make-hub --user <this account>` with `ShellExecuteExW` and the `runas`
  verb, hidden, waits and reads the exit code (`sk_client::makehub::Exit`, the same messages
  as Setup). Declining the prompt returns to the choice silently. On success the app finds the
  hub (`hubhere::find`), gives the worker its admin port (`Job::SetAdmin`) and goes to the
  hub's EZ Start without restarting.
- **Mac:** the app opens its own copy of "Install the household hub" (in
  `ShareKeep.app/Contents/Resources`) in Terminal, which asks for the password; nothing of
  ShareKeep's runs as root. The app looks for the hub every 3 s (and on "I've done it").
- A hub folder that is found but can't be used (not owned by the administrators, or this
  account may not read `admin.token`) is said under the second button, with what to do.
- Dev builds: `--hub-data <folder>` still works; a build not in Program Files shows the
  choice with the second button explained but not offered.

