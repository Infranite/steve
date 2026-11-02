//! The `action` node (signals-automation.md §3.2): a registry of [`Action`]s. Built in: `item.create`,
//! `item.link`, `notify`, `digest`, `ignore` and `report` (schedules); the channel module registers
//! `reply_as` and `send_to_space` (effectful, so they run only after an approval or with a grant).
//!
//! Schedules keep their own wording (schedules.md §5): their `item.create` and `report` go through
//! [`run`], as before.

use serde_json::{Value, json};
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};

use crate::exec::REPORT_TIMEOUT;
use crate::ports::ItemRequest;
use crate::runs::{self, RunRow};
use crate::schedules::{self, Schedule};
use crate::{FlowCx, FlowResult, digest};
use steve_modules::Tier;

/// What one action did.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionOutcome {
    Done {
        output: Value,
        /// `flow_runs.outcome`.
        outcome: &'static str,
        /// `flow_runs.outcome_label`, before the catch-up prefix.
        label: String,
        item_id: Option<String>,
        cost: f64,
    },
    /// Worth one more try later (the model gateway failed).
    Retry {
        error: String,
        cost: f64,
    },
    Fail {
        error: String,
        cost: f64,
    },
}

fn fail(error: impl Into<String>) -> ActionOutcome {
    ActionOutcome::Fail {
        error: error.into(),
        cost: 0.0,
    }
}

/// Runs the action `cfg` for `run`.
pub async fn run(cx: &FlowCx, run: &RunRow, cfg: &Value) -> ActionOutcome {
    let res = match cfg["action"].as_str().unwrap_or("") {
        "item.create" => item_create(cx, run, cfg).await,
        "report" => report(cx, run, cfg).await,
        "digest" => digest_action(cx, run, cfg),
        other => Ok(fail(format!("不认识的动作：{other}"))),
    };
    res.unwrap_or_else(|e| {
        tracing::error!(run = %run.id, "action failed: {e}");
        fail(e.to_string())
    })
}

fn schedule_of(cx: &FlowCx, run: &RunRow) -> FlowResult<Option<Schedule>> {
    cx.store().reader(|c| schedules::get_any(c, &run.flow_id))
}

/// The bot to hand the item to: the configured one if it is usable, else the main bot with a note
/// for the label (“（<Bot> 不可用，交给了 Steve）”).
fn pick_bot(
    c: &rusqlite::Connection,
    ws: &str,
    want: &str,
) -> FlowResult<(String, Option<String>)> {
    let unavailable = match schedules::bot_of(c, ws, want)? {
        Some((_, true)) => return Ok((want.to_string(), None)),
        Some((name, false)) => name,
        None => want.to_string(),
    };
    let main: Option<String> = rusqlite::OptionalExtension::optional(c.query_row(
        "SELECT id FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1",
        [],
        |r| r.get(0),
    ))?;
    let main = main.ok_or_else(|| steve_modules::ApiError::rule("没有主 Bot"))?;
    Ok((
        main,
        Some(format!("（{unavailable} 不可用，交给了 Steve）")),
    ))
}

async fn item_create(cx: &FlowCx, run: &RunRow, cfg: &Value) -> FlowResult<ActionOutcome> {
    let Some(s) = schedule_of(cx, run)? else {
        return Ok(fail("建事项的动作目前只用于定时任务"));
    };
    let tz = s.spec.zone();
    let at = run.at();
    let ws = run.workspace_id.clone();
    let (bot, note, previous) = cx.store().reader(|c| -> FlowResult<_> {
        let (bot, note) = pick_bot(c, &ws, cfg["bot"].as_str().unwrap_or(&s.spec.bot))?;
        let prev = runs::last_real(c, &run.flow_id, Some(&run.id))?;
        Ok((bot, note, prev))
    })?;
    let mut planned = format!("计划时间 {}", schedules::local(at, tz, "%Y-%m-%d %H:%M"));
    if run.trigger_kind == "catch_up" {
        let n = run.context["missed"].as_u64().unwrap_or(1);
        planned.push_str(&format!("，补跑，错过 {n} 次"));
    }
    let last = previous
        .map(|p| format!("。上次结果：{}", p.outcome_label))
        .unwrap_or_default();
    let what = cfg["goal"].as_str().unwrap_or(&s.spec.what);
    let propose = cfg["mode"] == "propose";
    let req = ItemRequest {
        ws: ws.clone(),
        title: format!("{} · {}", s.name, schedules::local(at, tz, "%m-%d")),
        goal: format!("{what}\n\n（定时任务「{}」，{planned}{last}）", s.name),
        bot,
        project: cfg["project"].as_str().map(String::from),
        tag_ids: cfg["tags"]
            .as_array()
            .map(|t| {
                t.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        source: ("定时".into(), "sched".into()),
        propose,
        accept_auto: cfg["accept"] == "auto",
        run_id: run.id.clone(),
    };
    let item_id = match cx.core.create_item(req).await {
        Ok(id) => id,
        Err(e) => return Ok(fail(format!("建事项失败：{e}"))),
    };
    cx.store().transaction(|tx| {
        links::add(
            tx,
            &ws,
            &NewLink::new(
                ObjRef::new("item", &item_id),
                "from",
                ObjRef::new("flow_run", &run.id),
                "sched",
            ),
        )
    })?;
    let mut label = if propose {
        format!("提了方案 {item_id}")
    } else {
        format!("建立 {item_id}")
    };
    if let Some(n) = note {
        label.push_str(&n);
    }
    Ok(ActionOutcome::Done {
        output: json!({ "item_id": item_id }),
        outcome: "item",
        label,
        item_id: Some(item_id),
        cost: 0.0,
    })
}

/// The report prompt: what to report, the `/汇报` text, digest entries, changed items.
pub fn report_prompt(what: &str, digest_text: &str, notes: &[String], items: &[String]) -> String {
    let list = |xs: &[String]| {
        if xs.is_empty() {
            "（没有）".to_string()
        } else {
            xs.iter()
                .map(|x| format!("- {x}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    format!(
        "你是 Steve，用户的个人助理。按下面的要求，给用户写一段中文汇报，直接输出汇报正文，不要客套，不要编造没有给出的事。\n\
         要求：{what}\n\
         现在的概况：{digest_text}\n\
         自上次汇报以来记下的事：\n{}\n\
         自上次汇报以来有变化的事项（编号 · 标题 · 现在）：\n{}",
        list(notes),
        list(items)
    )
}

async fn report(cx: &FlowCx, run: &RunRow, cfg: &Value) -> FlowResult<ActionOutcome> {
    let ws = run.workspace_id.clone();
    let what = cfg["what"].as_str().unwrap_or("").to_string();
    let (entries, items) = cx.store().reader(|c| -> FlowResult<_> {
        let since = runs::last_real(c, &run.flow_id, Some(&run.id))?
            .map(|p| p.started)
            .unwrap_or(0);
        let entries = digest::pending(c, &ws, since)?;
        let mut st = c.prepare_cached(
            "SELECT id, title, now FROM items WHERE workspace_id = ? AND updated > ? AND archived = 0 ORDER BY updated DESC LIMIT 30",
        )?;
        let items = st
            .query_map(rusqlite::params![ws, since], |r| {
                Ok(format!(
                    "{} · {} · {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((entries, items))
    })?;
    let digest_text = cx.core.digest_text(&ws).await.unwrap_or_else(|e| {
        tracing::warn!("digest_text: {e}");
        String::new()
    });
    let notes: Vec<String> = entries.iter().map(|e| e.text.clone()).collect();
    let prompt = report_prompt(&what, &digest_text, &notes, &items);
    let out = match steve_modules::gateway::tagged(
        "flow",
        &ws,
        cx.modules.gateway.run(Tier::Smart, &prompt, REPORT_TIMEOUT),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            return Ok(ActionOutcome::Retry {
                error: format!("写汇报失败：{e}"),
                cost: 0.0,
            });
        }
    };
    let cost = out.cost_usd.unwrap_or(0.0);
    let text = out.text.trim().to_string();
    if text.is_empty() {
        return Ok(ActionOutcome::Retry {
            error: "写汇报失败：模型没有给出内容".into(),
            cost,
        });
    }
    let meta = json!({ "schedule": run.flow_id, "run": run.id });
    if let Err(e) = cx.core.post_steve_message(&ws, &text, meta).await {
        return Ok(ActionOutcome::Retry {
            error: format!("汇报没发出去：{e}"),
            cost,
        });
    }
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let now = cx.now();
    cx.store()
        .transaction(|tx| digest::mark_taken(tx, &ids, now))?;
    Ok(ActionOutcome::Done {
        output: json!({ "text": text, "digest": ids.len() }),
        outcome: "report",
        label: "汇报已发出".into(),
        item_id: None,
        cost,
    })
}

fn digest_action(cx: &FlowCx, run: &RunRow, cfg: &Value) -> FlowResult<ActionOutcome> {
    let text = cfg["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Ok(fail("没有要记下的内容"));
    }
    let now = cx.now();
    let id = cx
        .store()
        .transaction(|tx| digest::add(tx, &run.workspace_id, &text, None, Some(&run.id), now))?;
    Ok(ActionOutcome::Done {
        output: json!({ "digest_id": id }),
        outcome: "digest",
        label: "记进了汇报".into(),
        item_id: None,
        cost: 0.0,
    })
}

// ---------- the action registry ----------

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use steve_store::NewDecision;

use super::{
    DRY_EFFECT, DRY_WRITE, Grant, NodeKind, NodeOutcome, RunCx, opt_str, req_str, skipped,
};
use crate::signals::source_label;

/// One action of the `action` node.
#[async_trait]
pub trait Action: Send + Sync {
    fn name(&self) -> &'static str;

    /// Reaches outside Steve (sends a message, pays): gated like any effectful node.
    fn effectful(&self, cfg: &Value) -> bool {
        let _ = cfg;
        false
    }

    fn grant(&self, cfg: &Value) -> Option<Grant> {
        let _ = cfg;
        None
    }

    fn validate(&self, cfg: &Value) -> Result<(), String>;

    /// The event outcome it leads to (for dry runs, which do not run it).
    fn outcome(&self) -> Option<&'static str> {
        None
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome;
}

/// The registered actions.
#[derive(Clone)]
pub struct Actions(HashMap<String, Arc<dyn Action>>);

impl Default for Actions {
    fn default() -> Self {
        let mut a = Self(HashMap::new());
        a.add(Arc::new(ItemCreate));
        a.add(Arc::new(ItemLink));
        a.add(Arc::new(Notify));
        a.add(Arc::new(Digest));
        a.add(Arc::new(Ignore));
        a.add(Arc::new(Report));
        a
    }
}

impl Actions {
    pub fn add(&mut self, a: Arc<dyn Action>) {
        self.0.insert(a.name().to_string(), a);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Action>> {
        self.0.get(name)
    }
}

/// The `action` node.
pub struct ActionNode {
    actions: Actions,
}

impl ActionNode {
    pub fn new(actions: Actions) -> Self {
        Self { actions }
    }

    fn of(&self, cfg: &Value) -> Option<&Arc<dyn Action>> {
        self.actions.get(cfg["action"].as_str().unwrap_or(""))
    }
}

#[async_trait]
impl NodeKind for ActionNode {
    fn kind(&self) -> &'static str {
        "action"
    }

    fn effectful(&self, cfg: &Value) -> bool {
        // an action nobody registered is treated as effectful: never run unasked
        self.of(cfg).is_none_or(|a| a.effectful(cfg))
    }

    fn grant(&self, cfg: &Value) -> Option<Grant> {
        self.of(cfg).and_then(|a| a.grant(cfg))
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let name = opt_str(cfg, "action")?.unwrap_or("");
        match self.actions.get(name) {
            Some(a) => a.validate(cfg),
            None if name.is_empty() => Err("要选一个动作".into()),
            None => Err(format!("不认识的动作：{name}")),
        }
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        match self.of(cfg) {
            Some(a) => a.run(cx, cfg, input).await,
            None => NodeOutcome::Fail(format!(
                "不认识的动作：{}",
                cfg["action"].as_str().unwrap_or("?")
            )),
        }
    }

    /// Dry runs write nothing: the action is described, its outcome noted.
    async fn dry_run(&self, cx: &mut RunCx<'_>, cfg: &Value, _input: &Value) -> NodeOutcome {
        let a = self.of(cfg);
        let effect = a.is_none_or(|a| a.effectful(cfg));
        if let Some(o) = a.and_then(|a| a.outcome()) {
            cx.effects.outcome = Some(o.into());
        }
        let mut out = skipped(if effect { DRY_EFFECT } else { DRY_WRITE });
        out["action"] = cfg["action"].clone();
        NodeOutcome::next(out)
    }
}

/// An [`ActionOutcome`] of the schedule actions as a node outcome.
fn from_schedule(cx: &mut RunCx<'_>, o: ActionOutcome) -> NodeOutcome {
    match o {
        ActionOutcome::Done {
            output,
            outcome,
            label,
            item_id,
            cost,
        } => {
            cx.effects.cost += cost;
            cx.effects.outcome = Some(outcome.into());
            cx.effects.label = Some(label);
            if item_id.is_some() {
                cx.effects.item_id = item_id;
            }
            NodeOutcome::next(output)
        }
        ActionOutcome::Retry { error, cost } => {
            cx.effects.cost += cost;
            NodeOutcome::Retry(error)
        }
        ActionOutcome::Fail { error, cost } => {
            cx.effects.cost += cost;
            NodeOutcome::Fail(error)
        }
    }
}

fn is_schedule(cx: &RunCx<'_>) -> bool {
    schedule_of(cx.flow, &cx.run).ok().flatten().is_some()
}

fn check_mode(cfg: &Value) -> Result<(), String> {
    match opt_str(cfg, "mode")?.unwrap_or("start") {
        "start" | "propose" => {}
        _ => return Err("mode 只能是 start 或 propose".into()),
    }
    match opt_str(cfg, "accept")?.unwrap_or("inherit") {
        "auto" | "inherit" => {}
        _ => return Err("accept 只能是 auto 或 inherit".into()),
    }
    for k in ["bot", "project", "title", "goal"] {
        opt_str(cfg, k)?;
    }
    super::str_list(cfg, "tags")?;
    for k in ["title", "goal"] {
        if let Some(t) = cfg[k].as_str() {
            crate::expr::check_template(t)?;
        }
    }
    Ok(())
}

fn cut(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// The original text of an event for an item's goal (≤ 4000 characters).
fn event_text(ev: &Value) -> String {
    let p = &ev["payload"];
    let t = p["text"]
        .as_str()
        .map(String::from)
        .or_else(|| (!p.is_null()).then(|| p.to_string()))
        .unwrap_or_default();
    cut(&t, 4000)
}

/// The bot an event item goes to: configured, else the source's “先交给谁判断”, else the main bot.
fn event_bot(
    c: &rusqlite::Connection,
    ws: &str,
    cfg_bot: Option<&str>,
    ev: &Value,
) -> FlowResult<(String, Option<String>)> {
    let triage = match ev["connection_id"].as_str() {
        Some(conn) => crate::sources::settings(c, ws, conn)?.triage_bot,
        None => None,
    };
    match cfg_bot
        .filter(|b| !b.is_empty())
        .map(String::from)
        .or(triage)
    {
        Some(b) => pick_bot(c, ws, &b),
        None => pick_bot(c, ws, "").map(|(b, _)| (b, None)),
    }
}

/// `item.create` for event and manual flows: the event's summary and text, `source:["<来源>","sig"]`,
/// links `from` the event and the run and `raised_by` the sender.
pub async fn item_create_event(cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
    let ev = cx.event().clone();
    let ws = cx.ws().to_string();
    let bot_cfg = cfg["bot"].as_str().map(|b| cx.render(b, input));
    let picked = cx
        .flow
        .store()
        .reader(|c| event_bot(c, &ws, bot_cfg.as_deref(), &ev));
    let (bot, note) = match picked {
        Ok(b) => b,
        Err(e) => return NodeOutcome::Fail(e.to_string()),
    };
    let summary = ev["summary"].as_str().unwrap_or("").to_string();
    let title = cfg["title"]
        .as_str()
        .map(|t| cx.render(t, input))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| summary.clone());
    let title = if title.trim().is_empty() {
        cx.flow
            .store()
            .reader(|c| crate::flows::name_of(c, &cx.run.flow_id))
            .unwrap_or_default()
    } else {
        cut(&title, 80)
    };
    let goal = cfg["goal"]
        .as_str()
        .map(|g| cx.render(g, input))
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| {
            let text = event_text(&ev);
            if text.is_empty() {
                summary.clone()
            } else {
                format!("{summary}\n\n{text}")
            }
        });
    let src = ev["source"].as_str().unwrap_or("manual");
    let propose = cfg["mode"] == "propose";
    let req = ItemRequest {
        ws: ws.clone(),
        title,
        goal,
        bot,
        project: cfg["project"].as_str().map(String::from),
        tag_ids: super::str_list(cfg, "tags").unwrap_or_default(),
        source: (source_label(src).to_string(), "sig".into()),
        propose,
        accept_auto: cfg["accept"] == "auto",
        run_id: cx.run.id.clone(),
    };
    let item_id = match cx.flow.core.create_item(req).await {
        Ok(id) => id,
        Err(e) => return NodeOutcome::Fail(format!("建事项失败：{e}")),
    };
    let evt = ev["id"].as_str().map(String::from);
    let party = ev["actor_party_id"].as_str().map(String::from);
    let run_id = cx.run.id.clone();
    let linked = cx.flow.store().transaction(|tx| -> FlowResult<()> {
        let item = || ObjRef::new("item", &item_id);
        links::add(
            tx,
            &ws,
            &NewLink::new(item(), "from", ObjRef::new("flow_run", &run_id), "sig"),
        )?;
        if let Some(e) = &evt {
            links::add(
                tx,
                &ws,
                &NewLink::new(item(), "from", ObjRef::new("event", e), "sig"),
            )?;
        }
        if let Some(p) = &party {
            links::add(
                tx,
                &ws,
                &NewLink::new(item(), "raised_by", ObjRef::new("party", p), "sig"),
            )?;
        }
        Ok(())
    });
    if let Err(e) = linked {
        tracing::warn!(item = %item_id, "item links: {e}");
    }
    let mut label = if propose {
        format!("提了方案 {item_id}")
    } else {
        format!("建了事项 {item_id}")
    };
    if let Some(n) = note {
        label.push_str(&n);
    }
    cx.effects.outcome = Some("item".into());
    cx.effects.label = Some(label);
    cx.effects.item_id = Some(item_id.clone());
    NodeOutcome::next(json!({ "item_id": item_id }))
}

pub struct ItemCreate;

#[async_trait]
impl Action for ItemCreate {
    fn name(&self) -> &'static str {
        "item.create"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        check_mode(cfg)
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("item")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if is_schedule(cx) {
            let o = item_create(cx.flow, &cx.run, cfg)
                .await
                .unwrap_or_else(|e| fail(e.to_string()));
            return from_schedule(cx, o);
        }
        item_create_event(cx, cfg, input).await
    }
}

/// `item.link {by, fallback}`.
pub struct ItemLink;

#[async_trait]
impl Action for ItemLink {
    fn name(&self) -> &'static str {
        "item.link"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let by = opt_str(cfg, "by")?.unwrap_or("");
        if !crate::link::METHODS.contains(&by) {
            return Err("by 只能是 branch、ref 或 thread".into());
        }
        match opt_str(cfg, "fallback")?.unwrap_or("none") {
            "create" => check_mode(cfg),
            "none" => Ok(()),
            _ => Err("fallback 只能是 create 或 none".into()),
        }
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("linked")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let ev = cx.event().clone();
        let ws = cx.ws().to_string();
        let by = cfg["by"].as_str().unwrap_or("");
        let found = cx
            .flow
            .store()
            .reader(|c| cx.flow.finders.find(c, &ws, by, &ev));
        let item = match found {
            Ok(i) => i,
            Err(e) => return NodeOutcome::Fail(e.to_string()),
        };
        let Some(item) = item else {
            if cfg["fallback"] == "create" {
                return item_create_event(cx, cfg, input).await;
            }
            cx.effects.label = Some("没有找到对应的事项".into());
            return NodeOutcome::next(json!({ "found": false }));
        };
        let summary = ev["summary"].as_str().unwrap_or("");
        let src = source_label(ev["source"].as_str().unwrap_or(""));
        let text = event_text(&ev);
        let body = if text.is_empty() {
            format!("{src}：{summary}")
        } else {
            format!("{src}：{summary}\n\n{text}")
        };
        if let Err(e) = cx.flow.core.supplement(&ws, &item, &body, "auto").await {
            return NodeOutcome::Fail(format!("补充到 {item} 失败：{e}"));
        }
        if let Some(e) = ev["id"].as_str() {
            let r = cx.flow.store().transaction(|tx| {
                links::add(
                    tx,
                    &ws,
                    &NewLink::new(
                        ObjRef::new("item", &item),
                        "from",
                        ObjRef::new("event", e),
                        "sig",
                    ),
                )
            });
            if let Err(err) = r {
                tracing::warn!(item = %item, "item.link link: {err}");
            }
        }
        cx.effects.outcome = Some("linked".into());
        cx.effects.label = Some(format!("关联到 {item}"));
        cx.effects.item_id = Some(item.clone());
        NodeOutcome::next(json!({ "found": true, "item_id": item }))
    }
}

/// `subject_type` of `notify` decisions; options “知道了 / 转成事项”.
pub const SIGNAL_SUBJECT: &str = "signal";
pub const NOTIFY_OPTIONS: [&str; 2] = ["done", "to_item"];

/// `notify {text}`: an `ask` decision about the event.
pub struct Notify;

#[async_trait]
impl Action for Notify {
    fn name(&self) -> &'static str {
        "notify"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        crate::expr::check_template(req_str(cfg, "text", "提醒的内容", 500)?)
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("need")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let text = cx.render(cfg["text"].as_str().unwrap_or(""), input);
        let subject = cx
            .event_id()
            .map(String::from)
            .unwrap_or_else(|| cx.run.id.clone());
        let d = NewDecision {
            workspace_id: cx.ws().to_string(),
            kind: "ask".into(),
            subject_type: SIGNAL_SUBJECT.into(),
            subject_id: subject,
            item_id: None,
            session_id: None,
            question: text.clone(),
            draft: None,
            options: Some(NOTIFY_OPTIONS.map(String::from).to_vec()),
            requested_by: "steve".into(),
        };
        let opened = cx.flow.store().transaction(|tx| {
            steve_store::base::decisions::open(tx, &d).map_err(crate::FlowError::from)
        });
        match opened {
            Ok(d) => {
                cx.effects.outcome = Some("need".into());
                cx.effects.label = Some(cut(&text, 60));
                cx.effects.decision_id = Some(d.id.clone());
                NodeOutcome::next(json!({ "decision_id": d.id }))
            }
            Err(e) => NodeOutcome::Fail(e.to_string()),
        }
    }
}

/// `digest {text}`.
pub struct Digest;

#[async_trait]
impl Action for Digest {
    fn name(&self) -> &'static str {
        "digest"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        match cfg["text"].as_str() {
            Some(t) => crate::expr::check_template(t),
            None => Ok(()),
        }
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("digest")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let tpl = cfg["text"].as_str().unwrap_or("");
        let text = if tpl.trim().is_empty() {
            cx.event()["summary"].as_str().unwrap_or("").to_string()
        } else {
            cx.render(tpl, input)
        };
        let mut c = cfg.clone();
        c["text"] = json!(text);
        if cx.event_id().is_none() {
            let o = digest_action(cx.flow, &cx.run, &c).unwrap_or_else(|e| fail(e.to_string()));
            return from_schedule(cx, o);
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return NodeOutcome::Fail("没有要记下的内容".into());
        }
        let now = cx.now();
        let (ws, evt, run) = (
            cx.ws().to_string(),
            cx.event_id().map(String::from),
            cx.run.id.clone(),
        );
        match cx
            .flow
            .store()
            .transaction(|tx| digest::add(tx, &ws, &text, evt.as_deref(), Some(&run), now))
        {
            Ok(id) => {
                cx.effects.outcome = Some("digest".into());
                cx.effects.label = Some("进汇报".into());
                NodeOutcome::next(json!({ "digest_id": id }))
            }
            Err(e) => NodeOutcome::Fail(e.to_string()),
        }
    }
}

/// `ignore {reason}`.
pub struct Ignore;

#[async_trait]
impl Action for Ignore {
    fn name(&self) -> &'static str {
        "ignore"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        opt_str(cfg, "reason")?;
        Ok(())
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("ignored")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let reason = cx.render(cfg["reason"].as_str().unwrap_or(""), input);
        cx.effects.outcome = Some("ignored".into());
        cx.effects.label = Some("已忽略".into());
        if !reason.trim().is_empty() {
            cx.effects.reason = Some(reason.trim().to_string());
        }
        NodeOutcome::next(json!({ "ignored": true, "reason": reason }))
    }
}

/// `report {what}` (schedules): Steve reports in the main thread.
pub struct Report;

#[async_trait]
impl Action for Report {
    fn name(&self) -> &'static str {
        "report"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        opt_str(cfg, "what")?;
        Ok(())
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("report")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, _input: &Value) -> NodeOutcome {
        let o = report(cx.flow, &cx.run, cfg)
            .await
            .unwrap_or_else(|e| fail(e.to_string()));
        from_schedule(cx, o)
    }
}
