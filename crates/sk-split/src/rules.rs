//! A book's rules over time: versions with effective dates, category defaults and named
//! templates.

use crate::method::Rule;
use crate::SplitError;
use serde::{Deserialize, Serialize};
use sk_core::{NaiveDate, RuleId};
use std::collections::BTreeMap;

/// One version of a rule, in force from `effective` until the next version's date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleVersion {
    pub effective: NaiveDate,
    pub rule: Rule,
}

/// The rule that applies to an entry, and which version it was (stored with the entry, so
/// a later change never rewrites an old entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub id: RuleId,
    pub effective: NaiveDate,
    pub rule: &'a Rule,
}

/// All the rules of an agreement, with their history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleBook {
    rules: BTreeMap<RuleId, Vec<RuleVersion>>,
    /// Category name → its default rule.
    defaults: BTreeMap<String, RuleId>,
    /// The rule for a category with no default of its own.
    fallback: Option<RuleId>,
    /// Saved templates by name: a rule to copy into any agreement.
    templates: BTreeMap<String, Rule>,
}

impl RuleBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a version of rule `id`, in force from `effective`. The rule is checked first.
    /// Versions may be added in any order, but two versions of one rule can't share a date.
    pub fn add_version(
        &mut self,
        id: RuleId,
        effective: NaiveDate,
        rule: Rule,
    ) -> Result<(), SplitError> {
        rule.validate()?;
        let versions = self.rules.entry(id).or_default();
        match versions.binary_search_by(|v| v.effective.cmp(&effective)) {
            Ok(_) => Err(SplitError::DuplicateEffectiveDate(id, effective)),
            Err(at) => {
                versions.insert(at, RuleVersion { effective, rule });
                Ok(())
            }
        }
    }

    /// The version of rule `id` in force on `date`.
    pub fn rule_at(&self, id: RuleId, date: NaiveDate) -> Option<Resolved<'_>> {
        let versions = self.rules.get(&id)?;
        let v = versions.iter().rev().find(|v| v.effective <= date)?;
        Some(Resolved {
            id,
            effective: v.effective,
            rule: &v.rule,
        })
    }

    /// All versions of rule `id`, oldest first.
    pub fn versions(&self, id: RuleId) -> &[RuleVersion] {
        self.rules.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_default(&mut self, category: impl Into<String>, id: RuleId) {
        self.defaults.insert(category.into(), id);
    }

    /// The fallback rule, if one is set.
    pub fn fallback(&self) -> Option<RuleId> {
        self.fallback
    }

    pub fn set_fallback(&mut self, id: RuleId) {
        self.fallback = Some(id);
    }

    /// The rule for an entry: the entry's own choice if it made one, else its category's
    /// default, else the fallback; in the version in force on the entry's date.
    pub fn resolve(
        &self,
        category: &str,
        date: NaiveDate,
        chosen: Option<RuleId>,
    ) -> Result<Resolved<'_>, SplitError> {
        let id = chosen
            .or_else(|| self.defaults.get(category).copied())
            .or(self.fallback)
            .ok_or_else(|| SplitError::NoRuleFor(category.to_string()))?;
        self.rule_at(id, date)
            .ok_or(SplitError::RuleNotInForce(id, date))
    }

    pub fn save_template(&mut self, rule: Rule) -> Result<(), SplitError> {
        rule.validate()?;
        self.templates.insert(rule.name.clone(), rule);
        Ok(())
    }

    pub fn template(&self, name: &str) -> Option<&Rule> {
        self.templates.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::{split, Method, SplitInput};
    use sk_core::MemberId;

    const A: MemberId = MemberId(1);
    const B: MemberId = MemberId(2);

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn pct(a: u32) -> Rule {
        Rule::new(
            "Groceries",
            Method::Percent([(A, a), (B, 10_000 - a)].into_iter().collect()),
            &[A, B],
        )
    }

    #[test]
    fn a_change_applies_from_its_date_forward_only() {
        let mut book = RuleBook::new();
        book.add_version(RuleId(1), d(2027, 1, 1), pct(5000))
            .unwrap();
        book.add_version(RuleId(1), d(2027, 3, 1), pct(7000))
            .unwrap();
        book.set_default("Groceries", RuleId(1));

        let feb = book.resolve("Groceries", d(2027, 2, 28), None).unwrap();
        assert_eq!(feb.effective, d(2027, 1, 1));
        let s = split(feb.rule, &SplitInput::amount(1000)).unwrap();
        assert_eq!(s[&A], 500);

        let mar = book.resolve("Groceries", d(2027, 3, 1), None).unwrap();
        assert_eq!(split(mar.rule, &SplitInput::amount(1000)).unwrap()[&A], 700);
    }

    #[test]
    fn before_the_first_version_no_rule_is_in_force() {
        let mut book = RuleBook::new();
        book.add_version(RuleId(1), d(2027, 1, 1), pct(5000))
            .unwrap();
        book.set_fallback(RuleId(1));
        assert_eq!(
            book.resolve("Anything", d(2026, 12, 31), None),
            Err(SplitError::RuleNotInForce(RuleId(1), d(2026, 12, 31)))
        );
    }

    #[test]
    fn category_default_then_fallback_then_error() {
        let mut book = RuleBook::new();
        assert!(matches!(
            book.resolve("Rent", d(2027, 1, 1), None),
            Err(SplitError::NoRuleFor(_))
        ));
        book.add_version(RuleId(1), d(2027, 1, 1), pct(5000))
            .unwrap();
        book.add_version(RuleId(2), d(2027, 1, 1), pct(6000))
            .unwrap();
        book.set_fallback(RuleId(1));
        book.set_default("Rent", RuleId(2));
        assert_eq!(
            book.resolve("Rent", d(2027, 2, 1), None).unwrap().id,
            RuleId(2)
        );
        assert_eq!(
            book.resolve("Food", d(2027, 2, 1), None).unwrap().id,
            RuleId(1)
        );
        assert_eq!(
            book.resolve("Food", d(2027, 2, 1), Some(RuleId(2)))
                .unwrap()
                .id,
            RuleId(2)
        );
    }

    #[test]
    fn duplicate_dates_and_invalid_rules_are_refused() {
        let mut book = RuleBook::new();
        book.add_version(RuleId(1), d(2027, 1, 1), pct(5000))
            .unwrap();
        assert!(book
            .add_version(RuleId(1), d(2027, 1, 1), pct(4000))
            .is_err());
        let bad = Rule::new("x", Method::Percent([(A, 1)].into_iter().collect()), &[A]);
        assert!(book.add_version(RuleId(2), d(2027, 1, 1), bad).is_err());
    }

    #[test]
    fn templates_by_name() {
        let mut book = RuleBook::new();
        book.save_template(pct(6000)).unwrap();
        assert_eq!(book.template("Groceries"), Some(&pct(6000)));
    }
}
