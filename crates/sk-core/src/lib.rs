//! ShareKeep core types — part of the MoneyKeep family.
//!
//! No I/O, no UI: used by the split engine, the household service, the desktop app and the
//! web app alike. Every amount is an integer number of minor units (cents); there is no
//! floating point anywhere in ShareKeep's money paths.

pub mod money;

pub use chrono::NaiveDate;
pub use money::Currency;

use serde::{Deserialize, Serialize};
use std::fmt;

/// An amount in minor units (cents for USD). Negative amounts are refunds or credits.
pub type Cents = i64;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

id_type!(
    /// A household member, unique within a book.
    MemberId,
    "m"
);
id_type!(
    /// A split rule, unique within a book. Its versions share the id.
    RuleId,
    "r"
);
id_type!(
    /// A deposit (held or spending), unique within a book.
    DepositId,
    "d"
);
id_type!(
    /// An agreement within a book.
    AgreementId,
    "a"
);

/// A whole-day span, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateSpan {
    pub first: NaiveDate,
    pub last: NaiveDate,
}

impl DateSpan {
    /// A span from `first` to `last`, both included. `None` when `last` is before `first`.
    pub fn new(first: NaiveDate, last: NaiveDate) -> Option<Self> {
        (first <= last).then_some(Self { first, last })
    }

    /// Number of days in the span, both ends counted.
    pub fn days(&self) -> u32 {
        // A span is at most a few thousand days; the cast cannot truncate in practice.
        ((self.last - self.first).num_days() + 1) as u32
    }

    /// The days this span shares with `other`, if any.
    pub fn overlap(&self, other: &DateSpan) -> Option<DateSpan> {
        DateSpan::new(self.first.max(other.first), self.last.min(other.last))
    }

    pub fn contains(&self, day: NaiveDate) -> bool {
        self.first <= day && day <= self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn spans_count_both_ends() {
        let s = DateSpan::new(d(2027, 2, 1), d(2027, 2, 28)).unwrap();
        assert_eq!(s.days(), 28);
        assert_eq!(
            DateSpan::new(d(2027, 2, 1), d(2027, 2, 1)).unwrap().days(),
            1
        );
        assert!(DateSpan::new(d(2027, 2, 2), d(2027, 2, 1)).is_none());
    }

    #[test]
    fn overlap_is_the_shared_days() {
        let month = DateSpan::new(d(2027, 3, 1), d(2027, 3, 31)).unwrap();
        let stay = DateSpan::new(d(2027, 3, 12), d(2027, 4, 30)).unwrap();
        assert_eq!(month.overlap(&stay).unwrap().days(), 20);
        let before = DateSpan::new(d(2027, 1, 1), d(2027, 1, 31)).unwrap();
        assert!(month.overlap(&before).is_none());
    }

    #[test]
    fn ids_serialize_as_plain_numbers() {
        assert_eq!(serde_json::to_string(&MemberId(7)).unwrap(), "7");
        assert_eq!(MemberId(7).to_string(), "m7");
    }
}
