//! Balances and settle-up for one agreement.
//!
//! A member's balance is what the others owe them: positive means they are owed, negative
//! means they owe. Every posting keeps the balances summing to zero.

use crate::SplitError;
use serde::{Deserialize, Serialize};
use sk_core::{Cents, DepositId, MemberId};
use std::collections::BTreeMap;

/// Who paid an expense.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Paid {
    Member(MemberId),
    /// A spending deposit paid: balances don't move, the pot's equity does
    /// (see [`crate::deposits`]).
    Pot(DepositId),
}

/// One posting that moves balances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Posting {
    /// An expense (or a refund, with negative shares) and how it was divided.
    Expense {
        paid: Paid,
        shares: BTreeMap<MemberId, Cents>,
    },
    /// A settle-up payment from one member to another.
    Payment {
        from: MemberId,
        to: MemberId,
        amount: Cents,
    },
}

/// The agreement's running balances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balances(BTreeMap<MemberId, Cents>);

impl Balances {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, m: MemberId) -> Cents {
        self.0.get(&m).copied().unwrap_or(0)
    }

    pub fn iter(&self) -> impl Iterator<Item = (MemberId, Cents)> + '_ {
        self.0.iter().map(|(m, v)| (*m, *v))
    }

    /// Always zero: what is owed equals what is due.
    pub fn sum(&self) -> Cents {
        self.0.values().sum()
    }

    fn add(&mut self, m: MemberId, v: Cents) -> Result<(), SplitError> {
        let e = self.0.entry(m).or_default();
        *e = e.checked_add(v).ok_or(SplitError::Overflow)?;
        Ok(())
    }

    /// Apply one posting. A failed posting changes nothing.
    pub fn apply(&mut self, posting: &Posting) -> Result<(), SplitError> {
        let mut next = self.clone();
        match posting {
            Posting::Expense { paid, shares } => {
                if let Paid::Member(p) = paid {
                    let total = shares
                        .values()
                        .try_fold(0 as Cents, |a, v| a.checked_add(*v))
                        .ok_or(SplitError::Overflow)?;
                    next.add(*p, total)?;
                    for (m, v) in shares {
                        next.add(*m, -*v)?;
                    }
                }
            }
            Posting::Payment { from, to, amount } => {
                if from == to {
                    return Err(SplitError::PaymentToSelf(*from));
                }
                if *amount <= 0 {
                    return Err(SplitError::NonPositivePayment);
                }
                next.add(*from, *amount)?;
                next.add(*to, -*amount)?;
            }
        }
        *self = next;
        Ok(())
    }
}

/// One settle-up payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    pub from: MemberId,
    pub to: MemberId,
    pub amount: Cents,
}

/// Payments that clear every balance: at most one fewer than the members with a balance.
/// The largest debt pays the largest credit first (ties: lower member id first).
pub fn settle_up(balances: &Balances) -> Vec<Transfer> {
    let mut owe: Vec<(MemberId, Cents)> = balances
        .iter()
        .filter(|(_, v)| *v < 0)
        .map(|(m, v)| (m, -v))
        .collect();
    let mut owed: Vec<(MemberId, Cents)> = balances.iter().filter(|(_, v)| *v > 0).collect();
    let order = |a: &(MemberId, Cents), b: &(MemberId, Cents)| b.1.cmp(&a.1).then(a.0.cmp(&b.0));
    owe.sort_by(order);
    owed.sort_by(order);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < owe.len() && j < owed.len() {
        let amount = owe[i].1.min(owed[j].1);
        out.push(Transfer {
            from: owe[i].0,
            to: owed[j].0,
            amount,
        });
        owe[i].1 -= amount;
        owed[j].1 -= amount;
        if owe[i].1 == 0 {
            i += 1;
        }
        if owed[j].1 == 0 {
            j += 1;
        }
    }
    out
}

/// "Net between us": what `a` owes `b` across several agreements' settle-ups (negative
/// when `b` owes `a`), so they can settle with one payment. Each agreement's own balances
/// stay as recorded.
pub fn net_between(a: MemberId, b: MemberId, settle_ups: &[Vec<Transfer>]) -> Cents {
    settle_ups
        .iter()
        .flatten()
        .map(|t| {
            if t.from == a && t.to == b {
                t.amount
            } else if t.from == b && t.to == a {
                -t.amount
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: MemberId = MemberId(1);
    const B: MemberId = MemberId(2);
    const C: MemberId = MemberId(3);

    fn expense(p: MemberId, shares: &[(MemberId, Cents)]) -> Posting {
        Posting::Expense {
            paid: Paid::Member(p),
            shares: shares.iter().copied().collect(),
        }
    }

    #[test]
    fn a_member_paid_expense_moves_balances_and_keeps_them_at_zero() {
        let mut b = Balances::new();
        b.apply(&expense(A, &[(A, 3000), (B, 3000), (C, 3000)]))
            .unwrap();
        assert_eq!((b.get(A), b.get(B), b.get(C)), (6000, -3000, -3000));
        assert_eq!(b.sum(), 0);
    }

    #[test]
    fn a_pot_paid_expense_leaves_balances_alone() {
        let mut b = Balances::new();
        b.apply(&Posting::Expense {
            paid: Paid::Pot(DepositId(1)),
            shares: [(A, 500), (B, 500)].into_iter().collect(),
        })
        .unwrap();
        assert_eq!(b.get(A), 0);
    }

    #[test]
    fn settle_up_clears_everything_in_at_most_n_minus_1_payments() {
        let mut b = Balances::new();
        b.apply(&expense(A, &[(A, 3000), (B, 3000), (C, 3000)]))
            .unwrap();
        b.apply(&expense(B, &[(A, 1000), (B, 1000), (C, 1000)]))
            .unwrap();
        let t = settle_up(&b);
        assert!(t.len() <= 2);
        for x in &t {
            b.apply(&Posting::Payment {
                from: x.from,
                to: x.to,
                amount: x.amount,
            })
            .unwrap();
        }
        assert!(b.iter().all(|(_, v)| v == 0));
    }

    #[test]
    fn payments_must_be_positive_and_between_two_people() {
        let mut b = Balances::new();
        assert!(b
            .apply(&Posting::Payment {
                from: A,
                to: A,
                amount: 5
            })
            .is_err());
        assert!(b
            .apply(&Posting::Payment {
                from: A,
                to: B,
                amount: 0
            })
            .is_err());
    }

    #[test]
    fn net_between_two_agreements() {
        let apartment = vec![Transfer {
            from: A,
            to: B,
            amount: 5000,
        }];
        let groceries = vec![Transfer {
            from: B,
            to: A,
            amount: 1200,
        }];
        assert_eq!(net_between(A, B, &[apartment, groceries]), 3800);
    }
}
