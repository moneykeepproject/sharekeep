# The web app (milestone 4c)

Phones and PC browsers use ShareKeep through a small installable web app (a PWA) that the hub
serves on its HTTPS port, `https://<hub address>:15443/`. `sk-webapp` holds its files (built
into the hub binary); `sk-view` works out what every screen shows, for both this app and the
desktop app.

## Why plain JavaScript and not WebAssembly

The plan was a WASM build of the Slint screens. The web app is instead plain HTML, CSS and
JavaScript (about 600 lines, no outside code, no build step), and everything that needs the
engine runs on the hub:

- `GET /api/v1/view`: every screen's content for this device's member (`sk_view::view::snapshot`).
- `POST /api/v1/preview {expense}`: the live split while an expense is typed.
- `POST /api/v1/examples {rule}`: the rule editor's examples.

So the money arithmetic has one implementation (Rust, on the hub), phones download ~40 KB
instead of a multi-megabyte WASM file, Slint's WASM text input (weak on phones: no autofill,
awkward keyboards) is avoided, and the app builds with the rest of the workspace with no wasm
toolchain. The cost: the expense preview and rule examples need the hub (offline, an expense
still saves to the outbox, without a preview).

## Pairing

The hub computer's QR code opens the certificate page (`http://<address>:15080/?pair=<code>`).
Step 1 installs the certificate; step 2's button opens `https://<address>:15443/#pair=<code>`,
where the app asks for the household PIN and a device name. The fragment never reaches the hub
or a log; the app removes it from the address bar at once. The token is kept in the browser's
local storage for this site only.

## Behaviour

- Screens as the desktop app: Home, an agreement's Activity / Members / Rules / Periods,
  Approvals, Settings; sheets for an expense, a payment, a rule proposal (the same slider rules:
  percentages always total 100, locks, opt-out switches).
- Expenses, payments and approvals go into an outbox in local storage and are sent in order;
  one the hub refuses is shown in Settings and never resent. Rule proposals, disputes, deletes
  and period closes need the hub and say so when it can't be reached.
- Syncs every 30 s while visible, when the page becomes visible, and when the browser comes
  back online.
- The service worker keeps the app's own files for opening offline; `/api/` is never cached.
- Delete needs a second tap within 4 s (no browser modal dialogs).
- Security: CSP `default-src 'self'` with no inline script or style; text is always set with
  `textContent`; no outside requests (`sk-webapp`'s unit tests check these).

## Testing

`cargo test -p sk-webapp` checks the files; the hub's tests fetch the app over TLS and check
the three endpoints. `crates/sk-webapp/browser-test.py` runs the whole app in headless Chrome
against a real hub (pairing from the QR link, an expense with its split, a percentage rule,
a payment made offline and sent later, exact and itemized rules, an itemized receipt, the
first checkpoint offered after pairing, Verify, checking a kept and a changed checkpoint, and
a period's CSV); it needs Python's playwright and is not part of CI.

## The certificate page

The hub's certificate page (`http://<address>:15080/`) shows the iPhone and Android steps from
`sk_view::phone`, the same copy the desktop's EZ Start step 5c shows.

## Hyperbook

- The app remembers the last head it saw (`seen`), the last 30 heads it checked (`trail`)
  and the restore records the history already held (`knownRestores`). Each sync asks the hub
  for `seen`; if it changed, only a restore the phone hasn't met before, right after the
  record it continues from, and with the history up to that record matching the phone's
  latest pin there, becomes an offer (Accept, read-only until then). Anything else is the
  warning that the history was changed. The phone never carries on silently because the hub
  says the member accepted elsewhere: it still offers, and Accept then only agrees on this
  device.
- Settings > Hyperbook: the head, "last saw", Save a checkpoint (copy, save as a file),
  Check a checkpoint (against the hub), Check a statement (`crypto.subtle` SHA-256 of a
  picked PDF, matched against the view's seals; the file stays on the device). Print prints
  only the checkpoint sheet. Joining's last step takes an optional checkpoint ("I have a
  checkpoint"): the first sync checks the hub against it. If it isn't there, a warning offers
  "Forget the checkpoint" (two taps); a household restore covering it becomes the usual
  accept offer. The "history was changed" warning offers "Accept the hub's history". The first
  checkpoint is offered after pairing and after closing a period.
- Settings > Verify (`/api/v1/verify`); Periods > Export to a spreadsheet (CSV).

## Never an empty screen

`boot.js` loads before `app.js` (same origin, so the CSP allows it). If the app can't start (a
script error, data it can't read, or `app.js` not running within 4 s of loading), the page says
"ShareKeep couldn't start on this device" with the error, **Try again** (reload) and **Start over
on this device** (two taps: forgets this device's pairing and saved data, the kept files and the
service worker, then pairing starts again; nothing in the household's book is lost). Every screen
is drawn inside a guard that shows any error the same way.

## Help and questions

`sk_view::help::FAQ` is the one list of questions and answers (a blank screen, clearing the
browser's data for ShareKeep, the hub can't be reached, pairing codes, wrong person or a new
phone, what is lost). The hub serves it at `/help` (no script, `app.css`, the same security
headers; the service worker keeps a copy). The web app's Settings has **Open help**; the
"couldn't start" screen links to `/help#blank`. The desktop app shows the same text in
Settings > Help and questions.

## Pictures and PDFs on expenses

A new expense can take up to 5 pictures or PDFs ("+ Picture or PDF"); an expense already
recorded takes them from its card ("+ Picture"), in a closed period too. A photo is shrunk on
the phone to 1600 px on its long side (a JPEG of a few hundred KB) before it is sent; a PDF
goes as it is (up to 10 MB). Files are sent to the hub at once (they need the hub); the
`attachment` records wait in the outbox with the expense and follow it. Tapping a file shows
it (a picture) or offers Open and Save, after checking its SHA-256 against the book; "Take it
off this expense" (two taps) records a removal.
