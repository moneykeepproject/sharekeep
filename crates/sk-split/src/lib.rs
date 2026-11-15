//! ShareKeep's split engine — part of the MoneyKeep family.
//!
//! Pure functions and plain data: no I/O, no clock, no floating point. Everything here is
//! tested exhaustively, because every balance the household sees comes from it.
//!
//! - [`method`]: the ten split methods, rules, caps and floors.
//! - [`alloc`]: exact integer allocation and the visible rounding rule.
//! - [`rules`]: rule versions with effective dates, category defaults, templates.
//! - [`ledger`]: balances, settle-up, "net between us".
//! - [`deposits`]: held and spending deposits, equity, buy-outs, refunds, deductions.
//! - [`prorate`]: days present, prepaid proration, the leaving statement.

pub mod alloc;
pub mod deposits;
pub mod ledger;
pub mod method;
pub mod prorate;
pub mod rules;

pub use alloc::Rounding;
pub use ledger::{settle_up, Balances, Paid, Posting, Transfer};
pub use method::{split, Item, Limit, Method, Rule, SplitInput};
pub use rules::RuleBook;

use sk_core::{Cents, MemberId, NaiveDate, RuleId};
use std::fmt;

/// Why a split, rule or posting was refused. Nothing changes when one is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// Every weight is zero, so there is nothing to divide by.
    NoWeight,
    /// The rule leaves nobody to share (everyone opted out).
    NoParticipants,
    /// A member has no slider value, days or reading.
    MissingInput(MemberId),
    /// The rule names someone who isn't one of its members.
    NotAMember(MemberId),
    /// The entry names someone who isn't sharing under the rule.
    NotAParticipant(MemberId),
    PercentNot100 {
        total_bp: u64,
    },
    ExactExceedsTotal {
        fixed: Cents,
        total: Cents,
    },
    NoItems,
    ItemWithoutMembers(usize),
    /// Tax, tip or fees need lines with a positive total to spread over.
    ExtrasNeedPositiveLines,
    LimitMinAboveMax(MemberId),
    NegativeLimit(MemberId),
    /// The caps and floors can't add up to the amount.
    LimitsInfeasible,
    DuplicateEffectiveDate(RuleId, NaiveDate),
    NoRuleFor(String),
    RuleNotInForce(RuleId, NaiveDate),
    PaymentToSelf(MemberId),
    NonPositivePayment,
    NotASpendingDeposit,
    NotRefundable,
    InsufficientFunds {
        balance: Cents,
        needed: Cents,
    },
    StakeTooSmall {
        member: MemberId,
        stake: Cents,
    },
    Overflow,
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SplitError::*;
        match self {
            NoWeight => write!(f, "every share is zero, so the amount can't be divided"),
            NoParticipants => write!(f, "everyone has opted out of this rule"),
            MissingInput(m) => write!(f, "{m} has no value set for this rule"),
            NotAMember(m) => write!(f, "{m} isn't covered by this rule"),
            NotAParticipant(m) => write!(f, "{m} isn't sharing under this rule"),
            PercentNot100 { total_bp } => write!(
                f,
                "the percentages add up to {}.{:02}%, not 100%",
                total_bp / 100,
                total_bp % 100
            ),
            ExactExceedsTotal { fixed, total } => write!(
                f,
                "the fixed amounts ({fixed} cents) are more than the total ({total} cents)"
            ),
            NoItems => write!(f, "an itemized split needs at least one line"),
            ItemWithoutMembers(i) => write!(f, "line {} has nobody assigned", i + 1),
            ExtrasNeedPositiveLines => {
                write!(f, "tax, tip and fees need lines with a positive total")
            }
            LimitMinAboveMax(m) => write!(f, "{m}'s floor is above their cap"),
            NegativeLimit(m) => write!(f, "{m}'s cap or floor is negative"),
            LimitsInfeasible => write!(f, "the caps and floors can't add up to this amount"),
            DuplicateEffectiveDate(r, d) => write!(f, "{r} already has a version from {d}"),
            NoRuleFor(c) => write!(f, "no rule applies to the category \"{c}\""),
            RuleNotInForce(r, d) => write!(f, "{r} has no version in force on {d}"),
            PaymentToSelf(m) => write!(f, "{m} can't pay themselves"),
            NonPositivePayment => write!(f, "the amount must be more than zero"),
            NotASpendingDeposit => write!(f, "only a spending deposit can pay bills"),
            NotRefundable => write!(f, "this deposit isn't refundable"),
            InsufficientFunds { balance, needed } => write!(
                f,
                "the deposit holds {balance} cents but {needed} cents are needed"
            ),
            StakeTooSmall { member, stake } => {
                write!(f, "{member} owns only {stake} cents of this deposit")
            }
            Overflow => write!(f, "the amount is too large"),
        }
    }
}

impl std::error::Error for SplitError {}
