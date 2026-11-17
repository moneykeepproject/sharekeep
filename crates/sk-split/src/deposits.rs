//! Deposits: held (parked and returned later) and spending (drawn down to pay bills).
//!
//! A deposit tracks each member's stake. For a held deposit the stake is the part of it the
//! member owns; for a spending deposit it is the member's equity: what they paid in minus
//! their share of what the pot paid. The stakes always add up to the deposit's balance.

use crate::alloc::{allocate, Rounding, RoundingCtx};
use crate::SplitError;
use serde::{Deserialize, Serialize};
use sk_core::{Cents, DepositId, MemberId, RuleId};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositKind {
    /// Parked and returned later: a rental security or utility deposit.
    Held,
    /// Drawn down to pay bills: the household pot.
    Spending,
}

/// Who has the money.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Holder {
    Landlord,
    Utility,
    Member(MemberId),
    HouseholdPot,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refundable {
    Yes,
    No,
    /// Refundable except this amount.
    Partly {
        non_refundable: Cents,
    },
}

/// What happens when a spending deposit can't cover a bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Shortfall {
    /// Warn and refuse the pot payment.
    #[default]
    Block,
    /// The pot pays what it has; the member paying covers the rest, split by the same rule.
    Debts,
}

/// A deposit's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deposit {
    pub id: DepositId,
    pub kind: DepositKind,
    /// "Rental security", "Electric", "Household expenses" or a custom name.
    pub label: String,
    /// `None`: linked to the whole agreement; `Some`: only touches that rule's bills.
    pub rule: Option<RuleId>,
    pub holder: Holder,
    pub refundable: Refundable,
    #[serde(default)]
    pub shortfall: Shortfall,
}

/// A deposit's money and who it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositState {
    balance: Cents,
    stakes: BTreeMap<MemberId, Cents>,
}

/// What a refund paid out to each member.
pub type Payouts = BTreeMap<MemberId, Cents>;

/// Per-member amounts: a bill's shares, or one part of them.
pub type Shares = BTreeMap<MemberId, Cents>;

impl DepositState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The money in the deposit now.
    pub fn balance(&self) -> Cents {
        self.balance
    }

    /// A member's stake (held) or equity (spending). Equity can be negative.
    pub fn stake(&self, m: MemberId) -> Cents {
        self.stakes.get(&m).copied().unwrap_or(0)
    }

    pub fn stakes(&self) -> &BTreeMap<MemberId, Cents> {
        &self.stakes
    }

    fn change(&mut self, m: MemberId, v: Cents) -> Result<(), SplitError> {
        let e = self.stakes.entry(m).or_default();
        *e = e.checked_add(v).ok_or(SplitError::Overflow)?;
        Ok(())
    }

    /// Undo an earlier change exactly: take `balance` back off the deposit and each
    /// member's `stakes` change off their part. Refused when the money has gone meanwhile, or
    /// (for a held deposit) when someone's part is now smaller than what is taken back.
    pub fn reverse(
        &mut self,
        kind: DepositKind,
        balance: Cents,
        stakes: &Shares,
    ) -> Result<(), SplitError> {
        let mut next = self.clone();
        next.balance = next
            .balance
            .checked_sub(balance)
            .ok_or(SplitError::Overflow)?;
        if next.balance < 0 {
            return Err(SplitError::InsufficientFunds {
                balance: self.balance,
                needed: balance,
            });
        }
        for (m, v) in stakes {
            next.change(*m, -*v)?;
            if kind == DepositKind::Held && next.stake(*m) < 0 {
                return Err(SplitError::StakeTooSmall {
                    member: *m,
                    stake: self.stake(*m),
                });
            }
        }
        *self = next;
        Ok(())
    }

    /// A member pays in (a move-in deposit, a contribution, a top-up).
    pub fn pay_in(&mut self, m: MemberId, amount: Cents) -> Result<(), SplitError> {
        if amount <= 0 {
            return Err(SplitError::NonPositivePayment);
        }
        self.balance = self
            .balance
            .checked_add(amount)
            .ok_or(SplitError::Overflow)?;
        self.change(m, amount)
    }

    /// The pot pays a bill (spending deposits). `shares` is the bill split by its rule; each
    /// member's equity falls by their share. Refused when the pot can't cover it.
    pub fn spend(
        &mut self,
        kind: DepositKind,
        shares: &BTreeMap<MemberId, Cents>,
    ) -> Result<(), SplitError> {
        if kind != DepositKind::Spending {
            return Err(SplitError::NotASpendingDeposit);
        }
        let total: Cents = shares.values().sum();
        if total > self.balance {
            return Err(SplitError::InsufficientFunds {
                balance: self.balance,
                needed: total,
            });
        }
        self.balance -= total;
        for (m, v) in shares {
            self.change(*m, -*v)?;
        }
        Ok(())
    }

    /// An incoming member buys an outgoing member's part of a held deposit. The deposit
    /// itself stays where it is (with the landlord); only ownership moves. The incoming
    /// member pays the outgoing one directly.
    pub fn buy_out(
        &mut self,
        from: MemberId,
        to: MemberId,
        amount: Cents,
    ) -> Result<(), SplitError> {
        if amount <= 0 {
            return Err(SplitError::NonPositivePayment);
        }
        if amount > self.stake(from) {
            return Err(SplitError::StakeTooSmall {
                member: from,
                stake: self.stake(from),
            });
        }
        self.change(from, -amount)?;
        self.change(to, amount)
    }

    /// Damage, cleaning or fees taken from a held deposit, split by a chosen rule
    /// (`shares`: all on one member, or any method).
    pub fn deduct(&mut self, shares: &BTreeMap<MemberId, Cents>) -> Result<(), SplitError> {
        let total: Cents = shares.values().sum();
        if total <= 0 {
            return Err(SplitError::NonPositivePayment);
        }
        if total > self.balance {
            return Err(SplitError::InsufficientFunds {
                balance: self.balance,
                needed: total,
            });
        }
        self.balance -= total;
        for (m, v) in shares {
            self.change(*m, -*v)?;
        }
        Ok(())
    }

    /// The landlord or utility returns `amount`; it goes to the members by their stakes.
    /// Returns what each member receives.
    pub fn refund(&mut self, deposit: &Deposit, amount: Cents) -> Result<Payouts, SplitError> {
        if amount <= 0 {
            return Err(SplitError::NonPositivePayment);
        }
        let keep = match deposit.refundable {
            Refundable::Yes => 0,
            Refundable::Partly { non_refundable } => non_refundable,
            Refundable::No => return Err(SplitError::NotRefundable),
        };
        if amount > self.balance - keep {
            return Err(SplitError::InsufficientFunds {
                balance: self.balance - keep,
                needed: amount,
            });
        }
        let weights: BTreeMap<MemberId, u64> = self
            .stakes
            .iter()
            .filter(|(_, v)| **v > 0)
            .map(|(m, v)| (*m, *v as u64))
            .collect();
        let payouts = allocate(
            amount,
            &weights,
            Rounding::LargestFraction,
            RoundingCtx::default(),
        )?;
        self.balance -= amount;
        for (m, v) in &payouts {
            self.change(*m, -*v)?;
        }
        Ok(payouts)
    }

    /// Interest credited to the deposit, shared by the members' stakes.
    pub fn credit_interest(&mut self, amount: Cents) -> Result<(), SplitError> {
        if amount == 0 {
            return Ok(());
        }
        let weights: BTreeMap<MemberId, u64> = self
            .stakes
            .iter()
            .filter(|(_, v)| **v > 0)
            .map(|(m, v)| (*m, *v as u64))
            .collect();
        let shares = allocate(
            amount,
            &weights,
            Rounding::LargestFraction,
            RoundingCtx::default(),
        )?;
        self.balance = self
            .balance
            .checked_add(amount)
            .ok_or(SplitError::Overflow)?;
        for (m, v) in shares {
            self.change(m, v)?;
        }
        Ok(())
    }
}

/// Simple interest at the book's rate (basis points a year; the default is 0) for `days`,
/// rounded down to the cent.
pub fn interest(balance: Cents, rate_bp_per_year: u32, days: u32) -> Cents {
    if balance <= 0 {
        return 0;
    }
    let v = i128::from(balance) * i128::from(rate_bp_per_year) * i128::from(days) / (10_000 * 365);
    v as Cents
}

/// Under [`Shortfall::Debts`]: split a bill the pot can only partly cover into the part the
/// pot pays and the part the paying member covers, both in proportion to the bill's shares.
pub fn split_shortfall(
    shares: &BTreeMap<MemberId, Cents>,
    available: Cents,
) -> Result<(Shares, Shares), SplitError> {
    let total: Cents = shares.values().sum();
    let from_pot = available.clamp(0, total.max(0));
    let weights: BTreeMap<MemberId, u64> = shares
        .iter()
        .map(|(m, v)| (*m, (*v).max(0) as u64))
        .collect();
    let pot = allocate(
        from_pot,
        &weights,
        Rounding::LargestFraction,
        RoundingCtx::default(),
    )?;
    let member = shares
        .iter()
        .map(|(m, v)| (*m, v - pot.get(m).copied().unwrap_or(0)))
        .collect();
    Ok((pot, member))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: MemberId = MemberId(1);
    const B: MemberId = MemberId(2);
    const C: MemberId = MemberId(3);

    fn held() -> Deposit {
        Deposit {
            id: DepositId(1),
            kind: DepositKind::Held,
            label: "Rental security".into(),
            rule: None,
            holder: Holder::Landlord,
            refundable: Refundable::Yes,
            shortfall: Shortfall::Block,
        }
    }

    fn shares(pairs: &[(MemberId, Cents)]) -> BTreeMap<MemberId, Cents> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn spending_equity_is_paid_in_minus_share_spent() {
        let mut pot = DepositState::new();
        pot.pay_in(A, 10_000).unwrap();
        pot.pay_in(B, 10_000).unwrap();
        pot.spend(DepositKind::Spending, &shares(&[(A, 3000), (B, 3000)]))
            .unwrap();
        assert_eq!(pot.balance(), 14_000);
        assert_eq!(pot.stake(A), 7000);
        assert_eq!(pot.stakes().values().sum::<Cents>(), pot.balance());
    }

    #[test]
    fn the_pot_refuses_what_it_cannot_cover() {
        let mut pot = DepositState::new();
        pot.pay_in(A, 1000).unwrap();
        assert!(matches!(
            pot.spend(DepositKind::Spending, &shares(&[(A, 800), (B, 800)])),
            Err(SplitError::InsufficientFunds { .. })
        ));
        assert_eq!(pot.balance(), 1000);
        assert!(pot.spend(DepositKind::Held, &shares(&[(A, 1)])).is_err());
    }

    #[test]
    fn shortfall_becomes_a_member_paid_part() {
        let bill = shares(&[(A, 800), (B, 800)]);
        let (pot, member) = split_shortfall(&bill, 1000).unwrap();
        assert_eq!(pot.values().sum::<Cents>(), 1000);
        assert_eq!(member.values().sum::<Cents>(), 600);
        assert_eq!(pot[&A] + member[&A], 800);
    }

    #[test]
    fn buy_out_moves_ownership_not_money() {
        let mut d = DepositState::new();
        d.pay_in(A, 20_000).unwrap();
        d.pay_in(B, 20_000).unwrap();
        d.buy_out(B, C, 20_000).unwrap();
        assert_eq!(d.balance(), 40_000);
        assert_eq!((d.stake(B), d.stake(C)), (0, 20_000));
        assert!(d.buy_out(B, C, 1).is_err());
    }

    #[test]
    fn deductions_then_refund_by_stake() {
        let mut d = DepositState::new();
        d.pay_in(A, 20_000).unwrap();
        d.pay_in(B, 20_000).unwrap();
        // cleaning, all on B
        d.deduct(&shares(&[(B, 5000)])).unwrap();
        let paid = d.refund(&held(), 35_000).unwrap();
        assert_eq!(paid, shares(&[(A, 20_000), (B, 15_000)]));
        assert_eq!(d.balance(), 0);
    }

    #[test]
    fn partly_refundable_keeps_its_fee() {
        let mut dep = held();
        dep.refundable = Refundable::Partly {
            non_refundable: 2500,
        };
        let mut d = DepositState::new();
        d.pay_in(A, 5000).unwrap();
        assert!(d.refund(&dep, 5000).is_err());
        assert_eq!(d.refund(&dep, 2500).unwrap()[&A], 2500);
        dep.refundable = Refundable::No;
        assert_eq!(d.refund(&dep, 1), Err(SplitError::NotRefundable));
    }

    #[test]
    fn interest_defaults_to_nothing_and_is_shared_by_stake() {
        assert_eq!(interest(20_000, 0, 365), 0);
        assert_eq!(interest(20_000, 100, 365), 200); // 1% a year
        let mut d = DepositState::new();
        d.pay_in(A, 30_000).unwrap();
        d.pay_in(B, 10_000).unwrap();
        d.credit_interest(400).unwrap();
        assert_eq!((d.stake(A), d.stake(B)), (30_300, 10_100));
        assert_eq!(d.stakes().values().sum::<Cents>(), d.balance());
    }
}
