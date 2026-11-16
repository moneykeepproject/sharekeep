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
  the keeper (a keeper's own proposal is in force at once).
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

## Statement PDFs (`sk-report`)

US Letter, built-in Helvetica (GroupKeep's page writer), header band, "Who owes whom", members,
by category, entries (disputed rows highlighted), page numbers. The hub saves it as
`statements/<book id>/<agreement>-<first>-<last>.pdf`; in a Hyperbook it then appends a `seal`
record with the PDF's SHA-256. Devices download it from `/api/v1/statements/<agreement>/<first>`.

## Not yet

Spending-deposit top-ups on a schedule, interest crediting on a schedule, and recurring bills
posting expected entries come with the app screens that drive them (4b).
