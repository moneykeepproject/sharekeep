# Household service and client (milestone 3)

What `sk-hub` and `sk-client` must do. From the owner's "ShareKeep Design Spec v0.1"
(sections Wi-Fi hub, Data model) and the decisions table (own service and certificate,
pairing code copied from MoneyKeep).

## The hub (`sk-hub`, program `sharekeep-hub`)

- One computer per household runs it; it is the only writer to the books, which live in
  its data folder (`books/`). Creating a book takes a name only: no file dialog.
- **Listeners** (ShareKeep's own, apart from MoneyKeep's capture server on 14443/14080):
  - devices: HTTPS on `<hub address>:15443`, TLS (rustls) relayed to a private loopback
    HTTP server;
  - the certificate page: plain HTTP on `<hub address>:15080` — `/` (steps for Android,
    iPhone/iPad and computers, the fingerprint), `/sharekeep-ca.crt`,
    `/sharekeep-ca.mobileconfig`; anything else redirects to HTTPS;
  - the hub computer's desktop app: plain HTTP on `127.0.0.1:15081`, with the admin token
    from the data folder.
- **Certificate authority** (copied from MoneyKeep): ECDSA P-256, 10 years, name-constrained
  to the hub's address (and local name), "ShareKeep Hub CA (<computer>)". The server
  certificate (825 days) is renewed 30 days before it ends, checked hourly, without dropping
  devices. Changing the address makes a new authority (devices trust it again).
- **Pairing**: the hub computer makes a pairing code for one member of one book (8 characters
  shown as `ABCD-EFGH`, typed forgivingly; one use; 10 minutes; the QR code carries
  `https://<host>:15443/#pair=<code>`). The device sends the code, the household PIN and a
  device name. Five wrong PINs burn a code; ten failures in 10 minutes pause all pairing for
  10 minutes. The PIN (6–12 digits) is kept only as an Argon2id hash.
- **Devices** get a 32-byte random token; the hub keeps its SHA-256 in `hub.sqlite`. A
  revoked device is refused at once. Pairings survive restarts and upgrades (the registry
  has migrations; a newer one is refused with a message to update).
- **Records**: a device reads its book's history in pages (≤ 1,000) and adds records of the
  device kinds (expense, payment, approval, dispute, entry-edit, entry-delete,
  rule-proposal, deposit, member-join, member-leave, period-close, schedule, recurring,
  restore-accept). Since 4a every
  record is checked by the agreement engine first (`docs/specs/agreement-engine.md`). The hub stamps each
  record with the device's member and name; a request id makes a resend return the first
  record instead of adding another. Bodies must be JSON objects. The hub computer may also
  add member-add, agreement-add and schedule records and change book settings.
- **Encrypted books** stay locked after a restart until someone at the hub computer enters
  the password (or recovery key); devices get "locked" (423) meanwhile.
- **Backups**: one a day, made with the day's first change, in `backups/`.
- **Guided recovery**: a Hyperbook that doesn't verify is listed with `broken: N` and isn't
  opened. The hub computer's admin port answers `POST books/<file>/recovery` (the problem and
  every backup, checked), `lost {backup}` (the broken book's records after the backup: what
  restoring it loses, unverified), `export-broken` (the history as it is) and `restore
  {backup}`: a restored copy becomes the open book, the broken file is set aside, and a hub
  `restore` record is appended. Nothing is deleted.
- **Verify and CSV** for the web app: `GET /api/v1/verify` and
  `GET /api/v1/csv/<agreement>/<first day>` (`{file, text}`).
- **Keys** (the authority's and the server's) are files in `keys/` readable only by the
  hub's account. Logs carry method, path and status only.

## The client (`sk-client`, used by the desktop app on every PC)

- HTTPS trusting only the hub's authority, with the device token.
- **Outbox**: only expenses, payments and approvals can wait offline; edits, rule changes and
  everything else are sent now or not at all. Items keep their order and survive restarts;
  each is saved after it's sent. An item the hub can't take now (offline, 5xx, 408, 429)
  stops the flush and waits; an item the hub refuses outright moves to a refused list for
  the person to see.
- **Replica**: the device's copy of the history, checked as it loads and as it syncs. Each
  sync asks again for the device's own head: if the hub no longer has it unchanged, the
  history was rewritten — the sync stops, warns, and keeps the copy as it was. If the hub's
  history has a `restore` continuing from a record this device has unchanged, the sync
  answers `Restored` with an offer instead (which records aren't in the backup);
  `accept_restore` drops the device's later records, fetches the hub's, and the app then
  submits `restore-accept`. Until then the device sends nothing. "Last synced" is kept for
  the offline banner and "This device last saw record N on <date>".

## Later (milestone 4)

Installing and running the hub as a Windows service and a macOS launchd daemon (with the
data-folder permissions and the PowerShell lessons from MoneyKeep), keys sealed by the
operating system (4d), and the web app the hub serves (4c). The agreement engine's checks
and period close with seals arrived in 4a.
