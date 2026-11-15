//! Joining and leaving: days present in a period, and the leaving statement.

use crate::deposits::{DepositKind, DepositState};
use crate::ledger::Balances;
use serde::{Deserialize, Serialize};
use sk_core::{Cents, DateSpan, DepositId, MemberId, NaiveDate};
use std::collections::BTreeMap;

/// A member's time in an agreement: from the join date, to the leave date if they left
/// (both days counted as present).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub joined: NaiveDate,
    pub left: Option<NaiveDate>,
}

impl Membership {
    /// Days present during `period`.
    pub fn days_in(&self, period: &DateSpan) -> u32 {
        let last = self.left.unwrap_or(period.last);
        DateSpan::new(self.joined, last)
            .and_then(|stay| stay.overlap(period))
            .map_or(0, |s| s.days())
    }
}

/// Days present for every member in `period`: the input to a by-days split of a recurring
/// bill when someone joins or leaves part way through.
pub fn days_present(
    members: &BTreeMap<MemberId, Membership>,
    period: &DateSpan,
) -> BTreeMap<MemberId, u32> {
    members
        .iter()
        .map(|(m, s)| (*m, s.days_in(period)))
        .collect()
}

/// The part of a prepaid item (annual insurance, a yearly subscription) used by
/// `days_used` of `days_total`, rounded down.
pub fn prorate(amount: Cents, days_used: u32, days_total: u32) -> Cents {
    if days_total == 0 {
        return 0;
    }
    let v = i128::from(amount) * i128::from(days_used.min(days_total)) / i128::from(days_total);
    v as Cents
}

/// What a leaving member settles, in one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveStatement {
    pub member: MemberId,
    /// Their agreement balance (after the current period's bills, prorated to the leave
    /// date, are posted): positive = they are owed.
    pub balance: Cents,
    /// Their equity in each spending deposit: paid back to them (or owed by them) now.
    pub equity: BTreeMap<DepositId, Cents>,
    /// Their stake in each held deposit: settled by a buy-out now, or pending until the
    /// holder refunds it.
    pub held: BTreeMap<DepositId, Cents>,
}

impl LeaveStatement {
    /// The amount settled when they leave: balance plus spending equity. Positive: the
    /// household pays them; negative: they pay the household.
    pub fn settle_now(&self) -> Cents {
        self.balance + self.equity.values().sum::<Cents>()
    }

    /// Held deposits not yet bought out or refunded.
    pub fn pending(&self) -> Cents {
        self.held.values().sum()
    }
}

/// Build the leaving statement from the agreement's balances and deposits.
pub fn leave_statement(
    member: MemberId,
    balances: &Balances,
    deposits: &[(DepositId, DepositKind, &DepositState)],
) -> LeaveStatement {
    let mut equity = BTreeMap::new();
    let mut held = BTreeMap::new();
    for (id, kind, state) in deposits {
        let s = state.stake(member);
        if s == 0 {
            continue;
        }
        match kind {
            DepositKind::Spending => equity.insert(*id, s),
            DepositKind::Held => held.insert(*id, s),
        };
    }
    LeaveStatement {
        member,
        balance: balances.get(member),
        equity,
        held,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Paid, Posting};
    use crate::method::{split, Method, Rule, SplitInput};

    const A: MemberId = MemberId(1);
    const B: MemberId = MemberId(2);

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn leaving_on_the_20th_of_march_counts_20_days() {
        let march = DateSpan::new(d(2027, 3, 1), d(2027, 3, 31)).unwrap();
        let mut members = BTreeMap::new();
        members.insert(
            A,
            Membership {
                joined: d(2026, 1, 1),
                left: None,
            },
        );
        members.insert(
            B,
            Membership {
                joined: d(2026, 6, 1),
                left: Some(d(2027, 3, 20)),
            },
        );
        let days = days_present(&members, &march);
        assert_eq!(days[&A], 31);
        assert_eq!(days[&B], 20);

        // a joiner from the 12th
        let j = Membership {
            joined: d(2027, 3, 12),
            left: None,
        };
        assert_eq!(j.days_in(&march), 20);
        // gone before the period
        let gone = Membership {
            joined: d(2026, 1, 1),
            left: Some(d(2027, 2, 28)),
        };
        assert_eq!(gone.days_in(&march), 0);
    }

    #[test]
    fn prepaid_items_prorate_down() {
        assert_eq!(prorate(120_000, 73, 365), 24_000);
        assert_eq!(prorate(100, 1, 3), 33);
        assert_eq!(prorate(100, 0, 0), 0);
    }

    #[test]
    fn leave_statement_adds_balance_and_equity_and_lists_held_deposits() {
        let march = DateSpan::new(d(2027, 3, 1), d(2027, 3, 31)).unwrap();
        let mut members = BTreeMap::new();
        members.insert(
            A,
            Membership {
                joined: d(2026, 1, 1),
                left: None,
            },
        );
        members.insert(
            B,
            Membership {
                joined: d(2026, 1, 1),
                left: Some(d(2027, 3, 20)),
            },
        );
        // March rent, paid by A, prorated by days
        let rule = Rule::new("Rent", Method::Days, &[A, B]);
        let shares = split(
            &rule,
            &SplitInput {
                amount: 51_000,
                days: days_present(&members, &march),
                payer: Some(A),
                ..Default::default()
            },
        )
        .unwrap();
        let mut bal = Balances::new();
        bal.apply(&Posting::Expense {
            paid: Paid::Member(A),
            shares,
        })
        .unwrap();

        let mut pot = DepositState::new();
        pot.pay_in(A, 10_000).unwrap();
        pot.pay_in(B, 10_000).unwrap();
        let mut security = DepositState::new();
        security.pay_in(B, 20_000).unwrap();

        let st = leave_statement(
            B,
            &bal,
            &[
                (DepositId(1), DepositKind::Spending, &pot),
                (DepositId(2), DepositKind::Held, &security),
            ],
        );
        assert_eq!(st.balance, -20_000); // 51,000 × 20/51
        assert_eq!(st.equity[&DepositId(1)], 10_000);
        assert_eq!(st.settle_now(), -10_000);
        assert_eq!(st.pending(), 20_000);
    }
}
