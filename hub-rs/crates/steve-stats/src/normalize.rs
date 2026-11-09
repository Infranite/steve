//! One token convention for every source: Anthropic's, where `input` excludes the cache
//! and `output` includes thinking. Missing or `null` fields stay `None`; they are never read as 0.

use serde_json::Value;
use steve_store::Tokens;

/// Token figures as an adapter or the CLI reported them, before conversion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawUsage {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub reasoning: Option<i64>,
}

/// Field names per figure, across the shapes seen: ACP `PromptResponse.usage`
/// (camelCase), `_meta.quota.model_usage[].token_count`, `claude -p` `usage` (snake_case) and its
/// `modelUsage` entries, and the OpenAI names codex-acp carries.
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

fn field(v: &Value, names: &[&str]) -> Option<i64> {
    names.iter().find_map(|n| {
        let x = v.get(*n)?;
        x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))
    })
}

impl RawUsage {
    /// Reads any of the known usage objects; unknown or non-numeric fields are `None`.
    pub fn from_json(v: &Value) -> Self {
        RawUsage {
            input: field(v, INPUT),
            output: field(v, OUTPUT),
            cache_read: field(v, CACHE_READ),
            cache_write: field(v, CACHE_WRITE),
            reasoning: field(v, REASONING),
        }
    }
}

/// Converts `raw` to Steve's convention (Anthropic's: cache reads apart from `input`).
///
/// `input_includes_cache`: the figures count cache reads inside `input` (OpenAI's convention, an
/// adapter's declared `usage_input_includes_cache`), so when `cache_read` is reported and not larger
/// than `input`, it is taken out. `reasoning` is kept as reported and not added to `output`, which
/// already contains it.
pub fn normalize(raw: RawUsage, input_includes_cache: bool) -> Tokens {
    let mut input = raw.input;
    if input_includes_cache
        && let (Some(i), Some(r)) = (raw.input, raw.cache_read)
        && i >= r
    {
        input = Some(i - r);
    }
    Tokens {
        input,
        output: raw.output,
        cache_read: raw.cache_read,
        cache_write: raw.cache_write,
        reasoning: raw.reasoning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(
        i: Option<i64>,
        o: Option<i64>,
        r: Option<i64>,
        w: Option<i64>,
        th: Option<i64>,
    ) -> Tokens {
        Tokens {
            input: i,
            output: o,
            cache_read: r,
            cache_write: w,
            reasoning: th,
        }
    }

    #[test]
    fn table() {
        let cases: &[(&str, Value, bool, Tokens)] = &[
            // claude-agent-acp PromptResponse.usage: unchanged
            (
                "claude-code",
                json!({"inputTokens": 12, "outputTokens": 340, "cachedReadTokens": 50_000, "cachedWriteTokens": 900, "totalTokens": 51_252}),
                true,
                t(Some(12), Some(340), Some(50_000), Some(900), None),
            ),
            // _meta.quota.model_usage[].token_count
            (
                "claude-code",
                json!({"inputTokens": 3, "cachedInputTokens": 7, "cachedWriteTokens": 1, "outputTokens": 9, "reasoningOutputTokens": 4, "totalTokens": 20}),
                true,
                t(Some(3), Some(9), Some(7), Some(1), Some(4)),
            ),
            // claude -p usage, measured 2026-11-01 (claude-haiku-5-5 via the gateway)
            (
                "claude-cli",
                json!({"input_tokens": 2, "cache_creation_input_tokens": 5727, "cache_read_input_tokens": 11097, "output_tokens": 4}),
                true,
                t(Some(2), Some(4), Some(11_097), Some(5_727), None),
            ),
            // claude -p modelUsage entry
            (
                "claude-cli",
                json!({"inputTokens": 2, "outputTokens": 4, "cacheReadInputTokens": 11097, "cacheCreationInputTokens": 5727, "costUSD": 0.0008}),
                true,
                t(Some(2), Some(4), Some(11_097), Some(5_727), None),
            ),
            // codex: cache reads taken out of input, reasoning not added to output
            (
                "codex",
                json!({"inputTokens": 1000, "outputTokens": 300, "cachedReadTokens": 800, "thoughtTokens": 120}),
                true,
                t(Some(200), Some(300), Some(800), None, Some(120)),
            ),
            // codex with the switch off: as reported
            (
                "codex",
                json!({"inputTokens": 1000, "outputTokens": 300, "cachedReadTokens": 800}),
                false,
                t(Some(1000), Some(300), Some(800), None, None),
            ),
            // codex where input < cache_read cannot be the OpenAI convention: unchanged
            (
                "codex",
                json!({"inputTokens": 10, "cachedReadTokens": 800}),
                true,
                t(Some(10), None, Some(800), None, None),
            ),
            // codex without a cache figure
            (
                "codex",
                json!({"inputTokens": 10, "outputTokens": 1}),
                true,
                t(Some(10), Some(1), None, None, None),
            ),
            // missing and null fields stay None, not 0
            (
                "claude-code",
                json!({"inputTokens": 5, "outputTokens": null, "cachedReadTokens": null}),
                true,
                t(Some(5), None, None, None, None),
            ),
            ("claude-code", json!({}), true, Tokens::default()),
            ("claude-code", json!(null), true, Tokens::default()),
        ];
        for (i, (h, v, sw, want)) in cases.iter().enumerate() {
            // the cache reads are inside the input only where the adapter counts them so
            let inside = *h == "codex" && *sw;
            assert_eq!(normalize(RawUsage::from_json(v), inside), *want, "case {i}");
        }
    }
}
