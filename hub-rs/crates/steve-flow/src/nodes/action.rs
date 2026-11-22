//! The `action` node: a registry of [`Action`]s. Built in: `item.create`,
//! `item.link`, `notify`, `digest`, `ignore` and `report` (schedules); the channel module registers
//! `reply_as` and `send_to_space` (effectful, so they run only after an approval or with a grant).
//!
//! Schedules keep their own wording: their `item.create` and `report` go through
//! [`run`], as before.

use serde_json::{Value, json};
use steve_store::base::ObjRef;
use steve_store::base::links::{Links, NewLink};

use crate::exec::REPORT_TIMEOUT;
use crate::ports::ItemRequest;
use crate::runs::RunRow;
use crate::schedules::{self, Schedule};
use crate::{FlowCx, FlowResult};
use steve_modules::Tier;
use steve_modules::outbound::quote_untrusted;

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
        "report" => report(cx, run, run.cursor.as_deref().unwrap_or("action"), cfg).await,
        "digest" => digest_action(cx, run, cfg),
        other => Ok(fail(format!("不认识的动作：{other}"))),
    };
    res.unwrap_or_else(|e| {
        tracing::error!(run = %run.id, "action failed: {e}");
        fail(e.to_string())
    })
}

fn schedule_of(cx: &FlowCx, run: &RunRow) -> FlowResult<Option<Schedule>> {
    Ok(cx.store().read(|c| schedules::get_any(c, &run.flow_id))?)
}

/// The bot to hand the item to: the configured one if it is usable, else the main bot with a note
/// for the label (“（<Bot> 不可用，交给了 Steve）”).
fn pick_bot(
    c: &mut impl steve_store::Read,
    ws: &str,
    want: &str,
) -> FlowResult<(String, Option<String>)> {
    let mut bots = steve_store::BotRows(c);
    let unavailable = match bots.local(ws, want)? {
        Some((_, true)) => return Ok((want.to_string(), None)),
        Some((name, false)) => name,
        None => want.to_string(),
    };
    let main = bots
        .main()?
        .ok_or_else(|| steve_modules::ApiError::rule("没有主 Bot"))?;
    Ok((
        main,
        Some(format!("（{unavailable} 不可用，交给了 Steve）")),
    ))
}

async fn item_create(cx: &FlowCx, run: &RunRow, cfg: &Value) -> FlowResult<ActionOutcome> {
    let Some(s) = schedule_of(cx, run)? else {
        return Ok(fail("建事项的动作目前只用于定时任务"));
    };
    // Runs pin their graph version. Never lend a newly configured requester's identity to
    // an older goal; removing/changing authorization also invalidates waiting old runs.
    let requester = cfg["requester"].as_str().map(str::to_string);
    if requester != s.spec.requester {
        return Ok(fail("Schedule requester changed; start a new run"));
    }
    let tz = s.spec.zone();
    let at = run.at();
    let ws = run.workspace_id.clone();
    let (bot, note, previous) = cx.store().read(|r| {
        let previous = steve_store::RunRows(r).last_real(&ws, &run.flow_id, Some(&run.id))?;
        Ok(pick_bot(r, &ws, cfg["bot"].as_str().unwrap_or(&s.spec.bot))
            .map(|(bot, note)| (bot, note, previous)))
    })??;
    let mut planned = format!("计划时间 {}", schedules::local(at, tz, "%Y-%m-%d %H:%M"));
    if run.trigger_kind == "catch_up" {
        let n = run.context["missed"].as_u64().unwrap_or(1);
        planned.push_str(&format!("，补跑，错过 {n} 次"));
    }
    let last = previous
        .map(|p| {
            format!(
                "。上次结果：{}",
                steve_i18n::stored(|| crate::runs::display_text(&p).outcome_label)
            )
        })
        .unwrap_or_default();
    let what = cfg["goal"].as_str().unwrap_or(&s.spec.what);
    let propose = cfg["mode"] == "propose";
    // Recheck membership at execution, not just when the schedule was configured.
    cx.store()
        .read(|c| Ok(schedules::check_refs(c, &ws, &s.spec)))??;
    // and that its item could run at all (#222): otherwise no item is made, the run records why
    // and nothing tries again by itself
    let mut spec = s.spec.clone();
    spec.bot = bot.clone();
    if let Err(why) = schedules::can_run(cx.core.as_ref(), cx.store(), &ws, &spec) {
        return Ok(fail(why));
    }
    let req = ItemRequest {
        requester,
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
    cx.store().write(|w| {
        Links(w).add(
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

/// A caller-provided RunRow cannot publish in another workspace or borrow another flow's id.
fn report_scope(c: &mut impl steve_store::Read, run: &RunRow) -> FlowResult<()> {
    let owned = steve_store::RunRows(c).belongs_to(&run.workspace_id, &run.flow_id, &run.id)?;
    if !owned {
        return Err(steve_modules::ApiError::not_found("运行"));
    }
    Ok(())
}

async fn report(cx: &FlowCx, run: &RunRow, node: &str, cfg: &Value) -> FlowResult<ActionOutcome> {
    let ws = run.workspace_id.clone();
    let what = cfg["what"].as_str().unwrap_or("").to_string();
    // Recover before digest collection or model work after a committed publication.
    let (existing, entries, items) = cx.store().read(|r| {
        if !steve_store::RunRows(r).belongs_to(&ws, &run.flow_id, &run.id)? {
            return Err(steve_store::StoreError::NotFound("没有这个运行"));
        }
        if let Some(result) = steve_store::FeedReads(r).report_result(&ws, &run.id, node)? {
            return Ok((Some(result), Vec::new(), Vec::new()));
        }
        let since = steve_store::RunRows(r)
            .last_real_start(&ws, &run.flow_id, &run.id)?
            .unwrap_or(0);
        let entries = steve_store::DigestNotes(r).pending(&ws, since)?;
        let items = steve_store::ItemReads(r)
            .changed_since(&ws, since, 30)?
            .into_iter()
            .map(|(id, title, now)| format!("{id} · {title} · {now}"))
            .collect();
        Ok((None, entries, items))
    })?;
    if let Some(output) = existing {
        let cost = output["cost_usd"].as_f64().unwrap_or(0.0);
        return Ok(report_done(output, cost));
    }
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
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let now = cx.now();
    let content = steve_modules::outbound::Content {
        title: schedule_of(cx, run)?
            .map(|s| s.name)
            .unwrap_or_else(|| "Report".into()),
        markdown: text.clone(),
        ..Default::default()
    };
    let result = json!({ "text": text, "digest": ids.len(), "cost_usd": cost });
    let published = cx.store().write(|w| -> FlowResult<Value> {
        report_scope(w, run)?;
        let (result, inserted) = w.publish_report(&steve_store::feed::RunReport {
            ws: &ws,
            flow: &run.flow_id,
            run: &run.id,
            node,
            content: &content,
            result: &result,
            now,
        })?;
        if inserted {
            steve_store::DigestNotes(w).mark_taken(&ids, now)?;
        }
        Ok(result)
    });
    match published {
        Ok(output) => Ok(report_done(output, cost)),
        Err(e) => Ok(ActionOutcome::Retry {
            error: format!("汇报未发布到信息流：{e}"),
            cost,
        }),
    }
}
fn report_done(output: Value, cost: f64) -> ActionOutcome {
    ActionOutcome::Done {
        output,
        outcome: "report",
        label: "已发布到信息流".into(),
        item_id: None,
        cost,
    }
}

fn digest_action(cx: &FlowCx, run: &RunRow, cfg: &Value) -> FlowResult<ActionOutcome> {
    let text = cfg["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Ok(fail("没有要记下的内容"));
    }
    let now = cx.now();
    let id = cx.store().write(|w| {
        steve_store::DigestNotes(w).add(&run.workspace_id, &text, None, Some(&run.id), now)
    })?;
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
    DRY_EFFECT, DRY_WRITE, Grant, NodeKind, NodeOutcome, Rule, RunCx, one_of, opt_str, req_str,
    skipped,
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

    /// What [`Action::validate`] enforces, for the draft prompt. Default: the built-in action of the
    /// same name's (so wrappers and replacements keep them).
    fn rules(&self) -> Vec<Rule> {
        builtin_rules(self.name())
    }

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

    /// The names, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.0.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// `mode` of `item.create` (absent: `start`).
pub const ITEM_MODES: [&str; 2] = ["start", "propose"];
/// `accept` of `item.create` (absent: `inherit`, the Bot's or project's acceptance).
pub const ITEM_ACCEPTS: [&str; 2] = ["auto", "inherit"];
/// `fallback` of `item.link` (absent: `none`).
pub const LINK_FALLBACKS: [&str; 2] = ["create", "none"];
const NOTIFY_MAX: usize = 500;

fn item_rules() -> Vec<Rule> {
    vec![
        Rule::new(
            &["flow.node.item_mode"],
            format!("mode 只能是 {}（不填是 start）", one_of(&ITEM_MODES)),
        ),
        Rule::new(
            &["flow.node.item_accept"],
            format!(
                "accept 只能是 {}（不填是 inherit，沿用 Bot 或项目的验收方式），没有别的验收方式，拿不准就不填",
                one_of(&ITEM_ACCEPTS)
            ),
        ),
        Rule::new(
            &["flow.node.text_list_only", "flow.node.text_only"],
            "bot、project、title、goal 是文字，tags 是文字列表；bot 只能是下面列出的 Bot id",
        ),
    ]
}

/// The rules of the built-in actions, by name.
pub fn builtin_rules(name: &str) -> Vec<Rule> {
    match name {
        "item.create" => item_rules(),
        "item.link" => vec![
            Rule::new(
                &["flow.node.link_by"],
                format!("by 只能是 {}", one_of(&crate::link::METHODS)),
            ),
            Rule::new(
                &["flow.node.link_fallback"],
                format!(
                    "fallback 只能是 {}（不填是 none），create 时 mode、accept 等同 item.create 的规则",
                    one_of(&LINK_FALLBACKS)
                ),
            ),
        ],
        "notify" => vec![Rule::new(
            &["flow.node.missing_reminder", "flow.node.too_long_reminder"],
            format!("text 必填，最多 {NOTIFY_MAX} 字"),
        )],
        "ignore" => vec![Rule::new(&[], "reason 是文字")],
        "digest" => vec![Rule::new(&[], "text 是文字，可以不填")],
        "report" => vec![Rule::new(
            &[],
            "what 是文字；destination 不填或为 workspace_feed，发布到信息流，工作空间成员可见",
        )],
        _ => Vec::new(),
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

    fn name_rule(&self) -> Rule {
        Rule::new(
            &["flow.node.action_missing", "flow.node.action_unknown"],
            format!("action 必填，只能是 {}", one_of(&self.actions.names())),
        )
    }

    /// One action's rules, each prefixed with its name.
    fn action_rules(&self, name: &str) -> Vec<Rule> {
        let Some(a) = self.actions.get(name) else {
            return Vec::new();
        };
        a.rules()
            .into_iter()
            .map(|r| Rule::new(r.keys, format!("{name} 的 {}", r.text)))
            .collect()
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

    fn rules(&self) -> Vec<Rule> {
        let mut v = vec![self.name_rule()];
        for name in self.actions.names() {
            v.extend(self.action_rules(name));
        }
        v
    }

    fn rules_for(&self, cfg: &Value) -> Vec<Rule> {
        let mut v = vec![self.name_rule()];
        if let Some(name) = cfg["action"].as_str() {
            v.extend(self.action_rules(name));
        }
        v
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let name = opt_str(cfg, "action")?.unwrap_or("");
        match self.actions.get(name) {
            Some(a) => a.validate(cfg),
            None if name.is_empty() => Err(steve_i18n::t!("flow.node.action_missing")),
            None => Err(steve_i18n::t!("flow.node.action_unknown", action = name)),
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
    if !ITEM_MODES.contains(&opt_str(cfg, "mode")?.unwrap_or("start")) {
        return Err(steve_i18n::t!("flow.node.item_mode"));
    }
    if !ITEM_ACCEPTS.contains(&opt_str(cfg, "accept")?.unwrap_or("inherit")) {
        return Err(steve_i18n::t!("flow.node.item_accept"));
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
    c: &mut impl steve_store::Read,
    ws: &str,
    cfg_bot: Option<&str>,
    ev: &Value,
) -> FlowResult<(String, Option<String>)> {
    let triage = match ev["connection_id"].as_str() {
        Some(conn) => steve_store::FlowSources(c).triage(ws, conn)?,
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
        .read(|c| Ok(event_bot(c, &ws, bot_cfg.as_deref(), &ev)))
        .map_err(steve_modules::ApiError::from)
        .and_then(|result| result);
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
            .read(|c| crate::flows::name_of(c, &cx.run.flow_id))
            .unwrap_or_default()
    } else {
        cut(&title, 80)
    };
    let src = ev["source"].as_str().unwrap_or("manual");
    // what an outside event brings (its text, the values a goal template pulls from it) goes between
    // the untrusted-data markers; the goal the user wrote, and a manual or scheduled run, stay theirs
    let outside = crate::signals::external_source(src);
    let quote = |t: String| {
        if outside && !t.trim().is_empty() {
            quote_untrusted(&t)
        } else {
            t
        }
    };
    let goal = cfg["goal"]
        .as_str()
        .map(|g| cx.render_with(g, input, quote))
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| {
            let text = event_text(&ev);
            quote(if text.is_empty() {
                summary.clone()
            } else {
                format!("{summary}\n\n{text}")
            })
        });
    let propose = cfg["mode"] == "propose";
    let req = ItemRequest {
        requester: None,
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
    // the tag its sessions ask of a Node (B7)
    if let Some(tag) = cfg["node_tag"].as_str().filter(|t| !t.trim().is_empty()) {
        let set = cx
            .flow
            .store()
            .write(|w| steve_store::nodes::set_item_tag(w, &item_id, Some(tag)));
        if !matches!(set, Ok(true)) {
            tracing::warn!(item = %item_id, "node_tag {tag} not set: {set:?}");
        }
    }
    let evt = ev["id"].as_str().map(String::from);
    let party = ev["actor_party_id"].as_str().map(String::from);
    let run_id = cx.run.id.clone();
    let linked = cx
        .flow
        .store()
        .write(|w| link_created_item(w, &ws, &item_id, &run_id, evt.as_deref(), party.as_deref()));
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

/// Attach the new item's run, event and actor provenance within one transaction.
fn link_created_item(
    w: &mut steve_store::W<'_>,
    ws: &str,
    item_id: &str,
    run_id: &str,
    evt: Option<&str>,
    party: Option<&str>,
) -> FlowResult<()> {
    let item = || ObjRef::new("item", item_id);
    Links(w).add(
        ws,
        &NewLink::new(item(), "from", ObjRef::new("flow_run", run_id), "sig"),
    )?;
    if let Some(e) = evt {
        Links(w).add(
            ws,
            &NewLink::new(item(), "from", ObjRef::new("event", e), "sig"),
        )?;
    }
    if let Some(p) = party {
        Links(w).add(
            ws,
            &NewLink::new(item(), "raised_by", ObjRef::new("party", p), "sig"),
        )?;
    }
    Ok(())
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
            return Err(steve_i18n::t!("flow.node.link_by"));
        }
        match opt_str(cfg, "fallback")?.unwrap_or("none") {
            "create" => check_mode(cfg),
            f if LINK_FALLBACKS.contains(&f) => Ok(()),
            _ => Err(steve_i18n::t!("flow.node.link_fallback")),
        }
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("linked")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let ev = cx.event().clone();
        let ws = cx.ws().to_string();
        let by = cfg["by"].as_str().unwrap_or("");
        let found = cx.flow.finders.find(cx.flow.store(), &ws, by, &ev);
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
            let r = cx.flow.store().write(|w| {
                Links(w).add(
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
        crate::expr::check_template(req_str(cfg, "text", super::Required::Reminder, NOTIFY_MAX)?)
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
        let opened = cx
            .flow
            .store()
            .write(|w| steve_store::DecisionRows(w).open(&d));
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
            .write(|w| steve_store::DigestNotes(w).add(&ws, &text, evt.as_deref(), Some(&run), now))
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

/// `report {what}`: publishes generated content to the workspace Feed.
pub struct Report;

#[async_trait]
impl Action for Report {
    fn name(&self) -> &'static str {
        "report"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        opt_str(cfg, "what")?;
        if cfg
            .get("destination")
            .is_some_and(|d| d != "workspace_feed")
        {
            return Err("Report destination must be workspace_feed".into());
        }
        Ok(())
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("report")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, _input: &Value) -> NodeOutcome {
        let o = report(cx.flow, &cx.run, &cx.node.id, cfg)
            .await
            .unwrap_or_else(|e| fail(e.to_string()));
        from_schedule(cx, o)
    }
}
