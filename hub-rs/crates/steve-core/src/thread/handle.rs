//! Single-task execution: deterministic rules first (zero router calls), bounded router call for
//! ambiguous messages, capability preflight (zero tool-less probe calls), and atomic fenced commit.

use super::decide::{self, DecideCtx};
use super::handoff;
use super::offer::{ThreadItem, confirms};
use super::research::SYSTEM;
use super::{AT, Conv, SayTo, Steps, Stop, lock, route_step};
use crate::Core;
use crate::error::Result;
use crate::gateway::{Offer, Route, Said};
use crate::repo::FileRef;
use crate::util::{js_head, now};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::{LazyLock, Mutex};
use steve_modules::contacts::item_people::Mention;
use steve_store::{CommitOutcome, CommitReplyInput, ReceiptDispatch, ThreadTask, thread};

/// “别建事项，直接在这告诉我”: a spoken correction back into the conversation.
pub(super) static TO_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"别建|不用建|不要建|不建事项|别做成|不用做成|直接(在这里?|在对话里)?(告诉|说|讲|回答)|在这里?(告诉|说|讲|回答)")
        .expect("valid regex")
});

/// Asking how an item is going, not telling it something.
pub(super) static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"进展|进度|做完没|做完了吗|完成了吗|怎么样了|到哪了|到哪一步").expect("valid regex")
});

const REROUTE_WINDOW: i64 = 10 * 60 * 1000;

/// Publishes `steve.thinking = false` when the task exits (even on early return or error).
struct ThinkingGuard<'a> {
    core: &'a Core,
    ws: &'a str,
    task_id: &'a str,
    active: bool,
    is_main: bool,
}

impl Drop for ThinkingGuard<'_> {
    fn drop(&mut self) {
        if self.is_main {
            self.core.end_live_main(self.ws, self.task_id);
        }
        if self.active {
            self.core
                .store()
                .store()
                .publish(steve_types::LiveEvent::from(
                    steve_types::WorkLiveEvent::SteveThinking {
                        on: false,
                        ws: self.ws.to_owned().into(),
                        phase: None,
                        started_at: None,
                        at: Some(steve_types::Millis(now())),
                    },
                ));
        }
    }
}

/// Resolved route metadata carried into `commit_reply`.
pub(super) struct RouteInfo {
    pub kind: String,
    pub model_kind: String,
    pub guard: Option<String>,
    pub model: Option<String>,
    pub confidence: Option<f64>,
    pub signals: Vec<String>,
    pub fallback: bool,
    pub ms: i64,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub offer: Option<Offer>,
    pub step_json: Option<Value>,
}

impl Default for RouteInfo {
    fn default() -> Self {
        Self {
            kind: "answer".into(),
            model_kind: "answer".into(),
            guard: None,
            model: None,
            confidence: None,
            signals: vec![],
            fallback: false,
            ms: 0,
            project: None,
            tags: vec![],
            offer: None,
            step_json: None,
        }
    }
}

impl Core {
    pub(super) async fn execute_task(&self, task: &ThreadTask, lease: &str, stop: &Stop) {
        let ws = task.workspace_id.as_str();
        let space = task.space.as_str();
        let conv: Conv = match task.conv_json.as_deref() {
            Some(raw) => match serde_json::from_str::<Conv>(raw) {
                Ok(c) if c.space == space => c,
                _ => {
                    tracing::error!(task = %task.id, ws, space, "corrupt or mismatched task conv_json");
                    let mut meta = Map::new();
                    meta.insert("origin".into(), json!(task.origin_message_id));
                    meta.insert("rerun".into(), json!(task.origin_message_id));
                    meta.insert("error".into(), json!(true));
                    meta.insert(SYSTEM.into(), json!(true));
                    let _ = self.commit_simple(
                        task,
                        lease,
                        false,
                        "failed",
                        Some("（队列任务数据损坏，未自动执行，需要时请重试）"),
                        None,
                        Value::Object(meta),
                        &RouteInfo {
                            kind: "unknown".into(),
                            model_kind: "unknown".into(),
                            ..Default::default()
                        },
                        None,
                        None,
                        ReceiptDispatch::NoDispatch,
                        Some("corrupt_task"),
                        Some("corrupt or mismatched conv_json"),
                    );
                    return;
                }
            },
            None => {
                if space == super::MAIN {
                    Conv::main()
                } else {
                    Conv {
                        space: space.into(),
                        research: false,
                        items: false,
                        owner_only: false,
                        start: false,
                        hands_on: false,
                    }
                }
            }
        };
        let t0 = now();
        let quiet = conv.guest();
        let is_main = !quiet && space == super::MAIN;
        if is_main {
            self.begin_live_main(ws, &task.id, Some(&task.origin_message_id), t0);
        }
        if !quiet {
            self.store().store().publish(steve_types::LiveEvent::from(
                steve_types::WorkLiveEvent::SteveThinking {
                    on: true,
                    ws: ws.to_owned().into(),
                    phase: None,
                    started_at: Some(steve_types::Millis(t0)),
                    at: Some(steve_types::Millis(t0)),
                },
            ));
        }
        let _thinking = ThinkingGuard {
            core: self,
            ws,
            task_id: &task.id,
            active: !quiet,
            is_main,
        };
        let steps = Mutex::new(Steps::default());
        if let Err(e) = self
            .execute_task_inner(task, lease, stop, &conv, &steps, t0)
            .await
        {
            tracing::error!(task = %task.id, ws, space, "thread task failed: {e}");
            let text = format!("（出错了：{}）", js_head(&e.to_string(), 120));
            let mut meta = Map::new();
            meta.insert("origin".into(), json!(task.origin_message_id));
            meta.insert("error".into(), json!(true));
            meta.insert(SYSTEM.into(), json!(true));
            meta.insert("steps".into(), lock(&steps).json(now() - t0));
            let _ = self.commit_simple(
                task,
                lease,
                false,
                "failed",
                Some(&text),
                None,
                Value::Object(meta),
                &RouteInfo {
                    kind: "unknown".into(),
                    model_kind: "unknown".into(),
                    ..Default::default()
                },
                None,
                None,
                ReceiptDispatch::NoDispatch,
                Some("task_error"),
                Some(&e.to_string()),
            );
        }
        // If `stop` / `stop_run` committed right before `set_task_token` or a non-stop
        // `commit_reply`, that commit returned `Fenced` while leaving this task `claimed` under
        // our lease with `cancel_requested_at != NULL`. Settle it cleanly as `stopped` so the
        // space head never stays stuck in `claimed`.
        let still_claimed_cancelled = self
            .store()
            .store()
            .read(|r| thread::task_by_id(r, &task.id))
            .ok()
            .flatten()
            .is_some_and(|cur| {
                cur.status == "claimed"
                    && cur.lease_token.as_deref() == Some(lease)
                    && cur.generation == task.generation
                    && cur.cancel_requested_at.is_some()
            });
        if still_claimed_cancelled {
            let _ = self.commit_stopped(task, lease, &steps, t0, &RouteInfo::default());
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_simple(
        &self,
        task: &ThreadTask,
        lease: &str,
        allow_cancelled_stop: bool,
        outcome: &str,
        reply_text: Option<&str>,
        reply_item_id: Option<&str>,
        reply_meta: Value,
        route: &RouteInfo,
        route_outcome: Option<&str>,
        item_id: Option<&str>,
        dispatch: ReceiptDispatch<'_>,
        error_code: Option<&str>,
        error_detail: Option<&str>,
    ) -> Result<CommitOutcome> {
        let committed = self.store().store().write(|w| {
            thread::commit_reply(
                w,
                &CommitReplyInput {
                    task_id: &task.id,
                    lease_token: lease,
                    expected_generation: task.generation,
                    allow_cancelled_stop,
                    outcome,
                    reply_text,
                    existing_reply_id: None,
                    reply_item_id,
                    reply_meta: &reply_meta,
                    error_code,
                    route_kind: &route.kind,
                    model_kind: &route.model_kind,
                    route_guard: route.guard.as_deref(),
                    route_model: route.model.as_deref(),
                    route_confidence: route.confidence,
                    route_signals: &route.signals,
                    route_fallback: route.fallback,
                    route_ms: route.ms,
                    route_outcome,
                    item_id,
                    dispatch,
                    delivery_ref_id: None,
                    model_call_id: None,
                    user_id: task.user_id.as_deref(),
                    project_id: route.project.as_deref().or(task.project_id.as_deref()),
                    answer_model: None,
                    harness: None,
                    harness_session: None,
                    tools: 0,
                    turns: 0,
                    first_text_ms: None,
                    error_detail,
                    record_attention_commit: task.space == super::MAIN && task.user_id.is_some(),
                },
            )
        })?;
        if let CommitOutcome::Committed {
            reply_message_id: Some(ref id),
            ..
        } = committed
        {
            if let ReceiptDispatch::Dispatched(disp_id) = dispatch {
                let _ = self.store().store().write(|w| {
                    thread::set_dispatch_reply_if_origin(w, disp_id, &task.origin_message_id, id)
                });
            }
            if let Some(item) = reply_item_id {
                super::flowback::adopt_quiet(self.store(), item, id);
            }
            self.said(&task.workspace_id, &task.space, id);
        }
        Ok(committed)
    }

    fn commit_stopped(
        &self,
        task: &ThreadTask,
        lease: &str,
        steps: &Mutex<Steps>,
        t0: i64,
        route: &RouteInfo,
    ) -> Result<()> {
        let mut meta = Map::new();
        meta.insert("origin".into(), json!(task.origin_message_id));
        meta.insert("stopped".into(), json!(true));
        meta.insert(SYSTEM.into(), json!(true));
        meta.insert("steps".into(), lock(steps).json(now() - t0));
        self.commit_simple(
            task,
            lease,
            true,
            "stopped",
            Some("好，停下了。要我换个方式就直接说。"),
            None,
            Value::Object(meta),
            route,
            None,
            None,
            ReceiptDispatch::NoDispatch,
            None,
            None,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_task_inner(
        &self,
        task: &ThreadTask,
        lease: &str,
        stop: &Stop,
        conv: &Conv,
        steps: &Mutex<Steps>,
        t0: i64,
    ) -> Result<()> {
        let db = self.store();
        let ws = task.workspace_id.as_str();
        let space = task.space.as_str();
        let mid = task.origin_message_id.as_str();
        let guest = conv.guest();
        let user = task.user_id.as_deref();

        if stop.is_stopped() || task.cancel_requested_at.is_some() {
            return self.commit_stopped(task, lease, steps, t0, &RouteInfo::default());
        }

        let Some(om) = db.message(mid)? else {
            // Origin was deleted; settle cleanly with no reply.
            self.commit_simple(
                task,
                lease,
                true,
                "stopped",
                None,
                None,
                Value::Null,
                &RouteInfo::default(),
                None,
                None,
                ReceiptDispatch::NoDispatch,
                Some("origin_missing"),
                None,
            )?;
            return Ok(());
        };
        let text = om.text.as_str();
        let files: Vec<FileRef> = task
            .files_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| {
                om.meta
                    .get("files")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default()
            });
        let mentions: Vec<Mention> = task
            .mentions_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let to = SayTo {
            item: task.to_item.clone(),
            reply_to: task.to_reply_to.clone(),
        };

        // 0. Forced inline revision (from suggest("here") or reroute(Inline)).
        if let Some(forced) = task.forced_kind.as_deref() {
            let route_json: Option<Value> = task
                .route_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            let tags = route_json
                .as_ref()
                .and_then(|v| v.get("tags"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let info = RouteInfo {
                kind: forced.into(),
                model_kind: forced.into(),
                guard: Some("forced".into()),
                project: task.project_id.clone(),
                tags,
                step_json: route_json,
                ..Default::default()
            };
            return self
                .execute_inline(task, lease, stop, conv, text, &files, info, t0)
                .await;
        }

        // 1. Fixed rule: `/汇报` in MAIN (0 model calls).
        if !guest && text.trim() == "/汇报" {
            let i = Steps::begin(steps, "digest", json!({}));
            let mut d = Steps::end(steps, i, self.digest_text(ws), |_| json!({}))?;
            let notes = self
                .modules()
                .map(|m| m.registry.take_digest(ws))
                .unwrap_or_default();
            for n in notes {
                d.push_str(&format!("\n· {n}"));
            }
            let route = RouteInfo {
                kind: "status".into(),
                model_kind: "status".into(),
                guard: Some("fixed".into()),
                confidence: Some(1.0),
                ..Default::default()
            };
            let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
            self.commit_simple(
                task,
                lease,
                false,
                "answered",
                Some(&d),
                None,
                meta,
                &route,
                None,
                None,
                ReceiptDispatch::NoDispatch,
                None,
                None,
            )?;
            return Ok(());
        }

        // 2. Fixed rule: explicit SayTo (focus bar or quoting an item's message) in MAIN (0 model calls).
        let target = match (&to.item, &to.reply_to) {
            (Some(i), _) => Some(i.clone()),
            (None, Some(r)) => db.message(r)?.and_then(|m| m.item_id),
            _ => None,
        };
        if let Some(id) = target.filter(|_| !guest)
            && db
                .item(&id)?
                .is_some_and(|i| i.workspace_id == ws && !i.is_closed())
        {
            if PROGRESS.is_match(text) {
                let status = self.status_text(&id)?;
                let route = RouteInfo {
                    kind: "status".into(),
                    model_kind: "status".into(),
                    guard: Some("fixed".into()),
                    confidence: Some(1.0),
                    ..Default::default()
                };
                let meta =
                    json!({"origin": mid, "status": id, "steps": lock(steps).json(now() - t0)});
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "answered",
                    Some(&status),
                    None,
                    meta,
                    &route,
                    None,
                    Some(&id),
                    ReceiptDispatch::NoDispatch,
                    None,
                    None,
                )?;
                return Ok(());
            }
            self.phase(ws, "supplement");
            let i = Steps::begin(
                steps,
                "supplement",
                json!({"item": id, "files": files.len()}),
            );
            if let Some(disp_id) = Steps::end(
                steps,
                i,
                self.forward_open(&id, text, &files, mid),
                |disp| json!({"sent": disp.is_some()}),
            )? {
                let route = RouteInfo {
                    kind: "supplement".into(),
                    model_kind: "supplement".into(),
                    guard: Some("fixed".into()),
                    confidence: Some(1.0),
                    ..Default::default()
                };
                let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
                let ack = format!("已作为补充送到 {id}。");
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "handed_off",
                    Some(&ack),
                    Some(&id),
                    meta,
                    &route,
                    None,
                    Some(&id),
                    ReceiptDispatch::Dispatched(&disp_id),
                    None,
                    None,
                )?;
                return Ok(());
            }
        }

        // 3. Explicit `@Bot` mention (Rule 1) -> 0 router calls!
        let named = if !guest || conv.items {
            self.named_bot(ws, text)?
        } else {
            None
        };
        if let Some(bot) = named {
            let stripped = AT.replace(text, "").trim().to_string();
            let goal = if stripped.is_empty() {
                text.to_string()
            } else {
                stripped
            };
            let r = Route {
                kind: "item".into(),
                confidence: 1.0,
                signals: vec!["explicit".into()],
                title: js_head(&goal, 24).to_string(),
                goal,
                why: "显式 @Bot 指定".into(),
                ..Route::default()
            };
            let d = decide::Decision {
                kind: "item".into(),
                guard: Some("1"),
                offer: None,
            };
            let step = route_step(&r, Some(&d), 0);
            lock(steps).route = Some(step.clone());
            let info = RouteInfo {
                kind: "item".into(),
                model_kind: "item".into(),
                guard: Some("1".into()),
                confidence: Some(1.0),
                signals: r.signals.clone(),
                step_json: Some(step),
                ..Default::default()
            };
            return self.execute_item(
                task,
                lease,
                text,
                &files,
                user,
                &mentions,
                &r,
                Some(bot),
                conv.start,
                &info,
                steps,
                t0,
            );
        }

        // 4. Short confirmation of an open offer ("好，改吧") -> 0 router calls!
        if confirms(text)
            && let Some(offer_msg) = self.open_offer_before(ws, mid)?
        {
            let h = self.offer_as(ws, &offer_msg.id, true, conv.start, None, user)?;
            let (outcome, dispatch) = match h.dispatch_ref_id.as_deref() {
                Some(id) => ("handed_off", ReceiptDispatch::Dispatched(id)),
                None => ("answered", ReceiptDispatch::NoDispatch),
            };
            let _ = self.store().store().write(|w| {
                thread::commit_reply(
                    w,
                    &CommitReplyInput {
                        task_id: &task.id,
                        lease_token: lease,
                        expected_generation: task.generation,
                        allow_cancelled_stop: false,
                        outcome,
                        reply_text: None,
                        existing_reply_id: (!h.message_id.is_empty())
                            .then_some(h.message_id.as_str()),
                        reply_item_id: None,
                        reply_meta: &Value::Null,
                        error_code: None,
                        route_kind: "item",
                        model_kind: "item",
                        route_guard: Some("offer"),
                        route_model: None,
                        route_confidence: Some(1.0),
                        route_signals: &[],
                        route_fallback: false,
                        route_ms: 0,
                        route_outcome: Some("offer_taken"),
                        item_id: h.item.as_ref().map(|i| i.id.as_str()),
                        dispatch,
                        delivery_ref_id: None,
                        model_call_id: None,
                        user_id: task.user_id.as_deref(),
                        project_id: task.project_id.as_deref(),
                        answer_model: None,
                        harness: None,
                        harness_session: None,
                        tools: 0,
                        turns: 0,
                        first_text_ms: None,
                        error_detail: None,
                        record_attention_commit: false,
                    },
                )
            })?;
            return Ok(());
        }

        // 5. Explicit item phrase ("帮我建个事项：...") -> 0 router calls!
        if (!guest || conv.items)
            && decide::explicit_item(text)
            && !decide::is_config_phrase(text)
            && !TO_INLINE.is_match(text)
        {
            let cleaned = strip_explicit_prefix(text);
            let r = Route {
                kind: "item".into(),
                confidence: 1.0,
                signals: vec!["explicit".into()],
                title: js_head(&cleaned, 24).to_string(),
                goal: text.to_string(),
                why: "用户明确要求建事项".into(),
                ..Route::default()
            };
            let d = decide::Decision {
                kind: "item".into(),
                guard: Some("2"),
                offer: None,
            };
            let step = route_step(&r, Some(&d), 0);
            lock(steps).route = Some(step.clone());
            let info = RouteInfo {
                kind: "item".into(),
                model_kind: "item".into(),
                guard: Some("2".into()),
                confidence: Some(1.0),
                signals: r.signals.clone(),
                step_json: Some(step),
                ..Default::default()
            };
            return self.execute_item(
                task, lease, text, &files, user, &mentions, &r, None, conv.start, &info, steps, t0,
            );
        }

        // 6. Tool-needing request preflight before any router or tool-less answer call (when
        //    research is off and hands_on + items is on, or when routed to answer below).
        if !conv.research && conv.hands_on && conv.items && decide::needs_machine_tools(text) {
            return self.preflight_hands_on_item(
                task,
                lease,
                conv,
                text,
                &files,
                &RouteInfo {
                    kind: "answer".into(),
                    model_kind: "answer".into(),
                    guard: Some("preflight".into()),
                    confidence: Some(1.0),
                    ..Default::default()
                },
                steps,
                t0,
            );
        }

        // 7. Onboarding meeting, obvious short reply ("谢谢"), or plain question -> 0 router calls!
        let history = self.history_before(ws, mid)?;
        let meeting = !guest
            && space == super::MAIN
            && !decide::names_work(text)
            && super::research::asked_by_owner(db, user)
            && self.meeting(ws)?;

        let deterministic = if meeting {
            Some((
                Route {
                    kind: "research".into(),
                    confidence: 1.0,
                    why: "初次见面，没有问路由".into(),
                    ..Route::fallback(text, None)
                }
                .plain(),
                "meet",
            ))
        } else if decide::obvious_reply(text, !files.is_empty()) {
            Some((
                Route {
                    kind: "answer".into(),
                    confidence: 1.0,
                    why: "简短寒暄或致谢，直接回复".into(),
                    ..Route::fallback(text, None)
                }
                .plain(),
                "obvious",
            ))
        } else if decide::without_router(text, !files.is_empty(), guest, &[]).is_some() {
            let c = self.route_ctx_in(ws, guest)?;
            decide::without_router(text, !files.is_empty(), guest, &c.focus).map(|kind| {
                let project = c.steve_project.clone().or_else(|| {
                    c.projects
                        .iter()
                        .find(|p| !p.archived)
                        .map(|p| p.id.clone())
                });
                (
                    Route {
                        kind: kind.into(),
                        confidence: 1.0,
                        why: "纯问题，没有问路由".into(),
                        ..Route::fallback(text, project)
                    }
                    .plain(),
                    "q",
                )
            })
        } else {
            None
        };

        // 8. Ambiguous message -> single router model call raced against stop.
        let r0 = now();
        let (r, ms, det_guard) = match deterministic {
            Some((r, g)) => (r, now() - r0, Some(g)),
            None => {
                lock(&self.0.thread.running_phase)
                    .insert((ws.to_string(), space.to_string()), "route");
                if !guest {
                    self.phase(ws, "route");
                }
                let routed = super::routed_text(text, &files);
                let maybe = tokio::select! {
                    res = self.route_in(ws, &routed, &history, false, guest) => Some(res?),
                    _ = stop.stopped() => None,
                };
                lock(&self.0.thread.running_phase).remove(&(ws.to_string(), space.to_string()));
                let Some(r) = maybe.filter(|_| !stop.is_stopped()) else {
                    return self.commit_stopped(task, lease, steps, t0, &RouteInfo::default());
                };
                (r, now() - r0, None)
            }
        };

        if stop.is_stopped() {
            return self.commit_stopped(task, lease, steps, t0, &RouteInfo::default());
        }

        let mut d = decide::decide(&r, text, DecideCtx { named_bot: false });
        if guest {
            d = decide::for_guest(d, &r, conv.research, conv.items);
        }
        // Obvious greetings stay a single lightweight answer; owner-only chat questions use research.
        if det_guard != Some("obvious") {
            d = decide::for_owner(d, conv.owner_only && conv.research);
        }
        if !guest
            && d.kind == "supplement"
            && let Some(id) = r.item.as_deref()
            && db.item(id)?.is_some_and(|i| i.is_closed())
        {
            d.kind = "answer".into();
            d.guard = Some("closed");
            if d.offer.is_none() {
                let title = if r.title.trim().is_empty() {
                    js_head(text, 24).to_string()
                } else {
                    r.title.clone()
                };
                let goal = if r.goal.trim().is_empty() {
                    text.to_string()
                } else {
                    r.goal.clone()
                };
                d.offer = Some(crate::gateway::Offer::new(title, goal));
            }
        }
        if let Some(g) = det_guard {
            d.guard = Some(g);
        }
        let step = route_step(&r, Some(&d), ms);
        lock(steps).route = Some(step.clone());
        let mut info = RouteInfo {
            kind: d.kind.clone(),
            model_kind: r.kind.clone(),
            guard: d.guard.map(String::from),
            model: r.model.clone(),
            confidence: (!r.fallback).then_some(r.confidence),
            signals: r.signals.clone(),
            fallback: r.fallback,
            ms,
            project: r.project.clone(),
            tags: r.tag_ids.clone(),
            offer: d.offer.clone(),
            step_json: Some(step),
        };

        match d.kind.as_str() {
            "answer" | "research" => {
                self.execute_inline(task, lease, stop, conv, text, &files, info, t0)
                    .await
            }
            "config" => {
                let request = with_earlier(text, &history);
                self.execute_config(task, lease, &request, user, &info, steps, t0)
                    .await
            }
            "ask" => {
                let q = if r.reply.trim().is_empty() {
                    "你指的是哪个？".to_string()
                } else {
                    r.reply.trim().to_string()
                };
                let meta = json!({
                    "origin": mid,
                    "ask": {"options": r.options},
                    "steps": lock(steps).json(now() - t0),
                });
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "answered",
                    Some(&q),
                    None,
                    meta,
                    &info,
                    None,
                    None,
                    ReceiptDispatch::NoDispatch,
                    None,
                    None,
                )?;
                Ok(())
            }
            "supplement" => {
                let id = r.item.clone().unwrap_or_default();
                if !guest && task.space == super::MAIN {
                    self.phase(ws, "supplement");
                }
                let i = Steps::begin(
                    steps,
                    "supplement",
                    json!({"item": id, "files": files.len()}),
                );
                if let Some(disp_id) = Steps::end(
                    steps,
                    i,
                    self.forward_open(&id, text, &files, mid),
                    |disp| json!({"sent": disp.is_some()}),
                )? {
                    let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
                    let ack = format!("已作为补充送到 {id}。");
                    self.commit_simple(
                        task,
                        lease,
                        false,
                        "handed_off",
                        Some(&ack),
                        Some(&id),
                        meta,
                        &info,
                        None,
                        Some(&id),
                        ReceiptDispatch::Dispatched(&disp_id),
                        None,
                        None,
                    )?;
                } else {
                    // Item closed or unavailable: answer inline with a clickable offer instead of a 2-choice card.
                    info.kind = "answer".into();
                    info.guard = Some("closed".into());
                    if let Some(step) = info.step_json.as_mut().and_then(Value::as_object_mut) {
                        step.insert("kind".into(), json!("answer"));
                        step.insert("guard".into(), json!("closed"));
                    }
                    if info.offer.is_none() {
                        let title = if r.title.trim().is_empty() {
                            js_head(text, 24).to_string()
                        } else {
                            r.title.clone()
                        };
                        let goal = if r.goal.trim().is_empty() {
                            text.to_string()
                        } else {
                            r.goal.clone()
                        };
                        info.offer = Some(crate::gateway::Offer::new(title, goal));
                    }
                    return self
                        .execute_inline(task, lease, stop, conv, text, &files, info, t0)
                        .await;
                }
                Ok(())
            }
            "status" => {
                let id = r.item.clone().unwrap_or_default();
                let st = self.status_text(&id)?;
                let meta =
                    json!({"origin": mid, "status": id, "steps": lock(steps).json(now() - t0)});
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "answered",
                    Some(&st),
                    None,
                    meta,
                    &info,
                    None,
                    Some(&id),
                    ReceiptDispatch::NoDispatch,
                    None,
                    None,
                )?;
                Ok(())
            }
            "reroute" => {
                self.execute_spoken_reroute(task, lease, text, &r, user, &info, steps, t0)
                    .await
            }
            _ => self.execute_item(
                task, lease, text, &files, user, &mentions, &r, None, conv.start, &info, steps, t0,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn preflight_hands_on_item(
        &self,
        task: &ThreadTask,
        lease: &str,
        conv: &Conv,
        text: &str,
        files: &[FileRef],
        info: &RouteInfo,
        steps: &Mutex<Steps>,
        t0: i64,
    ) -> Result<()> {
        let ws = &task.workspace_id;
        let mid = &task.origin_message_id;
        let _one = lock(&self.0.thread.acting);
        match self.thread_item_once(ThreadItem {
            ws,
            origin: mid,
            title: js_head(text, 24).to_string(),
            goal: text.to_string(),
            bot: None,
            project: info.project.clone(),
            tags: vec![],
            files: files.to_vec(),
            from_question: decide::pure_question(text),
            answer: None,
            start: conv.start,
            by: task.user_id.clone(),
        }) {
            Ok((item, already, disp_id)) => {
                if already {
                    self.commit_simple(
                        task,
                        lease,
                        false,
                        "handed_off",
                        None,
                        None,
                        Value::Null,
                        info,
                        Some(handoff::NEEDS_TOOLS_OUTCOME),
                        Some(&item.id),
                        ReceiptDispatch::Dispatched(&disp_id),
                        None,
                        None,
                    )?;
                    return Ok(());
                }
                metrics::counter!("steve_gateway_handoffs_total").increment(1);
                let reply_text = Self::unplaced_reply(&item).unwrap_or_else(|| {
                    handoff::handed_off(
                        &item.id,
                        &self.bot_name(&item),
                        item.phase == crate::cycle::READY,
                    )
                });
                let meta = json!({
                    "origin": mid,
                    "steps": lock(steps).json(now() - t0),
                    "handoff": {"why": handoff::NEEDS_TOOLS_OUTCOME, "item": item.id},
                });
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "handed_off",
                    Some(&reply_text),
                    Some(&item.id),
                    meta,
                    info,
                    Some(handoff::NEEDS_TOOLS_OUTCOME),
                    Some(&item.id),
                    ReceiptDispatch::Dispatched(&disp_id),
                    None,
                    None,
                )?;
                Ok(())
            }
            Err(e) => {
                tracing::warn!(origin = mid, "preflight hand-off failed: {e}");
                let reply_text = handoff::not_handed_off(&e.to_string());
                let meta = json!({
                    "origin": mid,
                    "error": true,
                    SYSTEM: true,
                    "steps": lock(steps).json(now() - t0),
                });
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "failed",
                    Some(&reply_text),
                    None,
                    meta,
                    info,
                    None,
                    None,
                    ReceiptDispatch::NoDispatch,
                    Some("handoff_failed"),
                    Some(&e.to_string()),
                )?;
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_config(
        &self,
        task: &ThreadTask,
        lease: &str,
        text: &str,
        user: Option<&str>,
        info: &RouteInfo,
        steps: &Mutex<Steps>,
        t0: i64,
    ) -> Result<()> {
        let ws = &task.workspace_id;
        if task.space == super::MAIN {
            self.phase(ws, "config");
        }
        let i = Steps::begin(
            steps,
            "draft_config",
            json!({"request": js_head(text, 200)}),
        );
        let res = self.config_request(ws, text, user).await;
        let (ask, ds) = Steps::end(steps, i, res, |(_, ds)| {
            json!({
                "targets": ds.iter().map(|d| d.title.clone()).collect::<Vec<_>>(),
                "changes": ds
                    .iter()
                    .filter_map(|d| d.proposal.as_ref())
                    .map(|p| p["changes"].as_array().map_or(0, Vec::len))
                    .sum::<usize>(),
            })
        })?;
        let applied = if ask {
            vec![]
        } else {
            self.config_auto_apply(ws, user, &ds).await
        };
        let (reply_text, extra_meta) = config_reply(ask, &ds, &applied);
        let mut meta = match extra_meta {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        meta.insert("origin".into(), json!(task.origin_message_id));
        meta.insert("steps".into(), lock(steps).json(now() - t0));
        self.commit_simple(
            task,
            lease,
            false,
            "answered",
            Some(&reply_text),
            None,
            Value::Object(meta),
            info,
            None,
            None,
            ReceiptDispatch::NoDispatch,
            None,
            None,
        )?;
        Ok(())
    }

    fn status_text(&self, id: &str) -> Result<String> {
        let Some(i) = self.store().item(id)? else {
            return Ok(format!("没找到 {id}。"));
        };
        let mut s = format!(
            "{}「{}」：{}。",
            i.id,
            i.title,
            i.outcome.as_deref().unwrap_or_else(|| i.phase.as_str())
        );
        if !i.plan.is_empty() {
            let done = i.plan.iter().filter(|p| p.done).count();
            s.push_str(&format!("计划完成 {done}/{} 步。", i.plan.len()));
        }
        if let Some(d) = self
            .store()
            .decisions_of_item(id)?
            .into_iter()
            .rfind(|d| d.kind == "accept" && d.status == steve_types::DecisionStatus::Open)
        {
            let first = d.draft.split("\n\n").next().unwrap_or("");
            s.push_str(&format!("\n\n已经交付，等你验收：{}", js_head(first, 600)));
        }
        Ok(s)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_spoken_reroute(
        &self,
        task: &ThreadTask,
        lease: &str,
        text: &str,
        r: &Route,
        user: Option<&str>,
        info: &RouteInfo,
        steps: &Mutex<Steps>,
        t0: i64,
    ) -> Result<()> {
        let ws = &task.workspace_id;
        let mid = &task.origin_message_id;
        let msgs = self.store().messages(ws)?;
        if TO_INLINE.is_match(text) {
            let dispatches = self
                .store()
                .store()
                .read(|rd| thread::dispatches_of_workspace(rd, ws))?;
            let target = dispatches.into_iter().find(|t| {
                let Some(ref iid) = t.target_item_id else {
                    return false;
                };
                r.item.as_deref().is_none_or(|i| i == iid)
                    && now() - t.created <= REROUTE_WINDOW
                    && self
                        .store()
                        .item(iid)
                        .ok()
                        .flatten()
                        .is_some_and(|i| !i.is_closed())
            });
            if let Some(t) = target
                && let Some(item_id) = t.target_item_id.clone()
                && let Some(rm) = t.reply_message_id.clone()
            {
                let h = self
                    .reroute(ws, &rm, super::RerouteTo::Inline, user)
                    .await?;
                let meta = json!({
                    "origin": mid,
                    "reply_to": h.message_id,
                    "steps": lock(steps).json(now() - t0),
                });
                let reply_text = format!("好，{item_id} 不做了，改在这里回答。");
                self.commit_simple(
                    task,
                    lease,
                    false,
                    "answered",
                    Some(&reply_text),
                    None,
                    meta,
                    info,
                    None,
                    Some(&item_id),
                    ReceiptDispatch::NoDispatch,
                    None,
                    None,
                )?;
                return Ok(());
            }
        } else {
            let target = msgs
                .iter()
                .rev()
                .filter(|m| m.id != *mid)
                .take(10)
                .find(|m| {
                    m.author == "steve" && m.meta.get("chat").is_some_and(crate::gateway::truthy)
                });
            if let Some(m) = target.cloned() {
                let h = self
                    .reroute(ws, &m.id, super::RerouteTo::Item, user)
                    .await?;
                let item = h.item.map(|i| i.id);
                let reply_text = item.as_ref().map(|i| format!("好，已经做成 {i}。"));
                let (outcome, dispatch) = match h.dispatch_ref_id.as_deref() {
                    Some(id) => ("handed_off", ReceiptDispatch::Dispatched(id)),
                    None => ("answered", ReceiptDispatch::NoDispatch),
                };
                let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
                self.commit_simple(
                    task,
                    lease,
                    false,
                    outcome,
                    reply_text.as_deref(),
                    item.as_deref(),
                    meta,
                    info,
                    None,
                    item.as_deref(),
                    dispatch,
                    None,
                    None,
                )?;
                return Ok(());
            }
        }
        let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
        self.commit_simple(
            task,
            lease,
            false,
            "answered",
            Some("是哪一条？我没找到最近能改的那条。"),
            None,
            meta,
            info,
            None,
            None,
            ReceiptDispatch::NoDispatch,
            None,
            None,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_item(
        &self,
        task: &ThreadTask,
        lease: &str,
        text: &str,
        files: &[FileRef],
        user: Option<&str>,
        mentions: &[Mention],
        r: &Route,
        named: Option<crate::repo::Bot>,
        start: bool,
        info: &RouteInfo,
        steps: &Mutex<Steps>,
        t0: i64,
    ) -> Result<()> {
        let db = self.store();
        let ws = &task.workspace_id;
        let mid = &task.origin_message_id;
        if let (Some(b), Some(rs)) = (&named, lock(steps).route.as_mut()) {
            rs["picked"] = json!(b.id);
        }
        let goal = if named.is_some() {
            AT.replace(text, "").to_string()
        } else if !r.goal.trim().is_empty() && r.goal.trim() != text.trim() {
            format!("{}\n\n（用户原话：{text}）", r.goal.trim())
        } else {
            text.to_string()
        };
        let mut made = vec![];
        for t in &r.new_tags {
            made.push(db.tag_create(ws, &t.name, &t.cat, &t.description, "Steve")?);
        }
        let mut tag_ids: Vec<String> = vec![];
        for id in r.tag_ids.iter().chain(made.iter().map(|t| &t.id)) {
            if !tag_ids.contains(id) {
                tag_ids.push(id.clone());
            }
        }
        let bot = named.as_ref().map(|b| b.id.clone());
        if task.space == super::MAIN {
            self.phase(ws, "item");
        }
        let i = Steps::begin(
            steps,
            "create_item",
            json!({"title": r.title, "bot": bot, "project": r.project, "tags": tag_ids, "files": files.len()}),
        );
        let _one = lock(&self.0.thread.acting);
        let res = self.thread_item_once(ThreadItem {
            ws,
            origin: mid,
            title: r.title.clone(),
            goal,
            bot,
            project: r.project.clone(),
            tags: tag_ids.clone(),
            files: files.to_vec(),
            from_question: r.signals.iter().any(|s| s == "question"),
            answer: None,
            start,
            by: user.map(String::from),
        });
        let (item, already, disp_id) = Steps::end(
            steps,
            i,
            res,
            |(i, _, _)| json!({"item": i.id, "bot": i.bot_id, "project": i.project_id}),
        )?;
        if already {
            self.commit_simple(
                task,
                lease,
                false,
                "handed_off",
                None,
                None,
                Value::Null,
                info,
                None,
                Some(&item.id),
                ReceiptDispatch::Dispatched(&disp_id),
                None,
                None,
            )?;
            return Ok(());
        }
        let tag_ids = db.tags_of_item(&item.id)?;
        let tag_text = if tag_ids.is_empty() {
            String::new()
        } else {
            let mut names = vec![];
            for id in &tag_ids {
                let name = db.tag(id)?.map(|t| t.name).unwrap_or_default();
                names.push(format!(
                    "{name}{}",
                    if made.iter().any(|m| &m.id == id) {
                        "（新建）"
                    } else {
                        ""
                    }
                ));
            }
            format!(" 标签：{}。", names.join("、"))
        };
        let head = Self::unplaced_reply(&item).unwrap_or_else(|| match &named {
            Some(b) => format!("按你指定的，交给 {} Bot 了。", b.name),
            None => format!("好，交给 {} 了，做完请你验收。", self.bot_name(&item)),
        });
        let people = match self.item_people(ws, user, &item.id, text, mentions) {
            Some(n) => format!(" {n}。"),
            None => String::new(),
        };
        let reply_text = format!("{head}{tag_text}{people}");
        let meta = json!({"origin": mid, "steps": lock(steps).json(now() - t0)});
        self.commit_simple(
            task,
            lease,
            false,
            "handed_off",
            Some(&reply_text),
            Some(&item.id),
            meta,
            info,
            None,
            Some(&item.id),
            ReceiptDispatch::Dispatched(&disp_id),
            None,
            None,
        )?;
        Ok(())
    }
}

fn strip_explicit_prefix(text: &str) -> String {
    let t = text.trim();
    for sep in ['：', ':'] {
        if let Some((_, rest)) = t.split_once(sep)
            && !rest.trim().is_empty()
        {
            return rest.trim().to_string();
        }
    }
    t.to_string()
}

fn with_earlier(text: &str, history: &[Said]) -> String {
    match history.iter().rev().find(|m| m.author != "me") {
        Some(m) if !m.text.trim().is_empty() => format!(
            "{text}\n\n（Steve 上一条回复如下。只有用户这句话沿用它时（如“按这个方案”“就按你说的改”）才照它改，否则忽略：\n{}）",
            js_head(m.text.trim(), 6000)
        ),
        _ => text.to_string(),
    }
}

fn listed(d: &crate::changes::Drafted) -> String {
    let mut lines: Vec<String> = d
        .current
        .iter()
        .take(20)
        .map(|(k, v)| format!("- {k}：{v}"))
        .collect();
    if d.current.len() > 20 {
        lines.push(format!("- ……还有 {} 项", d.current.len() - 20));
    }
    lines.join("\n")
}

fn config_reply(
    ask: bool,
    ds: &[crate::changes::Drafted],
    applied: &[String],
) -> (String, Option<Value>) {
    if ask {
        let parts: Vec<String> = ds
            .iter()
            .map(|d| format!("「{}」现在是：\n{}", d.title, listed(d)))
            .collect();
        return (
            format!(
                "{}\n\n要改的话直接说改成什么，我起草好给你确认。",
                parts.join("\n\n")
            ),
            None,
        );
    }
    let drafted: Vec<&Value> = ds.iter().filter_map(|d| d.proposal.as_ref()).collect();
    let unchanged: Vec<String> = ds
        .iter()
        .filter(|d| d.proposal.is_none())
        .map(|d| {
            let now = listed(d);
            if now.is_empty() {
                format!("我没看出要改「{}」的哪一项，换个说法试试？", d.title)
            } else {
                format!(
                    "我没看出要改「{}」的哪一项，换个说法试试？现在是：\n{now}",
                    d.title
                )
            }
        })
        .collect();
    let is_applied = |p: &Value| {
        p["id"]
            .as_str()
            .is_some_and(|id| applied.iter().any(|a| a == id))
    };
    let titled = |pred: &dyn Fn(&Value) -> bool| -> Vec<&str> {
        ds.iter()
            .filter(|d| d.proposal.as_ref().is_some_and(pred))
            .map(|d| d.title.as_str())
            .collect()
    };
    let (done, todo) = (titled(&is_applied), titled(&|p| !is_applied(p)));
    let done_text = match done.as_slice() {
        [] => String::new(),
        [t] => format!("已经改好了「{t}」，不对的话在卡片上撤销："),
        many => format!(
            "已经改好了 {} 处（{}），不对的话在卡片上撤销：",
            many.len(),
            many.iter()
                .map(|t| format!("「{t}」"))
                .collect::<Vec<_>>()
                .join("")
        ),
    };
    let todo_text = match todo.as_slice() {
        [] => String::new(),
        [t] if done.is_empty() => format!("这是配置改动，我起草好了，确认后应用到「{t}」："),
        [t] => format!("「{t}」的改动起草好了，确认后应用："),
        many => format!(
            "这是配置改动，我起草好了 {} 处，确认后分别应用：",
            many.len()
        ),
    };
    let head = [done_text, todo_text]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let text = std::iter::once(head)
        .chain(unchanged)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut meta = match drafted.as_slice() {
        [] => None,
        [p] => Some(json!({"proposal": p})),
        many => Some(json!({"proposals": many})),
    };
    if let Some(m) = meta.as_mut().filter(|_| !applied.is_empty()) {
        m["cfg_state"] = Value::Object(
            applied
                .iter()
                .map(|id| (id.clone(), json!("applied")))
                .collect(),
        );
    }
    (text, meta)
}
