//! Gateway usage records (stats.md §2.4): every model call Steve makes itself — routing, chat answers,
//! drafts, the modules' calls — is written to `usage_records` (`source=gateway`) with what it was for.
//!
//! [`RecordingGateway`] wraps the real gateway once, and both [`crate::Core`] and the modules
//! ([`crate::SharedGateway`]) use that one instance. Callers say what a call is for with the task-locals
//! re-exported here ([`purpose`], [`tagged`], [`for_item`]); an untagged call is booked as `other`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use steve_store::{ModelUsage, NewUsage, Store};

use crate::error::Result;
use crate::gateway::{Attempt, ModelGateway, OnText, Printed, Tier};

pub use steve_modules::gateway::{CallTag, current_tag, for_item, purpose, tagged};

/// The harness name gateway rows are booked under (`claude -p`).
pub const HARNESS: &str = "claude-cli";

/// A [`ModelGateway`] that writes one usage row per model for every call, failed ones included (`ok=0`).
pub struct RecordingGateway {
    inner: Arc<dyn ModelGateway>,
    store: Store,
}

impl RecordingGateway {
    pub fn new(inner: Arc<dyn ModelGateway>, store: Store) -> Self {
        RecordingGateway { inner, store }
    }

    /// Writes the call's rows. A failure to write is logged, never passed to the caller: statistics
    /// must not break a chat answer.
    fn record(&self, a: &Attempt) {
        if let Err(e) = self.try_record(a, &current_tag()) {
            tracing::warn!("stats: gateway usage not recorded: {e}");
        }
    }

    fn try_record(&self, a: &Attempt, tag: &CallTag) -> steve_store::Result<()> {
        record(&self.store, a, tag)
    }
}

/// Writes the usage rows of one model call made outside the gateway (an inline research run), booked
/// under `tag` the same way the gateway books its own.
pub fn record(store: &Store, a: &Attempt, tag: &CallTag) -> steve_store::Result<()> {
    let ts = steve_store::now();
    let ws = tag.workspace_id.as_str();
    let tz: Option<String> = if ws.is_empty() {
        None
    } else {
        store.settings().get(ws, "stats.tz", None)?
    };
    let day = steve_stats::day(ts, steve_stats::tz(tz.as_deref()));
    let (bot_id, project_id) = match &tag.item_id {
        Some(item) => store
            .reader::<_, steve_store::StoreError>(|c| {
                Ok(c.query_row(
                    "SELECT bot_id, project_id FROM items WHERE id = ?",
                    [item],
                    |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()?)
            })?
            .unwrap_or_default(),
        None => (None, None),
    };
    let ok = a.result.is_ok();
    let mut rows = a.usage.clone();
    /* a try that spent nothing (a 503 reporting zeros) is not a call of its own */
    rows.retain(|u| {
        let t = &u.tokens;
        [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
            .iter()
            .any(|x| x.is_some_and(|x| x != 0))
            || u.cost_reported.is_some_and(|c| c != 0.0)
    });
    if rows.is_empty() {
        /* nothing reported (a timeout, a crash): still one row, so the call and its failure count */
        rows.push(ModelUsage {
            model: a.model.clone(),
            detail: json!({"scope": "none"}),
            ..ModelUsage::default()
        });
    }
    for (i, mut usage) in rows.into_iter().enumerate() {
        let first = i == 0;
        if first && !a.failures.is_empty() {
            if let Value::Object(d) = &mut usage.detail {
                d.insert("failures".into(), json!(a.failures));
            } else {
                usage.detail = json!({"failures": a.failures});
            }
        }
        store.stats().usage_add(&NewUsage {
            workspace_id: ws.to_string(),
            ts,
            day: day.clone(),
            source: "gateway".into(),
            purpose: tag.purpose.into(),
            harness: if a.harness.is_empty() {
                HARNESS
            } else {
                a.harness
            }
            .into(),
            turn_id: None,
            session_id: None,
            item_id: tag.item_id.clone(),
            bot_id: bot_id.clone(),
            project_id: project_id.clone(),
            usage,
            /* the call's time and fallbacks once, on its first row */
            duration_ms: first.then(|| i64::try_from(a.duration_ms).unwrap_or(i64::MAX)),
            ok,
            attempts: if first {
                i64::from(a.attempts.max(1))
            } else {
                1
            },
        })?;
    }
    Ok(())
}

#[async_trait]
impl ModelGateway for RecordingGateway {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
    ) -> Result<Printed> {
        self.attempt(prompt, tier, timeout, extra, None)
            .await
            .result
    }
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        self.attempt(prompt, tier, timeout, extra, Some(on_text))
            .await
            .result
    }
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let a = self
            .inner
            .attempt(prompt, tier, timeout, extra, on_text)
            .await;
        self.record(&a);
        a
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::gateway::{ClaudeCli, SharedGateway, usage_of};
    use steve_modules::Gateway as _;
    use steve_store::Tokens;

    /// `claude -p --output-format json` as measured on 2026-11-01 (claude-haiku-5-5 via the gateway).
    const RECORDED: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":2140,"num_turns":1,"result":"ok","session_id":"x","total_cost_usd":0.000829045,"usage":{"input_tokens":2,"cache_creation_input_tokens":5727,"cache_read_input_tokens":11097,"output_tokens":4},"modelUsage":{"claude-haiku-5-5":{"inputTokens":2,"outputTokens":4,"cacheReadInputTokens":11097,"cacheCreationInputTokens":5727,"webSearchRequests":0,"costUSD":0.000829045,"contextWindow":200000,"costBasis":"list"}}}"#;
    /// A gateway 503 that still says what it used.
    const BUSY: &str = r#"{"type":"result","subtype":"error","is_error":true,"result":"API Error: 503 upstream unavailable","total_cost_usd":0,"usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}"#;

    fn haiku() -> Tokens {
        Tokens {
            input: Some(2),
            output: Some(4),
            cache_read: Some(11_097),
            cache_write: Some(5_727),
            reasoning: None,
        }
    }

    /// A stand-in `claude`: model `haiku` answers with [`RECORDED`], `busy` with [`BUSY`] (exit 1),
    /// `slow` hangs, anything else dies without output. Streaming runs print the same `result` line.
    fn fake(dir: &std::path::Path) -> Arc<dyn ModelGateway> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("ok.json"), RECORDED).unwrap();
        std::fs::write(dir.join("busy.json"), BUSY).unwrap();
        let exe = dir.join("claude");
        let script = format!(
            "#!/bin/sh\nfor a; do case \"$p\" in --model) m=\"$a\";; esac; p=\"$a\"; done\ncase \"$m\" in\n  haiku) cat '{0}/ok.json';;\n  busy) cat '{0}/busy.json'; exit 1;;\n  slow) sleep 5;;\n  *) echo boom >&2; exit 2;;\nesac\n",
            dir.display()
        );
        std::fs::write(&exe, script).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        Arc::new(ClaudeCli {
            codex: None,
            executable: exe,
            fast: vec!["busy".into(), "haiku".into()],
            smart: vec!["slow".into(), "dead".into()],
            cwd: dir.into(),
            models: None,
        })
    }

    fn store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("steve.sqlite3"), steve_store::FnPublisher(|_| {})).unwrap()
    }

    #[derive(Debug, PartialEq)]
    struct Row {
        ws: String,
        purpose: String,
        model: String,
        tokens: Tokens,
        cost: Option<f64>,
        ok: bool,
        attempts: i64,
        item: Option<String>,
        detail: Value,
    }

    fn rows(s: &Store) -> Vec<Row> {
        s.with_conn(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id, purpose, model, input, output, cache_read, cache_write, reasoning, cost_reported, ok, attempts, item_id, detail, source, harness, duration_ms FROM usage_records ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| {
                    assert_eq!(r.get::<_, String>(13)?, "gateway");
                    assert_eq!(r.get::<_, String>(14)?, HARNESS);
                    Ok(Row {
                        ws: r.get(0)?,
                        purpose: r.get(1)?,
                        model: r.get(2)?,
                        tokens: Tokens {
                            input: r.get(3)?,
                            output: r.get(4)?,
                            cache_read: r.get(5)?,
                            cache_write: r.get(6)?,
                            reasoning: r.get(7)?,
                        },
                        cost: r.get(8)?,
                        ok: r.get(9)?,
                        attempts: r.get(10)?,
                        item: r.get(11)?,
                        detail: serde_json::from_str(&r.get::<_, String>(12)?).unwrap(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .unwrap()
    }

    #[test]
    fn the_recorded_cli_output_gives_four_figures_and_the_model() {
        let d: Value = serde_json::from_str(RECORDED).unwrap();
        let u = usage_of(&d, "asked-for");
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].model, "claude-haiku-5-5");
        assert_eq!(u[0].tokens, haiku());
        assert_eq!(u[0].cost_reported, Some(0.000829045));
        assert_eq!(u[0].detail, json!({"scope": "all"}));
        /* only `usage`: booked to the model asked for, the total cost on it */
        let mut d2 = d.clone();
        d2.as_object_mut().unwrap().remove("modelUsage");
        let u = usage_of(&d2, "asked-for");
        assert_eq!(
            (u[0].model.as_str(), u[0].tokens, u[0].cost_reported),
            ("asked-for", haiku(), Some(0.000829045))
        );
        assert_eq!(u[0].detail, json!({"scope": "main"}));
        /* two models: each its own row and its own cost */
        let two = json!({"result":"x","total_cost_usd":0.5,"modelUsage":{
            "claude-opus-5-5":{"inputTokens":10,"outputTokens":20,"costUSD":0.4},
            "claude-haiku-5-5":{"inputTokens":1,"outputTokens":null,"costUSD":0.1}}});
        let u = usage_of(&two, "m");
        assert_eq!(u.len(), 2);
        assert_eq!(u[1].tokens.output, None, "null stays unreported");
        assert_eq!(
            u.iter().map(|r| r.cost_reported).collect::<Vec<_>>(),
            [Some(0.4), Some(0.1)]
        );
        /* nothing reported */
        assert!(usage_of(&json!({"result":"x"}), "m").is_empty());
    }

    #[tokio::test]
    async fn a_call_that_falls_back_is_one_row_with_two_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw = RecordingGateway::new(fake(dir.path()), s.clone());
        let p = tagged(
            "route",
            "default",
            gw.call("hi", Tier::Fast, Duration::from_secs(10), &[]),
        )
        .await
        .unwrap();
        assert_eq!((p.text.as_str(), p.model.as_str()), ("ok", "haiku"));
        assert_eq!(
            p.cost_usd,
            Some(0.000829045),
            "the module budget figure is unchanged"
        );
        assert_eq!(p.attempts, 2);
        let r = rows(&s);
        assert_eq!(r.len(), 1, "the 503 try spent nothing: {r:?}");
        assert_eq!(
            (
                r[0].model.as_str(),
                r[0].attempts,
                r[0].ok,
                r[0].purpose.as_str(),
                r[0].ws.as_str()
            ),
            ("claude-haiku-5-5", 2, true, "route", "default")
        );
        assert_eq!((r[0].tokens, r[0].cost), (haiku(), Some(0.000829045)));
        assert_eq!(
            r[0].detail,
            json!({"scope": "all", "failures": ["busy: API Error: 503 upstream unavailable"]})
        );
        /* the daily cube agrees */
        let (calls, input, hit_base): (i64, i64, i64) = s
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT SUM(calls), SUM(cache_read), SUM(hit_base) FROM stat_daily WHERE source = 'gateway' AND purpose = 'route'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!((calls, input, hit_base), (1, 11_097, 2 + 11_097 + 5_727));
    }

    #[tokio::test]
    async fn a_failed_untagged_call_is_still_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw = RecordingGateway::new(fake(dir.path()), s.clone());
        // `slow` times out, then `dead` fails at once; the bound leaves room for a loaded test run to
        // spawn the script (300 ms was flaky)
        let e = gw
            .call("hi", Tier::Smart, Duration::from_millis(1500), &[])
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "Command failed: boom");
        let r = rows(&s);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(
            (
                r[0].purpose.as_str(),
                r[0].ws.as_str(),
                r[0].ok,
                r[0].attempts
            ),
            ("other", "", false, 2)
        );
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens),
            ("dead", Tokens::default())
        );
        assert_eq!(
            r[0].detail,
            json!({"scope": "none", "failures": ["slow: Command timed out after 1500 ms", "dead: Command failed: boom"]})
        );
        let failed: i64 = s
            .with_conn(
                |c| Ok(c.query_row("SELECT failed_calls FROM stat_daily", [], |r| r.get(0))?),
            )
            .unwrap();
        assert_eq!(failed, 1);
    }

    #[tokio::test]
    async fn module_calls_and_streaming_answers_are_recorded_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw: Arc<dyn ModelGateway> =
            Arc::new(RecordingGateway::new(fake(dir.path()), s.clone()));
        /* the modules' gateway, as a flow calls it */
        let o = tagged(
            "flow",
            "default",
            for_item(
                "it_none",
                SharedGateway(gw.clone()).run(
                    steve_modules::Tier::Fast,
                    "hi",
                    Duration::from_secs(10),
                ),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            (o.text.as_str(), o.cost_usd, o.attempts),
            ("ok", Some(0.000829045), 2)
        );
        assert_eq!(o.usage.last().map(|u| u.tokens), Some(haiku()));
        /* a chat answer, streamed */
        let seen = std::sync::Mutex::new(0);
        let fast_only = RecordingGateway::new(
            Arc::new(ClaudeCli {
                codex: None,
                executable: dir.path().join("claude"),
                fast: vec!["haiku".into()],
                smart: vec!["haiku".into()],
                cwd: dir.path().into(),
                models: None,
            }),
            s.clone(),
        );
        purpose(
            "chat",
            fast_only.call_streaming("hi", Tier::Smart, Duration::from_secs(10), &[], &|_| {
                *seen.lock().unwrap() += 1
            }),
        )
        .await
        .unwrap();
        let r = rows(&s);
        let summary: Vec<_> = r
            .iter()
            .map(|r| (r.purpose.as_str(), r.model.as_str(), r.item.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("flow", "claude-haiku-5-5", Some("it_none")),
                ("chat", "claude-haiku-5-5", None),
            ]
        );
        assert_eq!(r[1].tokens, haiku());
        assert!(
            *seen.lock().unwrap() == 0,
            "the result line alone has no deltas"
        );
    }
}
