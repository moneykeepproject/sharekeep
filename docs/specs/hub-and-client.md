# Household service and client (milestone 3)

What `sk-hub` and `sk-client` must do. From the owner's "ShareKeep Design Spec v0.1"
(sections Wi-Fi hub, Data model) and the decisions table (own service and certificate,
pairing code copied from MoneyKeep).

## The hub (`sk-hub`, program `sharekeep-hub`)

- One computer per household runs it; it is the only writer to the books, which live in
  its data folder (`books/`). Creating a book takes a name only: no file dialog.
- **Listeners** (ShareKeep's own, apart from MoneyKeep's capture server on 14443/14080):
  - devices: HTTPS on `<hub address>:15443`, TLS (rustls) relayed to a private loopback
    HTTP server (4 workers); at most 64 connections, 8 from one address; a request must
    arrive within 10 s until it carries a device token; a request without a token that sends
    a body (pairing) first takes one of 2 places (1 per address), so guests never hold the
    workers paired devices need; refused ones are closed and answered 503;
  - the certificate page: plain HTTP on `<hub address>:15080` — `/` (steps for Android,
    iPhone/iPad and computers, the fingerprint), `/sharekeep-ca.crt`,
    `/sharekeep-ca.mobileconfig`; anything else redirects to HTTPS. The page (and the steps
    in `sk_view::phone`, shared with the desktop's EZ Start) has the person **compare the
    fingerprint with the hub computer first**, and says to stop if it differs, before any
    download button: pairing starts on plain HTTP, and that comparison is what tells the
    real hub from another device answering for it (review M8). A newly paired device works at once: the owner decided
    (2026-11-17) against a confirmation click on the hub computer. A small front passes
    it to its own loopback server: at most 32 connections, 8 from one address, each closed
    after 15 s without traffic;
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
  record instead of adding another. Bodies must be JSON objects. The hub computer adds only
  member-add, agreement-add and schedule records and an agreement's **first** rule proposal
  (in the name of a person picked; the others approve on their devices), and changes book
  settings after the engine checks them. Everything else (approvals, payments, expenses,
  closes, restore acceptances…) comes from the person's own device; the engine also refuses
  an approval or restore acceptance whose device is `hub`, and no device can be paired under
  that name.
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
  history has a `restore` this device doesn't already hold, sitting right after a record this
  device has unchanged (`restore.seq == restored_to.seq + 1`), the sync answers `Restored`
  with an offer instead (which records aren't in the backup); a restore the device already
  holds never excuses a later change;
  `accept_restore` drops the device's later records, fetches the hub's, and the app then
  submits `restore-accept`. Until then the device sends nothing. A record whose kind isn't in
  `sk_ledger::KINDS` (the hub runs a newer ShareKeep) stops the sync: the device keeps the
  history up to the record before it and says to update ShareKeep.
- **Starting from a checkpoint**: a checkpoint given when pairing (`Replica::expect`) must be
  in the hub's history before the device keeps anything; otherwise the sync answers
  `CheckpointNotKept` and the device stays empty until the history contains it or the member
  forgets the checkpoint (`forget_expected`).
- The admin `lost` answer also has `text`: the same list in plain words, to save and enter
  again. "Last synced" is kept for
  the offline banner and "This device last saw record N on <date>".

## Later (milestone 4)

Installing and running the hub as a Windows service and a macOS launchd daemon (with the
data-folder permissions and the PowerShell lessons from MoneyKeep), keys sealed by the
operating system (4d), and the web app the hub serves (4c). The agreement engine's checks
and period close with seals arrived in 4a.

## Attached files

`POST /api/v1/files` (a paired device; body: the file, at most 10 MB; JPEG, PNG, WebP or PDF,
told by its first bytes) keeps the file in `files/<book id>/<sha256>` in the hub's data folder
and answers `{sha256, mime, size}`. `GET /api/v1/files/<sha256>` returns it. An `attachment`
record is accepted only once the hub holds the file it names, with that size and kind. Files
are never deleted (a removal is a record; the history still names the file). The desktop app
keeps a file it opened in `<book folder>/files/` and opens it with the computer's own viewer
(explorer.exe, open, xdg-open), after checking its SHA-256.
