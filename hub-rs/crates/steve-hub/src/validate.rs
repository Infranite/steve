//! zod-compatible request validation.
//!
//! zod reports every failing field, in schema order, as a dotted path (`avatar.color`, `ids.0`); the API
//! answers `400 参数不对：<paths joined by 、>`. serde stops at the first error and words it differently, so
//! handlers walk the parsed JSON with [`Check`] in the same field order as the zod schema and build typed
//! values from it. Unknown keys are ignored (zod strips them).

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::ApiError;
use crate::js::utf16_len;

/// String constraints (`z.string().min().max().regex()`), lengths in UTF-16 units like JS.
#[derive(Debug, Clone, Copy, Default)]
pub struct Str {
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub pattern: Option<&'static str>,
}

impl Str {
    pub const ANY: Str = Str {
        min: None,
        max: None,
        pattern: None,
    };

    pub const fn len(min: usize, max: usize) -> Str {
        Str {
            min: Some(min),
            max: Some(max),
            pattern: None,
        }
    }

    pub const fn min(min: usize) -> Str {
        Str {
            min: Some(min),
            max: None,
            pattern: None,
        }
    }

    pub const fn max(max: usize) -> Str {
        Str {
            min: None,
            max: Some(max),
            pattern: None,
        }
    }

    pub const fn pattern(p: &'static str) -> Str {
        Str {
            min: None,
            max: None,
            pattern: Some(p),
        }
    }

    /// One entry per failing check (zod runs them all).
    fn failures(&self, s: &str) -> usize {
        let n = utf16_len(s);
        let mut bad = 0;
        if self.min.is_some_and(|m| n < m) {
            bad += 1;
        }
        if self.max.is_some_and(|m| n > m) {
            bad += 1;
        }
        if let Some(p) = self.pattern
            && !Regex::new(p).is_ok_and(|r| r.is_match(s))
        {
            bad += 1;
        }
        bad
    }
}

/// Number constraints (`z.number().int().min().max()`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Num {
    pub int: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// `.positive()` and friends: strictly greater.
    pub gt: Option<f64>,
}

impl Num {
    pub const ANY: Num = Num {
        int: false,
        min: None,
        max: None,
        gt: None,
    };

    fn failures(&self, f: f64) -> usize {
        let mut bad = 0;
        // zod's int format check aborts the rest (`-1.5` against `.int().positive()` is one issue)
        if self.int && (f.fract() != 0.0 || f.abs() > 9_007_199_254_740_991.0) {
            return 1;
        }
        if self.min.is_some_and(|m| f < m) {
            bad += 1;
        }
        if self.max.is_some_and(|m| f > m) {
            bad += 1;
        }
        if self.gt.is_some_and(|m| f <= m) {
            bad += 1;
        }
        bad
    }
}

/// How a key may be absent or null: `.optional()`, `.nullable()`, both, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    Required,
    Optional,
    Nullable,
    OptionalNullable,
}

/// A field that passed: absent, `null`, or a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Field<T> {
    Missing,
    Null,
    Value(T),
}

impl<T> Field<T> {
    /// `undefined` → `None`, `null` → `Some(None)` (the shape `Option<Option<T>>` handlers use).
    pub fn nested(self) -> Option<Option<T>> {
        match self {
            Field::Missing => None,
            Field::Null => Some(None),
            Field::Value(v) => Some(Some(v)),
        }
    }

    pub fn value(self) -> Option<T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }
}

/// Walks one JSON object, collecting zod-style issue paths.
pub struct Check<'a> {
    obj: Option<&'a Map<String, Value>>,
    prefix: String,
    issues: Vec<String>,
}

impl<'a> Check<'a> {
    /// `z.object(…)` at the top level: a non-object body is one issue with the empty path.
    pub fn object(v: &'a Value) -> Self {
        Self::at(v, String::new())
    }

    fn at(v: &'a Value, prefix: String) -> Self {
        match v.as_object() {
            Some(o) => Check {
                obj: Some(o),
                prefix,
                issues: vec![],
            },
            None => {
                let path = prefix.strip_suffix('.').unwrap_or(&prefix).to_string();
                Check {
                    obj: None,
                    prefix,
                    issues: vec![path],
                }
            }
        }
    }

    fn path(&self, k: &str) -> String {
        format!("{}{k}", self.prefix)
    }

    fn issue(&mut self, k: &str, times: usize) {
        for _ in 0..times {
            let p = self.path(k);
            self.issues.push(p);
        }
    }

    /// Looks up `k` and applies the presence rule; `Err(())` once an issue was recorded.
    fn lookup(&mut self, k: &str, presence: Presence) -> Result<Option<&'a Value>, ()> {
        let Some(obj) = self.obj else { return Err(()) };
        match (obj.get(k), presence) {
            (None, Presence::Optional | Presence::OptionalNullable) => Ok(None),
            (Some(Value::Null), Presence::Nullable | Presence::OptionalNullable) => {
                Ok(Some(&Value::Null))
            }
            (None | Some(Value::Null), _) => {
                self.issue(k, 1);
                Err(())
            }
            (Some(v), _) => Ok(Some(v)),
        }
    }

    fn string_field(&mut self, k: &str, rule: Str, presence: Presence) -> Field<String> {
        match self.lookup(k, presence) {
            Err(()) | Ok(None) => Field::Missing,
            Ok(Some(Value::Null)) => Field::Null,
            Ok(Some(Value::String(s))) => {
                let bad = rule.failures(s);
                if bad > 0 {
                    self.issue(k, bad);
                    Field::Missing
                } else {
                    Field::Value(s.clone())
                }
            }
            Ok(Some(_)) => {
                self.issue(k, 1);
                Field::Missing
            }
        }
    }

    /// `z.string()` (required).
    pub fn str(&mut self, k: &str, rule: Str) -> String {
        self.string_field(k, rule, Presence::Required)
            .value()
            .unwrap_or_default()
    }

    /// `z.string().optional()`.
    pub fn opt_str(&mut self, k: &str, rule: Str) -> Option<String> {
        self.string_field(k, rule, Presence::Optional).value()
    }

    /// `z.string().nullable()` (key required).
    pub fn nullable_str(&mut self, k: &str, rule: Str) -> Option<String> {
        self.string_field(k, rule, Presence::Nullable).value()
    }

    /// `z.string().nullable().optional()`: absent → `None`, null → `Some(None)`.
    pub fn opt_nullable_str(&mut self, k: &str, rule: Str) -> Option<Option<String>> {
        self.string_field(k, rule, Presence::OptionalNullable)
            .nested()
    }

    fn enum_field(&mut self, k: &str, allowed: &[&str], presence: Presence) -> Option<String> {
        match self.lookup(k, presence) {
            Ok(Some(Value::String(s))) if allowed.contains(&s.as_str()) => Some(s.clone()),
            Ok(None) | Err(()) => None,
            Ok(Some(_)) => {
                self.issue(k, 1);
                None
            }
        }
    }

    /// `z.enum([...])` (required); returns `""` after an issue.
    pub fn enum_(&mut self, k: &str, allowed: &[&str]) -> String {
        self.enum_field(k, allowed, Presence::Required)
            .unwrap_or_default()
    }

    /// `z.enum([...]).optional()` (or `.default()` via `unwrap_or`).
    pub fn opt_enum(&mut self, k: &str, allowed: &[&str]) -> Option<String> {
        self.enum_field(k, allowed, Presence::Optional)
    }

    fn bool_field(&mut self, k: &str, presence: Presence) -> Option<bool> {
        match self.lookup(k, presence) {
            Ok(Some(Value::Bool(b))) => Some(*b),
            Ok(None) | Err(()) => None,
            Ok(Some(_)) => {
                self.issue(k, 1);
                None
            }
        }
    }

    pub fn opt_bool(&mut self, k: &str) -> Option<bool> {
        self.bool_field(k, Presence::Optional)
    }

    fn num_field(&mut self, k: &str, rule: Num, presence: Presence) -> Field<f64> {
        match self.lookup(k, presence) {
            Err(()) | Ok(None) => Field::Missing,
            Ok(Some(Value::Null)) => Field::Null,
            Ok(Some(Value::Number(n))) => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                let bad = rule.failures(f);
                if bad > 0 {
                    self.issue(k, bad);
                    Field::Missing
                } else {
                    Field::Value(f)
                }
            }
            Ok(Some(_)) => {
                self.issue(k, 1);
                Field::Missing
            }
        }
    }

    /// `z.number()` (required); `0` after an issue.
    pub fn num(&mut self, k: &str, rule: Num) -> f64 {
        self.num_field(k, rule, Presence::Required)
            .value()
            .unwrap_or(0.0)
    }

    pub fn opt_num(&mut self, k: &str, rule: Num) -> Option<f64> {
        self.num_field(k, rule, Presence::Optional).value()
    }

    /// `z.number().nullable()` (key required).
    pub fn nullable_num(&mut self, k: &str, rule: Num) -> Option<f64> {
        self.num_field(k, rule, Presence::Nullable).value()
    }

    /// The raw JSON number for passing through unchanged (keeps `4` vs `4.5` as sent).
    pub fn raw(&self, k: &str) -> Option<&'a Value> {
        self.obj.and_then(|o| o.get(k))
    }

    fn str_array_field(
        &mut self,
        k: &str,
        min: Option<usize>,
        max: Option<usize>,
        presence: Presence,
    ) -> Option<Vec<String>> {
        self.str_array_rule(k, Str::ANY, min, max, presence)
    }

    fn str_array_rule(
        &mut self,
        k: &str,
        elem: Str,
        min: Option<usize>,
        max: Option<usize>,
        presence: Presence,
    ) -> Option<Vec<String>> {
        match self.lookup(k, presence) {
            Ok(Some(Value::Array(a))) => {
                let mut out = Vec::with_capacity(a.len());
                let mut ok = true;
                for (i, x) in a.iter().enumerate() {
                    match x {
                        Value::String(s) if elem.failures(s) == 0 => out.push(s.clone()),
                        Value::String(s) => {
                            ok = false;
                            let p = format!("{k}.{i}");
                            self.issue(&p, elem.failures(s));
                        }
                        _ => {
                            ok = false;
                            let p = format!("{k}.{i}");
                            self.issue(&p, 1);
                        }
                    }
                }
                let mut bad = 0;
                if min.is_some_and(|m| a.len() < m) {
                    bad += 1;
                }
                if max.is_some_and(|m| a.len() > m) {
                    bad += 1;
                }
                self.issue(k, bad);
                (ok && bad == 0).then_some(out)
            }
            Ok(None) | Err(()) => None,
            Ok(Some(_)) => {
                self.issue(k, 1);
                None
            }
        }
    }

    /// `z.array(z.string()).min().max()` (required); empty after an issue.
    pub fn str_array(&mut self, k: &str, min: Option<usize>, max: Option<usize>) -> Vec<String> {
        self.str_array_field(k, min, max, Presence::Required)
            .unwrap_or_default()
    }

    pub fn opt_str_array(
        &mut self,
        k: &str,
        min: Option<usize>,
        max: Option<usize>,
    ) -> Option<Vec<String>> {
        self.str_array_field(k, min, max, Presence::Optional)
    }

    /// `z.array(<string rule>).min().max().optional()`.
    pub fn opt_str_array_of(
        &mut self,
        k: &str,
        elem: Str,
        min: Option<usize>,
        max: Option<usize>,
    ) -> Option<Vec<String>> {
        self.str_array_rule(k, elem, min, max, Presence::Optional)
    }

    /// A nested `z.object` under `k`. Returns `None` when absent and optional (or after an issue);
    /// pass the child back to [`Check::absorb`] when done.
    pub fn sub(&mut self, k: &str, optional: bool) -> Option<Check<'a>> {
        let presence = if optional {
            Presence::Optional
        } else {
            Presence::Required
        };
        match self.lookup(k, presence) {
            Ok(Some(v)) if v.is_object() => Some(Check::at(v, format!("{}.", self.path(k)))),
            Ok(None) | Err(()) => None,
            Ok(Some(_)) => {
                self.issue(k, 1);
                None
            }
        }
    }

    pub fn absorb(&mut self, child: Check<'a>) {
        self.issues.extend(child.issues);
    }

    /// The whole object (for `z.record`), if it is one.
    pub fn map(&self) -> Option<&'a Map<String, Value>> {
        self.obj
    }

    /// `400 参数不对：…` if anything failed.
    pub fn finish(self) -> Result<(), ApiError> {
        if self.issues.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Invalid(self.issues))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issues(v: Value, f: impl FnOnce(&mut Check)) -> Vec<String> {
        let mut c = Check::object(&v);
        f(&mut c);
        match c.finish() {
            Ok(()) => vec![],
            Err(ApiError::Invalid(p)) => p,
            Err(e) => panic!("{e:?}"),
        }
    }

    #[test]
    fn top_level_type() {
        assert_eq!(
            issues(json!([1]), |c| {
                c.str("text", Str::min(1));
            }),
            vec![""]
        );
        assert_eq!(
            issues(json!("x"), |c| {
                c.str("text", Str::min(1));
            }),
            vec![""]
        );
    }

    #[test]
    fn strings() {
        let f = |c: &mut Check| {
            c.str("name", Str::len(1, 40));
            c.opt_str("description", Str::max(400));
            c.opt_str("item_prefix", Str::pattern(r"^[A-Z]{1,6}$"));
        };
        assert_eq!(issues(json!({}), f), vec!["name"]);
        assert_eq!(
            issues(json!({"name": "", "item_prefix": "abc"}), f),
            vec!["name", "item_prefix"]
        );
        assert_eq!(
            issues(json!({"name": "a".repeat(41), "description": null}), f),
            vec!["name", "description"]
        );
        assert!(issues(json!({"name": "工作", "extra": 1}), f).is_empty());
        // 40 CJK chars are 40 UTF-16 units: fine; 21 emoji are 42 units
        assert!(issues(json!({"name": "字".repeat(40)}), f).is_empty());
        assert_eq!(issues(json!({"name": "😀".repeat(21)}), f), vec!["name"]);
    }

    #[test]
    fn nullable_and_optional() {
        let mut v = json!({"label": null, "until": 5});
        let mut c = Check::object(&v);
        assert_eq!(c.nullable_str("label", Str::ANY), None);
        assert_eq!(c.nullable_num("until", Num::ANY), Some(5.0));
        assert!(c.finish().is_ok());
        v = json!({});
        assert_eq!(
            issues(v, |c| {
                c.nullable_str("label", Str::ANY);
                c.nullable_num("until", Num::ANY);
            }),
            vec!["label", "until"]
        );
        let v = json!({"project": null});
        let mut c = Check::object(&v);
        assert_eq!(c.opt_nullable_str("project", Str::ANY), Some(None));
        assert_eq!(c.opt_nullable_str("bot", Str::ANY), None);
    }

    #[test]
    fn numbers_int_check_aborts() {
        let f = |c: &mut Check| {
            c.opt_num(
                "proactive",
                Num {
                    int: true,
                    min: Some(0.0),
                    max: Some(2.0),
                    gt: None,
                },
            );
        };
        assert!(issues(json!({"proactive": 2}), f).is_empty());
        assert_eq!(issues(json!({"proactive": 1.5}), f), vec!["proactive"]);
        assert_eq!(issues(json!({"proactive": -1.5}), f), vec!["proactive"]);
        assert_eq!(issues(json!({"proactive": "1"}), f), vec!["proactive"]);
    }

    #[test]
    fn arrays_and_nested() {
        let f = |c: &mut Check| {
            c.str_array("ids", Some(1), Some(2));
            if let Some(mut a) = c.sub("avatar", true) {
                a.opt_str("color", Str::pattern(r"^#[0-9a-fA-F]{6}$"));
                c.absorb(a);
            }
        };
        assert_eq!(issues(json!({"ids": []}), f), vec!["ids"]);
        assert_eq!(issues(json!({"ids": ["a", 1]}), f), vec!["ids.1"]);
        assert_eq!(
            issues(json!({"ids": ["a"], "avatar": {"color": "red"}}), f),
            vec!["avatar.color"]
        );
        assert_eq!(
            issues(json!({"ids": ["a"], "avatar": 3}), f),
            vec!["avatar"]
        );
        let g = |c: &mut Check| {
            if let Some(mut t) = c.sub("target", false) {
                t.enum_("kind", &["bot"]);
                c.absorb(t);
            }
            c.str("text", Str::len(1, 4000));
        };
        assert_eq!(issues(json!({}), g), vec!["target", "text"]);
        assert_eq!(
            issues(json!({"target": {}, "text": "x"}), g),
            vec!["target.kind"]
        );
    }

    #[test]
    fn enums() {
        let f = |c: &mut Check| {
            c.enum_("action", &["pause", "resume"]);
        };
        assert!(issues(json!({"action": "pause"}), f).is_empty());
        assert_eq!(issues(json!({"action": "nap"}), f), vec!["action"]);
        assert_eq!(issues(json!({}), f), vec!["action"]);
    }
}
