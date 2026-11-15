# Split engine (milestone 1)

What `sk-core` and `sk-split` must do. Taken from the owner's "ShareKeep Design Spec v0.1"
(sections Split methods, Deposits, Settlement and leaving); a change of behaviour starts
here.

## Money

- Every amount is an integer number of minor units (`Cents`, an `i64`). No floating point.
- A negative amount is a refund: it divides exactly like a charge, mirrored.
- Every split adds up to the entry's amount to the cent.

## Methods

| Method | Input | Notes |
|---|---|---|
| Equal | members | |
| Percentages | basis points per member | included members total exactly 10,000 (100%) |
| Shares | whole-number weights | |
| Exact amounts | fixed amounts + a remainder member | refused if the fixed amounts exceed the total |
| Itemized | receipt lines, each shared equally by its members | tax, tip and fees = amount − lines, spread pro rata to each member's lines |
| By income | ratio parts | no salaries needed; every input is visible to all members |
| By room | per-room weights | area or rent per room |
| By days present | days per member, per entry | 0 days → pays nothing |
| By usage | readings or counts, per entry | |
| Opt-out | any method with excluded members | excluded members pay nothing |

A rule names its members; every included member must have an input (a missing one is an
error, never a silent zero), and no input may name someone outside the rule.

## Caps and floors

Per member, per entry, for the weighted methods (not exact amounts or itemized). A member
over a cap is fixed at the cap and the rest is re-divided among the others by the same
weights; floors likewise. If no division can meet them, the entry is refused. For a refund
they limit the size of the refund share. (Per-period caps come with the period engine.)

## Rounding

Each share is the exact share rounded down; the leftover cents (fewer than the members)
are given out by the rule's rounding setting:

- **Rotate** (default): among members whose exact share has a fraction, in member order,
  starting at (entry sequence × leftover) so a run of identical entries hands out cents in
  one even cycle. After any number of identical entries, members' totals differ by at most
  1 cent.
- **Largest fraction first** (ties: lower member id).
- **Payer**: all leftover cents to the payer, when the payer has a share.

## Rule versions

A rule has versions with effective dates. An entry uses the version in force on its date
and stores the rule id, version date and resulting cents, so a later change never rewrites
it. Before a rule's first version no rule is in force (an error). An entry's rule is its
own choice, else its category's default, else the agreement's fallback.

## Balances and settle-up

- Balance = paid − owed; balances always sum to zero.
- A pot-paid expense does not move balances; it moves the pot's equity.
- Settle-up clears every balance with at most (members with a balance − 1) payments,
  largest debt to largest credit first.
- "Net between us" sums two members' settle-up payments across agreements.

## Deposits

- Each deposit tracks per-member stakes; stakes always sum to the deposit's balance.
- Held: pay in, buy-out (ownership moves, money stays with the holder), deduction (split
  by a chosen rule), refund (paid out by stake; refused for non-refundable, and the
  non-refundable part of a partly refundable deposit stays).
- Spending: pay in, spend (each member's equity falls by their share). A bill the pot
  can't cover is refused (Block) or split into a pot part and a member-paid part (Debts).
- Interest: a book setting in basis points a year, default 0; credited by stake.

## Joining and leaving

- Days present in a period count the join and leave days; members outside the period
  have 0 days.
- Prepaid items prorate by days used, rounded down.
- The leaving statement lists the member's balance, spending equity (settled now) and
  held stakes (pending until bought out or refunded).
