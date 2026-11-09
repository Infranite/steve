//! Gateway usage records: every model call Steve makes itself — routing, chat answers,
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
use crate::gateway::{Attempt, ModelGateway, OnText, Printed, ThinkTools, Tier};

pub use steve_modules::gateway::{CallTag, current_tag, for_item, purpose, tagged};

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
            // the adapter the call ran on; a call that never chose one, Steve's default adapter's
            harness: if a.harness.is_empty() {
                steve_acp::adapters::DEFAULT_ADAPTER.0.to_string()
            } else {
                a.harness.clone()
            },
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
        tools: &ThinkTools,
    ) -> Result<Printed> {
        self.attempt(prompt, tier, timeout, tools, None)
            .await
            .result
    }
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        self.attempt(prompt, tier, timeout, tools, Some(on_text))
            .await
            .result
    }
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        // Background callers share the durable upgrade gate with conversations. Keep one
        // permit across every fallback so maintenance cannot seal between model attempts;
        // an already-admitted conversation may still finish its nested calls.
        let _activity = self.store.activity("model").await;
        let a = self
            .inner
            .attempt(prompt, tier, timeout, tools, on_text)
            .await;
        self.record(&a);
        a
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::gateway::SharedGateway;
    use steve_modules::Gateway as _;
    use steve_store::Tokens;

    fn haiku() -> Tokens {
        Tokens {
            input: Some(2),
            output: Some(4),
            cache_read: Some(11_097),
            cache_write: Some(5_727),
            reasoning: None,
        }
    }

    /// The stand-in adapter ([`crate::testkit::FakeAcp`]): model `haiku` answers "ok" with its
    /// usage and cost, `busy` with a 503, `slow` hangs, `broken` dies.
    fn fake(dir: &std::path::Path) -> Arc<dyn ModelGateway> {
        let acp = crate::testkit::FakeAcp::new(dir, "ok");
        Arc::new(crate::InProcess {
            one: acp.one(),
            fast: vec!["busy".into(), "haiku".into()],
            smart: vec!["slow".into(), "broken".into()],
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
                    assert_eq!(r.get::<_, String>(14)?, "claude-code");
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

    #[tokio::test]
    async fn a_call_that_falls_back_is_one_row_with_two_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw = RecordingGateway::new(fake(dir.path()), s.clone());
        let p = tagged(
            "route",
            "default",
            gw.call("hi", Tier::Fast, Duration::from_secs(10), &ThinkTools::None),
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
            ("haiku", 2, true, "route", "default")
        );
        assert_eq!((r[0].tokens, r[0].cost), (haiku(), Some(0.000829045)));
        assert_eq!(
            r[0].detail,
            json!({"scope": "main", "failures": ["busy: API Error: 503 upstream unavailable"]})
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
        // `slow` times out, and a timeout ends the chain (#9): `dead` is not tried; the bound leaves room
        // for a loaded test run to spawn the script (300 ms was flaky)
        let e = gw
            .call(
                "hi",
                Tier::Smart,
                Duration::from_millis(1500),
                &ThinkTools::None,
            )
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "Command timed out after 1500 ms");
        let r = rows(&s);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(
            (
                r[0].purpose.as_str(),
                r[0].ws.as_str(),
                r[0].ok,
                r[0].attempts
            ),
            ("other", "", false, 1)
        );
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens),
            ("slow", Tokens::default())
        );
        assert_eq!(
            r[0].detail,
            json!({"scope": "none", "failures": ["slow: [timeout] Command timed out after 1500 ms"]})
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
            Arc::new(crate::InProcess {
                one: crate::testkit::FakeAcp::new(dir.path(), "ok").one(),
                fast: vec!["haiku".into()],
                smart: vec!["haiku".into()],
                models: None,
            }),
            s.clone(),
        );
        purpose(
            "chat",
            fast_only.call_streaming(
                "hi",
                Tier::Smart,
                Duration::from_secs(10),
                &ThinkTools::None,
                &|_| *seen.lock().unwrap() += 1,
            ),
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
            [("flow", "haiku", Some("it_none")), ("chat", "haiku", None),]
        );
        assert_eq!(r[1].tokens, haiku());
        assert_eq!(*seen.lock().unwrap(), 1, "the one chunk is heard");
    }

    /// One try as [`Scripted`] saw it: model, effort, harness, timeout in ms, streaming.
    type Try = (String, Option<String>, String, u128, bool);

    /// A [`crate::gateway::OneModel`] that answers like [`fake`]'s script without a process, and notes
    /// what each try was given.
    struct Scripted {
        tries: std::sync::Mutex<Vec<Try>>,
    }

    #[async_trait::async_trait]
    impl crate::gateway::OneModel for Scripted {
        async fn once(
            &self,
            call: &crate::gateway::OneCall<'_>,
            model: &str,
            effort: Option<&str>,
            on_text: Option<crate::gateway::OnText<'_>>,
        ) -> crate::gateway::Tried {
            self.tries.lock().unwrap().push((
                model.to_string(),
                effort.map(str::to_string),
                call.harness.to_string(),
                call.timeout.as_millis(),
                on_text.is_some(),
            ));
            let (reply, usage) = match model {
                "haiku" => (
                    Ok(("ok".to_string(), Some(0.000829045))),
                    vec![ModelUsage {
                        model: model.into(),
                        tokens: haiku(),
                        cost_reported: Some(0.000829045),
                        detail: json!({"scope": "main"}),
                    }],
                ),
                "busy" => (
                    Err(crate::error::msg("API Error: 503 upstream unavailable")),
                    vec![],
                ),
                "slow" => (
                    Err(crate::error::msg(format!(
                        "Command timed out after {} ms",
                        call.timeout.as_millis()
                    ))),
                    vec![],
                ),
                _ => (
                    Err(crate::error::msg(
                        "Command failed: the adapter exited unexpectedly",
                    )),
                    vec![],
                ),
            };
            let timed_out = model == "slow";
            crate::gateway::Tried {
                reply,
                usage,
                timed_out,
            }
        }
    }

    /// What the statistics see of an attempt, its wall time left out.
    fn seen(a: &Attempt) -> String {
        let result = match &a.result {
            Ok(p) => format!(
                "ok {:?} {} {:?} {} {:?}",
                p.text, p.model, p.cost_usd, p.attempts, p.usage
            ),
            Err(e) => format!("err {e}"),
        };
        format!(
            "{result} | {} {} {:?} {:?} {:?}",
            a.attempts, a.model, a.failures, a.usage, a.harness
        )
    }

    /// The fallback over a scripted single model gives the same attempts as over the CLI: model order,
    /// failures, merged usage, the answer and its cost.
    #[tokio::test]
    async fn the_fallback_over_a_scripted_model_matches_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake(dir.path());
        let scripted = crate::gateway::Fallback {
            one: Scripted {
                tries: Default::default(),
            },
            fast: vec!["busy".into(), "haiku".into()],
            smart: vec!["slow".into(), "broken".into()],
            models: None,
        };
        let seen_text = std::sync::Mutex::new(0);
        let on_text = |_: &str| *seen_text.lock().unwrap() += 1;
        for (tier, timeout, streaming) in [
            (Tier::Fast, Duration::from_secs(10), false),
            (Tier::Fast, Duration::from_secs(10), true),
            (Tier::Smart, Duration::from_millis(1500), false),
        ] {
            let text: Option<crate::gateway::OnText<'_>> = streaming.then_some(&on_text);
            let want = cli
                .attempt("hi", tier, timeout, &ThinkTools::None, text)
                .await;
            let got = scripted
                .attempt("hi", tier, timeout, &ThinkTools::None, text)
                .await;
            assert_eq!(seen(&got), seen(&want), "{tier:?} streaming {streaming}");
        }
        let a = &tries(&scripted);
        assert_eq!(
            a,
            &[
                ("busy".to_string(), None, cc(), 10_000, false),
                ("haiku".into(), None, cc(), 10_000, false),
                ("busy".into(), None, cc(), 10_000, true),
                ("haiku".into(), None, cc(), 10_000, true),
                // #9: a timeout ends the chain, the next model is not tried
                ("slow".into(), None, cc(), 1500, false),
            ]
        );
    }

    /// The adapter a call that chose none runs on.
    fn cc() -> String {
        steve_acp::adapters::DEFAULT_ADAPTER.0.into()
    }

    fn tries(f: &crate::gateway::Fallback<Scripted>) -> Vec<Try> {
        f.one.tries.lock().unwrap().clone()
    }
}
