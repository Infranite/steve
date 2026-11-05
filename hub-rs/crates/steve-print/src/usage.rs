//! The tokens and dollars a print run reports, as node-api [`UsageReport`]s. One convention for both
//! harnesses, Anthropic's: `input` excludes the cache, `output` includes thinking, and `reasoning` is
//! the part of `output` reported separately. Missing or `null` figures stay `None`, never 0.

use serde_json::Value;
use steve_node_api::{Epoch, Measurement, Money, Tokens, UsageReport, UsageScope};

/// Field names per figure across the shapes a print run gives: `claude -p` `usage` (snake_case), its
/// `modelUsage` entries (camelCase) and Codex's `turn.completed` `usage` (the same tables as the
/// statistics' normalizer, so both book the same numbers).
const INPUT: &[&str] = &["inputTokens", "input_tokens"];
const OUTPUT: &[&str] = &["outputTokens", "output_tokens"];
const CACHE_READ: &[&str] = &[
    "cachedReadTokens",
    "cachedInputTokens",
    "cacheReadInputTokens",
    "cache_read_input_tokens",
    "cached_input_tokens",
];
const CACHE_WRITE: &[&str] = &[
    "cachedWriteTokens",
    "cacheCreationInputTokens",
    "cache_creation_input_tokens",
    "cache_write_input_tokens",
];
const REASONING: &[&str] = &[
    "thoughtTokens",
    "reasoningOutputTokens",
    "reasoning_output_tokens",
];

fn field(v: &Value, names: &[&str]) -> Option<u64> {
    let n = names.iter().find_map(|n| {
        let x = v.get(*n)?;
        x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))
    })?;
    u64::try_from(n).ok()
}

/// The tokens of one usage object. `codex`: OpenAI counts cache reads inside `input`, so when
/// `cache_read` is reported and not larger than `input` it is taken out.
pub fn tokens(v: &Value, codex: bool) -> Tokens {
    let cache_read = field(v, CACHE_READ);
    let mut input = field(v, INPUT);
    if codex
        && let (Some(i), Some(r)) = (input, cache_read)
        && i >= r
    {
        input = Some(i - r);
    }
    Tokens {
        input,
        output: field(v, OUTPUT),
        cache_read,
        cache_write: field(v, CACHE_WRITE),
        reasoning: field(v, REASONING),
    }
}

/// Dollars as a decimal: the shortest text that reads back as the same `f64`.
pub fn money(usd: f64) -> Money {
    Money {
        decimal: usd.to_string(),
        currency: "USD".into(),
    }
}

/// [`money`] read back.
pub fn cost_f64(m: &Money) -> Option<f64> {
    m.decimal.parse().ok()
}

/// The reports of one run: each gets the run's own meter epoch (a print run is one process).
struct Meter {
    run: String,
    n: usize,
}

impl Meter {
    fn new() -> Meter {
        Meter {
            run: uuid::Uuid::new_v4().simple().to_string(),
            n: 0,
        }
    }

    fn report(
        &mut self,
        model: &str,
        scope: UsageScope,
        tokens: Tokens,
        cost: Option<f64>,
    ) -> UsageReport {
        self.n += 1;
        UsageReport {
            report_id: format!("{}.{}", self.run, self.n),
            model: (!model.is_empty()).then(|| model.to_string()),
            meter_epoch: Epoch::new(self.run.clone()).expect("a uuid is a valid id"),
            scope,
            measurement: Measurement::Final,
            tokens,
            context_used: None,
            context_limit: None,
            cost: cost.map(money),
        }
    }
}

/// The usage a `claude -p` `result` object reports: one report (scope All) per key of `modelUsage`
/// (the models that actually answered, sub-calls included), else one (scope Main) from `usage` booked
/// to `model`, the model asked for. Dollars: each model's `costUSD`, else `total_cost_usd` on the
/// first report.
pub fn claude_usage(d: &Value, model: &str) -> Vec<UsageReport> {
    let total = d.get("total_cost_usd").and_then(Value::as_f64);
    let mut m = Meter::new();
    let mut rows: Vec<UsageReport> = match (d.get("modelUsage"), d.get("usage")) {
        (Some(Value::Object(mu)), _) if !mu.is_empty() => mu
            .iter()
            .map(|(k, u)| {
                m.report(
                    k,
                    UsageScope::All,
                    tokens(u, false),
                    u.get("costUSD").and_then(Value::as_f64),
                )
            })
            .collect(),
        (_, Some(u @ Value::Object(_))) => {
            vec![m.report(model, UsageScope::Main, tokens(u, false), None)]
        }
        _ if total.is_some() => vec![m.report(model, UsageScope::Main, Tokens::default(), None)],
        _ => vec![],
    };
    if rows.iter().all(|r| r.cost.is_none())
        && let Some(r) = rows.first_mut()
    {
        r.cost = total.map(money);
    }
    rows
}

/// Codex's `turn.completed` `usage`, booked to `model` (Codex reports no per-model split).
pub fn codex_usage(u: &Value, model: &str) -> UsageReport {
    Meter::new().report(model, UsageScope::Main, tokens(u, true), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dollars_read_back_exactly() {
        for f in [0.000829045, 0.0, 0.5, 1e-12, 123456.789, 0.1 + 0.2, 7.0] {
            let m = money(f);
            assert_eq!(cost_f64(&m), Some(f), "{}", m.decimal);
            assert!(!m.decimal.contains('e'), "a plain decimal: {}", m.decimal);
        }
    }

    #[test]
    fn codex_input_leaves_out_the_cache() {
        let t = tokens(
            &json!({"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 5, "reasoning_output_tokens": 2}),
            true,
        );
        assert_eq!(
            (t.input, t.cache_read, t.output, t.reasoning),
            (Some(60), Some(40), Some(5), Some(2))
        );
        // input below the cache cannot be OpenAI's convention: as reported
        let t = tokens(&json!({"inputTokens": 10, "cachedReadTokens": 800}), true);
        assert_eq!((t.input, t.cache_read), (Some(10), Some(800)));
        // Claude's convention is already Anthropic's
        let t = tokens(
            &json!({"input_tokens": 100, "cache_read_input_tokens": 40}),
            false,
        );
        assert_eq!(t.input, Some(100));
        assert_eq!(tokens(&json!(null), true), Tokens::default());
    }

    #[test]
    fn claude_reports_per_model_or_for_the_run() {
        let two = json!({"result":"x","total_cost_usd":0.5,"modelUsage":{
            "claude-opus-5-5":{"inputTokens":10,"outputTokens":20,"costUSD":0.4},
            "claude-haiku-5-5":{"inputTokens":1,"outputTokens":null,"costUSD":0.1}}});
        let r = claude_usage(&two, "m");
        assert_eq!(
            r.iter().map(|r| r.model.as_deref()).collect::<Vec<_>>(),
            [Some("claude-opus-5-5"), Some("claude-haiku-5-5")]
        );
        assert!(
            r.iter()
                .all(|r| r.scope == UsageScope::All && r.measurement == Measurement::Final)
        );
        assert_eq!(r[1].tokens.output, None);
        assert_eq!(r[0].cost.as_ref().unwrap().decimal, "0.4");
        assert_ne!(r[0].report_id, r[1].report_id);
        assert_eq!(r[0].meter_epoch, r[1].meter_epoch);
        // only a total: one empty report carrying it
        let r = claude_usage(&json!({"total_cost_usd": 0.25}), "asked");
        assert_eq!(
            (
                r.len(),
                r[0].model.as_deref(),
                r[0].scope,
                r[0].cost.as_ref().map(|m| m.decimal.as_str())
            ),
            (1, Some("asked"), UsageScope::Main, Some("0.25"))
        );
        assert!(claude_usage(&json!({"result": "x"}), "m").is_empty());
    }
}
