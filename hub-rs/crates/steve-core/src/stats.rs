//! Gateway usage records: every model call Steve makes itself — routing, chat answers,
//! drafts, the modules' calls — is written to `usage_records` (`source=gateway`) with what it was for.
//!
//! [`RecordingGateway`] wraps the real gateway once, and both [`crate::Core`] and the modules
//! ([`crate::SharedGateway`]) use that one instance. Callers say what a call is for with the task-locals
//! re-exported here ([`purpose`], [`tagged`], [`for_item`]); an untagged call is booked as `other`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(test)]
use serde_json::{Value, json};
#[cfg(test)]
use steve_store::ModelUsage;
use steve_store::Store;

use crate::error::Result;
use crate::gateway::{Attempt, ModelGateway, OnText, Printed, ThinkTools, Tier};

pub use steve_modules::gateway::{CallTag, current_tag, for_item, purpose, tagged};

/// One logical call, persisted before dispatch and carried through every fallback.
#[derive(Clone)]
pub struct CallAccount {
    pub store: Store,
    pub id: String,
    pub workspace: String,
    tag: CallTag,
}
#[derive(Clone)]
pub struct AttemptAccount {
    pub call: CallAccount,
    pub id: String,
}
tokio::task_local! {
    static CALL: CallAccount;
    static ATTEMPT: AttemptAccount;
}
pub fn current_call() -> Option<CallAccount> {
    CALL.try_with(Clone::clone).ok()
}
pub fn current_attempt() -> Option<AttemptAccount> {
    ATTEMPT.try_with(Clone::clone).ok()
}
impl CallAccount {
    pub fn begin(store: &Store, tag: &CallTag) -> steve_store::Result<Self> {
        Self::begin_owned(store, tag, None, None)
    }
    pub fn begin_owned(
        store: &Store,
        tag: &CallTag,
        bot: Option<String>,
        project: Option<String>,
    ) -> steve_store::Result<Self> {
        if let Err(e) = store.stats().gateway_apply_local(128) {
            tracing::warn!("stats: pending local usage: {e}");
        }
        let workspace = if tag.workspace_id.is_empty() {
            match tag.item_id.as_deref() {
                Some(item) => store
                    .items()
                    .get(item)?
                    .map(|i| i.workspace_id)
                    .unwrap_or_default(),
                None => String::new(),
            }
        } else {
            tag.workspace_id.clone()
        };
        let ts = steve_store::now();
        let tz: Option<String> = store.settings().get(&workspace, "stats.tz", None)?;
        let id = uuid::Uuid::new_v4().to_string();
        store
            .stats()
            .gateway_begin(&steve_store::gateway_ledger::CallBegin {
                id: id.clone(),
                workspace: workspace.clone(),
                item: tag.item_id.clone(),
                bot,
                project,
                purpose: tag.purpose.into(),
                started: ts,
                day: steve_stats::day(ts, steve_stats::tz(tz.as_deref())),
            })?;
        let mut captured = tag.clone();
        captured.workspace_id = workspace.clone();
        Ok(Self {
            store: store.clone(),
            id,
            workspace,
            tag: captured,
        })
    }
    pub async fn scope<F: std::future::Future>(&self, f: F) -> F::Output {
        // Model selection, dispatch and the bill use the captured owner even after the caller's
        // task-local tag is gone. Nested purpose/item scopes inside f still work normally.
        steve_modules::gateway::with_call_tag(self.tag.clone(), CALL.scope(self.clone(), f)).await
    }
    pub async fn tracked_scope<F: std::future::Future>(&self, f: F) -> F::Output {
        let _guard = CallGuard(Some(self.clone()));
        self.scope(f).await
    }
    pub fn attempt(&self, harness: &str, model: &str) -> steve_store::Result<AttemptAccount> {
        let id = uuid::Uuid::new_v4().to_string();
        self.store
            .stats()
            .gateway_attempt(&self.id, &id, harness, model)?;
        Ok(AttemptAccount {
            call: self.clone(),
            id,
        })
    }
    pub fn finish(&self, a: &Attempt) -> steve_store::Result<()> {
        self.store.stats().gateway_finish(
            &self.id,
            &steve_store::gateway_ledger::CallFinish {
                ok: a.result.is_ok(),
                model: a.model.clone(),
                harness: if a.harness.is_empty() {
                    steve_acp::adapters::DEFAULT_ADAPTER.0.into()
                } else {
                    a.harness.clone()
                },
                attempts: a.attempts,
                duration_ms: a.duration_ms,
                failures: a.failures.clone(),
                usage: a.usage.clone(),
            },
        )
    }
    pub fn cancel(&self, reason: &str) -> steve_store::Result<()> {
        self.store.stats().gateway_cancel(&self.id, reason)
    }
}
impl AttemptAccount {
    pub async fn scope<F: std::future::Future>(&self, f: F) -> F::Output {
        ATTEMPT.scope(self.clone(), f).await
    }
    pub fn report(&self, r: &steve_node_api::UsageReport) -> steve_store::Result<bool> {
        let received =
            self.call
                .store
                .stats()
                .gateway_local_receive(&self.id, r, steve_store::now())?;
        if let Err(e) = self.call.store.stats().gateway_apply_local(128) {
            tracing::warn!("stats: local report durably queued: {e}");
        }
        Ok(received)
    }
}
struct CallGuard(Option<CallAccount>);
impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            if let Err(e) = c.cancel("caller dropped") {
                tracing::warn!("stats: cancel: {e}");
            }
        }
    }
}
/// A gateway whose live and final bill survives its caller.
pub struct RecordingGateway {
    inner: Arc<dyn ModelGateway>,
    store: Store,
}
impl RecordingGateway {
    pub fn new(inner: Arc<dyn ModelGateway>, store: Store) -> Self {
        Self { inner, store }
    }
}
/// Compatibility entry for a completed external call. Live runners allocate CallAccount first.
pub fn record(store: &Store, a: &Attempt, tag: &CallTag) -> steve_store::Result<()> {
    if let Some(c) = current_call() {
        return c.finish(a);
    }
    CallAccount::begin(store, tag)?.finish(a)
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
        let account = match CallAccount::begin(&self.store, &current_tag()) {
            Ok(c) => c,
            Err(e) => {
                return Attempt {
                    result: Err(crate::error::msg(format!("usage ownership: {e}"))),
                    attempts: 0,
                    duration_ms: 0,
                    usage: vec![],
                    model: String::new(),
                    failures: vec![],
                    harness: String::new(),
                };
            }
        };
        let mut guard = CallGuard(Some(account.clone()));
        let a = account
            .scope(self.inner.attempt(prompt, tier, timeout, tools, on_text))
            .await;
        match account.finish(&a) {
            Ok(()) => guard.0 = None,
            Err(e) => tracing::warn!("stats: gateway result not recorded: {e}"),
        }
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
        duration_ms: Option<i64>,
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
                        duration_ms: r.get(15)?,
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
    async fn an_item_supplies_missing_workspace_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.ensure_workspace().unwrap();
        s.bots()
            .seed(
                "owner",
                false,
                &steve_store::NewBot {
                    name: "Owner".into(),
                    harness: "test".into(),
                    ..Default::default()
                },
                steve_store::WS,
            )
            .unwrap();
        let item = s
            .items()
            .create(
                steve_store::WS,
                &steve_store::NewItem {
                    tags: vec![
                        s.tags()
                            .kinds(steve_store::WS)
                            .unwrap()
                            .into_iter()
                            .find(|tag| tag.name == "feat")
                            .unwrap()
                            .id,
                    ],
                    title: "bill".into(),
                    goal: "bill".into(),
                    bot_id: "owner".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let item_id = item.id.clone();
        let c = CallAccount::begin(
            &s,
            &CallTag {
                item_id: Some(item.id),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c.workspace, steve_store::WS);
        c.scope(async {
            assert_eq!(current_tag().workspace_id, steve_store::WS);
            assert_eq!(current_tag().item_id, Some(item_id));
        })
        .await;
        s.reader::<_,steve_store::StoreError>(|db| {
            let row:(String,Option<String>,i64)=db.query_row("SELECT workspace_id,bot_id,(SELECT COUNT(*) FROM gateway_attempts) FROM gateway_calls WHERE call_id=?",[&c.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(row,(steve_store::WS.into(),Some("owner".into()),0));Ok(())
        }).unwrap();
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
            json!({"scope": "main", "failures": ["busy: API Error: 503 upstream unavailable"], "call_id": r[0].detail["call_id"], "pending": false, "accounting_tokens": true})
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
            json!({"scope": "none", "failures": ["slow: [timeout] Command timed out after 1500 ms"], "call_id": r[0].detail["call_id"], "pending": false, "accounting_tokens": true})
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
                "tool" => (Err(crate::error::CoreError::NeedsTools), vec![]),
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
                no_fallback: false,
                reported_model: None,
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

    /// A model that reached for a tool the call does not allow, for a caller that hands such a
    /// request on (a trigger word's answer): the chain ends like on a timeout, the next model has
    /// no tools either.
    #[tokio::test]
    async fn a_request_that_needs_tools_is_not_tried_on_the_next_model_when_handed_on() {
        let scripted = crate::gateway::Fallback {
            one: Scripted {
                tries: Default::default(),
            },
            fast: vec!["tool".into(), "haiku".into()],
            smart: vec![],
            models: None,
        };
        let a = steve_modules::gateway::hands_on_needs_tools(scripted.attempt(
            "hi",
            Tier::Fast,
            Duration::from_secs(10),
            &ThinkTools::None,
            None,
        ))
        .await;
        assert!(matches!(a.result, Err(crate::error::CoreError::NeedsTools)));
        assert_eq!(a.attempts, 1);
        assert_eq!(tries(&scripted).len(), 1, "haiku is not tried");
    }

    /// Every other caller (the router, mail triage, a summary, a config draft) falls back to the
    /// next model as for any fast error: that one may answer without a tool.
    #[tokio::test]
    async fn a_request_that_needs_tools_falls_back_for_other_callers() {
        let scripted = crate::gateway::Fallback {
            one: Scripted {
                tries: Default::default(),
            },
            fast: vec!["tool".into(), "haiku".into()],
            smart: vec![],
            models: None,
        };
        let a = crate::stats::purpose(
            "route",
            scripted.attempt(
                "hi",
                Tier::Fast,
                Duration::from_secs(10),
                &ThinkTools::None,
                None,
            ),
        )
        .await;
        assert_eq!(a.result.expect("haiku answers").model, "haiku");
        assert_eq!(a.attempts, 2);
        assert_eq!(tries(&scripted).len(), 2);
    }

    /// The adapter a call that chose none runs on.
    fn cc() -> String {
        steve_acp::adapters::DEFAULT_ADAPTER.0.into()
    }

    fn tries(f: &crate::gateway::Fallback<Scripted>) -> Vec<Try> {
        f.one.tries.lock().unwrap().clone()
    }

    fn zero(model: &str) -> ModelUsage {
        ModelUsage {
            model: model.into(),
            tokens: Tokens {
                input: Some(0),
                output: Some(0),
                cache_read: Some(0),
                cache_write: Some(0),
                reasoning: Some(0),
            },
            cost_reported: Some(0.0),
            detail: json!({"scope": "main"}),
        }
    }

    fn measured(usage: Vec<ModelUsage>, ok: bool) -> Attempt {
        Attempt {
            result: if ok {
                Ok(Printed::default())
            } else {
                Err(crate::error::msg("cancelled"))
            },
            attempts: 2,
            duration_ms: 123,
            usage,
            model: "answer".into(),
            failures: vec!["failed: unavailable".into()],
            harness: cc(),
        }
    }

    fn counts(s: &Store) -> (i64, i64, i64, i64) {
        s.with_conn(|c| Ok(c.query_row(
            "SELECT SUM(calls), SUM(failed_calls), SUM(output), SUM(reasoning) FROM stat_daily WHERE source = 'gateway'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)).unwrap()
    }

    #[test]
    fn reported_zero_and_missing_usage_stay_distinct_on_success_and_failure() {
        for ok in [true, false] {
            for reported in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let s = store(dir.path());
                let usage = if reported {
                    vec![zero("answer")]
                } else {
                    vec![]
                };
                record(&s, &measured(usage, ok), &CallTag::default()).unwrap();
                let r = rows(&s);
                assert_eq!(r.len(), 1);
                assert_eq!(
                    r[0].tokens,
                    if reported {
                        zero("answer").tokens
                    } else {
                        Tokens::default()
                    }
                );
                assert_eq!(r[0].cost, reported.then_some(0.0));
                assert_eq!(r[0].model, "answer");
                assert_eq!(
                    (r[0].attempts, r[0].duration_ms, r[0].ok),
                    (2, Some(123), ok)
                );
                assert_eq!(r[0].detail["scope"], if reported { "main" } else { "none" });
                assert_eq!(counts(&s), (1, i64::from(!ok), 0, 0));
            }
        }
    }

    #[test]
    fn zero_cost_only_is_reported_without_inventing_zero_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut cost_only = zero("answer");
        cost_only.tokens = Tokens::default();
        record(&s, &measured(vec![cost_only], true), &CallTag::default()).unwrap();
        let r = rows(&s);
        assert_eq!(r[0].tokens, Tokens::default());
        assert_eq!(r[0].cost, Some(0.0));
    }

    #[test]
    fn unspent_retry_keeps_zero_evidence_without_an_extra_call_or_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let paid = ModelUsage {
            model: "answer".into(),
            tokens: haiku(),
            cost_reported: Some(0.1),
            detail: json!({"scope":"main"}),
        };
        record(
            &s,
            &measured(vec![zero("failed"), paid], true),
            &CallTag::default(),
        )
        .unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (
                r[0].model.as_str(),
                r[0].tokens,
                r[0].attempts,
                r[0].duration_ms
            ),
            ("answer", haiku(), 2, Some(123))
        );
        assert_eq!(counts(&s), (1, 0, 4, 0));
        assert_eq!(
            r[0].detail["unspent_usage"],
            json!([{
                "model":"failed", "tokens":{"input":0,"output":0,"cache_read":0,"cache_write":0,"reasoning":0},
                "cost_reported":0.0, "detail":{"scope":"main"}
            }])
        );
    }

    #[test]
    fn all_zero_retries_book_the_answering_model_once_and_keep_partial_unknowns() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut failed = zero("failed");
        failed.tokens.reasoning = None;
        failed.cost_reported = None;
        record(
            &s,
            &measured(vec![failed, zero("answer")], true),
            &CallTag::default(),
        )
        .unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens),
            ("answer", zero("answer").tokens)
        );
        assert_eq!(
            r[0].detail["unspent_usage"][0]["tokens"]["reasoning"],
            Value::Null
        );
        assert_eq!(
            r[0].detail["unspent_usage"][0]["cost_reported"],
            Value::Null
        );
        assert_eq!(counts(&s), (1, 0, 0, 0));
    }

    #[test]
    fn a_failed_models_zero_cannot_replace_the_answering_models_missing_usage() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        record(
            &s,
            &measured(vec![zero("failed")], true),
            &CallTag::default(),
        )
        .unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens, r[0].cost),
            ("answer", Tokens::default(), None)
        );
        assert_eq!(r[0].detail["unspent_usage"][0]["tokens"]["output"], 0);
        assert_eq!(counts(&s), (1, 0, 0, 0));
    }

    #[test]
    fn a_single_zero_report_preserves_the_adapters_canonical_model_name() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut a = measured(vec![zero("canonical")], true);
        a.attempts = 1;
        record(&s, &a, &CallTag::default()).unwrap();
        assert_eq!(
            (rows(&s)[0].model.as_str(), rows(&s)[0].tokens),
            ("canonical", zero("canonical").tokens)
        );
    }

    struct Metered;

    #[async_trait]
    impl crate::gateway::OneModel for Metered {
        async fn once(
            &self,
            _: &crate::gateway::OneCall<'_>,
            model: &str,
            _: Option<&str>,
            _: Option<OnText<'_>>,
        ) -> crate::gateway::Tried {
            use steve_node_api::{Epoch, Measurement, Money, Tokens, UsageReport, UsageScope};
            let end = if model == "failed" { 6 } else { 12 };
            let report = |measurement, output| UsageReport {
                report_id: format!("{measurement:?}"),
                model: Some(model.into()),
                meter_epoch: Epoch::new("meter").unwrap(),
                scope: UsageScope::Main,
                measurement,
                tokens: Tokens {
                    output: Some(output),
                    reasoning: Some(output / 2),
                    ..Tokens::default()
                },
                cost: Some(Money {
                    decimal: (output as f64 / 100.0).to_string(),
                    currency: "USD".into(),
                }),
                context_used: None,
                context_limit: None,
            };
            let final_report = report(Measurement::Final, end);
            crate::gateway::Tried::of(
                if model == "failed" {
                    Err(crate::gateway::OneShotError::Failed("unavailable".into()))
                } else {
                    Ok(("ok".into(), None))
                },
                vec![
                    report(Measurement::Cumulative, end - 2),
                    final_report.clone(),
                    final_report,
                ],
            )
        }
    }

    #[tokio::test]
    async fn consumed_failure_and_retry_keep_usage_and_count_metadata_on_the_first_model() {
        let gateway = crate::gateway::Fallback {
            one: Metered,
            fast: vec!["failed".into(), "answer".into()],
            smart: vec![],
            models: None,
        };
        let a = gateway
            .attempt(
                "hi",
                Tier::Fast,
                Duration::from_secs(1),
                &ThinkTools::None,
                None,
            )
            .await;
        assert!(a.result.is_ok());
        assert_eq!(a.attempts, 2);
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        record(&s, &a, &CallTag::default()).unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens.output, r[0].attempts),
            ("failed", Some(6), 2)
        );
        assert_eq!(
            (r[1].model.as_str(), r[1].tokens.output, r[1].attempts),
            ("answer", Some(12), 1)
        );
        assert!(r[0].duration_ms.is_some());
        assert_eq!(r[1].duration_ms, None);
        assert_eq!(
            counts(&s),
            (2, 0, 18, 9),
            "reasoning is already part of output"
        );
        assert_eq!(r[0].detail["failures"].as_array().unwrap().len(), 1);
        assert!(r[1].detail.get("failures").is_none());
    }

    #[test]
    fn cancelled_attempt_books_consumed_usage_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut usage = zero("answer");
        usage.tokens.output = Some(12);
        usage.tokens.reasoning = Some(4);
        usage.cost_reported = Some(0.12);
        record(&s, &measured(vec![usage], false), &CallTag::default()).unwrap();
        assert_eq!(counts(&s), (1, 1, 12, 4));
        assert_eq!(rows(&s)[0].attempts, 2);
    }
    #[test]
    fn explicitly_zero_usage_is_not_replaced_by_an_unknown_gateway_row() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.ensure_workspace().unwrap();
        let a = Attempt {
            result: Ok(Printed::default()),
            attempts: 1,
            duration_ms: 1,
            usage: vec![ModelUsage {
                model: "zero-model".into(),
                tokens: Tokens {
                    input: Some(0),
                    output: Some(0),
                    cache_read: Some(0),
                    cache_write: Some(0),
                    reasoning: None,
                },
                cost_reported: Some(0.0),
                ..Default::default()
            }],
            model: "zero-model".into(),
            failures: vec![],
            harness: "claude-code".into(),
        };
        record(
            &s,
            &a,
            &CallTag {
                workspace_id: steve_store::WS.into(),
                ..Default::default()
            },
        )
        .unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].tokens.input, Some(0));
        assert_eq!(r[0].tokens.output, Some(0));
        assert_eq!(r[0].cost, Some(0.0));
    }
}
