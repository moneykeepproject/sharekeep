//! Exact integer allocation of an amount by weights, with a visible rounding rule.
//!
//! Every share is the exact share rounded down, plus at most one leftover cent (except under
//! [`Rounding::Payer`], where the payer takes every leftover cent). The shares always add up
//! to the amount to the cent.

use crate::SplitError;
use serde::{Deserialize, Serialize};
use sk_core::{Cents, MemberId};
use std::collections::BTreeMap;

/// Who receives the cents left over after every share is rounded down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rounding {
    /// The default: leftover cents rotate through the members whose exact share had a
    /// fraction, in member order, starting at a different member for each entry (the
    /// entry's sequence number), so they even out over time.
    #[default]
    Rotate,
    /// Leftover cents go to the largest fractions first (ties: lower member id first).
    LargestFraction,
    /// Every leftover cent goes to the payer, when the payer has a share; otherwise as
    /// [`Rounding::LargestFraction`].
    Payer,
}

/// Context the rounding rule needs.
#[derive(Debug, Clone, Copy, Default)]
pub struct RoundingCtx {
    /// The entry's sequence number in the book (rotates the extra cent).
    pub seq: u64,
    /// Who paid, when a member paid.
    pub payer: Option<MemberId>,
}

/// Divide `total` by `weights`. Members with weight 0 get 0 but stay in the result.
///
/// Fails with [`SplitError::NoWeight`] when every weight is 0 and `total` is not 0.
pub fn allocate(
    total: Cents,
    weights: &BTreeMap<MemberId, u64>,
    rounding: Rounding,
    ctx: RoundingCtx,
) -> Result<BTreeMap<MemberId, Cents>, SplitError> {
    let mut out: BTreeMap<MemberId, Cents> = weights.keys().map(|m| (*m, 0)).collect();
    if total == 0 {
        return Ok(out);
    }
    let sum: i128 = weights.values().map(|w| i128::from(*w)).sum();
    if sum == 0 {
        return Err(SplitError::NoWeight);
    }
    let negative = total < 0;
    let t = i128::from(total).abs();

    let mut given: i128 = 0;
    // (member, remainder) for members whose exact share has a fraction
    let mut fractions: Vec<(MemberId, i128)> = Vec::new();
    for (m, w) in weights {
        let num = t * i128::from(*w);
        let base = num / sum;
        let rem = num % sum;
        given += base;
        // base <= t <= i64::MAX, so it fits
        out.insert(*m, base as Cents);
        if rem > 0 {
            fractions.push((*m, rem));
        }
    }
    let leftover = (t - given) as usize;
    debug_assert!(leftover <= fractions.len());

    if leftover > 0 {
        let payer_has_share = ctx
            .payer
            .is_some_and(|p| weights.get(&p).is_some_and(|w| *w > 0));
        match rounding {
            Rounding::Payer if payer_has_share => {
                if let Some(p) = ctx.payer {
                    *out.entry(p).or_default() += leftover as Cents;
                }
            }
            Rounding::Rotate => {
                // fractions is in member order (BTreeMap iteration)
                // Start where the previous entry's cents would have ended (seq × leftover),
                // so a run of identical entries hands out its cents in one even cycle.
                let n = fractions.len();
                let start = (u128::from(ctx.seq) * leftover as u128 % n as u128) as usize;
                for k in 0..leftover {
                    let (m, _) = fractions[(start + k) % n];
                    *out.entry(m).or_default() += 1;
                }
            }
            Rounding::LargestFraction | Rounding::Payer => {
                fractions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                for (m, _) in fractions.iter().take(leftover) {
                    *out.entry(*m).or_default() += 1;
                }
            }
        }
    }

    if negative {
        for v in out.values_mut() {
            *v = -*v;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pairs: &[(u32, u64)]) -> BTreeMap<MemberId, u64> {
        pairs.iter().map(|(m, w)| (MemberId(*m), *w)).collect()
    }

    fn get(map: &BTreeMap<MemberId, Cents>, m: u32) -> Cents {
        map[&MemberId(m)]
    }

    #[test]
    fn a_dollar_three_ways_rotates_the_extra_cent() {
        let weights = w(&[(1, 1), (2, 1), (3, 1)]);
        for seq in 0..3u64 {
            let out = allocate(
                100,
                &weights,
                Rounding::Rotate,
                RoundingCtx { seq, payer: None },
            )
            .unwrap();
            assert_eq!(out.values().sum::<Cents>(), 100);
            let extra = (seq % 3) as u32 + 1;
            assert_eq!(get(&out, extra), 34, "seq {seq}");
        }
    }

    #[test]
    fn whole_shares_never_get_an_extra_cent() {
        // 2:1:1 of $1.01 → 50.5, 25.25, 25.25; only fractional shares can receive cents
        let weights = w(&[(1, 2), (2, 1), (3, 1)]);
        let out = allocate(101, &weights, Rounding::Rotate, RoundingCtx::default()).unwrap();
        assert_eq!(out.values().sum::<Cents>(), 101);
        // 102 for weights 2:1:1 → 51, 25.5, 25.5: member 1's share is whole
        let out = allocate(
            102,
            &weights,
            Rounding::Rotate,
            RoundingCtx {
                seq: 5,
                payer: None,
            },
        )
        .unwrap();
        assert_eq!(get(&out, 1), 51);
    }

    #[test]
    fn largest_fraction_first() {
        let weights = w(&[(1, 1), (2, 2)]);
        // 10 cents 1:2 → 3.33, 6.67 → member 2 gets the cent
        let out = allocate(
            10,
            &weights,
            Rounding::LargestFraction,
            RoundingCtx::default(),
        )
        .unwrap();
        assert_eq!((get(&out, 1), get(&out, 2)), (3, 7));
    }

    #[test]
    fn payer_takes_every_leftover_cent() {
        let weights = w(&[(1, 1), (2, 1), (3, 1)]);
        let ctx = RoundingCtx {
            seq: 0,
            payer: Some(MemberId(2)),
        };
        let out = allocate(101, &weights, Rounding::Payer, ctx).unwrap();
        assert_eq!(get(&out, 2), 35);
        assert_eq!(get(&out, 1), 33);
    }

    #[test]
    fn refunds_mirror_charges() {
        let weights = w(&[(1, 1), (2, 1), (3, 1)]);
        let out = allocate(-100, &weights, Rounding::Rotate, RoundingCtx::default()).unwrap();
        assert_eq!(out.values().sum::<Cents>(), -100);
        assert_eq!(get(&out, 1), -34);
    }

    #[test]
    fn zero_weights_are_refused_unless_nothing_to_divide() {
        let weights = w(&[(1, 0), (2, 0)]);
        assert_eq!(
            allocate(5, &weights, Rounding::Rotate, RoundingCtx::default()),
            Err(SplitError::NoWeight)
        );
        assert!(allocate(0, &weights, Rounding::Rotate, RoundingCtx::default()).is_ok());
    }

    #[test]
    fn huge_amounts_do_not_overflow() {
        let weights = w(&[(1, u64::from(u32::MAX)), (2, 1)]);
        let out = allocate(
            i64::MAX,
            &weights,
            Rounding::LargestFraction,
            RoundingCtx::default(),
        )
        .unwrap();
        assert_eq!(
            out.values().map(|v| i128::from(*v)).sum::<i128>(),
            i128::from(i64::MAX)
        );
    }
}
