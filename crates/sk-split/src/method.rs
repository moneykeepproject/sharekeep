//! Split methods and rules: how one entry's amount is divided among the members.

use crate::alloc::{allocate, Rounding, RoundingCtx};
use crate::SplitError;
use serde::{Deserialize, Serialize};
use sk_core::{Cents, MemberId};
use std::collections::{BTreeMap, BTreeSet};

/// Basis points in 100%.
pub const FULL_BP: u32 = 10_000;

/// The ten ways ShareKeep divides an amount. Opt-out (the tenth) is not a variant: any rule
/// can exclude members, see [`Rule::excluded`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    /// Evenly among the members included.
    Equal,
    /// Fixed percentages in basis points (1% = 100); the included members' points total
    /// exactly 10,000.
    Percent(BTreeMap<MemberId, u32>),
    /// Whole-number weights ("couple 2, roommate 1").
    Shares(BTreeMap<MemberId, u32>),
    /// Fixed amounts; `remainder` takes whatever is left of the entry's amount.
    Exact {
        amounts: BTreeMap<MemberId, Cents>,
        remainder: MemberId,
    },
    /// Receipt lines assigned to members, given with each entry ([`SplitInput::items`]).
    Itemized,
    /// Pro rata to income, entered as ratio parts (no salaries needed): 64 and 36.
    Income(BTreeMap<MemberId, u32>),
    /// Per-room weights: an area, or a fixed rent per room.
    Room(BTreeMap<MemberId, u32>),
    /// Pro rata to days in the household during the period ([`SplitInput::days`]).
    Days,
    /// Pro rata to meter readings or usage counts ([`SplitInput::usage`]).
    Usage,
}

impl Method {
    /// The method's name as the rule editor shows it.
    pub fn label(&self) -> &'static str {
        match self {
            Method::Equal => "Equal",
            Method::Percent(_) => "Percentages",
            Method::Shares(_) => "Shares",
            Method::Exact { .. } => "Exact amounts",
            Method::Itemized => "Itemized",
            Method::Income(_) => "By income",
            Method::Room(_) => "By room",
            Method::Days => "By days present",
            Method::Usage => "By usage",
        }
    }
}

/// A per-member limit on one entry's share. Limits apply to the weighted methods (all but
/// exact amounts and itemized, whose amounts are already explicit). For a refund (a
/// negative amount) they limit the size of the refund share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Limit {
    pub max: Option<Cents>,
    pub min: Option<Cents>,
}

/// A split rule: a method, who it covers, who opted out, limits and the rounding rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// The name shown in the editor and on templates ("Rent 2027").
    pub name: String,
    pub method: Method,
    /// The agreement's members this rule covers.
    pub members: BTreeSet<MemberId>,
    /// Members who opted out: they pay nothing under this rule.
    #[serde(default)]
    pub excluded: BTreeSet<MemberId>,
    #[serde(default)]
    pub limits: BTreeMap<MemberId, Limit>,
    #[serde(default)]
    pub rounding: Rounding,
    /// Per-member caps on the total share of one period under this rule, applied when the
    /// period closes (see [`period_cap_adjustment`]).
    #[serde(default)]
    pub period_caps: BTreeMap<MemberId, Cents>,
}

impl Rule {
    /// A rule with no opt-outs, limits or special rounding.
    pub fn new(name: impl Into<String>, method: Method, members: &[MemberId]) -> Self {
        Rule {
            name: name.into(),
            method,
            members: members.iter().copied().collect(),
            excluded: BTreeSet::new(),
            limits: BTreeMap::new(),
            rounding: Rounding::default(),
            period_caps: BTreeMap::new(),
        }
    }

    /// The members who share under this rule: covered and not opted out.
    pub fn participants(&self) -> BTreeSet<MemberId> {
        self.members.difference(&self.excluded).copied().collect()
    }

    /// Check the rule on its own, before any entry: every participant has an input, the
    /// percentages total 100%, no input names someone outside the rule, limits make sense.
    pub fn validate(&self) -> Result<(), SplitError> {
        let p = self.participants();
        if p.is_empty() {
            return Err(SplitError::NoParticipants);
        }
        let check_map = |map: &BTreeMap<MemberId, u32>| -> Result<(), SplitError> {
            for m in map.keys() {
                if !self.members.contains(m) {
                    return Err(SplitError::NotAMember(*m));
                }
            }
            for m in &p {
                if !map.contains_key(m) {
                    return Err(SplitError::MissingInput(*m));
                }
            }
            Ok(())
        };
        match &self.method {
            Method::Equal | Method::Itemized | Method::Days | Method::Usage => {}
            Method::Percent(bp) => {
                check_map(bp)?;
                let total: u64 = p.iter().map(|m| u64::from(bp[m])).sum();
                if total != u64::from(FULL_BP) {
                    return Err(SplitError::PercentNot100 { total_bp: total });
                }
            }
            Method::Shares(w) | Method::Income(w) | Method::Room(w) => check_map(w)?,
            Method::Exact { amounts, remainder } => {
                if !p.contains(remainder) {
                    return Err(SplitError::NotAParticipant(*remainder));
                }
                for m in amounts.keys() {
                    if !p.contains(m) {
                        return Err(SplitError::NotAParticipant(*m));
                    }
                }
            }
        }
        for (m, cap) in &self.period_caps {
            if !self.members.contains(m) {
                return Err(SplitError::NotAMember(*m));
            }
            if *cap < 0 {
                return Err(SplitError::NegativeLimit(*m));
            }
        }
        for (m, l) in &self.limits {
            if !self.members.contains(m) {
                return Err(SplitError::NotAMember(*m));
            }
            if let (Some(min), Some(max)) = (l.min, l.max) {
                if min > max {
                    return Err(SplitError::LimitMinAboveMax(*m));
                }
            }
            if l.min.is_some_and(|v| v < 0) || l.max.is_some_and(|v| v < 0) {
                return Err(SplitError::NegativeLimit(*m));
            }
        }
        Ok(())
    }
}

/// One receipt line for an itemized split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub amount: Cents,
    /// The members who share this line, equally.
    pub members: Vec<MemberId>,
}

/// What one entry brings to the split, beyond its rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitInput {
    /// The entry's amount; negative for a refund.
    pub amount: Cents,
    /// The entry's sequence number in the book (rotates the extra cent).
    pub seq: u64,
    /// The member who paid, if a member paid (not a pot).
    pub payer: Option<MemberId>,
    /// Receipt lines, for [`Method::Itemized`]. Tax, tip and fees are the entry's amount
    /// minus the lines' total, spread in proportion to each member's lines.
    #[serde(default)]
    pub items: Vec<Item>,
    /// Days present in the period, for [`Method::Days`].
    #[serde(default)]
    pub days: BTreeMap<MemberId, u32>,
    /// Meter readings or usage counts, for [`Method::Usage`].
    #[serde(default)]
    pub usage: BTreeMap<MemberId, u64>,
}

impl SplitInput {
    pub fn amount(amount: Cents) -> Self {
        SplitInput {
            amount,
            ..Default::default()
        }
    }
}

/// Divide one entry under `rule`. The result has a share for every participant (and only
/// for participants) and always adds up to the entry's amount.
pub fn split(rule: &Rule, input: &SplitInput) -> Result<BTreeMap<MemberId, Cents>, SplitError> {
    rule.validate()?;
    let p = rule.participants();
    let ctx = RoundingCtx {
        seq: input.seq,
        payer: input.payer,
    };
    let weights_from = |map: &BTreeMap<MemberId, u32>| -> BTreeMap<MemberId, u64> {
        p.iter().map(|m| (*m, u64::from(map[m]))).collect()
    };
    let shares = match &rule.method {
        Method::Equal => {
            let w = p.iter().map(|m| (*m, 1)).collect();
            limited(rule, input.amount, &w, ctx)?
        }
        Method::Percent(map) | Method::Shares(map) | Method::Income(map) | Method::Room(map) => {
            limited(rule, input.amount, &weights_from(map), ctx)?
        }
        Method::Days => {
            let w = per_participant(&p, &input.days, |v| u64::from(*v))?;
            limited(rule, input.amount, &w, ctx)?
        }
        Method::Usage => {
            let w = per_participant(&p, &input.usage, |v| *v)?;
            limited(rule, input.amount, &w, ctx)?
        }
        Method::Exact { amounts, remainder } => {
            let fixed: Cents = amounts.values().sum();
            let rest = input
                .amount
                .checked_sub(fixed)
                .ok_or(SplitError::Overflow)?;
            if (input.amount >= 0 && rest < 0) || (input.amount < 0 && rest > 0) {
                return Err(SplitError::ExactExceedsTotal {
                    fixed,
                    total: input.amount,
                });
            }
            let mut out: BTreeMap<MemberId, Cents> = p.iter().map(|m| (*m, 0)).collect();
            for (m, a) in amounts {
                out.insert(*m, *a);
            }
            *out.entry(*remainder).or_default() += rest;
            out
        }
        Method::Itemized => itemized(&p, rule.rounding, input, ctx)?,
    };
    debug_assert_eq!(shares.values().sum::<Cents>(), input.amount);
    Ok(shares)
}

fn per_participant<V>(
    p: &BTreeSet<MemberId>,
    map: &BTreeMap<MemberId, V>,
    f: impl Fn(&V) -> u64,
) -> Result<BTreeMap<MemberId, u64>, SplitError> {
    for m in map.keys() {
        if !p.contains(m) {
            return Err(SplitError::NotAParticipant(*m));
        }
    }
    p.iter()
        .map(|m| {
            map.get(m)
                .map(|v| (*m, f(v)))
                .ok_or(SplitError::MissingInput(*m))
        })
        .collect()
}

/// Weighted allocation with the rule's caps and floors. Members who hit a cap (or a floor)
/// are fixed there and the rest is re-divided among the others by the same weights.
fn limited(
    rule: &Rule,
    amount: Cents,
    weights: &BTreeMap<MemberId, u64>,
    ctx: RoundingCtx,
) -> Result<BTreeMap<MemberId, Cents>, SplitError> {
    if rule.limits.is_empty() {
        return allocate(amount, weights, rule.rounding, ctx);
    }
    // Limits bound the size of a share; a refund mirrors the charge.
    let sign = if amount < 0 { -1 } else { 1 };
    let total = amount.checked_abs().ok_or(SplitError::Overflow)?;
    let mut fixed: BTreeMap<MemberId, Cents> = BTreeMap::new();
    loop {
        let remaining = total - fixed.values().sum::<Cents>();
        let free: BTreeMap<MemberId, u64> = weights
            .iter()
            .filter(|(m, _)| !fixed.contains_key(m))
            .map(|(m, w)| (*m, *w))
            .collect();
        if free.is_empty() {
            if remaining != 0 {
                return Err(SplitError::LimitsInfeasible);
            }
            break;
        }
        let alloc = match allocate(remaining, &free, rule.rounding, ctx) {
            Ok(a) => a,
            Err(SplitError::NoWeight) => return Err(SplitError::LimitsInfeasible),
            Err(e) => return Err(e),
        };
        let lim = |m: &MemberId| rule.limits.get(m).copied().unwrap_or_default();
        let over: Vec<MemberId> = alloc
            .iter()
            .filter(|(m, v)| lim(m).max.is_some_and(|max| **v > max))
            .map(|(m, _)| *m)
            .collect();
        if !over.is_empty() {
            for m in over {
                fixed.insert(m, lim(&m).max.unwrap_or_default());
            }
            continue;
        }
        let under: Vec<MemberId> = alloc
            .iter()
            .filter(|(m, v)| lim(m).min.is_some_and(|min| **v < min))
            .map(|(m, _)| *m)
            .collect();
        if !under.is_empty() {
            for m in under {
                fixed.insert(m, lim(&m).min.unwrap_or_default());
            }
            continue;
        }
        fixed.extend(alloc);
        break;
    }
    if fixed.values().sum::<Cents>() != total {
        return Err(SplitError::LimitsInfeasible);
    }
    Ok(fixed.into_iter().map(|(m, v)| (m, sign * v)).collect())
}

/// Per-period caps, applied when a period closes: given each member's total share under a
/// rule in the period, the excess over a member's cap moves to the others in proportion to
/// their own totals (so it follows the rule's method as applied). Returns the zero-sum
/// adjustment to post ([`crate::ledger::Posting::Adjustment`]); empty when no cap is hit.
pub fn period_cap_adjustment(
    rule: &Rule,
    totals: &BTreeMap<MemberId, Cents>,
    seq: u64,
) -> Result<BTreeMap<MemberId, Cents>, SplitError> {
    if rule.period_caps.is_empty() || totals.values().all(|v| *v <= 0) {
        return Ok(BTreeMap::new());
    }
    let total: Cents = totals.values().map(|v| (*v).max(0)).sum();
    let weights: BTreeMap<MemberId, u64> = totals
        .iter()
        .map(|(m, v)| (*m, (*v).max(0) as u64))
        .collect();
    let mut capped = rule.clone();
    capped.method = Method::Shares(BTreeMap::new());
    capped.limits = rule
        .period_caps
        .iter()
        .filter(|(m, _)| totals.contains_key(m))
        .map(|(m, c)| {
            (
                *m,
                Limit {
                    max: Some(*c),
                    min: None,
                },
            )
        })
        .collect();
    // Members who share nothing this period can't absorb an excess: their weight is 0.
    let fixed = limited(&capped, total, &weights, RoundingCtx { seq, payer: None })?;
    let mut out = BTreeMap::new();
    for (m, new) in fixed {
        let old = totals.get(&m).copied().unwrap_or(0).max(0);
        if new != old {
            out.insert(m, new - old);
        }
    }
    Ok(out)
}

fn itemized(
    p: &BTreeSet<MemberId>,
    rounding: Rounding,
    input: &SplitInput,
    ctx: RoundingCtx,
) -> Result<BTreeMap<MemberId, Cents>, SplitError> {
    if input.items.is_empty() {
        return Err(SplitError::NoItems);
    }
    let mut subtotal: BTreeMap<MemberId, Cents> = p.iter().map(|m| (*m, 0)).collect();
    let mut lines_total: Cents = 0;
    for (i, item) in input.items.iter().enumerate() {
        if item.members.is_empty() {
            return Err(SplitError::ItemWithoutMembers(i));
        }
        let mut w = BTreeMap::new();
        for m in &item.members {
            if !p.contains(m) {
                return Err(SplitError::NotAParticipant(*m));
            }
            w.insert(*m, 1u64);
        }
        // each line rotates from a different start, so a long receipt evens out too
        let line_ctx = RoundingCtx {
            seq: ctx.seq.wrapping_add(i as u64),
            ..ctx
        };
        for (m, v) in allocate(item.amount, &w, rounding, line_ctx)? {
            *subtotal.entry(m).or_default() += v;
        }
        lines_total = lines_total
            .checked_add(item.amount)
            .ok_or(SplitError::Overflow)?;
    }
    let extras = input
        .amount
        .checked_sub(lines_total)
        .ok_or(SplitError::Overflow)?;
    if extras != 0 {
        if subtotal.values().any(|v| *v < 0) {
            return Err(SplitError::ExtrasNeedPositiveLines);
        }
        let w: BTreeMap<MemberId, u64> = subtotal
            .iter()
            .map(|(m, v)| (*m, (*v).max(0) as u64))
            .collect();
        let spread = allocate(extras, &w, rounding, ctx).map_err(|e| match e {
            SplitError::NoWeight => SplitError::ExtrasNeedPositiveLines,
            e => e,
        })?;
        for (m, v) in spread {
            *subtotal.entry(m).or_default() += v;
        }
    }
    Ok(subtotal)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: MemberId = MemberId(1);
    const B: MemberId = MemberId(2);
    const C: MemberId = MemberId(3);

    fn map<V: Copy>(pairs: &[(MemberId, V)]) -> BTreeMap<MemberId, V> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn equal_three_ways() {
        let r = Rule::new("Internet", Method::Equal, &[A, B, C]);
        let s = split(&r, &SplitInput::amount(9000)).unwrap();
        assert_eq!(s, map(&[(A, 3000), (B, 3000), (C, 3000)]));
    }

    #[test]
    fn percentages_must_total_100() {
        let r = Rule::new(
            "Rent",
            Method::Percent(map(&[(A, 6000), (B, 3000)])),
            &[A, B],
        );
        assert_eq!(
            split(&r, &SplitInput::amount(100)),
            Err(SplitError::PercentNot100 { total_bp: 9000 })
        );
        let r = Rule::new(
            "Rent",
            Method::Percent(map(&[(A, 6000), (B, 4000)])),
            &[A, B],
        );
        let s = split(&r, &SplitInput::amount(200_000)).unwrap();
        assert_eq!(s, map(&[(A, 120_000), (B, 80_000)]));
    }

    #[test]
    fn opt_out_pays_nothing_and_percentages_count_only_participants() {
        let mut r = Rule::new(
            "Streaming",
            Method::Percent(map(&[(A, 5000), (B, 5000), (C, 2000)])),
            &[A, B, C],
        );
        r.excluded.insert(C);
        let s = split(&r, &SplitInput::amount(1599)).unwrap();
        assert!(!s.contains_key(&C));
        assert_eq!(s.values().sum::<Cents>(), 1599);
    }

    #[test]
    fn shares_two_one() {
        let r = Rule::new("Food", Method::Shares(map(&[(A, 2), (B, 1)])), &[A, B]);
        let s = split(&r, &SplitInput::amount(300)).unwrap();
        assert_eq!(s, map(&[(A, 200), (B, 100)]));
    }

    #[test]
    fn exact_with_remainder() {
        let r = Rule::new(
            "Dinner",
            Method::Exact {
                amounts: map(&[(A, 5000)]),
                remainder: B,
            },
            &[A, B],
        );
        let s = split(&r, &SplitInput::amount(8347)).unwrap();
        assert_eq!(s, map(&[(A, 5000), (B, 3347)]));
        assert!(matches!(
            split(&r, &SplitInput::amount(4000)),
            Err(SplitError::ExactExceedsTotal { .. })
        ));
    }

    #[test]
    fn itemized_spreads_tax_and_tip_pro_rata() {
        let r = Rule::new("Groceries", Method::Itemized, &[A, B]);
        let input = SplitInput {
            amount: 1100, // lines 1000 + 100 tax
            items: vec![
                Item {
                    amount: 600,
                    members: vec![A],
                },
                Item {
                    amount: 400,
                    members: vec![A, B],
                },
            ],
            ..Default::default()
        };
        let s = split(&r, &input).unwrap();
        // A: 600 + 200 = 800 → +80 tax; B: 200 → +20
        assert_eq!(s, map(&[(A, 880), (B, 220)]));
    }

    #[test]
    fn income_ratio() {
        let r = Rule::new(
            "Groceries",
            Method::Income(map(&[(A, 64), (B, 36)])),
            &[A, B],
        );
        let s = split(&r, &SplitInput::amount(10_000)).unwrap();
        assert_eq!(s, map(&[(A, 6400), (B, 3600)]));
    }

    #[test]
    fn room_weights() {
        let r = Rule::new(
            "Rent",
            Method::Room(map(&[(A, 180), (B, 120), (C, 100)])),
            &[A, B, C],
        );
        let s = split(&r, &SplitInput::amount(200_000)).unwrap();
        assert_eq!(s, map(&[(A, 90_000), (B, 60_000), (C, 50_000)]));
    }

    #[test]
    fn days_present() {
        let r = Rule::new("Electric", Method::Days, &[A, B]);
        let input = SplitInput {
            amount: 9100,
            days: map(&[(A, 31), (B, 20)]),
            ..Default::default()
        };
        let s = split(&r, &input).unwrap();
        assert_eq!(s.values().sum::<Cents>(), 9100);
        // 9100 × 31/51 = 5531.37: the exact share rounded down, or up by the rotating cent
        assert!(s[&A] == 5531 || s[&A] == 5532);
        let missing = SplitInput {
            amount: 9100,
            days: map(&[(A, 31)]),
            ..Default::default()
        };
        assert_eq!(split(&r, &missing), Err(SplitError::MissingInput(B)));
    }

    #[test]
    fn usage_readings() {
        let r = Rule::new("Car", Method::Usage, &[A, B]);
        let input = SplitInput {
            amount: 6000,
            usage: map(&[(A, 300), (B, 100)]),
            ..Default::default()
        };
        assert_eq!(split(&r, &input).unwrap(), map(&[(A, 4500), (B, 1500)]));
    }

    #[test]
    fn a_cap_moves_the_excess_to_the_others() {
        let mut r = Rule::new("Utilities", Method::Equal, &[A, B, C]);
        r.limits.insert(
            A,
            Limit {
                max: Some(10_000),
                min: None,
            },
        );
        let s = split(&r, &SplitInput::amount(45_000)).unwrap();
        assert_eq!(s, map(&[(A, 10_000), (B, 17_500), (C, 17_500)]));
    }

    #[test]
    fn a_floor_is_met_first() {
        let mut r = Rule::new("Rent", Method::Shares(map(&[(A, 1), (B, 9)])), &[A, B]);
        r.limits.insert(
            A,
            Limit {
                max: None,
                min: Some(3000),
            },
        );
        let s = split(&r, &SplitInput::amount(10_000)).unwrap();
        assert_eq!(s, map(&[(A, 3000), (B, 7000)]));
    }

    #[test]
    fn caps_that_cannot_cover_the_amount_are_refused() {
        let mut r = Rule::new("Utilities", Method::Equal, &[A, B]);
        for m in [A, B] {
            r.limits.insert(
                m,
                Limit {
                    max: Some(100),
                    min: None,
                },
            );
        }
        assert_eq!(
            split(&r, &SplitInput::amount(500)),
            Err(SplitError::LimitsInfeasible)
        );
    }

    #[test]
    fn a_period_cap_moves_the_excess_at_close() {
        let mut r = Rule::new("Utilities", Method::Equal, &[A, B, C]);
        r.period_caps.insert(A, 10_000);
        let totals = map(&[(A, 15_000), (B, 15_000), (C, 15_000)]);
        let adj = period_cap_adjustment(&r, &totals, 0).unwrap();
        assert_eq!(adj, map(&[(A, -5000), (B, 2500), (C, 2500)]));
        assert_eq!(adj.values().sum::<Cents>(), 0);
        // under the cap: nothing to post
        let low = map(&[(A, 9000), (B, 9000), (C, 9000)]);
        assert!(period_cap_adjustment(&r, &low, 0).unwrap().is_empty());
        // everyone capped below the total: refused
        r.period_caps.insert(B, 1);
        r.period_caps.insert(C, 1);
        assert_eq!(
            period_cap_adjustment(&r, &totals, 0),
            Err(SplitError::LimitsInfeasible)
        );
    }

    #[test]
    fn names_outside_the_rule_are_refused() {
        let r = Rule::new("x", Method::Shares(map(&[(A, 1), (C, 1)])), &[A, B]);
        assert_eq!(r.validate(), Err(SplitError::NotAMember(C)));
    }
}
