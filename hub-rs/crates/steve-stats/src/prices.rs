//! The price table (setting `stats.prices`): dollars per million tokens. Costs are always
//! estimates computed at query time; nothing here is a bill.
//!
//! A model is priced by [`PriceBook::quote`]: the table first (its rows, or the built-in defaults, are
//! the manual prices and win), then the prices synced from Models.dev ([`crate::catalog`]) as they were
//! on the day being priced, so a later catalog change does not move the estimates of past days.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use chrono_tz::Tz;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use steve_store::Tokens;

use crate::catalog::{self, Version};

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// A model id, or a prefix ending in `*`.
    #[serde(rename = "match")]
    pub pattern: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default)]
    pub note: String,
}

impl Price {
    /// The estimated dollars for `t`; unreported figures count as 0.
    pub fn cost(&self, t: &Tokens) -> f64 {
        let n = |v: Option<i64>| v.unwrap_or(0) as f64;
        (n(t.input) * self.input
            + n(t.output) * self.output
            + n(t.cache_read) * self.cache_read
            + n(t.cache_write) * self.cache_write)
            / 1e6
    }

    fn exact(&self, model: &str) -> bool {
        !self.pattern.ends_with('*') && self.pattern == model
    }

    fn prefix(&self, model: &str) -> bool {
        self.pattern
            .strip_suffix('*')
            .is_some_and(|p| model.starts_with(p))
    }
}

/// Routes whose real billing is unknown and that are priced as the official model behind them
/// (coordinator decision 2026-11-01: `kiro/*` = the official price, shown as an estimate).
const PROXIED: &[&str] = &["kiro/"];

fn row(
    pattern: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    note: &str,
) -> Price {
    Price {
        pattern: pattern.into(),
        input,
        output,
        cache_read,
        cache_write,
        note: note.into(),
    }
}

/// Anthropic list prices (2026-10-06; cache writes at the 5-minute TTL = 1.25 × input). Haiku was
/// checked against a measured `total_cost_usd` on 2026-11-01. `kiro/…` models fall back to these
/// through [`lookup`] unless the table has a row of its own for them.
pub fn default_prices() -> Vec<Price> {
    vec![
        row("claude-opus-5-5", 4.0, 20.0, 0.2, 5.0, "官方标价"),
        row("claude-sonnet-5-5", 2.0, 10.0, 0.2, 2.5, "官方标价"),
        row(
            "claude-haiku-5-5",
            0.1,
            0.5,
            0.01,
            0.125,
            "官方标价，提示词 ≤100K；更长的提示词官方价是 0.50/2.50，这里不区分",
        ),
    ]
}

/// The notes the Hub writes itself (the default rows', a catalog quote's), each a whole note or one
/// part of a "；"-joined note.
const NOTE_KEYS: &[&str] = &[
    "stats.price_note.official",
    "stats.price_note.haiku",
    "stats.price_note.no_cache",
    "stats.price_note.no_cache_read",
    "stats.price_note.no_cache_write",
    "stats.price_note.tiered",
];

/// A price `note` in the request's language: the Hub's own notes translated, one you wrote as written.
pub fn note_shown(note: &str) -> String {
    if let Some(s) = steve_i18n::reword!(note, NOTE_KEYS) {
        return s;
    }
    let zh_sep = rust_i18n::t!("stats.price_note.sep", locale = steve_i18n::ZH_CN);
    let parts: Vec<&str> = note.split(&*zh_sep).collect();
    let shown: Vec<Option<String>> = parts
        .iter()
        .map(|p| steve_i18n::reword!(p, NOTE_KEYS))
        .collect();
    if parts.len() < 2 || shown.iter().any(Option::is_none) {
        return note.to_owned();
    }
    shown
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(&steve_i18n::t!("stats.price_note.sep"))
}

/// The rows with their notes in the request's language ([`note_shown`]).
pub fn rows_shown(rows: &[Price]) -> Vec<Price> {
    rows.iter()
        .map(|p| Price {
            note: note_shown(&p.note),
            ..p.clone()
        })
        .collect()
}

/// The row a model is priced with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Priced<'a> {
    pub price: &'a Price,
    /// Set when the model went through a route whose billing is unknown (`"kiro/"`) and is priced as
    /// the official model behind it; the page marks such costs as a rough estimate.
    pub via: Option<&'static str>,
}

/// A model id without a bracketed suffix such as `[1m]`.
pub fn base_model(model: &str) -> &str {
    let m = model.trim();
    match m.find('[') {
        Some(i) if m.ends_with(']') => m[..i].trim_end(),
        _ => m,
    }
}

/// The table row of `m` itself: an exact row first, then the first matching prefix row.
fn direct<'a>(prices: &'a [Price], m: &str) -> Option<&'a Price> {
    prices
        .iter()
        .find(|p| p.exact(m))
        .or_else(|| prices.iter().find(|p| p.prefix(m)))
}

/// Finds `model`'s price in the table alone: an exact row first, then the first matching prefix row,
/// both in table order, after dropping a `[…]` suffix. A `kiro/<model>` with no row of its own is
/// priced as `<model>`. `None` = unpriced (its tokens go to "未定价").
pub fn lookup<'a>(prices: &'a [Price], model: &str) -> Option<Priced<'a>> {
    let m = base_model(model);
    if m.is_empty() {
        return None;
    }
    if let Some(price) = direct(prices, m) {
        return Some(Priced { price, via: None });
    }
    PROXIED.iter().find_map(|route| {
        let inner = m.strip_prefix(route)?;
        let p = lookup(prices, inner)?;
        Some(Priced {
            price: p.price,
            via: Some(p.via.unwrap_or(route)),
        })
    })
}

/// Where a quoted price comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// The price table: the workspace's own rows, or the built-in defaults.
    Table,
    /// Synced from Models.dev.
    ModelsDev,
}

/// A model's price for one day.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// For a catalog price `match` is `provider/model`; prices it does not list are filled in as
    /// the input price, and `note` says so.
    pub price: Price,
    /// As [`Priced::via`].
    pub via: Option<&'static str>,
    pub source: PriceSource,
    /// For a catalog price: when the version in use was seen (0 = the first sync).
    pub since: i64,
}

/// A catalog model found for a model id, all its versions.
type Found = Option<Rc<(String, String, Vec<Version>)>>;

/// The prices in force for one read: the table and, when given a connection, the Models.dev catalog.
/// Catalog lookups are remembered for the life of the book (one request).
pub struct PriceBook<'c> {
    table: Vec<Price>,
    db: Option<&'c Connection>,
    tz: Option<Tz>,
    today: String,
    found: RefCell<HashMap<String, Found>>,
}

impl<'c> PriceBook<'c> {
    /// A book of `table` alone.
    pub fn new(table: Vec<Price>) -> Self {
        Self {
            table,
            db: None,
            tz: None,
            today: crate::day(steve_store::now(), None),
            found: RefCell::default(),
        }
    }

    /// Adds the catalog stored in `db`; days are cut in `tz`.
    pub fn with_catalog(mut self, db: &'c Connection, tz: Option<Tz>) -> Self {
        self.db = Some(db);
        self.tz = tz;
        self.today = crate::day(steve_store::now(), tz);
        self
    }

    /// Makes `model` resolve to `provider/id` with `versions` (tests, or a book without a database).
    pub fn preload(&self, model: &str, provider: &str, id: &str, versions: Vec<Version>) {
        self.found.borrow_mut().insert(
            model.to_lowercase(),
            Some(Rc::new((provider.into(), id.into(), versions))),
        );
    }

    pub fn table(&self) -> &[Price] {
        &self.table
    }

    /// Today in the book's zone, the day to quote "now" with.
    pub fn today(&self) -> &str {
        &self.today
    }

    fn found(&self, m: &str) -> Found {
        let key = m.to_lowercase();
        if let Some(f) = self.found.borrow().get(&key) {
            return f.clone();
        }
        let f = self.db.and_then(|db| {
            catalog::candidates(m).into_iter().find_map(|(p, id)| {
                match catalog::versions(db, &p, &id) {
                    Ok(vs) if !vs.is_empty() => Some(Rc::new((p, id, vs))),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!("price catalog read failed: {e}");
                        None
                    }
                }
            })
        });
        self.found.borrow_mut().insert(key, f.clone());
        f
    }

    fn catalog_quote(&self, m: &str, day: &str) -> Option<Quote> {
        let f = self.found(m)?;
        let (provider, id, vs) = &*f;
        let v = catalog::version_for(vs, day, self.tz)?;
        let r = v.rates;
        let mut note = vec![];
        match (r.cache_read, r.cache_write) {
            (None, None) => note.push("Models.dev 没列缓存价，缓存按输入价估"),
            (None, _) => note.push("Models.dev 没列缓存读取价，按输入价估"),
            (_, None) => note.push("Models.dev 没列缓存写入价，按输入价估"),
            _ => {}
        }
        if r.tiered {
            note.push("另有长上下文或分档价，这里按基础价");
        }
        Some(Quote {
            price: Price {
                pattern: format!("{provider}/{id}"),
                input: r.input,
                output: r.output,
                cache_read: r.cache_read.unwrap_or(r.input),
                cache_write: r.cache_write.unwrap_or(r.input),
                note: note.join("；"),
            },
            via: None,
            source: PriceSource::ModelsDev,
            since: v.since,
        })
    }

    /// `model`'s price on `day` (`YYYY-MM-DD`): a table row of its own, else the catalog's, else, for a
    /// `kiro/<model>`, the price of `<model>`. `None` = unpriced.
    pub fn quote(&self, model: &str, day: &str) -> Option<Quote> {
        // an hourly cell (`YYYY-MM-DDTHH:00`, ST-43) is priced as its day, so the hours add up to the day
        let day = day.get(..10).unwrap_or(day);
        let m = base_model(model);
        if m.is_empty() {
            return None;
        }
        if let Some(p) = direct(&self.table, m) {
            return Some(Quote {
                price: p.clone(),
                via: None,
                source: PriceSource::Table,
                since: 0,
            });
        }
        if let Some(q) = self.catalog_quote(m, day) {
            return Some(q);
        }
        PROXIED.iter().find_map(|route| {
            let inner = m.strip_prefix(route)?;
            let q = self.quote(inner, day)?;
            Some(Quote {
                via: Some(q.via.unwrap_or(route)),
                ..q
            })
        })
    }

    /// The estimated dollars of `t` of `model` on `day`; `None` when it has no price.
    pub fn cost(&self, model: &str, day: &str, t: &Tokens) -> Option<f64> {
        self.quote(model, day).map(|q| q.price.cost(t))
    }
}

/// Checks a table before it is saved: non-negative finite prices, a non-empty `match`, no duplicates.
/// `Err` names the offending field as `prices[i].<field>` for the `400 参数不对：…` message.
pub fn validate(prices: &[Price]) -> Result<(), String> {
    for (i, p) in prices.iter().enumerate() {
        let pat = p.pattern.trim();
        if pat.is_empty() || prices[..i].iter().any(|q| q.pattern.trim() == pat) {
            return Err(format!("prices[{i}].match"));
        }
        for (name, v) in [
            ("input", p.input),
            ("output", p.output),
            ("cache_read", p.cache_read),
            ("cache_write", p.cache_write),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(format!("prices[{i}].{name}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Hub's notes read in the reader's language; zh-CN and your own notes as written.
    #[test]
    fn notes_by_language() {
        let en = |n: &str| steve_i18n::with(steve_i18n::EN, || note_shown(n));
        let rows = steve_i18n::with(steve_i18n::EN, || rows_shown(&default_prices()));
        assert_eq!(rows[0].note, "List price");
        assert!(
            rows[2].note.starts_with("List price for prompts ≤100K; "),
            "{}",
            rows[2].note
        );
        assert_eq!(
            en("Models.dev 没列缓存价，缓存按输入价估；另有长上下文或分档价，这里按基础价"),
            "Models.dev lists no cache prices; cache is priced as input; Long-context or tiered prices also exist; the base price is used here"
        );
        assert_eq!(en("我自己的价；官方标价"), "我自己的价；官方标价");
        assert_eq!(en(""), "");
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || rows_shown(&default_prices()));
        assert_eq!(zh, default_prices());
    }

    fn pat(p: Option<Priced<'_>>) -> Option<(&str, Option<&str>)> {
        p.map(|p| (p.price.pattern.as_str(), p.via))
    }

    #[test]
    fn defaults_match_the_official_models() {
        let d = default_prices();
        for m in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] {
            assert_eq!(pat(lookup(&d, m)), Some((m, None)));
        }
        assert_eq!(pat(lookup(&d, "gpt-5-codex")), None);
        assert_eq!(pat(lookup(&d, "")), None);
        assert_eq!(pat(lookup(&d, "claude-opus-5")), None, "no fuzzy match");
    }

    #[test]
    fn exact_beats_prefix_whatever_the_order() {
        let t = vec![
            row("claude-*", 1.0, 1.0, 0.0, 0.0, ""),
            row("claude-opus-5-5", 4.0, 20.0, 0.2, 5.0, ""),
            row("claude-op*", 2.0, 2.0, 0.0, 0.0, ""),
        ];
        assert_eq!(
            pat(lookup(&t, "claude-opus-5-5")),
            Some(("claude-opus-5-5", None))
        );
        // among prefixes, table order decides
        assert_eq!(pat(lookup(&t, "claude-opus-6")), Some(("claude-*", None)));
        assert_eq!(pat(lookup(&t, "gpt-5")), None);
    }

    #[test]
    fn bracket_suffix_is_dropped() {
        let d = default_prices();
        assert_eq!(base_model("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(base_model("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(base_model("weird[x"), "weird[x");
        assert_eq!(
            pat(lookup(&d, "claude-opus-5-5[1m]")),
            Some(("claude-opus-5-5", None))
        );
    }

    #[test]
    fn kiro_is_priced_as_the_official_model_and_marked() {
        let d = default_prices();
        assert_eq!(
            pat(lookup(&d, "kiro/claude-opus-5-5")),
            Some(("claude-opus-5-5", Some("kiro/")))
        );
        assert_eq!(
            pat(lookup(&d, "kiro/claude-sonnet-5-5[1m]")),
            Some(("claude-sonnet-5-5", Some("kiro/")))
        );
        assert_eq!(pat(lookup(&d, "kiro/gpt-5")), None);
        // a row of its own wins over the mapping
        let mut t = vec![row("kiro/*", 1.2, 6.0, 0.06, 1.5, "按 3 折")];
        t.extend(d);
        assert_eq!(
            pat(lookup(&t, "kiro/claude-opus-5-5")),
            Some(("kiro/*", None))
        );
    }

    #[test]
    fn cost_matches_the_measured_haiku_call() {
        // claude -p via the gateway, 2026-11-01: total_cost_usd 0.000829045
        let d = default_prices();
        let p = lookup(&d, "claude-haiku-5-5").unwrap().price;
        let t = Tokens {
            input: Some(2),
            output: Some(4),
            cache_read: Some(11_097),
            cache_write: Some(5_727),
            reasoning: None,
        };
        assert!((p.cost(&t) - 0.000829045).abs() < 1e-12, "{}", p.cost(&t));
        assert_eq!(p.cost(&Tokens::default()), 0.0);
    }

    fn ver(since: i64, input: f64, cache_read: Option<f64>) -> Version {
        Version {
            since,
            name: String::new(),
            rates: catalog::Rates {
                input,
                output: input * 8.0,
                cache_read,
                cache_write: None,
                tiered: false,
            },
        }
    }

    #[test]
    fn the_table_wins_over_the_catalog() {
        let b = PriceBook::new(default_prices());
        // Models.dev lists Sonnet 5.5's cache read at 0.1; the table's official 0.2 stays
        b.preload(
            "claude-sonnet-5-5",
            "anthropic",
            "claude-sonnet-5-5",
            vec![ver(0, 2.0, Some(0.1))],
        );
        let q = b.quote("claude-sonnet-5-5", "2026-11-05").unwrap();
        assert_eq!((q.source, q.price.cache_read), (PriceSource::Table, 0.2));
        // a model the table lacks comes from the catalog, as `provider/model`
        b.preload("gpt-5", "openai", "gpt-5", vec![ver(0, 1.25, Some(0.125))]);
        let q = b.quote("gpt-5", "2026-11-05").unwrap();
        assert_eq!(q.source, PriceSource::ModelsDev);
        assert_eq!(q.price.pattern, "openai/gpt-5");
        assert_eq!(q.via, None);
        // a row of the user's own overrides the catalog
        let mut t = default_prices();
        t.push(row("gpt-*", 0.5, 4.0, 0.05, 0.0, "公司折扣"));
        let b = PriceBook::new(t);
        b.preload("gpt-5", "openai", "gpt-5", vec![ver(0, 1.25, Some(0.125))]);
        let q = b.quote("gpt-5", "2026-11-05").unwrap();
        assert_eq!(
            (q.source, q.price.input, q.price.pattern.as_str()),
            (PriceSource::Table, 0.5, "gpt-*")
        );
        assert_eq!(b.quote("unknown-model", "2026-11-05"), None);
    }

    #[test]
    fn catalog_units_and_missing_cache_prices() {
        let b = PriceBook::new(vec![]);
        b.preload("gpt-5", "openai", "gpt-5", vec![ver(0, 1.25, None)]);
        let q = b.quote("gpt-5", "2026-11-05").unwrap();
        // not listed: cache tokens at the input price, never free
        assert_eq!((q.price.cache_read, q.price.cache_write), (1.25, 1.25));
        assert!(q.price.note.contains("没列缓存价"), "{}", q.price.note);
        // USD per million: 1M input + 1M output = 1.25 + 10
        let t = Tokens {
            input: Some(1_000_000),
            output: Some(1_000_000),
            ..Tokens::default()
        };
        assert!((b.cost("gpt-5", "2026-11-05", &t).unwrap() - 11.25).abs() < 1e-9);
        // `[1m]` and case are dropped before matching
        assert!(b.quote("GPT-5[1m]", "2026-11-05").is_some());
    }

    #[test]
    fn catalog_changes_do_not_move_past_days() {
        let utc = crate::tz(Some("UTC"));
        let seen = 1_793_872_800_000; // 2026-11-05 10:00 UTC
        let b = PriceBook::new(vec![]);
        b.preload(
            "gpt-5",
            "openai",
            "gpt-5",
            vec![ver(0, 1.0, Some(0.1)), ver(seen, 2.0, Some(0.2))],
        );
        let b = PriceBook { tz: utc, ..b };
        let t = Tokens {
            input: Some(1_000_000),
            ..Tokens::default()
        };
        assert_eq!(b.cost("gpt-5", "2026-11-01", &t), Some(1.0));
        assert_eq!(b.cost("gpt-5", "2026-11-05", &t), Some(1.0));
        // an hour of that day is priced as the day (ST-43), so the hours add up to it
        assert_eq!(b.cost("gpt-5", "2026-11-05T00:00", &t), Some(1.0));
        assert_eq!(b.cost("gpt-5", "2026-11-05T23:00", &t), Some(1.0));
        assert_eq!(b.cost("gpt-5", "2026-11-06", &t), Some(2.0));
        assert_eq!(b.quote("gpt-5", "2026-11-06").unwrap().since, seen);
    }

    #[test]
    fn kiro_is_priced_through_the_catalog_too() {
        let b = PriceBook::new(vec![]);
        b.preload("gpt-5", "openai", "gpt-5", vec![ver(0, 1.25, Some(0.125))]);
        let q = b.quote("kiro/gpt-5", "2026-11-05").unwrap();
        assert_eq!(
            (q.source, q.via, q.price.pattern.as_str()),
            (PriceSource::ModelsDev, Some("kiro/"), "openai/gpt-5")
        );
    }

    #[test]
    fn validate_names_the_field() {
        let ok = default_prices();
        assert_eq!(validate(&ok), Ok(()));
        let mut bad = ok.clone();
        bad[1].cache_read = -1.0;
        assert_eq!(validate(&bad), Err("prices[1].cache_read".into()));
        let mut bad = ok.clone();
        bad[2].pattern = " ".into();
        assert_eq!(validate(&bad), Err("prices[2].match".into()));
        let mut bad = ok.clone();
        bad[2].pattern = "claude-opus-5-5".into();
        assert_eq!(validate(&bad), Err("prices[2].match".into()));
        let mut bad = ok;
        bad[0].output = f64::NAN;
        assert_eq!(validate(&bad), Err("prices[0].output".into()));
    }

    #[test]
    fn serde_uses_match() {
        let v = serde_json::to_value(&default_prices()[0]).unwrap();
        assert_eq!(v["match"], "claude-opus-5-5");
        let p: Price = serde_json::from_value(serde_json::json!({"match": "x*", "input": 1, "output": 2, "cache_read": 0, "cache_write": 0})).unwrap();
        assert_eq!(p.note, "");
        assert_eq!(p.pattern, "x*");
    }
}
