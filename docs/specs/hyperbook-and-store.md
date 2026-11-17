# Hyperbook and book files (milestone 2)

What `sk-chain` and `sk-store` must do. From the owner's "ShareKeep Design Spec v0.1"
(sections Hyperbook, Data model, Wi-Fi hub: backups) and the macOS file-dialog lesson.

## History records (`sk-chain`)

- Every change to a book is a record: sequence number (from 0), hub timestamp (ms, UTC),
  author (member, or none for the hub; device name ≤ 128 bytes), kind (1–64 of `a-z 0-9 _ . -`),
  JSON body (≤ 1 MiB, no fractional numbers), the previous record's hash, and its own hash.
- The hash is SHA-256 over a fixed encoding: `SKCHAIN1`, seq (u64 BE), ts (i64 BE), member
  flag + id (u32 BE), then device, kind and body as length-prefixed (u32 BE) bytes, then the
  previous hash. The body is canonical JSON: keys in byte order, no whitespace.
- Record 0 is `genesis` with the previous hash all zeros. A period seal is a `seal` record
  holding the agreement, the period, the head at closing and the statement PDF's SHA-256.
- Verification reports the first record that fails: wrong sequence (missing, repeated,
  reordered), wrong previous hash, or contents that don't match the hash. A member's app
  keeps the last head it saw; a chain that no longer contains it was rewritten.
- Export: one JSON record per line. `sk-verify <file> [<seq>:<hash>]` checks an export
  (exit 0 verified, 1 broken, 2 usage).
- Honest limit: a chain shows tampering; it can't stop someone who controls the hub PC from
  rewriting everything. The members' stored heads catch that.

## Book files (`sk-store`)

- One SQLite database per book, extension `.sharekeep`. Tables: `meta` (format, schema
  version, book id, name, currency, Hyperbook flag, creation time) and `records` (the
  history). Triggers refuse any update or deletion of a record.
- Every book's history is chained. A Hyperbook (chosen at creation, never changed) is
  verified completely every time it opens, and refuses to open if broken. A normal book
  opens without the check; `verify` still reports a break.
- Book settings (`deposit_interest_bp`, `dispute_mode`, `grace_days`, `one_keeper`) are
  changed only by appending a `setting` record; the latest one wins.
- Creating a book never opens a file dialog: the household service passes its own books
  folder and the name the user typed. The file name keeps letters, digits, spaces and
  `-_.,()'&`, is at most 80 characters, avoids Windows' reserved names, and takes the next
  free number (`Home (2).sharekeep`) rather than replace a file.
- One hub per book: a plain book holds SQLite's exclusive lock while open; an encrypted
  book holds an exclusive file lock. A second open is refused as "in use".
- Encryption (optional, chosen at creation): MoneyKeep's format with ShareKeep's magic
  `SKBOOKE1` and recovery keys `SKRK-…`. Argon2id password key (64 MiB, 3 passes) and an
  optional recovery key wrap one random data key; XChaCha20-Poly1305 encrypts the whole
  database image. The open book lives in memory; every change saves the encrypted image
  atomically and keeps the previous file as `<file>.prev`. A failed save puts the
  in-memory book back to the last saved file.
- Schema migrations run in order in one transaction; a book from a newer ShareKeep is
  refused with a message to update. Book version 3 (no table change) marks that the history
  may hold record kinds a version-2 ShareKeep doesn't know (`recurring`, `restore`,
  `restore-accept`), so an older version refuses the book instead of skipping records.
  Book version 4 (no table change) does the same for `attachment` records.
  Opening for recovery (a broken book, a backup) never upgrades or otherwise changes the
  file; a restored copy is upgraded when it opens. A SQLite file that isn't a ShareKeep book is refused
  and left untouched.
- Backups go to a folder as `<name> backup <label>.sharekeep` (encrypted books stay
  encrypted); a restore makes a separate copy `<name> restored <label>.sharekeep` and never
  replaces anything.

## Checkpoints (`sk_chain::checkpoint`)

A head written as one line, `ShareKeep checkpoint · <book> · record N · <date> · <64 hex>`,
and a QR code, for keeping outside the house. `parse` is forgiving (case, spaces, line breaks
inside the fingerprint; a book named "Records"). `check` answers kept, rewritten, or missing
(the history ends earlier). The hub doesn't store checkpoints: it is the party being checked.
QR codes carry the line with `|` separators (`for_qr`), because scanners can garble `·`;
`parse` reads both. `sk_report::checkpoint_pdf` prints a checkpoint: the line, the fingerprint
in groups of four for retyping, and the QR code.

## Recovery

`Store::open_for_recovery` opens a book without verifying it (reading only).
`scan_backups(dir, book_id)` lists a book's backups newest first, each checked (ok, broken at
record N, or unreadable). `set_aside_broken` renames a broken book to
`<name> (broken <date>).sharekeep-broken`, which no longer loads as a book but is kept.

## Later

Changing an encrypted book's password or recovery key and copying a normal book into a new
Hyperbook.
