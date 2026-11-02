//! The price table (setting `stats.prices`, stats.md §4.1): dollars per million tokens. Costs are always
//! estimates computed at query time from tokens and the current table; nothing here is a bill.

use serde::{Deserialize, Serialize};
use steve_store::Tokens;

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

/// Finds `model`'s price: an exact row first, then the first matching prefix row, both in table order,
/// after dropping a `[…]` suffix. A `kiro/<model>` with no row of its own is priced as `<model>`.
/// `None` = unpriced (its tokens go to "未定价").
pub fn lookup<'a>(prices: &'a [Price], model: &str) -> Option<Priced<'a>> {
    let m = base_model(model);
    if m.is_empty() {
        return None;
    }
    let direct = prices
        .iter()
        .find(|p| p.exact(m))
        .or_else(|| prices.iter().find(|p| p.prefix(m)));
    if let Some(price) = direct {
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
