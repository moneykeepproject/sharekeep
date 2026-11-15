//! Property tests for the split engine: thousands of random cases, checked against the rules
//! every household relies on. A small seeded generator keeps them repeatable and adds no
//! dependency; a failing case prints its seed.

use sk_core::{Cents, DateSpan, DepositId, MemberId, NaiveDate};
use sk_split::alloc::Rounding;
use sk_split::deposits::{DepositKind, DepositState};
use sk_split::ledger::{settle_up, Balances, Paid, Posting};
use sk_split::method::{split, Item, Limit, Method, Rule, SplitInput, FULL_BP};
use sk_split::prorate::{days_present, Membership};
use std::collections::BTreeMap;

/// xorshift64*: tiny, seeded, good enough for test cases.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const CASES: u64 = 4000;

fn members(n: usize) -> Vec<MemberId> {
    (1..=n as u32).map(MemberId).collect()
}

fn amount(r: &mut Rng) -> Cents {
    let a = match r.below(4) {
        0 => r.range(0, 10),
        1 => r.range(0, 10_000),
        2 => r.range(0, 10_000_000),
        _ => r.range(0, 1_000_000_000_000),
    };
    if r.chance(15) {
        -a
    } else {
        a
    }
}

fn rounding(r: &mut Rng) -> Rounding {
    match r.below(3) {
        0 => Rounding::Rotate,
        1 => Rounding::LargestFraction,
        _ => Rounding::Payer,
    }
}

/// A random percent map over `ms` totalling exactly 100%.
fn percents(r: &mut Rng, ms: &[MemberId]) -> BTreeMap<MemberId, u32> {
    let mut left = FULL_BP;
    let mut out = BTreeMap::new();
    for (i, m) in ms.iter().enumerate() {
        let v = if i + 1 == ms.len() {
            left
        } else {
            r.below(u64::from(left) + 1) as u32
        };
        left -= v;
        out.insert(*m, v);
    }
    out
}

fn weights(r: &mut Rng, ms: &[MemberId], max: u64) -> BTreeMap<MemberId, u32> {
    let mut w: BTreeMap<MemberId, u32> = ms.iter().map(|m| (*m, r.below(max + 1) as u32)).collect();
    // at least one non-zero weight
    let first = ms[0];
    if w.values().all(|v| *v == 0) {
        w.insert(first, 1);
    }
    w
}

/// A random weighted rule over `ms`, and the per-entry input it needs; the exact weights
/// are returned too, to check each share against its exact value.
fn weighted_case(r: &mut Rng, ms: &[MemberId]) -> (Rule, SplitInput, BTreeMap<MemberId, u64>) {
    let mut input = SplitInput {
        amount: amount(r),
        seq: r.next(),
        payer: Some(ms[r.below(ms.len() as u64) as usize]),
        ..Default::default()
    };
    let (method, w): (Method, BTreeMap<MemberId, u64>) = match r.below(7) {
        0 => (Method::Equal, ms.iter().map(|m| (*m, 1)).collect()),
        1 => {
            let p = percents(r, ms);
            let w = p.iter().map(|(m, v)| (*m, u64::from(*v))).collect();
            if w_all_zero(&w) {
                (Method::Equal, ms.iter().map(|m| (*m, 1)).collect())
            } else {
                (Method::Percent(p), w)
            }
        }
        2 => {
            let s = weights(r, ms, 5);
            let w = s.iter().map(|(m, v)| (*m, u64::from(*v))).collect();
            (Method::Shares(s), w)
        }
        3 => {
            let s = weights(r, ms, 1000);
            let w = s.iter().map(|(m, v)| (*m, u64::from(*v))).collect();
            (Method::Income(s), w)
        }
        4 => {
            let s = weights(r, ms, 400);
            let w = s.iter().map(|(m, v)| (*m, u64::from(*v))).collect();
            (Method::Room(s), w)
        }
        5 => {
            let s = weights(r, ms, 31);
            input.days = s.clone();
            (
                Method::Days,
                s.iter().map(|(m, v)| (*m, u64::from(*v))).collect(),
            )
        }
        _ => {
            let s: BTreeMap<MemberId, u64> = weights(r, ms, 100_000)
                .into_iter()
                .map(|(m, v)| (m, u64::from(v)))
                .collect();
            input.usage = s.clone();
            (Method::Usage, s)
        }
    };
    let mut rule = Rule::new("random", method, ms);
    rule.rounding = rounding(r);
    (rule, input, w)
}

fn w_all_zero(w: &BTreeMap<MemberId, u64>) -> bool {
    w.values().all(|v| *v == 0)
}

#[test]
fn every_split_adds_up_to_the_cent_and_stays_within_a_cent_of_exact() {
    for seed in 0..CASES {
        let mut r = Rng::new(seed);
        let ms = members(r.range(1, 8) as usize);
        let (rule, input, w) = weighted_case(&mut r, &ms);
        let shares = split(&rule, &input).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(
            shares.values().map(|v| i128::from(*v)).sum::<i128>(),
            i128::from(input.amount),
            "seed {seed}"
        );
        let total_w: i128 = w.values().map(|v| i128::from(*v)).sum();
        let n = ms.len() as i128;
        for (m, s) in &shares {
            // exact share × total weight, compared without division
            let exact_x = i128::from(input.amount) * i128::from(w[m]);
            let diff = (i128::from(*s) * total_w - exact_x).abs();
            let allowed = if rule.rounding == Rounding::Payer && input.payer == Some(*m) {
                n * total_w
            } else {
                total_w
            };
            assert!(
                diff < allowed,
                "seed {seed}: {m} got {s}, exact {}/{}",
                exact_x,
                total_w
            );
        }
    }
}

#[test]
fn rotating_cents_even_out_over_time() {
    for seed in 0..500 {
        let mut r = Rng::new(seed);
        let n = r.range(2, 7) as usize;
        let ms = members(n);
        let amt = r.range(1, 100_000);
        let rule = Rule::new("equal", Method::Equal, &ms);
        let mut totals: BTreeMap<MemberId, Cents> = BTreeMap::new();
        let entries = r.range(1, 60) as u64;
        for seq in 0..entries {
            let s = split(
                &rule,
                &SplitInput {
                    amount: amt,
                    seq,
                    ..Default::default()
                },
            )
            .unwrap();
            for (m, v) in s {
                *totals.entry(m).or_default() += v;
            }
        }
        let hi = totals.values().max().unwrap();
        let lo = totals.values().min().unwrap();
        assert!(hi - lo <= 1, "seed {seed}: totals spread {lo}..{hi}");
    }
}

#[test]
fn caps_and_floors_hold_whenever_a_split_succeeds() {
    let mut ok = 0;
    for seed in 0..CASES {
        let mut r = Rng::new(seed);
        let ms = members(r.range(2, 6) as usize);
        let (mut rule, mut input, _) = weighted_case(&mut r, &ms);
        input.amount = input.amount.rem_euclid(1_000_000);
        for m in &ms {
            if r.chance(40) {
                let a = r.range(0, 400_000);
                let b = r.range(0, 400_000);
                let limit = match r.below(3) {
                    0 => Limit {
                        max: Some(a.max(b)),
                        min: None,
                    },
                    1 => Limit {
                        max: None,
                        min: Some(a.min(b)),
                    },
                    _ => Limit {
                        max: Some(a.max(b)),
                        min: Some(a.min(b)),
                    },
                };
                rule.limits.insert(*m, limit);
            }
        }
        let Ok(shares) = split(&rule, &input) else {
            continue;
        };
        ok += 1;
        assert_eq!(shares.values().sum::<Cents>(), input.amount, "seed {seed}");
        for (m, s) in &shares {
            let l = rule.limits.get(m).copied().unwrap_or_default();
            assert!(l.max.is_none_or(|x| *s <= x), "seed {seed}: {m} over cap");
            assert!(
                l.min.is_none_or(|x| *s >= x),
                "seed {seed}: {m} under floor"
            );
        }
    }
    assert!(ok > CASES / 4, "too few feasible cases: {ok}");
}

#[test]
fn itemized_splits_add_up() {
    for seed in 0..CASES {
        let mut r = Rng::new(seed);
        let ms = members(r.range(1, 6) as usize);
        let lines = r.range(1, 12);
        let mut items = Vec::new();
        let mut total = 0;
        for _ in 0..lines {
            let a = r.range(0, 50_000);
            let mut who: Vec<MemberId> = ms.iter().copied().filter(|_| r.chance(50)).collect();
            if who.is_empty() {
                who.push(ms[0]);
            }
            total += a;
            items.push(Item {
                amount: a,
                members: who,
            });
        }
        let extras = if total > 0 { r.range(0, total / 4) } else { 0 };
        let mut rule = Rule::new("receipt", Method::Itemized, &ms);
        rule.rounding = rounding(&mut r);
        let input = SplitInput {
            amount: total + extras,
            seq: r.next(),
            payer: Some(ms[0]),
            items,
            ..Default::default()
        };
        let s = split(&rule, &input).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(s.values().sum::<Cents>(), input.amount, "seed {seed}");
        assert!(s.values().all(|v| *v >= 0), "seed {seed}");
    }
}

#[test]
fn balances_always_sum_to_zero_and_settle_up_clears_them() {
    for seed in 0..1000 {
        let mut r = Rng::new(seed);
        let n = r.range(2, 8) as usize;
        let ms = members(n);
        let mut bal = Balances::new();
        for _ in 0..r.range(1, 40) {
            let (rule, input, _) = weighted_case(&mut r, &ms);
            let mut input = input;
            input.amount = input.amount.clamp(-1_000_000, 1_000_000);
            let shares = split(&rule, &input).unwrap();
            let paid = if r.chance(10) {
                Paid::Pot(DepositId(1))
            } else {
                Paid::Member(ms[r.below(n as u64) as usize])
            };
            bal.apply(&Posting::Expense { paid, shares }).unwrap();
            assert_eq!(bal.sum(), 0, "seed {seed}");
        }
        let transfers = settle_up(&bal);
        let with_balance = bal.iter().filter(|(_, v)| *v != 0).count();
        assert!(transfers.len() < with_balance.max(1), "seed {seed}");
        for t in transfers {
            assert!(t.amount > 0);
            bal.apply(&Posting::Payment {
                from: t.from,
                to: t.to,
                amount: t.amount,
            })
            .unwrap();
        }
        assert!(bal.iter().all(|(_, v)| v == 0), "seed {seed}");
    }
}

#[test]
fn deposit_stakes_always_add_up_to_the_balance() {
    for seed in 0..CASES {
        let mut r = Rng::new(seed);
        let ms = members(r.range(1, 5) as usize);
        let pick = |r: &mut Rng| ms[r.below(ms.len() as u64) as usize];
        let mut d = DepositState::new();
        let kind = if r.chance(50) {
            DepositKind::Held
        } else {
            DepositKind::Spending
        };
        for _ in 0..r.range(1, 30) {
            let _ = match r.below(4) {
                0 => {
                    let m = pick(&mut r);
                    d.pay_in(m, r.range(1, 100_000))
                }
                1 => {
                    let rule = Rule::new("x", Method::Equal, &ms);
                    let s = split(&rule, &SplitInput::amount(r.range(1, 50_000))).unwrap();
                    d.spend(kind, &s)
                }
                2 => {
                    let (a, b) = (pick(&mut r), pick(&mut r));
                    let amt = r.range(1, 50_000);
                    if a != b {
                        d.buy_out(a, b, amt)
                    } else {
                        Ok(())
                    }
                }
                _ => d.credit_interest(r.range(0, 500)).or(Ok(())),
            };
            assert_eq!(
                d.stakes().values().sum::<Cents>(),
                d.balance(),
                "seed {seed}"
            );
            assert!(d.balance() >= 0, "seed {seed}");
        }
    }
}

#[test]
fn joining_and_leaving_move_no_money_in_or_out() {
    let day = |n: i64| NaiveDate::from_ymd_opt(2027, 1, 1).unwrap() + chrono::Days::new(n as u64);
    for seed in 0..1000 {
        let mut r = Rng::new(seed);
        let n = r.range(2, 6) as usize;
        let ms = members(n);
        let start = r.range(0, 300);
        let len = r.range(1, 60);
        let period = DateSpan::new(day(start), day(start + len - 1)).unwrap();
        let mut stays = BTreeMap::new();
        for m in &ms {
            let joined = day(r.range(0, 365));
            let left = r
                .chance(40)
                .then(|| day(r.range(0, 400)))
                .filter(|l| *l >= joined);
            stays.insert(*m, Membership { joined, left });
        }
        let days = days_present(&stays, &period);
        if days.values().all(|v| *v == 0) {
            continue;
        }
        let rule = Rule::new("rent", Method::Days, &ms);
        let input = SplitInput {
            amount: r.range(0, 1_000_000),
            seq: r.next(),
            days: days.clone(),
            ..Default::default()
        };
        let shares = split(&rule, &input).unwrap();
        assert_eq!(shares.values().sum::<Cents>(), input.amount, "seed {seed}");
        for (m, d) in &days {
            if *d == 0 {
                assert_eq!(shares[m], 0, "seed {seed}: absent member charged");
            }
        }
    }
}

#[test]
fn periods_tile_the_calendar_without_gaps_or_overlaps() {
    use chrono::Weekday;
    use sk_split::periods::{MonthDay, Schedule};
    let base = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let day = |n: i64| base + chrono::Days::new(n as u64);
    let weekdays = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    for seed in 0..400 {
        let mut r = Rng::new(seed);
        let start = day(r.range(0, 800));
        let s = match r.below(3) {
            0 => Schedule::Weekly {
                start,
                weekday: weekdays[r.below(7) as usize],
                every: r.range(1, 4) as u8,
            },
            1 => Schedule::Monthly {
                start,
                day: if r.chance(20) {
                    MonthDay::Last
                } else {
                    MonthDay::Day(r.range(1, 28) as u8)
                },
                every: [1u8, 2, 3, 6, 12][r.below(5) as usize],
            },
            _ => Schedule::EveryNDays {
                start,
                days: r.range(1, 365) as u16,
            },
        };
        s.validate().unwrap();
        let mut date = start;
        let mut prev: Option<DateSpan> = None;
        for _ in 0..40 {
            let p = s
                .period_containing(date)
                .unwrap_or_else(|| panic!("seed {seed}: no period for {date}"));
            assert!(p.contains(date), "seed {seed}");
            if let Some(q) = prev {
                assert_eq!(
                    q.last.succ_opt(),
                    Some(p.first),
                    "seed {seed}: gap or overlap"
                );
            } else {
                assert_eq!(p.first, start, "seed {seed}");
            }
            // any day inside lands in the same period
            let mid = p.first + chrono::Days::new(u64::from(p.days() - 1) / 2);
            assert_eq!(s.period_containing(mid), Some(p), "seed {seed}");
            prev = Some(p);
            date = p.last.succ_opt().unwrap();
        }
    }
}

#[test]
fn period_caps_hold_and_move_no_money() {
    use sk_split::method::period_cap_adjustment;
    for seed in 0..CASES {
        let mut r = Rng::new(seed);
        let ms = members(r.range(2, 6) as usize);
        let mut rule = Rule::new("x", Method::Equal, &ms);
        let totals: BTreeMap<MemberId, Cents> =
            ms.iter().map(|m| (*m, r.range(0, 200_000))).collect();
        for m in &ms {
            if r.chance(40) {
                rule.period_caps.insert(*m, r.range(0, 150_000));
            }
        }
        let Ok(adj) = period_cap_adjustment(&rule, &totals, r.next()) else {
            continue;
        };
        assert_eq!(adj.values().sum::<Cents>(), 0, "seed {seed}");
        for m in &ms {
            let after = totals[m] + adj.get(m).copied().unwrap_or(0);
            if let Some(cap) = rule.period_caps.get(m) {
                assert!(
                    after <= *cap || adj.is_empty() && totals[m] <= *cap,
                    "seed {seed}"
                );
            }
            assert!(after >= 0, "seed {seed}");
        }
    }
}
