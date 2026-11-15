//! Settle-up periods: weekly, monthly, every N days or custom; schedule changes at the end
//! of the current period; special events; recurring bill due dates; closing and the grace
//! period for late entries.

use crate::SplitError;
use chrono::{Datelike, Days, Months, Weekday};
use serde::{Deserialize, Serialize};
use sk_core::{DateSpan, MemberId, NaiveDate};
use std::collections::BTreeSet;

/// The day of the month a monthly schedule starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonthDay {
    /// The 1st to the 28th.
    Day(u8),
    /// The last day of the month.
    Last,
}

/// How an agreement's periods (or a bill's due dates) repeat. Every schedule starts on
/// `start`; when `start` isn't itself on the schedule's rhythm, the first period is a short
/// one up to the first regular start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schedule {
    /// Starting on `weekday`, every 1–4 weeks.
    Weekly {
        start: NaiveDate,
        weekday: Weekday,
        every: u8,
    },
    /// Starting on `day`, every 1, 2, 3, 6 or 12 months (counted from `start`'s month).
    Monthly {
        start: NaiveDate,
        day: MonthDay,
        every: u8,
    },
    /// Every `days` days (1–365) from `start`.
    EveryNDays { start: NaiveDate, days: u16 },
    /// Period end dates entered by hand, in order; the first period starts on `start`.
    Custom {
        start: NaiveDate,
        ends: Vec<NaiveDate>,
    },
}

/// Most periods a single lookup walks (about a century of daily periods).
const MAX_STEPS: u32 = 40_000;

fn add_days(d: NaiveDate, n: u64) -> Option<NaiveDate> {
    d.checked_add_days(Days::new(n))
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// The date `day` in the month `months` after `base`'s month.
fn day_in_month(base: NaiveDate, months: u32, day: MonthDay) -> Option<NaiveDate> {
    let first = first_of_month(base).checked_add_months(Months::new(months))?;
    match day {
        MonthDay::Day(d) => first.with_day(u32::from(d)),
        MonthDay::Last => first.checked_add_months(Months::new(1))?.pred_opt(),
    }
}

impl Schedule {
    pub fn start(&self) -> NaiveDate {
        match self {
            Schedule::Weekly { start, .. }
            | Schedule::Monthly { start, .. }
            | Schedule::EveryNDays { start, .. }
            | Schedule::Custom { start, .. } => *start,
        }
    }

    /// Check the schedule's settings against what the spec allows.
    pub fn validate(&self) -> Result<(), SplitError> {
        let bad = |s: &str| Err(SplitError::InvalidSchedule(s.to_string()));
        match self {
            Schedule::Weekly { every, .. } if !(1..=4).contains(every) => {
                bad("weekly schedules repeat every 1 to 4 weeks")
            }
            Schedule::Monthly { every, .. } if ![1, 2, 3, 6, 12].contains(every) => {
                bad("monthly schedules repeat every 1, 2, 3, 6 or 12 months")
            }
            Schedule::Monthly {
                day: MonthDay::Day(d),
                ..
            } if !(1..=28).contains(d) => {
                bad("a monthly start day is the 1st to the 28th, or the last day")
            }
            Schedule::EveryNDays { days, .. } if !(1..=365).contains(days) => {
                bad("a period is 1 to 365 days")
            }
            Schedule::Custom { start, ends } => {
                if ends.is_empty() {
                    return bad("a custom schedule needs at least one end date");
                }
                let mut prev: Option<NaiveDate> = None;
                for e in ends {
                    if *e < *start || prev.is_some_and(|p| *e <= p) {
                        return bad("custom end dates must be in order, on or after the start");
                    }
                    prev = Some(*e);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The first period start after `prev` (itself a period start), if there is one.
    fn next_start(&self, prev: NaiveDate) -> Option<NaiveDate> {
        match self {
            Schedule::Weekly {
                start,
                weekday,
                every,
            } => {
                let offset = (7 + weekday.num_days_from_monday()
                    - start.weekday().num_days_from_monday())
                    % 7;
                let anchor = add_days(*start, u64::from(offset))?;
                if prev < anchor {
                    return Some(anchor);
                }
                let step = 7 * i64::from(*every);
                let k = (prev - anchor).num_days() / step + 1;
                add_days(anchor, (k * step) as u64)
            }
            Schedule::Monthly { start, day, every } => {
                let every = u32::from(*every);
                let months_since =
                    (prev.year() - start.year()) * 12 + prev.month() as i32 - start.month() as i32;
                let mut k = (months_since.max(0) as u32 / every).saturating_sub(1);
                loop {
                    let b = day_in_month(*start, k * every, *day)?;
                    if b > prev {
                        return Some(b);
                    }
                    k += 1;
                }
            }
            Schedule::EveryNDays { start, days } => {
                let n = i64::from(*days);
                let k = (prev - *start).num_days().max(0) / n + 1;
                add_days(*start, (k * n) as u64)
            }
            Schedule::Custom { ends, .. } => {
                ends.iter().find(|e| **e >= prev).and_then(|e| e.succ_opt())
            }
        }
    }

    /// The period that contains `date`, or `None` before the start (or after a custom
    /// schedule's last end date).
    pub fn period_containing(&self, date: NaiveDate) -> Option<DateSpan> {
        let mut s = self.start();
        if date < s {
            return None;
        }
        for _ in 0..MAX_STEPS {
            let next = self.next_start(s)?;
            if date < next {
                return DateSpan::new(s, next.pred_opt()?);
            }
            s = next;
        }
        None
    }

    /// Every period that overlaps `span`, in order.
    pub fn periods_overlapping(&self, span: &DateSpan) -> Vec<DateSpan> {
        let mut out = Vec::new();
        let mut s = self.start();
        for _ in 0..MAX_STEPS {
            if s > span.last {
                break;
            }
            let Some(next) = self.next_start(s) else {
                break;
            };
            if let Some(p) = next.pred_opt().and_then(|e| DateSpan::new(s, e)) {
                if p.overlap(span).is_some() {
                    out.push(p);
                }
            }
            s = next;
        }
        out
    }

    /// The schedule's dates (period starts) within `span`: a recurring bill's due dates.
    pub fn dates_in(&self, span: &DateSpan) -> Vec<NaiveDate> {
        let mut out = Vec::new();
        let mut s = self.start();
        for _ in 0..MAX_STEPS {
            if s > span.last {
                break;
            }
            if span.contains(s) {
                out.push(s);
            }
            match self.next_start(s) {
                Some(n) => s = n,
                None => break,
            }
        }
        out
    }
}

/// An agreement's schedule over time. A change takes effect when the current period ends:
/// the current period is never cut short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleBook {
    versions: Vec<Schedule>,
}

impl ScheduleBook {
    pub fn new(first: Schedule) -> Result<Self, SplitError> {
        first.validate()?;
        Ok(ScheduleBook {
            versions: vec![first],
        })
    }

    fn version_at(&self, date: NaiveDate) -> Option<&Schedule> {
        self.versions.iter().rev().find(|s| s.start() <= date)
    }

    pub fn period_containing(&self, date: NaiveDate) -> Option<DateSpan> {
        let v = self.version_at(date)?;
        let p = v.period_containing(date)?;
        // A later version starting inside this period ends it early; by construction a
        // change only starts the day after a period ends, so this only trims a custom tail.
        let cut = self
            .versions
            .iter()
            .map(Schedule::start)
            .filter(|s| *s > p.first && *s <= p.last)
            .min();
        match cut {
            Some(c) => DateSpan::new(p.first, c.pred_opt()?),
            None => Some(p),
        }
    }

    /// Change the schedule from the end of the period containing `today`. `make` builds the
    /// new schedule from its start date (the day after that period ends). A change made
    /// again before it takes effect replaces the pending one. Returns the new start.
    pub fn change(
        &mut self,
        today: NaiveDate,
        make: impl FnOnce(NaiveDate) -> Schedule,
    ) -> Result<NaiveDate, SplitError> {
        if self.versions.last().is_some_and(|v| v.start() > today) {
            self.versions.pop();
        }
        let current = self
            .period_containing(today)
            .ok_or(SplitError::NoPeriod(today))?;
        let start = current.last.succ_opt().ok_or(SplitError::NoPeriod(today))?;
        let new = make(start);
        new.validate()?;
        if new.start() != start {
            return Err(SplitError::InvalidSchedule(
                "a new schedule starts the day after the current period ends".into(),
            ));
        }
        self.versions.push(new);
        Ok(start)
    }

    pub fn versions(&self) -> &[Schedule] {
        &self.versions
    }
}

/// A special event that settles on its own ("Beach week, Jul 4–11").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub name: String,
    pub span: DateSpan,
    pub members: BTreeSet<MemberId>,
}

/// Where an entry counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement<'a> {
    /// Only in this event's settlement, never in the regular period it falls in.
    Event(&'a Event),
    Period(DateSpan),
}

/// Where an entry on `date` counts: its event when tagged (the date must fall within the
/// event), otherwise the regular period containing it.
pub fn place<'a>(
    date: NaiveDate,
    event: Option<&'a Event>,
    schedule: &ScheduleBook,
) -> Result<Placement<'a>, SplitError> {
    match event {
        Some(e) if e.span.contains(date) => Ok(Placement::Event(e)),
        Some(_) => Err(SplitError::OutsideEvent(date)),
        None => schedule
            .period_containing(date)
            .map(Placement::Period)
            .ok_or(SplitError::NoPeriod(date)),
    }
}

/// Where a period stands on `today`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodStatus {
    Open,
    /// The period has ended; any member can close it. Late entries are still accepted
    /// until the grace period ends.
    ReadyToClose {
        late_entries_until: NaiveDate,
    },
    /// The grace period is over: no more late entries; it waits only to be closed.
    GraceOver,
}

/// Status of `period` on `today`, with a grace period of `grace_days` (default 7).
pub fn status(period: &DateSpan, today: NaiveDate, grace_days: u16) -> PeriodStatus {
    if today <= period.last {
        return PeriodStatus::Open;
    }
    let until = add_days(period.last, u64::from(grace_days)).unwrap_or(period.last);
    if today <= until {
        PeriodStatus::ReadyToClose {
            late_entries_until: until,
        }
    } else {
        PeriodStatus::GraceOver
    }
}

/// The default grace period for late entries, in days.
pub const DEFAULT_GRACE_DAYS: u16 = 7;

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn span(a: NaiveDate, b: NaiveDate) -> DateSpan {
        DateSpan::new(a, b).unwrap()
    }

    #[test]
    fn monthly_on_the_first() {
        let s = Schedule::Monthly {
            start: d(2027, 1, 1),
            day: MonthDay::Day(1),
            every: 1,
        };
        assert_eq!(
            s.period_containing(d(2027, 2, 14)),
            Some(span(d(2027, 2, 1), d(2027, 2, 28)))
        );
        assert_eq!(s.period_containing(d(2026, 12, 31)), None);
    }

    #[test]
    fn monthly_on_the_15th_with_a_short_first_period() {
        let s = Schedule::Monthly {
            start: d(2027, 1, 3),
            day: MonthDay::Day(15),
            every: 1,
        };
        assert_eq!(
            s.period_containing(d(2027, 1, 10)),
            Some(span(d(2027, 1, 3), d(2027, 1, 14)))
        );
        assert_eq!(
            s.period_containing(d(2027, 1, 20)),
            Some(span(d(2027, 1, 15), d(2027, 2, 14)))
        );
    }

    #[test]
    fn monthly_on_the_last_day_and_quarterly() {
        let s = Schedule::Monthly {
            start: d(2027, 1, 31),
            day: MonthDay::Last,
            every: 1,
        };
        assert_eq!(
            s.period_containing(d(2027, 2, 10)),
            Some(span(d(2027, 1, 31), d(2027, 2, 27)))
        );
        let q = Schedule::Monthly {
            start: d(2027, 1, 1),
            day: MonthDay::Day(1),
            every: 3,
        };
        assert_eq!(
            q.period_containing(d(2027, 5, 5)),
            Some(span(d(2027, 4, 1), d(2027, 6, 30)))
        );
    }

    #[test]
    fn weekly_from_a_monday_every_two_weeks() {
        // 2027-01-06 is a Wednesday; the first Monday after is 2027-01-11
        let s = Schedule::Weekly {
            start: d(2027, 1, 6),
            weekday: Weekday::Mon,
            every: 2,
        };
        assert_eq!(
            s.period_containing(d(2027, 1, 7)),
            Some(span(d(2027, 1, 6), d(2027, 1, 10)))
        );
        assert_eq!(
            s.period_containing(d(2027, 1, 20)),
            Some(span(d(2027, 1, 11), d(2027, 1, 24)))
        );
    }

    #[test]
    fn every_n_days_and_custom() {
        let s = Schedule::EveryNDays {
            start: d(2027, 1, 1),
            days: 10,
        };
        assert_eq!(
            s.period_containing(d(2027, 1, 25)),
            Some(span(d(2027, 1, 21), d(2027, 1, 30)))
        );
        let c = Schedule::Custom {
            start: d(2027, 1, 1),
            ends: vec![d(2027, 1, 9), d(2027, 2, 2)],
        };
        assert_eq!(
            c.period_containing(d(2027, 1, 10)),
            Some(span(d(2027, 1, 10), d(2027, 2, 2)))
        );
        assert_eq!(c.period_containing(d(2027, 2, 3)), None);
    }

    #[test]
    fn invalid_settings_are_refused() {
        let bad = [
            Schedule::Weekly {
                start: d(2027, 1, 1),
                weekday: Weekday::Mon,
                every: 5,
            },
            Schedule::Monthly {
                start: d(2027, 1, 1),
                day: MonthDay::Day(29),
                every: 1,
            },
            Schedule::Monthly {
                start: d(2027, 1, 1),
                day: MonthDay::Day(1),
                every: 4,
            },
            Schedule::EveryNDays {
                start: d(2027, 1, 1),
                days: 0,
            },
            Schedule::Custom {
                start: d(2027, 1, 5),
                ends: vec![d(2027, 1, 9), d(2027, 1, 9)],
            },
        ];
        for s in bad {
            assert!(s.validate().is_err(), "{s:?}");
        }
    }

    #[test]
    fn a_schedule_change_waits_for_the_current_period_to_end() {
        let mut book = ScheduleBook::new(Schedule::Monthly {
            start: d(2027, 1, 1),
            day: MonthDay::Day(1),
            every: 1,
        })
        .unwrap();
        let from = book
            .change(d(2027, 3, 10), |start| Schedule::Weekly {
                start,
                weekday: Weekday::Mon,
                every: 1,
            })
            .unwrap();
        assert_eq!(from, d(2027, 4, 1));
        // March is untouched
        assert_eq!(
            book.period_containing(d(2027, 3, 31)),
            Some(span(d(2027, 3, 1), d(2027, 3, 31)))
        );
        // 2027-04-01 is a Thursday: a short period to Sunday, then Monday weeks
        assert_eq!(
            book.period_containing(d(2027, 4, 2)),
            Some(span(d(2027, 4, 1), d(2027, 4, 4)))
        );
        assert_eq!(
            book.period_containing(d(2027, 4, 6)),
            Some(span(d(2027, 4, 5), d(2027, 4, 11)))
        );
        // changing again before it takes effect replaces the pending change
        book.change(d(2027, 3, 20), |start| Schedule::EveryNDays {
            start,
            days: 14,
        })
        .unwrap();
        assert_eq!(book.versions().len(), 2);
    }

    #[test]
    fn bill_due_dates() {
        let rent = Schedule::Monthly {
            start: d(2027, 1, 1),
            day: MonthDay::Day(1),
            every: 1,
        };
        let q1 = span(d(2027, 1, 1), d(2027, 3, 31));
        assert_eq!(
            rent.dates_in(&q1),
            vec![d(2027, 1, 1), d(2027, 2, 1), d(2027, 3, 1)]
        );
        assert_eq!(rent.periods_overlapping(&q1).len(), 3);
    }

    #[test]
    fn event_entries_skip_the_regular_period() {
        let book = ScheduleBook::new(Schedule::Monthly {
            start: d(2027, 1, 1),
            day: MonthDay::Day(1),
            every: 1,
        })
        .unwrap();
        let beach = Event {
            name: "Beach week".into(),
            span: span(d(2027, 7, 4), d(2027, 7, 11)),
            members: BTreeSet::new(),
        };
        assert!(matches!(
            place(d(2027, 7, 5), Some(&beach), &book),
            Ok(Placement::Event(_))
        ));
        assert_eq!(
            place(d(2027, 7, 5), None, &book),
            Ok(Placement::Period(span(d(2027, 7, 1), d(2027, 7, 31))))
        );
        assert!(place(d(2027, 7, 20), Some(&beach), &book).is_err());
    }

    #[test]
    fn closing_and_the_grace_period() {
        let march = span(d(2027, 3, 1), d(2027, 3, 31));
        assert_eq!(status(&march, d(2027, 3, 31), 7), PeriodStatus::Open);
        assert_eq!(
            status(&march, d(2027, 4, 1), 7),
            PeriodStatus::ReadyToClose {
                late_entries_until: d(2027, 4, 7)
            }
        );
        assert_eq!(status(&march, d(2027, 4, 8), 7), PeriodStatus::GraceOver);
    }
}
