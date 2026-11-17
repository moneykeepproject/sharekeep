# Agreement engine and statements (milestone 4a)

What `sk-ledger` and `sk-report` must do, and how the hub uses them. From the owner's
"ShareKeep Design Spec v0.1" (Data model, Rule editor and approval, Intervals, Deposits,
Settlement and leaving) and the decisions table.

## State from history

A book's state is its history replayed record by record: members, agreements (members with
join and leave dates, rule versions, schedule, events, deposits, closed periods), entries
(with their split worked out when posted), proposals and book settings. The hub replays a
book when it opens; devices replay their own copy with the same code, so everyone sees the
same numbers. A record from before a check existed that no longer applies is skipped and
listed; a book always opens.

## Checks on every new record

The hub applies each new record to a copy of the state first; if it fails, the record is
refused (HTTP 422) with a message for the person who sent it, and nothing is written.

| Kind | Who | Checks |
|---|---|---|
| member-add, agreement-add | hub computer only | unique ids, names 1–64 characters, members exist, valid schedule |
| schedule (change / event) | members | a change starts after the current period; event dates in order, event members in the agreement |
| rule-proposal | a member | the rule is valid and names only agreement members; no second version on the same date |
| approval | a member asked to approve | the proposal is still waiting; one decline declines it |
| member-join | members | the person is in the book, not already in the agreement, date not in a closed period |
| member-leave | a member | proposal; the leaving member and everyone staying approve |
| expense | members | amount 1 cent to one billion, category 1–64, a rule applies on that date, the split works, the date isn't in a closed period, an event's dates and members, the pot covers it (or the shortfall rule) |
| payment | the payer or the person paid | both in the agreement, different people, more than zero, not in a closed period |
| entry-edit, entry-delete | members | entry exists, isn't deleted, isn't in a closed period; an edit stays in its agreement; a pot gets back what the old entry took |
| dispute | members | the entry exists; `resolved` clears it |
| deposit (create, pay-in, buy-out, deduct, refund) | members | as `docs/specs/split-engine.md` (Deposits) |
| period-close | members | the period has ended (the record's UTC day is after it) and isn't already closed |
| setting | hub computer | `dispute_mode`, `one_keeper`, `deposit_interest_bp`, `grace_days` |

## Rules and approvals

- A proposal needs every other member active that day; a one-keeper household needs only
  the keeper (a keeper's own proposal is in force at once). `one_keeper` is chosen before the
  book's first rule proposal; afterwards the setting is refused.
- An approved rule version is added with its effective date; its categories get it as their
  default; the agreement's first rule (or one marked `fallback`) is the fallback.
- On an expense's date, members not in the agreement (or not in its event) are excluded from
  the rule. Equal and weighted rules re-divide; a percentage rule no longer totals 100% and
  the expense is refused with "the rule … needs updating" until a new version is approved.

## Balances

All live entries count, plus every close's per-period cap adjustments. Disputed entries count
unless the book's `dispute_mode` is `held_aside`. Balances always sum to zero.

## Closing a period

Closing locks the period (no new, edited or deleted entries dated in it), posts the per-period
cap adjustments, and produces the statement: total and by category, per member what they paid,
their share, payments made or received, cap adjustments and running balance, every entry
(disputed ones marked), and the payments that settle everyone up. Event entries settle on
their own and aren't on a period's statement.

## Restores (Hyperbooks)

`restore` is written only by the hub when it carries on from a backup (guided recovery): it
names the backup and the record it continues from (`restored_to {seq, hash}`, which must be
the record right before it) and where the broken book broke. `restore-accept {restore}` is a
device's acceptance; the ledger lists each restore with the members who accepted it.

## Verify (`sk_ledger::audit`)

Two answers from the records alone: "Has the history been changed?" (Hyperbooks: the chain
verifies) and "Do the numbers add up?" (every record replays under the rules, every expense's
shares add up to its amount to the cent, every seal names a record whose fingerprint matches).

## Statement PDFs (`sk-report`)

US Letter, built-in Helvetica (GroupKeep's page writer), header band, "Who owes whom", members,
by category, entries (disputed rows highlighted), page numbers. The hub saves it as
`statements/<book id>/<agreement>-<first>-<last>.pdf`; in a Hyperbook it then appends a `seal`
record with the PDF's SHA-256. Devices download it from `/api/v1/statements/<agreement>/<first>`.
Every statement prints "History at closing: record N • fingerprint …" (the period-close
record's head), and "Check a statement" (Hyperbooks) matches a PDF's SHA-256 against the
seals. A period, open or closed, also exports as CSV and as PDF, both from one sheet
(`sk_view::view::period_sheet`): each entry with what it moved for each member and their running
balances, opening and closing rows, and in a Hyperbook the head at export. The CSV
(`view::csv`) has one row per entry; the PDF (`sk_report::period_pdf`) has each person's
opening, change and closing balance, then the entries with a column per person (deleted and
disputed highlighted). Devices: the desktop makes both from its own copy; the web app gets
`/api/v1/csv/<agreement>/<first day>` and `/api/v1/period-pdf/<agreement>/<first day>`.

## Recurring bills and pot top-ups (`recurring`)

`{"op": "add", "agreement", "id", "name", "schedule", "what"}` where `what` is
`{"type": "bill", "amount", "category", "payer"?}` or `{"type": "top-up", "deposit", "amounts":
{member: cents}}` (a spending deposit only); `{"op": "stop", "agreement", "id"}`. Any member adds
or stops one (online). A schedule's `start` is its first due date.

They post nothing by themselves: `Ledger::expected(agreement, today)` lists each occurrence from
the day it was added (up to 92 days back, 7 ahead, never in a closed period) that hasn't been
recorded: a bill once, a top-up once per active member. An expense with `"due": {"recurring",
"date"}` records a bill (the real amount may differ); a deposit pay-in with the same `due`
records a member's top-up. Each occurrence is recorded once; deleting the expense makes it due
again. The screens show these under **Due** with Record / I paid.

## Interest on held deposits

When a period closes and the book's `deposit_interest_bp` setting is above 0, each held deposit
is credited simple interest on its balance for the period's days (rounded down to the cent),
shared by the members' stakes. Replaying the history gives the same amounts.

## When a new rule starts

A rule proposal starts with the next period, so a period is never divided two ways. The
exception is while no rule is in force today (an agreement's first rule, or a first rule that
starts later): then the proposal applies from the current period (`sk_view::view::
proposal_target`), as an earlier version of the default rule when there is one, so today's
bills can be divided. When no rule divides an expense, the expense sheet says why in words
(`rule_missing`: the waiting rule and who must approve it, or when the rule starts) and offers
"Propose how it's split", with the expense's category filled in. The Rules tab lists rules that
start later ("starts Dec 1").

## Screens

The rule editor (desktop and web) offers every method: equal, percentages, shares, by income,
by room, by days present, by usage, exact amounts (a fixed amount for each person, one person
takes whatever is left; the examples add a sample to the fixed total so something is left) and
itemized. An expense under a days, usage or itemized rule asks for each person's days, their
reading, or the receipt's lines and who shares each (tax and tip, the amount minus the lines,
follow the lines): the hub's preview answers `needs` with kind `days`, `usage` or `items`.
Both apps create deposits and pots, pay-ins, refunds, buy-outs and deductions, recurring
bills, special events, schedule changes, joining and leaving.

## Attachments

`attachment {target, sha256, name, mime, size, remove}`: anyone in the agreement adds a picture
or PDF to an expense, or takes one off, at any time (closed periods included: no money moves).
At most 5 per expense; JPEG, PNG, WebP or PDF; at most 10 MB; not on a deleted expense or a
payment. An edit keeps them. Book version 4.
