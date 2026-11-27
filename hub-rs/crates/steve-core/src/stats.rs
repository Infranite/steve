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

use crate::CoreError;
use crate::error::Result;
use crate::gateway::{
    Attempt, GatewayError, ModelGateway, OnText, Printed, ThinkTools, Tier, TimeoutPhase,
};

pub use steve_modules::gateway::{CallTag, current_tag, for_item, purpose, tagged};

/// One logical call, persisted before dispatch.
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
                node_id: Some("local".into()),
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
        let harness = if a.harness.is_empty() {
            steve_harness_spec::DEFAULT_ADAPTER.0.to_string()
        } else {
            a.harness.clone()
        };
        self.store.stats().gateway_finish(
            &self.id,
            &steve_store::gateway_ledger::CallFinish {
                ok: a.result.is_ok(),
                model: a.model.clone(),
                harness: harness.clone(),
                attempts: a.attempts,
                duration_ms: a.duration_ms,
                failures: a.failures.clone(),
                usage: a.usage.clone(),
            },
        )?;
        gateway_outcome(&a.result);
        steve_store::instrumentation::ModelCall {
            source: "gateway",
            harness,
            model: a.model.clone(),
            result: match &a.result {
                Ok(_) => "ok",
                Err(CoreError::Gateway(GatewayError::NeedsTools)) => "needs_tools",
                Err(CoreError::Gateway(GatewayError::Timeout(_))) => "timeout",
                Err(CoreError::Gateway(GatewayError::MissingCapability)) => "missing_capability",
                Err(CoreError::Gateway(GatewayError::ToolDenied)) => "tool_denied",
                Err(CoreError::Gateway(GatewayError::LocalNodeUpdating)) => "local_node_updating",
                Err(CoreError::Gateway(GatewayError::Startup(_))) => "startup",
                Err(_) => "error",
            },
            seconds: a.duration_ms as f64 / 1000.0,
            usage: a.usage.clone(),
            context_peak: None,
        }
        .record();
        Ok(())
    }
    pub fn cancel(&self, reason: &str) -> steve_store::Result<()> {
        self.store.stats().gateway_cancel(&self.id, reason)
    }
}
fn gateway_outcome(result: &Result<Printed>) {
    match result {
        Err(CoreError::Gateway(GatewayError::Timeout(phase))) => {
            metrics::counter!("steve_gateway_timeout_total", "phase" => phase.as_str())
                .increment(1);
        }
        Err(CoreError::Gateway(GatewayError::MissingCapability)) => {
            metrics::counter!("steve_gateway_preflight_rejections_total", "reason" => "missing_capability").increment(1);
        }
        Err(CoreError::Gateway(GatewayError::ToolDenied)) => {
            metrics::counter!("steve_gateway_preflight_rejections_total", "reason" => "tool_denied").increment(1);
        }
        Err(CoreError::Gateway(GatewayError::LocalNodeUpdating)) => {
            metrics::counter!("steve_gateway_preflight_rejections_total", "reason" => "local_node_updating").increment(1);
        }
        _ => {}
    }
}

impl AttemptAccount {
    pub async fn scope<F: std::future::Future>(&self, f: F) -> F::Output {
        ATTEMPT.scope(self.clone(), f).await
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
/// Records a completed external call made outside a [`CallAccount`] (live runners allocate one
/// first and finish it themselves).
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
        // The deadline starts before waiting for the maintenance admission gate. An admitted
        // conversation can still make its one nested call while the gate drains.
        let start = tokio::time::Instant::now();
        let deadline = start + timeout;
        let admission = tokio::time::timeout_at(deadline, self.store.activity("model")).await;
        let account = match CallAccount::begin(&self.store, &current_tag()) {
            Ok(c) => c,
            Err(e) => {
                return Attempt::before_dispatch(
                    crate::error::msg(format!("usage ownership: {e}")),
                    0,
                );
            }
        };
        let mut guard = CallGuard(Some(account.clone()));
        let mut a = if admission.is_err() || tokio::time::Instant::now() >= deadline {
            Attempt::before_dispatch(
                GatewayError::Timeout(TimeoutPhase::Queue).into(),
                u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
        } else {
            account
                .scope(async {
                    self.inner
                        .attempt(
                            prompt,
                            tier,
                            deadline.saturating_duration_since(tokio::time::Instant::now()),
                            tools,
                            on_text,
                        )
                        .await
                })
                .await
        };
        a.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Ok(printed) = &mut a.result {
            printed.duration_ms = a.duration_ms;
        }
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

    fn fake() -> Arc<dyn ModelGateway> {
        Arc::new(crate::SingleModelGateway {
            one: Scripted::default(),
            fast: vec!["haiku".into(), "busy".into()],
            smart: vec!["slow".into(), "broken".into()],
            models: None,
        })
    }

    fn store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("steve.sqlite3"), steve_store::FnPublisher(|_| {})).unwrap()
    }

    fn rows(s: &Store) -> Vec<steve_store::core_fixture::usage::Usage> {
        let rows = steve_store::core_fixture::usage::usage(s, None);
        for row in &rows {
            assert_eq!(row.source, "gateway");
            assert_eq!(row.harness, "claude-code");
        }
        rows
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
        let row = steve_store::core_fixture::usage::started_gateway_call(&s, &c.id);
        assert_eq!(row, (steve_store::WS.into(), Some("owner".into()), 0));
    }

    #[tokio::test]
    async fn a_successful_call_is_one_usage_row_with_one_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw = RecordingGateway::new(fake(), s.clone());
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
        assert_eq!(p.attempts, 1);
        let r = rows(&s);
        assert_eq!(r.len(), 1, "one planned request: {r:?}");
        assert_eq!(
            (
                r[0].model.as_str(),
                r[0].attempts,
                r[0].ok,
                r[0].purpose.as_str(),
                r[0].ws.as_str()
            ),
            ("haiku", 1, true, "route", "default")
        );
        assert_eq!((r[0].tokens, r[0].cost), (haiku(), Some(0.000829045)));
        assert_eq!(
            r[0].detail,
            json!({"scope": "main", "call_id": r[0].detail["call_id"], "pending": false, "accounting_tokens": true})
        );
        /* the daily cube agrees */
        let daily = steve_store::core_fixture::usage::gateway_daily(&s, Some("route"));
        assert_eq!(
            (daily.calls, daily.cache_read, daily.hit_base),
            (1, 11_097, 2 + 11_097 + 5_727)
        );
    }

    #[tokio::test]
    async fn a_failed_untagged_call_is_still_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw = RecordingGateway::new(fake(), s.clone());
        let e = gw
            .call(
                "hi",
                Tier::Smart,
                Duration::from_millis(1500),
                &ThinkTools::None,
            )
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "网关在 first_byte 阶段超时");
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
            json!({"scope": "none", "failures": ["slow: 网关在 first_byte 阶段超时"], "call_id": r[0].detail["call_id"], "pending": false, "accounting_tokens": true})
        );
        let daily = steve_store::core_fixture::usage::gateway_daily(&s, None);
        assert_eq!(daily.failures, 1);
    }

    #[tokio::test]
    async fn module_calls_and_streaming_answers_are_recorded_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let gw: Arc<dyn ModelGateway> = Arc::new(RecordingGateway::new(fake(), s.clone()));
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
            ("ok", Some(0.000829045), 1)
        );
        assert_eq!(o.usage.last().map(|u| u.tokens), Some(haiku()));
        /* a chat answer, streamed */
        let seen = std::sync::Mutex::new(0);
        let fast_only = RecordingGateway::new(
            Arc::new(crate::SingleModelGateway {
                one: Scripted::default(),
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

    #[derive(Default)]
    struct Scripted {
        tries: std::sync::Mutex<Vec<Try>>,
    }

    #[async_trait::async_trait]
    impl crate::gateway::OneModel for Scripted {
        async fn once(
            &self,
            call: &crate::gateway::OneCall<'_>,
            model: &str,
            effort: Option<steve_types::ReasoningEffort>,
            on_text: Option<crate::gateway::OnText<'_>>,
        ) -> crate::gateway::Tried {
            self.tries.lock().unwrap().push((
                model.to_string(),
                effort.map(|e| e.as_str().to_string()),
                call.harness.to_string(),
                call.spec
                    .deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis(),
                on_text.is_some(),
            ));
            let (reply, usage) = match model {
                "haiku" => (
                    Ok(("ok".to_string(), Some(0.000829045))),
                    vec![ModelUsage {
                        model: model.into(),
                        tokens: haiku(),
                        cost_reported: Some(0.000829045),
                        detail: json!({"scope": "main", "accounting_tokens": true}),
                    }],
                ),
                "busy" => (
                    Err(crate::error::msg("API Error: 503 upstream unavailable")),
                    vec![],
                ),
                "tool" => (
                    Err(steve_modules::gateway::GatewayError::NeedsTools.into()),
                    vec![],
                ),
                "slow" => (
                    Err(GatewayError::Timeout(TimeoutPhase::FirstByte).into()),
                    vec![],
                ),
                _ => (
                    Err(crate::error::msg(
                        "Command failed: the adapter exited unexpectedly",
                    )),
                    vec![],
                ),
            };
            if let (Some(on_text), Ok((text, _))) = (on_text, &reply) {
                on_text(text);
            }
            crate::gateway::Tried {
                reply,
                usage,
                reported_model: None,
            }
        }
    }

    #[tokio::test]
    async fn tool_refusal_and_fast_failures_never_route_to_a_second_model() {
        for first in ["tool", "busy"] {
            let scripted = crate::gateway::SingleModelGateway {
                one: Scripted::default(),
                fast: vec![first.into(), "haiku".into()],
                smart: vec![],
                models: None,
            };
            let a = scripted
                .attempt(
                    "hi",
                    Tier::Fast,
                    Duration::from_secs(10),
                    &ThinkTools::None,
                    None,
                )
                .await;
            assert!(a.result.is_err());
            assert_eq!(a.attempts, 1);
            assert_eq!(tries(&scripted).len(), 1);
        }
    }

    /// The adapter a call that chose none runs on.
    fn cc() -> String {
        steve_harness_spec::DEFAULT_ADAPTER.0.into()
    }

    fn tries(f: &crate::gateway::SingleModelGateway<Scripted>) -> Vec<Try> {
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
        let daily = steve_store::core_fixture::usage::gateway_daily(s, None);
        (daily.calls, daily.failures, daily.output, daily.reasoning)
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
            _: Option<steve_types::ReasoningEffort>,
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
    async fn consumed_failure_keeps_all_usage_without_a_retry() {
        let gateway = crate::gateway::SingleModelGateway {
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
        assert!(a.result.is_err());
        assert_eq!(a.attempts, 1);
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        record(&s, &a, &CallTag::default()).unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].model.as_str(), r[0].tokens.output, r[0].attempts),
            ("failed", Some(6), 1)
        );
        assert!(r[0].duration_ms.is_some());
        assert_eq!(
            counts(&s),
            (1, 1, 6, 3),
            "reasoning is already part of output"
        );
        assert_eq!(r[0].detail["failures"].as_array().unwrap().len(), 1);
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
