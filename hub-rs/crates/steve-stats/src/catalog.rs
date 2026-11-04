//! Prices synced from Models.dev (<https://models.dev/api.json>, MIT, the catalog opencode uses): every
//! provider's models with their `cost` in USD per million tokens, the unit of the price table. The Hub
//! fetches it (steve-hub `price_sync`); this module reads the JSON, keeps the changes in `price_catalog`
//! (migration 32) and finds a model's price there.
//!
//! A model is matched by provider and id, never by name alone: `provider/model` names its provider,
//! otherwise the provider is the lab its id belongs to (`claude-…` → `anthropic`, `gpt-…` → `openai`).
//! A trailing release date (`-20250929`) is an alias of the id without it.

use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use steve_store::Result;

/// The `price_sync.source` of Models.dev.
pub const SOURCE: &str = "models.dev";
/// Where the catalog is fetched from.
pub const URL: &str = "https://models.dev/api.json";

/// One priced model of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub provider: String,
    /// Lower-cased.
    pub model: String,
    pub name: String,
    pub rates: Rates,
}

/// Prices of one catalog version, USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    /// `None` = not listed: such tokens are priced as input (no discount assumed).
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    /// The catalog also lists long-context or tier prices; only the base price is applied.
    pub tiered: bool,
}

/// A price as it was from `since` (ms; 0 = the first sync that listed it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Version {
    pub since: i64,
    pub name: String,
    pub rates: Rates,
}

fn price(v: Option<&Value>) -> Option<f64> {
    v?.as_f64().filter(|x| x.is_finite() && *x >= 0.0)
}

/// Reads `api.json`: `{provider: {id, models: {id: {cost: {input, output, cache_read?, cache_write?}}}}}`.
/// Models without an input and output price are left out. `Err` when it is not that shape or lists no
/// price at all, so a broken download never replaces the cached prices.
pub fn parse(body: &[u8]) -> std::result::Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("不是 JSON：{e}"))?;
    let providers = v.as_object().ok_or("顶层不是对象")?;
    let mut out = vec![];
    for (pid, p) in providers {
        let Some(models) = p.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (mid, m) in models {
            let Some(cost) = m.get("cost").and_then(Value::as_object) else {
                continue;
            };
            let (Some(input), Some(output)) = (price(cost.get("input")), price(cost.get("output")))
            else {
                continue;
            };
            out.push(Entry {
                provider: pid.to_lowercase(),
                model: mid.trim().to_lowercase(),
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rates: Rates {
                    input,
                    output,
                    cache_read: price(cost.get("cache_read")),
                    cache_write: price(cost.get("cache_write")),
                    tiered: cost.contains_key("tiers") || cost.contains_key("context_over_200k"),
                },
            });
        }
    }
    if out.is_empty() {
        return Err("没有读到任何模型价格".into());
    }
    Ok(out)
}

fn latest(c: &Connection, provider: &str, model: &str) -> Result<Option<Version>> {
    Ok(c.prepare_cached(
        "SELECT since, name, input, output, cache_read, cache_write, tiered FROM price_catalog
         WHERE provider = ? AND model = ? ORDER BY since DESC LIMIT 1",
    )?
    .query_row([provider, model], row)
    .optional()?)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Version> {
    Ok(Version {
        since: r.get(0)?,
        name: r.get(1)?,
        rates: Rates {
            input: r.get(2)?,
            output: r.get(3)?,
            cache_read: r.get(4)?,
            cache_write: r.get(5)?,
            tiered: r.get(6)?,
        },
    })
}

/// Stores `entries` as seen at `now`, in the caller's transaction: a model seen for the first time gets
/// `since` 0, a changed price a new row from `now`; unchanged ones and models no longer listed are left
/// as they are. Returns how many rows were written.
pub fn save(c: &Connection, entries: &[Entry], now: i64) -> Result<usize> {
    let mut written = 0;
    for e in entries {
        let since = match latest(c, &e.provider, &e.model)? {
            Some(v) if v.rates == e.rates => continue,
            Some(_) => now,
            None => 0,
        };
        let r = &e.rates;
        written += c
            .prepare_cached(
                "INSERT OR REPLACE INTO price_catalog (provider, model, since, input, output, cache_read, cache_write, tiered, name)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?
            .execute(params![
                e.provider,
                e.model,
                since,
                r.input,
                r.output,
                r.cache_read,
                r.cache_write,
                r.tiered,
                e.name
            ])?;
    }
    Ok(written)
}

/// Every version of one catalog model, oldest first.
pub fn versions(c: &Connection, provider: &str, model: &str) -> Result<Vec<Version>> {
    Ok(c.prepare_cached(
        "SELECT since, name, input, output, cache_read, cache_write, tiered FROM price_catalog
         WHERE provider = ? AND model = ? ORDER BY since",
    )?
    .query_map([provider, model], row)?
    .collect::<rusqlite::Result<_>>()?)
}

/// The version that prices `day` (`YYYY-MM-DD` in `tz`): the newest one seen before that day. A change
/// seen on a day applies from the next one, so a day's estimate does not move once it is over.
pub fn version_for<'a>(vs: &'a [Version], day: &str, tz: Option<Tz>) -> Option<&'a Version> {
    vs.iter()
        .rfind(|v| v.since == 0 || crate::day(v.since, tz).as_str() < day)
        .or(vs.first())
}

/// The state of a source's sync.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SyncState {
    /// The last sync that worked (ms).
    pub synced: Option<i64>,
    /// The last try (ms).
    pub tried: Option<i64>,
    /// Why the last try failed; `None` when it worked.
    pub error: Option<String>,
    #[serde(skip)]
    pub etag: Option<String>,
    /// Priced models in the last sync.
    pub models: i64,
}

pub fn state(c: &Connection, source: &str) -> Result<SyncState> {
    Ok(c.prepare_cached(
        "SELECT synced, tried, error, etag, models FROM price_sync WHERE source = ?",
    )?
    .query_row([source], |r| {
        Ok(SyncState {
            synced: r.get(0)?,
            tried: r.get(1)?,
            error: r.get(2)?,
            etag: r.get(3)?,
            models: r.get(4)?,
        })
    })
    .optional()?
    .unwrap_or_default())
}

/// Records a sync that worked: `models` priced models (`None` = unchanged since the last one, a `304`).
pub fn record_ok(
    c: &Connection,
    source: &str,
    now: i64,
    etag: Option<&str>,
    models: Option<usize>,
) -> Result<()> {
    c.execute(
        "INSERT INTO price_sync (source, synced, tried, error, etag, models) VALUES (?1, ?2, ?2, NULL, ?3, COALESCE(?4, 0))
         ON CONFLICT (source) DO UPDATE SET synced = ?2, tried = ?2, error = NULL,
           etag = COALESCE(?3, etag), models = COALESCE(?4, models)",
        params![source, now, etag, models.map(|n| n as i64)],
    )?;
    Ok(())
}

/// Records a failed try; the prices of the last sync stay in use.
pub fn record_err(c: &Connection, source: &str, now: i64, error: &str) -> Result<()> {
    c.execute(
        "INSERT INTO price_sync (source, tried, error) VALUES (?1, ?2, ?3)
         ON CONFLICT (source) DO UPDATE SET tried = ?2, error = ?3",
        params![source, now, error],
    )?;
    Ok(())
}

/// The lab a model id belongs to, by its family prefix; the first is tried first.
const LABS: &[(&str, &[&str])] = &[
    ("claude-", &["anthropic"]),
    ("gpt-", &["openai"]),
    ("chatgpt-", &["openai"]),
    ("codex-", &["openai"]),
    ("o1", &["openai"]),
    ("o3", &["openai"]),
    ("o4", &["openai"]),
    ("gemini-", &["google"]),
    ("gemma-", &["google"]),
    ("deepseek-", &["deepseek"]),
    ("kimi-", &["moonshotai"]),
    ("moonshot-", &["moonshotai"]),
    ("glm-", &["zai", "zhipuai"]),
    ("qwen", &["alibaba"]),
    ("qwq-", &["alibaba"]),
    ("grok-", &["xai"]),
    ("mistral-", &["mistral"]),
    ("codestral", &["mistral"]),
    ("devstral", &["mistral"]),
    ("magistral", &["mistral"]),
    ("minimax-", &["minimax"]),
];

/// `-YYYYMMDD` or `-YYYY-MM-DD` at the end of an id.
fn without_date(m: &str) -> Option<&str> {
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if let Some((head, d)) = m.rsplit_once('-')
        && d.len() == 8
        && digits(d)
        && d.starts_with("20")
    {
        return Some(head);
    }
    let b = m.as_bytes();
    if b.len() > 11 && b[b.len() - 11] == b'-' {
        let d = &m[m.len() - 10..];
        let ok = d.len() == 10
            && d.starts_with("20")
            && d.as_bytes()[4] == b'-'
            && d.as_bytes()[7] == b'-'
            && digits(&d[..4])
            && digits(&d[5..7])
            && digits(&d[8..]);
        if ok {
            return Some(&m[..m.len() - 11]);
        }
    }
    None
}

/// The `(provider, model)` keys `model` (already without a `[…]` suffix) may be listed under, in order.
pub fn candidates(model: &str) -> Vec<(String, String)> {
    let m = model.trim().to_lowercase();
    let mut out = vec![];
    if let Some((p, rest)) = m.split_once('/') {
        if !p.is_empty() && !rest.is_empty() {
            out.push((p.to_string(), rest.to_string()));
        }
        return out;
    }
    let family = |prefix: &str| {
        m.strip_prefix(prefix).is_some_and(|r| {
            prefix.ends_with('-')
                || r.is_empty()
                || r.starts_with('-')
                || r.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        })
    };
    let Some((_, labs)) = LABS.iter().find(|(prefix, _)| family(prefix)) else {
        return out;
    };
    for lab in *labs {
        out.push((lab.to_string(), m.clone()));
        if let Some(base) = without_date(&m) {
            out.push((lab.to_string(), base.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "anthropic": {"id": "anthropic", "models": {
        "claude-opus-5-5": {"name": "Claude Opus 5.5", "cost": {"input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}},
        "claude-haiku-5-5": {"name": "Claude Haiku 5.5", "cost": {"input": 0.1, "output": 0.5, "cache_read": 0.01, "cache_write": 0.125,
          "tiers": [{"input": 0.5, "output": 2.5, "tier": {"type": "context", "size": 100000}}]}}
      }},
      "openai": {"id": "openai", "models": {
        "gpt-5": {"name": "GPT-5", "cost": {"input": 1.25, "output": 10, "cache_read": 0.125}},
        "gpt-5-codex": {"name": "GPT-5 Codex"},
        "MiniThing": {"cost": {"input": "x", "output": 1}}
      }},
      "lmstudio": {"id": "lmstudio", "models": {}}
    }"#;

    #[test]
    fn parse_reads_cost_per_million() {
        let e = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(e.len(), 3, "no cost, or a bad price, is left out");
        let gpt = e.iter().find(|e| e.model == "gpt-5").unwrap();
        assert_eq!(gpt.provider, "openai");
        assert_eq!(
            gpt.rates,
            Rates {
                input: 1.25,
                output: 10.0,
                cache_read: Some(0.125),
                cache_write: None,
                tiered: false
            }
        );
        assert!(
            e.iter()
                .find(|e| e.model == "claude-haiku-5-5")
                .unwrap()
                .rates
                .tiered
        );
    }

    #[test]
    fn parse_refuses_what_is_not_a_catalog() {
        assert!(parse(b"<html>").is_err());
        assert!(parse(b"[]").is_err());
        assert!(parse(b"{}").is_err());
        assert!(parse(br#"{"x": {"models": {"m": {}}}}"#).is_err());
    }

    #[test]
    fn candidates_name_the_provider() {
        let c = |m: &str| candidates(m);
        let k = |p: &str, m: &str| (p.to_string(), m.to_string());
        assert_eq!(
            c("claude-opus-5-5"),
            vec![k("anthropic", "claude-opus-5-5")]
        );
        assert_eq!(
            c("claude-sonnet-4-5-20250929"),
            vec![
                k("anthropic", "claude-sonnet-4-5-20250929"),
                k("anthropic", "claude-sonnet-4-5")
            ]
        );
        assert_eq!(
            c("gpt-4o-2024-08-06"),
            vec![k("openai", "gpt-4o-2024-08-06"), k("openai", "gpt-4o")]
        );
        assert_eq!(c("GPT-5.5"), vec![k("openai", "gpt-5.5")]);
        assert_eq!(c("o3"), vec![k("openai", "o3")]);
        assert_eq!(c("o4-mini"), vec![k("openai", "o4-mini")]);
        assert_eq!(
            c("glm-5.1"),
            vec![k("zai", "glm-5.1"), k("zhipuai", "glm-5.1")]
        );
        assert_eq!(
            c("openrouter/anthropic/claude-x"),
            vec![k("openrouter", "anthropic/claude-x")]
        );
        assert_eq!(
            c("deepseek/deepseek-v4-pro"),
            vec![k("deepseek", "deepseek-v4-pro")]
        );
        // no lab: unpriced rather than a guess
        assert!(c("omni-model").is_empty());
        assert!(c("my-local-llama").is_empty());
        assert!(c("").is_empty());
    }

    fn v(since: i64, input: f64) -> Version {
        Version {
            since,
            name: String::new(),
            rates: Rates {
                input,
                output: 1.0,
                cache_read: None,
                cache_write: None,
                tiered: false,
            },
        }
    }

    #[test]
    fn a_change_prices_the_days_after_it_was_seen() {
        let utc = crate::tz(Some("UTC"));
        // 2026-11-05 10:00 UTC
        let seen = 1_793_872_800_000;
        assert_eq!(crate::day(seen, utc), "2026-11-05");
        let vs = vec![v(0, 1.0), v(seen, 2.0)];
        let at = |d: &str| version_for(&vs, d, utc).unwrap().rates.input;
        assert_eq!(at("2026-10-01"), 1.0);
        assert_eq!(
            at("2026-11-05"),
            1.0,
            "the day it was seen keeps the old price"
        );
        assert_eq!(at("2026-11-06"), 2.0);
        assert_eq!(version_for(&[], "2026-11-06", utc), None);
    }

    /// Against a real download: `STEVE_MODELS_DEV_JSON=api.json cargo test -p steve-stats -- --ignored real_catalog`.
    #[test]
    #[ignore]
    fn real_catalog() {
        let path = std::env::var("STEVE_MODELS_DEV_JSON").expect("STEVE_MODELS_DEV_JSON");
        let e = parse(&std::fs::read(path).unwrap()).unwrap();
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../steve-store/migrations/032.sql"))
            .unwrap();
        save(&c, &e, 1).unwrap();
        println!("{} priced models", e.len());
        let book = crate::PriceBook::new(vec![]).with_catalog(&c, None);
        for m in [
            "claude-opus-5-5",
            "claude-sonnet-5-5[1m]",
            "claude-haiku-5-5",
            "kiro/claude-opus-5-5",
            "gpt-5.5",
            "gpt-5.3-codex",
            "gpt-5",
            "o3",
            "gemini-3.8-flash",
            "glm-5.1",
            "kimi-k3",
            "deepseek-v4-pro",
            "qwen3.6-plus",
            "claude-sonnet-4-5-20250929",
            "my-local-model",
        ] {
            let q = book.quote(m, "2026-11-05");
            println!(
                "{m:32} {:?}",
                q.map(|q| (
                    q.price.pattern,
                    q.price.input,
                    q.price.output,
                    q.price.cache_read,
                    q.price.cache_write,
                    q.via,
                    q.price.note
                ))
            );
        }
    }

    #[test]
    fn save_keeps_versions_and_state() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../steve-store/migrations/032.sql"))
            .unwrap();
        let mut e = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(save(&c, &e, 1_000).unwrap(), 3);
        assert_eq!(
            save(&c, &e, 2_000).unwrap(),
            0,
            "unchanged prices write nothing"
        );
        e[0].rates.input = 9.0;
        let opus = (e[0].provider.clone(), e[0].model.clone());
        e.truncate(1);
        assert_eq!(save(&c, &e, 3_000).unwrap(), 1);
        let vs = versions(&c, &opus.0, &opus.1).unwrap();
        assert_eq!(
            vs.iter().map(|v| v.since).collect::<Vec<_>>(),
            vec![0, 3_000]
        );
        assert_eq!(
            versions(&c, "openai", "gpt-5").unwrap().len(),
            1,
            "an unlisted model keeps its price"
        );

        assert_eq!(state(&c, SOURCE).unwrap(), SyncState::default());
        record_ok(&c, SOURCE, 10, Some("\"e1\""), Some(3)).unwrap();
        record_err(&c, SOURCE, 20, "超时").unwrap();
        let s = state(&c, SOURCE).unwrap();
        assert_eq!(
            (s.synced, s.tried, s.error.as_deref(), s.models),
            (Some(10), Some(20), Some("超时"), 3)
        );
        assert_eq!(s.etag.as_deref(), Some("\"e1\""));
        record_ok(&c, SOURCE, 30, None, None).unwrap();
        let s = state(&c, SOURCE).unwrap();
        assert_eq!(
            (s.synced, s.error, s.models, s.etag.as_deref()),
            (Some(30), None, 3, Some("\"e1\""))
        );
    }
}
