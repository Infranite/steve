//! orchestrator.ts: items ↔ sessions ↔ decisions. The only place that knows the item lifecycle.

use crate::error::{CoreError, Result, msg};
use crate::git;
use crate::lifecycle::{deps_open, may_resume, settle, supersede, tidy};
use crate::modules::Point;
use crate::patch;
use crate::release::NOTHING_TO_RELEASE;
use crate::repo::{
    Bot, BotExt, Decision, FileRef, Item, ItemExt, NewDecision, NewItem, NewSession, Parent,
    PlanStep, Queued, Session, WS,
};
use crate::runtime::{Check, McpEndpoint, Output, Prompted, TurnEnd};
use crate::thread::flowback::{Update, flow_quiet, recover_quiet};
use crate::util::{js_head, js_len, js_tail, now, or, token};
use crate::{Core, Mark, Tx};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use steve_modules::outbound::YOLO_ACTOR;

const PRO: [&str; 3] = ["只在必要时", "适中", "积极"];
const DAY: i64 = 24 * 3_600_000;
const INTERRUPTED: &str = "Hub 重启时这个会话正在运行，这一回合被中断了";
/// The message an interrupted turn is resumed with after a restart.
/// Put before a message the agent had already been given when the Hub stopped (its dispatch was
/// submitted): it may be done or half done, so it comes back to be checked, never redone blindly.
pub const RESUMED_NOTE: &str = "（以下是 Hub 重启前已经发给你的消息，你可能已经做了一部分或全部。只用来核对进度：先确认它的结果，已经做完的不要重做，没做完的接着做。）";

/// A queued message as the agent gets it: one delivered before a stop (`via` `resumed:<via>`) is
/// wrapped in [`RESUMED_NOTE`] and gets its own `via` back; any other is as it was.
pub fn resumed_text(via: &str, text: &str) -> (String, String) {
    match via.strip_prefix("resumed:") {
        Some(v) => (format!("{RESUMED_NOTE}\n\n{text}"), v.to_string()),
        None => (text.to_string(), via.to_string()),
    }
}

/// An interruption older than this is not continued by itself (it is reported, the intent dropped).
pub const RESUME_MAX_AGE_MS: i64 = 24 * 3_600_000;

pub const RESUME: &str =
    "Hub 刚重启，上一回合被中断了，请从中断处继续；中断前做到一半的操作，先确认结果再往下。";

/// What to create (orchestrator `NewItem`).
#[derive(Debug, Clone, Default)]
pub struct CreateItem {
    pub ws: String,
    pub goal: String,
    pub title: String,
    pub bot: Option<String>,
    /// `None`: not given (the bot's default project); `Some(None)`: explicitly no project.
    pub project: Option<Option<String>>,
    /// `[label, code]`, e.g. `["对话", "chat"]`; `None` keeps the column default.
    pub source: Option<(String, String)>,
    pub parent: Option<Parent>,
    pub tag_ids: Vec<String>,
    /// Start with a 方案 decision instead of a session.
    pub propose: bool,
    pub opening: Option<String>,
    /// Attachments for the first session's prompt.
    pub files: Vec<FileRef>,
    pub actor: Option<String>,
}

/// What [`Core::queued_send_now`] did with a queued message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendNow {
    /// The running turn was interrupted; the queue (this message first) is the next turn.
    Interrupted,
    /// Nothing was running: the queue (this message first) started a turn now.
    Sent,
    /// It cannot go yet; the reason is shown to the user. It stays first in the queue.
    Waiting(String),
}

/// The 409 for a queued message a turn already took.
fn already_sent() -> CoreError {
    CoreError::Module(steve_modules::ApiError::Conflict(steve_i18n::t!(
        "core.queue.already_sent"
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplementMode {
    Auto,
    Steer,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAction {
    Pause,
    Resume,
    /// The user marks the item done themselves (no acceptance, no release).
    Finish,
    /// Undo the last `Finish`, restoring the item as it was.
    Unfinish,
    Drop,
    Reopen,
    Archive,
    Unarchive,
}
impl ItemAction {
    pub fn parse(s: &str) -> Option<ItemAction> {
        Some(match s {
            "pause" => ItemAction::Pause,
            "resume" => ItemAction::Resume,
            "finish" => ItemAction::Finish,
            "unfinish" => ItemAction::Unfinish,
            "drop" => ItemAction::Drop,
            "reopen" => ItemAction::Reopen,
            "archive" => ItemAction::Archive,
            "unarchive" => ItemAction::Unarchive,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ItemAction::Pause => "pause",
            ItemAction::Resume => "resume",
            ItemAction::Finish => "finish",
            ItemAction::Unfinish => "unfinish",
            ItemAction::Drop => "drop",
            ItemAction::Reopen => "reopen",
            ItemAction::Archive => "archive",
            ItemAction::Unarchive => "unarchive",
        }
    }
    fn label(self) -> &'static str {
        match self {
            ItemAction::Pause => "暂停",
            ItemAction::Resume => "继续",
            ItemAction::Finish => "标记完成",
            ItemAction::Unfinish => "撤销了标记完成",
            ItemAction::Drop => "放弃",
            ItemAction::Reopen => "重新打开",
            ItemAction::Archive => "归档",
            ItemAction::Unarchive => "恢复",
        }
    }
}

/// A completion the user asked for themselves, kept (in memory) so the toast can withdraw it while the
/// close has not committed yet.
#[derive(Debug, Clone)]
pub(crate) struct MarkedDone {
    /// a turn was running and got cancelled
    busy: bool,
}

/// Result of a fork: `{"session": id}` or `{"item": id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Forked {
    #[serde(rename = "session")]
    Session(String),
    #[serde(rename = "item")]
    Item(String),
}

/// Where a decision's follow-up message goes (the `send` closures of orchestrator.ts).
pub(crate) enum To {
    /// The decision's session, or a new session of its item; superseding stale cards first.
    Decision {
        /// the decision's session if it was not ended when the decision was resolved
        live: Option<String>,
        item: Option<String>,
    },
    /// Straight to a session.
    Session(String),
}

/// The directory exists and a file can be created in it (an agent would work there).
fn cwd_usable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".steve-write-check-{}", crate::util::uid("w")));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn status_in(i: &Item, xs: &[&str]) -> bool {
    xs.contains(&i.status.as_str())
}

/// An item patch from a stored undo record (`{status, now, stage, plan}`).
fn undo_patch(u: &Value) -> Map<String, Value> {
    let mut p = Map::new();
    for k in ["status", "now", "stage", "plan"] {
        if let Some(v) = u.get(k).filter(|v| !v.is_null()) {
            p.insert(k.into(), v.clone());
        }
    }
    p
}

impl Core {
    /* ── seed: the personal workspace, Steve's own project, the main bot and the developer bot ── */
    /// Every boot: workspace `default`, bots `steve` and `dev`, and Steve's own project if `STEVE_REPO` is a git repo.
    pub fn seed(&self) -> Result<()> {
        let t = self.tx();
        let db = t.db();
        db.ensure_workspace()?;
        db.bot_seed(
            "steve",
            true,
            json!({
                "name": "Steve", "harness": "claude-code", "model": crate::models::STEVE_DEFAULT_CLAUDE, "role": "你的助手：理解需求、分派、汇报", "accept_mode": "inherit", "perm_mode": "default",
                "persona": "沉稳、简洁，先说结论；不客套", "avatar": {"color": "#7aa7c7", "round": 70},
                "rules": "能直接回答的就回答；是一件事就建事项交给合适的 Bot。只在有新结果或需要决定时打扰。",
            }),
            WS,
        )?;
        let repo = t.cfg().steve_repo.to_string_lossy().to_string();
        let mut steve_project = db.project_by_path(WS, &repo)?;
        if steve_project.is_none() && Path::new(&repo).join(".git").exists() {
            // A managed bundle is upgraded through steve_upgrade, not the source deploy script.
            let release = (std::env::var("STEVE_MANAGED").as_deref() != Ok("1")).then(|| {
                crate::repo::Release {
                    kind: "script".into(),
                    cmd: t
                        .cfg()
                        .home
                        .join("current/ops/release.sh")
                        .to_string_lossy()
                        .into(),
                    done_when: None,
                }
            });
            steve_project = Some(db.project_create(
                WS,
                "Steve",
                "Steve 自己的代码：Rust Hub、Web 界面、发布与运维脚本",
                &repo,
                "git",
                true,
                Some("dev"),
                release.as_ref(),
            )?);
        }
        db.bot_seed(
            "dev",
            false,
            json!({
                "name": "Steve 开发", "harness": "claude-code", "role": "开发 Steve 自己：Hub、前端、部署脚本", "accept_mode": "inherit", "perm_mode": "auto",
                "persona": "直接、话少", "default_project": steve_project.as_ref().map(|p| p.id.clone()), "avatar": {"color": "#8c7ae6", "round": 45},
                "rules": "在 git worktree 的分支上工作，不直接改 main。改完必须跑类型检查和测试；界面改动要截图自查。提交信息用中文说明改了什么。不推送、不发布，除非用 steve_request_approval 得到批准。",
            }),
            WS,
        )?;
        /* a Steve bot with no harness gets one (and a judgment model); one that has a harness is left
        as the user set it, model or not */
        db.store().with_conn(|c| {
            c.execute(
                "UPDATE bots SET harness = 'claude-code', model = coalesce(model, ?) WHERE id = 'steve' AND harness = ''",
                [crate::models::STEVE_DEFAULT_CLAUDE],
            )?;
            Ok(())
        })?;
        /* bots seeded before projects existed: give them their project and a face once */
        db.bot_seed_fixups(steve_project.as_ref().map(|p| p.id.as_str()))?;
        drop(t);
        /* Steve became a bot: the cheap purposes keep their fast list as an override, once */
        for ws in self.0.store.store().workspaces().ids()? {
            if self.models().migrate(&ws)? {
                tracing::info!("models: recommended overrides written for {ws}");
            }
        }
        Ok(())
    }

    /// The bot's own part of the prompt: written by the user, or generated from its settings.
    pub fn bot_prompt(&self, b: &Bot, ws: &str) -> Result<String> {
        self.tx().bot_prompt(b, ws)
    }

    pub fn create_item(&self, n: CreateItem) -> Result<Item> {
        self.tx().create_item(n)
    }
    pub fn start_session(
        &self,
        item: &str,
        opening: Option<&str>,
        bot: Option<&str>,
        files: &[FileRef],
    ) -> Result<Session> {
        self.tx().start_session_files(item, opening, bot, files)
    }
    /// The Steve MCP endpoint for an adapter that is starting; mints and stores a new token after a restart.
    pub fn mcp_endpoint(&self, session_id: &str) -> Result<McpEndpoint> {
        self.tx().mcp_endpoint(session_id)
    }
    /// The last agent text of a session (≤ ~400 chars), for handoffs and failure cards.
    pub fn last_agent_text(&self, session_id: &str) -> Result<String> {
        self.tx().last_agent_text(session_id)
    }

    /* ── Steve tools (mcp.ts actions); `s` is the session row as of the request ── */
    pub fn tool_update(&self, s: &Session, now: &str, plan: &[PlanStep]) -> Result<()> {
        self.tx().tool_update(s, now, plan)
    }
    pub fn tool_ask(&self, s: &Session, question: &str, options: &[String]) -> Result<()> {
        self.tx().tool_ask(s, question, options, None, None)
    }
    /// `steve_ask` with the agent's `recommended` option and `why` (written to the card's draft).
    pub fn tool_ask_with(
        &self,
        s: &Session,
        question: &str,
        options: &[String],
        recommended: Option<&str>,
        why: Option<&str>,
    ) -> Result<()> {
        self.tx().tool_ask(s, question, options, recommended, why)
    }
    pub fn tool_request_approval(&self, s: &Session, action: &str, detail: &str) -> Result<()> {
        self.tx().tool_request_approval(s, action, detail)
    }
    pub fn tool_deliver(
        &self,
        s: &Session,
        summary: &str,
        outputs: &[Output],
        checks: &[Check],
        unfinished: &[String],
    ) -> Result<()> {
        self.tx()
            .tool_deliver(s, summary, outputs, checks, unfinished)
    }
    pub fn tool_publish(&self, s: &Session, branch: &str, summary: &str) -> Result<()> {
        self.tx().tool_publish(s, branch, summary)
    }

    /// A turn ended (acp.ts turn-end handler).
    pub fn on_turn_end(&self, t: TurnEnd) -> Result<()> {
        self.tx().on_turn_end(t)
    }

    pub fn add_dep(&self, item: &str, dep: &str) -> Result<()> {
        self.tx().add_dep(item, dep)
    }
    pub fn rm_dep(&self, item: &str, dep: &str) -> Result<()> {
        self.tx().rm_dep(item, dep)
    }
    pub fn resolve(&self, id: &str, choice: &str, text: &str, actor: &str) -> Result<Decision> {
        self.tx().resolve(id, choice, text, actor)
    }
    pub fn undo_decision(&self, id: &str, actor: &str) -> Result<Decision> {
        self.tx().undo_decision(id, actor)
    }
    pub fn dismiss_decision(&self, id: &str, actor: &str) -> Result<Decision> {
        self.tx().dismiss_decision(id, actor)
    }
    pub fn supplement(
        &self,
        item: &str,
        text: &str,
        mode: SupplementMode,
        files: &[FileRef],
    ) -> Result<()> {
        self.tx().supplement(item, text, mode, files)
    }
    pub fn deliver_queued(&self, item: &str) -> Result<()> {
        self.tx().deliver_queued(item)
    }
    pub fn item_action(&self, id: &str, action: ItemAction, actor: &str) -> Result<Item> {
        self.tx().item_action(id, action, actor)
    }
    pub fn revise_goal(&self, id: &str, text: &str) -> Result<()> {
        self.tx().revise_goal(id, text)
    }
    pub fn reassign(&self, id: &str, bot: &str, actor: &str) -> Result<Item> {
        self.tx().reassign(id, bot, actor)
    }
    /// A message typed into a session: `插话` interrupts a running turn, `排队` waits behind it.
    pub fn session_message(
        &self,
        session_id: &str,
        text: &str,
        interrupt: bool,
        files: &[FileRef],
    ) -> Result<Prompted> {
        self.tx()
            .session_message(session_id, text, interrupt, files)
    }
    /// Change the text of a message still waiting in a session's queue (409 once a turn took it).
    /// For Node sessions too: a claimed row has left `queued` (runtime::claim_in), so this is the 409.
    pub fn queued_edit(&self, session_id: &str, queued_id: &str, text: &str) -> Result<Queued> {
        self.tx().queued_edit(session_id, queued_id, text)
    }
    /// Take a message back out of a session's queue (409 once a turn took it); returns it.
    /// For Node sessions too: a claimed row has left `queued` (runtime::claim_in), so this is the 409.
    pub fn queued_remove(&self, session_id: &str, queued_id: &str) -> Result<Queued> {
        self.tx().queued_remove(session_id, queued_id)
    }
    /// Send a queued message now: it moves to the head of the queue and the running turn is
    /// interrupted (like 中断), so the queue in order is the next turn; an idle session starts it
    /// at once. While the Hub holds (a release, upgrade maintenance), while the last turn's outcome
    /// is uncertain, or while nothing could take the queue yet (a session opening), it waits and
    /// says why; the message stays first.
    pub fn queued_send_now(&self, session_id: &str, queued_id: &str) -> Result<SendNow> {
        self.tx().queued_send_now(session_id, queued_id)
    }
    /// Steve tells an agent something (a module's answer to its request: a secret use, a GitHub
    /// action): the live session of `session_id` (else the item's) gets `text` as a prompt from
    /// Steve; an ended or missing one is replaced by a new session of the item that starts with it.
    pub fn send_to_session(
        &self,
        item_id: Option<&str>,
        session_id: Option<&str>,
        text: &str,
    ) -> Result<()> {
        self.tx().send_to_session(item_id, session_id, text)
    }
    /// A module opened an approval for a session's request (V4 `steve_secret_use`, E5
    /// `steve_mail_draft`, G4 tools): the session waits for the user like after
    /// `steve_request_approval` (pending = approval, the item “在等你批准”).
    pub fn mark_awaiting_approval(&self, session_id: &str) -> Result<()> {
        self.tx().mark_awaiting_approval(session_id)
    }
    /// A module that took over an item's wrap-up (GitHub after a merge) says it is done: the required
    /// `hooks.accepted` operation of the current cycle succeeds and the closing gate is checked. A report
    /// for an item that has no such operation open (another cycle, already closed) is recorded only — it
    /// never closes the item by itself.
    pub fn module_done(&self, item: &str, now_text: &str) -> Result<()> {
        let mut t = self.tx();
        match t.lc_open_op(item, crate::cycle::OP_HOOKS)? {
            Some(op) => {
                t.lc_op_set(&op.id, "succeeded", json!({"note": now_text}))?;
            }
            None => {
                t.db().timeline_add(
                    item,
                    "steve",
                    "sys",
                    &format!("{now_text}（已记录）"),
                    None,
                )?;
            }
        }
        Ok(())
    }
    /// A module's own result completes the item's work (a mail draft item whose mail went out): a
    /// completion request with that result as the evidence of an automatic acceptance. Ignored for a
    /// closed item (a late result of an earlier cycle).
    pub fn module_complete(&self, item: &str, evidence: &str) -> Result<()> {
        let mut t = self.tx();
        let i = t.item(item)?;
        if i.phase == crate::cycle::CLOSED {
            t.db().timeline_add(
                item,
                "steve",
                "sys",
                &format!("{evidence}（事项已结束，只记录）"),
                None,
            )?;
            return Ok(());
        }
        t.lc_request_complete(
            item,
            "steve",
            crate::cycle::Policy::Automatic,
            true,
            evidence,
            None,
            vec![evidence.to_string()],
        )?;
        Ok(())
    }
    /// A module reports a failure of its wrap-up: the operation stays open, a fail card asks what next.
    pub fn fail_item(&self, item: &str, why: &str) -> Result<()> {
        let mut t = self.tx();
        if let Some(op) = t.lc_open_op(item, crate::cycle::OP_HOOKS)? {
            t.db().store().transaction(|tx| {
                steve_store::operation_set_in(tx, &op.id, "running", &json!({"last_failure": why}))
            })?;
        }
        t.fail(item, why)
    }
    /// The lifecycle commands of the API (`accept` | `cancel` | `fail` | `new_cycle`), checked against
    /// the cycle and revision the caller saw (409 otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn lifecycle_command(
        &self,
        item: &str,
        command: &str,
        expected_cycle: i64,
        expected_revision: i64,
        reason: Option<&str>,
        goal: Option<&str>,
        actor: &str,
    ) -> Result<Item> {
        let mut t = self.tx();
        let i = t.item(item)?;
        if i.cycle != expected_cycle || i.revision != expected_revision {
            return Err(crate::cycle::conflict(steve_i18n::t!(
                "core.item.stale",
                cycle = i.cycle,
                revision = i.revision
            )));
        }
        if command == "fail" && reason.is_none_or(|r| r.trim().is_empty()) {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.item.fail_needs_reason"),
            ));
        }
        t.db().audit(
            actor,
            &format!("item.lifecycle.{command}"),
            item,
            &json!({"cycle": i.cycle, "revision": i.revision, "reason": reason}),
            &i.workspace_id,
        )?;
        match command {
            "accept" => {
                let c = crate::cycle::closing_of(&i)
                    .filter(|c| {
                        i.phase == crate::cycle::CLOSING && c.kind == crate::cycle::COMPLETE
                    })
                    .ok_or_else(|| {
                        crate::cycle::conflict(steve_i18n::t!("core.item.not_awaiting_acceptance"))
                    })?;
                if c.accepted {
                    return Err(crate::cycle::conflict(steve_i18n::t!(
                        "core.item.already_accepted"
                    )));
                }
                match c.decision.as_deref() {
                    Some(d) if t.db().decision(d)?.is_some_and(|d| d.status == "open") => {
                        t.resolve(d, "pass", "", actor)?;
                    }
                    _ => {
                        t.answer_line(item, actor, "验收通过", None)?;
                        t.lc_accept(item, crate::cycle::Policy::Human, Some("你确认了完成"))?;
                    }
                }
            }
            "cancel" => {
                t.db()
                    .timeline_add(item, actor, "me", "取消这个事项", None)?;
                t.lc_stop(
                    item,
                    crate::cycle::CANCEL,
                    actor,
                    reason
                        .filter(|r| !r.trim().is_empty())
                        .unwrap_or("你取消了"),
                    None,
                )?;
            }
            "fail" => {
                let why = reason
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| {
                        CoreError::Status(400, steve_i18n::t!("core.item.fail_needs_reason"))
                    })?;
                t.db()
                    .timeline_add(item, actor, "me", &format!("标记失败：{why}"), None)?;
                t.lc_stop(item, crate::cycle::FAIL, actor, why, None)?;
            }
            "new_cycle" => {
                t.lc_new_cycle(item, actor, goal)?;
            }
            _ => {
                return Err(CoreError::Status(
                    400,
                    steve_i18n::t!("core.error.invalid", fields = "command"),
                ));
            }
        }
        t.item(item)
    }
    /// A termination rule decided the item failed (a module's or a flow's rule, never a guess).
    pub fn fail_by_rule(&self, item: &str, rule: &str, why: &str) -> Result<Item> {
        let mut t = self.tx();
        t.db().timeline_add(
            item,
            "steve",
            "sys",
            &format!("按规则「{rule}」判定失败：{why}"),
            None,
        )?;
        t.lc_stop(item, crate::cycle::FAIL, "steve", why, Some(rule))
    }
    /// Commits every closing whose gate holds now (the periodic tick and after boot).
    pub fn lifecycle_tick(&self) -> Result<usize> {
        let n = self.tx().lc_tick()?;
        /* the merge lanes' windows close on time too (outside the core's lock) */
        if let Err(e) = self.lane_tick() {
            tracing::warn!("lane tick: {e}");
        }
        Ok(n)
    }
    /// The lifecycle projection of an item.
    pub fn lifecycle_view(&self, item: &str) -> Result<Value> {
        let t = self.tx();
        let i = t.item(item)?;
        t.lc_view(&i)
    }
    /// The lifecycle projection of every task item of `ws`, under one lock.
    pub fn lifecycle_views_ws(&self, ws: &str) -> Result<std::collections::HashMap<String, Value>> {
        let t = self.tx();
        let mut out = std::collections::HashMap::new();
        for i in t.db().items_all(ws)? {
            out.insert(i.id.clone(), t.lc_view(&i)?);
        }
        Ok(out)
    }
    /// The lifecycle projections of many items under one lock.
    pub fn lifecycle_views(
        &self,
        items: &[String],
    ) -> Result<std::collections::HashMap<String, Value>> {
        let t = self.tx();
        let mut out = std::collections::HashMap::new();
        for id in items {
            if let Some(i) = t.db().item(id)? {
                out.insert(id.clone(), t.lc_view(&i)?);
            }
        }
        Ok(out)
    }
    pub fn end_session(&self, session_id: &str) -> Result<()> {
        self.tx().end_session(session_id)
    }
    pub fn fork(
        &self,
        ws: &str,
        session_id: &str,
        seq: i64,
        goal: &str,
        as_session: bool,
        actor: &str,
    ) -> Result<Forked> {
        self.tx().fork(ws, session_id, seq, goal, as_session, actor)
    }
    /// Soft-delete a bot; its open items move to `to` (or the main bot where `to` isn't a member). Returns moved item ids.
    pub fn delete_bot(&self, id: &str, to: &str, actor: &str) -> Result<Vec<String>> {
        self.tx().delete_bot(id, to, actor)
    }
    /// Boot: report a landed release, expire stale cards, and durably prepare interrupted turns.
    /// Only `ready` starts them after the listener is bound.
    pub fn recover(&self) -> Result<()> {
        self.tx().recover()
    }
    /// Called only once the listener is bound. Recovery preparation itself starts no agents.
    pub fn ready(&self) {
        if !self.store().store().maintenance() {
            self.tx().resume(None);
        }
        /* closings that were waiting when the Hub stopped: resumed turns still hold them */
        if let Err(e) = self.lifecycle_tick() {
            tracing::warn!("closing check after boot: {e}");
        }
        /* merges and releases a restart cut off */
        if !self.store().store().maintenance() {
            self.lane_recover();
        }
    }
    /// Hold new turns for an upgrade drain, or stop holding for it (a release's hold stays).
    pub fn maintenance_hold(&self, on: bool) -> Result<()> {
        let mut t = self.tx();
        t.set_hold(crate::occupancy::Hold::Maintenance, on);
        if !on {
            t.flush_queued()?;
        }
        Ok(())
    }

    /// A turn of the session is in flight (the HTTP layer's view is the core's).
    pub fn is_busy(&self, sid: &str) -> bool {
        self.tx().busy(sid)
    }

    /// Give back a finished item's worktree and merged branch (housekeeping, finish, drop).
    pub fn tidy_branch(&self, item: &str) -> Result<()> {
        self.tx().tidy_branch(item)
    }
}
impl Tx<'_> {
    pub(crate) fn item(&self, id: &str) -> Result<Item> {
        self.db()
            .item(id)?
            .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))
    }

    /// `void acp.prompt(…)`: fire and forget, but log a refusal.
    pub(crate) fn prompt(&self, sid: &str, text: &str, via: &str) {
        self.prompt_files(sid, text, via, &[])
    }
    pub(crate) fn prompt_files(&self, sid: &str, text: &str, via: &str, files: &[FileRef]) {
        if let Err(e) = self.rt_prompt(sid, text, via, files) {
            tracing::warn!("prompt {sid}: {e}");
        }
    }

    /// [`AgentRuntime::prompt`] with a pending YOLO note in front.
    pub(crate) fn rt_prompt(
        &self,
        sid: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<Prompted> {
        let text = self.c.with_steve_note(sid, text);
        let text = self.c.with_yolo_note(sid, &text);
        self.rt().prompt(sid, &text, via, files)
    }

    pub(crate) fn active_session(&self, item: &str) -> Result<Option<Session>> {
        Ok(self
            .db()
            .sessions_of_item(item)?
            .into_iter()
            .rfind(|s| s.status != "ended"))
    }

    fn accept_of(&self, i: &Item) -> Result<String> {
        if let Some(m) = i
            .accept_mode
            .as_deref()
            .filter(|m| !m.is_empty() && *m != "inherit")
        {
            return Ok(m.into());
        }
        if let Some(b) = i
            .bot_id
            .as_deref()
            .map(|b| self.db().bot(b))
            .transpose()?
            .flatten()
            && !b.accept_mode.is_empty()
            && b.accept_mode != "inherit"
        {
            return Ok(b.accept_mode);
        }
        self.db().setting(&i.workspace_id, "accDefault", "manual")
    }

    pub(crate) fn bot_prompt(&self, b: &Bot, ws: &str) -> Result<String> {
        let mem = self.db().memories(&b.id, ws)?;
        let mem_text = if mem.is_empty() {
            String::new()
        } else {
            format!(
                "\n你记得的事：\n{}",
                mem.iter()
                    .map(|m| format!("- {}", m.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        if let Some(p) = b.prompt.as_deref().filter(|p| !p.is_empty()) {
            return Ok(format!("{p}{mem_text}"));
        }
        let who = if b.main {
            format!("{}，用户唯一直接打交道的助手", b.name)
        } else {
            format!("{} Bot，由 Steve 调用", b.name)
        };
        let lines = [
            format!("你是 {who}。"),
            format!("职责：{}", or(&b.role, "（未填写）")),
            if b.persona.is_empty() {
                String::new()
            } else {
                format!("说话方式：{}", b.persona)
            },
            format!(
                "主动程度：{}",
                usize::try_from(b.proactive)
                    .ok()
                    .and_then(|i| PRO.get(i))
                    .copied()
                    .unwrap_or("适中")
            ),
            if b.rules.is_empty() {
                String::new()
            } else {
                format!("工作规范：\n{}", b.rules)
            },
        ];
        Ok(lines
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            + &mem_text)
    }

    /// The working protocol every session starts with.
    fn protocol(&self, item: &Item, bot: &Bot, s: &Session) -> Result<String> {
        let p = self.db().project(item.project_id.as_deref())?;
        let known = if p.is_some() {
            vec![]
        } else {
            self.db().projects(&item.workspace_id)?
        };
        let mut lines = vec![
            format!(
                "You are working for Steve, the user's assistant, on item {}: “{}”.",
                item.id, item.title
            ),
            format!("Goal (from the user): {}", self.db().goal(&item.id)?),
            match &p {
                Some(p) => format!(
                    "Project: {} — {}.",
                    p.name,
                    or(&p.description, "no description")
                ),
                None => {
                    "This item has no project; you are in a scratch directory made for it.".into()
                }
            },
        ];
        if !known.is_empty() {
            lines.push("The user's projects (name — path — description):".into());
            lines.extend(known.iter().map(|x| {
                format!(
                    "- {} — {} — {}",
                    x.name,
                    x.path,
                    or(&x.description, "no description")
                )
            }));
            lines.push("If the work belongs to one of them, do not search the disk for code: use steve_ask to ask the user to attach it, with the project names as options (and “不需要”). When the user picks one, Steve moves the item into that project and a new session continues there.".into());
        }
        lines.push(format!(
            "Working directory: {}{}.",
            s.cwd,
            item.branch
                .as_deref()
                .filter(|b| !b.is_empty())
                .map(|b| format!(
                    " (git worktree on branch {b}; commit your work there, do not touch main)"
                ))
                .unwrap_or_default()
        ));
        /* agents share the machine: on 2026-11-01 one ran `pkill -f include-partial-messages` and killed two others */
        lines.push("Other agents run on this machine at the same time. Stop only processes you started yourself, by PID; never use pkill, killall or kill by name or pattern.".into());
        lines.push(self.bot_prompt(bot, &item.workspace_id)?);
        /* module context (knowledge, contact cards); "" when there is none and is dropped below */
        lines.push(self.c.session_context(item));
        /* YOLO on for this item: the lines; off: unchanged */
        let yolo = self.c.yolo_on(item);
        lines.push("How to work with Steve — use the steve MCP tools:".into());
        lines.push(crate::yolo::agent::UPDATE.into());
        if yolo {
            use crate::yolo::agent as y;
            lines.extend([y::ON, y::ASK, y::APPROVAL, y::DELIVER].map(String::from));
            /* 完全放开: the harness asks nothing, so the agent hears the brakes itself */
            lines.extend(self.c.yolo_full_lines(item));
        } else {
            lines.push("- steve_ask: when you need a decision from the user. Never ask questions in plain text. After calling it, end your turn.".into());
            lines.push("- steve_request_approval: before anything with outside effects (deploy, release, push, send, pay, delete). After calling it, end your turn.".into());
            lines.push("- steve_deliver: when the goal is met, with a Chinese summary, the outputs (branch, commits, files) and the checks you actually ran. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open. A delivery with other open steps is refused. Then end your turn.".into());
        }
        if p.as_ref().is_some_and(|p| p.release.is_some())
            && item.branch.as_deref().is_some_and(|b| !b.is_empty())
        {
            let mut l = String::from(
                "- steve_publish: only when the item changed code (commits on your branch), after the user has accepted the item and asked for it to go live. Commit everything on your branch first. Then end your turn. If you changed nothing (an explanation, a review, an answer), don't call it: accepting the item finishes it.",
            );
            if yolo && p.as_ref().is_some_and(|p| self.c.yolo_releases_itself(p)) {
                l.push_str(crate::yolo::agent::PUBLISH);
            }
            lines.push(l);
            /* the project's own rule of done (ST-31): what finishes the item, and what does not hold it */
            if let Some(rule) = p
                .as_ref()
                .and_then(|p| p.release.as_ref())
                .and_then(|r| r.done_when.as_deref())
                .filter(|r| *r == crate::upstream::DONE_MERGE || *r == crate::upstream::DONE_TAG)
            {
                lines.push(format!(
                    "- Done in this project means: {}. Once the user accepted your delivery, that is all that is left; a CI or Release build it starts is followed by Steve on its own — do not wait for it, end your turn. Steve checks the remote itself; never merge, tag or push again what already is there.",
                    if rule == crate::upstream::DONE_TAG {
                        "your commits are merged into the remote's default branch (the PR is merged) and a tag containing them is pushed"
                    } else {
                        "your commits are merged into the remote's default branch (the PR is merged)"
                    }
                ));
            }
        }
        lines.push("Do the work yourself; do not stop to ask for confirmation you don't need. Reply to the user in Chinese.".into());
        lines.retain(|l| !l.is_empty());
        Ok(lines.join("\n"))
    }

    /* ── items and sessions ── */
    /// The item's working directory: its scratch dir, the project dir, or a worktree on `steve/<id>`.
    fn prepare_cwd(&self, item: &Item) -> Result<(String, Option<String>)> {
        let Some(p) = self.db().project(item.project_id.as_deref())? else {
            let dir = self.cfg().scratch.join(&item.id);
            std::fs::create_dir_all(&dir)?;
            return Ok((dir.to_string_lossy().into(), None));
        };
        let path = Path::new(&p.path);
        if !path.exists() {
            return Err(msg(steve_i18n::t!(
                "core.project.dir_missing",
                path = p.path
            )));
        }
        if p.kind != "git" || !p.worktree || !git::is_git(path) {
            return Ok((p.path.clone(), None));
        }
        let branch = format!("steve/{}", item.id.to_lowercase());
        let wt: PathBuf = self.cfg().worktrees.join(&item.id);
        /* a new item waits for disk space; one that had a worktree gets its sources back (megabytes) */
        if !wt.exists() && item.cwd.as_deref().is_none_or(str::is_empty) {
            self.c.disk_floor()?;
        }
        git::add_worktree(path, &wt, &branch)
            .map_err(|e| msg(format!("Command failed: git worktree add\n{e}")))?;
        Ok((wt.to_string_lossy().into(), Some(branch)))
    }

    pub(crate) fn create_item(&mut self, n: CreateItem) -> Result<Item> {
        let db = self.db();
        let ws = n.ws.as_str();
        let bot = match n.bot.as_deref().filter(|b| !b.is_empty()) {
            Some(b) if db.bot_in_workspace(b, ws)? => db.bot(b)?,
            _ => None,
        };
        let bot = match bot {
            Some(b) => b,
            None => db.main_bot()?,
        };
        let project = match &n.project {
            Some(p) => db.project(p.as_deref().filter(|p| !p.is_empty()))?,
            None => db.project(bot.default_project.as_deref())?,
        };
        let project = project.filter(|p| p.workspace_id == ws);
        /* exactly one category, in the same write as the item: Steve's pick if nobody named one */
        let (tags, guessed) = crate::kinds::settle(db, ws, &n.tag_ids, &n.title, &n.goal)?;
        let item = db.item_create(
            ws,
            &NewItem {
                title: n.title.clone(),
                goal: n.goal.clone(),
                bot_id: bot.id.clone(),
                project_id: project.as_ref().map(|p| p.id.clone()),
                kind: None,
                status: Some(if n.propose { "wait" } else { "run" }.into()),
                stage: Some(if n.propose { "规划" } else { "定义" }.into()),
                source: n.source.clone(),
                parent: n.parent.clone(),
                tags,
            },
        )?;
        let where_ = project
            .as_ref()
            .map(|p| format!("，项目 {}", p.name))
            .unwrap_or_default();
        let kind = guessed
            .map(|g| format!("，按目标归为 {}", g.kind))
            .unwrap_or_default();
        db.timeline_add(
            &item.id,
            "steve",
            "sys",
            &format!("Steve 建立事项，交给{}{where_}{kind}", bot.display()),
            None,
        )?;
        let mut detail = json!({"bot": bot.id, "project": item.project_id});
        if let Some((a, b)) = &n.source {
            detail["source"] = json!([a, b]);
        }
        db.audit(
            n.actor.as_deref().unwrap_or("steve"),
            "item.create",
            &item.id,
            &detail,
            ws,
        )?;
        if n.propose {
            db.decision_create(NewDecision {
                workspace_id: ws.into(),
                kind: "propose".into(),
                subject_type: "item".into(),
                subject_id: item.id.clone(),
                item_id: Some(item.id.clone()),
                question: "先给你方案：按这个方向做？".into(),
                draft: Some(n.goal.clone()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                ..Default::default()
            })?;
            db.item_patch(&item.id, patch![now: "等你确认后开始"])?;
        } else {
            self.start_or_fail_files(&item.id, n.opening.as_deref(), None, &n.files)?;
        }
        self.hook(Point::Created, &item.id)?;
        self.item(&item.id)
    }

    /// `void startSession(…).catch((e) => fail(item, "开不了会话：…"))`
    pub(crate) fn start_or_fail(
        &mut self,
        item: &str,
        opening: Option<&str>,
        bot: Option<&str>,
    ) -> Result<()> {
        self.start_or_fail_files(item, opening, bot, &[])
    }

    /// [`Self::start_or_fail`] with attachments. The start runs in zh-CN: its error becomes the fail card,
    /// the timeline line and the flow update, stored for every member (and compared to find the same card).
    pub(crate) fn start_or_fail_files(
        &mut self,
        item: &str,
        opening: Option<&str>,
        bot: Option<&str>,
        files: &[FileRef],
    ) -> Result<()> {
        // the error is formatted inside too: an `ApiError`'s text is rendered when displayed
        steve_i18n::stored(
            || match self.start_session_files(item, opening, bot, files) {
                Err(e) => self.fail(item, &format!("开不了会话：{e}")),
                Ok(_) => Ok(()),
            },
        )
    }

    pub(crate) fn start_session(
        &mut self,
        item_id: &str,
        opening: Option<&str>,
        bot_override: Option<&str>,
    ) -> Result<Session> {
        self.start_session_files(item_id, opening, bot_override, &[])
    }

    pub(crate) fn start_session_files(
        &mut self,
        item_id: &str,
        opening: Option<&str>,
        bot_override: Option<&str>,
        files: &[FileRef],
    ) -> Result<Session> {
        self.lc_guard_open(item_id)?;
        /* preparing the worktree is the cycle's first actual work */
        self.lc_start(item_id)?;
        if let Some(b) = bot_override {
            self.db().item_patch(item_id, patch![bot_id: b])?;
        }
        let item = self.item(item_id)?;
        let bot = match self.db().bot(item.bot_id.as_deref().unwrap_or("steve"))? {
            Some(b) => b,
            None => self.db().main_bot()?,
        };
        let (cwd, branch) = match item
            .cwd
            .as_deref()
            .filter(|c| !c.is_empty() && Path::new(c).exists())
        {
            Some(c) => (c.to_string(), item.branch.clone()),
            None => self.prepare_cwd(&item)?,
        };
        self.db().item_patch(item_id, patch![cwd: cwd.as_str(), branch: branch.as_deref(), stage: "执行", status: "run", now: "正在开始"])?;
        let tok = token();
        let model = bot.model.clone().or_else(|| {
            self.c
                .models()
                .agent_model(&item.workspace_id, &bot.harness)
        });
        let s = self.db().session_create(&NewSession {
            workspace_id: item.workspace_id.clone(),
            item_id: Some(item_id.into()),
            bot_id: Some(bot.id.clone()),
            harness: bot.harness.clone(),
            model,
            reasoning_effort: bot.reasoning_effort.clone(),
            cwd,
            title: item.title.clone(),
            token: tok.clone(),
        })?;
        self.st.tokens.insert(s.id.clone(), tok);
        self.rt().created(&s.id);
        self.db().timeline_add(
            item_id,
            "steve",
            "session",
            &format!("开始会话 #{}", s.no),
            Some(&s.id),
        )?;
        let it = self.item(item_id)?;
        let head = self.protocol(&it, &bot, &s)?;
        self.c
            .0
            .yolo_sessions
            .started(&s.id, self.c.yolo_level(&it));
        let text = match opening.filter(|o| !o.is_empty()) {
            Some(o) => format!("{head}\n\n{o}"),
            None => head,
        };
        self.rt().prompt(&s.id, &text, "steve", files)?;
        Ok(s)
    }

    /// Session tokens live only in memory: after a restart each session gets a fresh one on its next start.
    pub(crate) fn mcp_endpoint(&mut self, sid: &str) -> Result<McpEndpoint> {
        let t = match self.st.tokens.get(sid) {
            Some(t) => t.clone(),
            None => {
                let t = token();
                self.st.tokens.insert(sid.into(), t.clone());
                self.db().rotate_token(sid, &t)?;
                t
            }
        };
        Ok(McpEndpoint {
            url: format!("http://127.0.0.1:{}/mcp", self.cfg().port),
            token: t,
        })
    }

    pub(crate) fn fail(&mut self, item_id: &str, why: &str) -> Result<()> {
        let i = self.item(item_id)?;
        /* one card per trouble: the same one still open is not asked again; an older stop card gives way
         * to the new one (a card about an effect nobody knows stays, it waits on you) */
        let open = self.db().decisions_of_item(item_id)?;
        if open
            .iter()
            .any(|d| d.status == "open" && d.kind == "fail" && d.question == why)
        {
            return settle(self.db(), item_id);
        }
        crate::lifecycle::supersede_where(
            self.db(),
            item_id,
            |d| {
                d.kind == "fail"
                    && matches!(d.subject_type.as_str(), "item" | crate::stall::STALL)
                    && !crate::cycle::unknown_effect_card(d)
                    && !d.options.iter().any(|o| o.starts_with("release_"))
            },
            "有了新的情况",
        )?;
        let mut opts: Vec<String> = vec!["continue".into(), "delivered".into(), "look".into()];
        if i.bot_id.as_deref() != Some("steve") {
            opts.push("tosteve".into());
        }
        let d = self.db().decision_create(NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "fail".into(),
            subject_type: "item".into(),
            subject_id: item_id.into(),
            item_id: Some(item_id.into()),
            question: why.into(),
            options: Some(opts),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        self.mark_fail(&i.workspace_id, item_id, why, &d.id); // stats (ST3)
        self.db().timeline_add(item_id, "steve", "sys", why, None)?;
        flow_quiet(
            self.db(),
            item_id,
            Update {
                kind: "fail",
                text: format!("{item_id} 卡住了：{why}"),
                decision: Some(d.id),
                ..Default::default()
            },
        );
        settle(self.db(), item_id)
    }
    /* ── Steve tools ── */
    fn by(s: &Session) -> String {
        s.bot_id.clone().unwrap_or_else(|| "agent".into())
    }

    pub(crate) fn tool_update(&mut self, s: &Session, now: &str, plan: &[PlanStep]) -> Result<()> {
        let Some(item) = s.item_id.as_deref() else {
            return Ok(());
        };
        let i = self.item(item)?;
        if i.phase == crate::cycle::CLOSED {
            /* the cycle is over: a plan sent after it is noted, never rewrites what closed */
            self.db().timeline_add(
                item,
                &Self::by(s),
                "sys",
                "事项已经结束，这次计划更新只记录，不改变结果",
                Some(&s.id),
            )?;
            return Ok(());
        }
        supersede(self.db(), item, &["fail"], "Bot 继续工作了")?;
        recover_quiet(self.db(), item);
        let i = self.item(item)?;
        let plan: Vec<PlanStep> = plan
            .iter()
            .map(|p| PlanStep {
                left: false,
                ..p.clone()
            })
            .collect();
        /* a plan that drops steps of the one before starts a new round: the old one is kept, not overwritten */
        let turn = plan_turn(&i.plan, &plan);
        if let Some(end) = turn {
            self.db()
                .plan_round_add(&i.workspace_id, item, &i.plan, end, &Self::by(s))?;
        }
        self.db().item_patch(item, patch![now: now, plan: plan])?;
        /* the bot reports progress, so it is working: a wait without a card (a new cycle it took up) is over */
        if i.status == "wait" {
            settle(self.db(), item)?;
        }
        let steps = |p: &[PlanStep]| p.iter().map(|x| x.step.clone()).collect::<Vec<_>>();
        let note = match turn {
            Some("done") => "上一轮计划已完成，开始新一轮计划".to_string(),
            Some(_) => format!(
                "换了一版计划，上一版没做完的 {} 步已废弃",
                i.plan.iter().filter(|p| !p.done).count()
            ),
            None if steps(&i.plan) != steps(&plan) => "更新了计划".to_string(),
            None => String::new(),
        };
        if !note.is_empty() {
            self.db()
                .timeline_add(item, &Self::by(s), "sys", &note, Some(&s.id))?;
        }
        /* a result waiting to close whose plan has open steps again is not the result any more */
        if i.phase == crate::cycle::CLOSING && plan.iter().any(|p| !p.done) {
            self.lc_invalidate(item, "计划里又有没做完的步骤")?;
        }
        Ok(())
    }

    pub(crate) fn tool_ask(
        &mut self,
        s: &Session,
        question: &str,
        options: &[String],
        recommended: Option<&str>,
        why: Option<&str>,
    ) -> Result<()> {
        /* the agent's leaning shows on the card (and YOLO may take it): `推荐：…\n理由：…` */
        let draft = crate::yolo::answer::hint_draft(recommended, why);
        let Some(item) = s.item_id.as_deref() else {
            /* a session without an item (forked, imported and continued, or its item was deleted) still gets an answerable card */
            self.db().decision_create(NewDecision {
                workspace_id: s.workspace_id.clone(),
                kind: "ask".into(),
                subject_type: "question".into(),
                subject_id: s.id.clone(),
                item_id: None,
                session_id: Some(s.id.clone()),
                question: question.into(),
                options: Some(options.to_vec()),
                draft: draft.clone(),
                requested_by: Self::by(s),
            })?;
            self.st.pending.insert(s.id.clone(), Mark::Asked);
            return Ok(());
        };
        supersede(self.db(), item, &["ask", "fail"], "Bot 提了新的问题")?;
        let d = self.db().decision_create(NewDecision {
            workspace_id: s.workspace_id.clone(),
            kind: "ask".into(),
            subject_type: "question".into(),
            subject_id: s.id.clone(),
            item_id: Some(item.into()),
            session_id: Some(s.id.clone()),
            question: question.into(),
            options: Some(options.to_vec()),
            draft,
            requested_by: Self::by(s),
        })?;
        self.db().item_patch(item, patch![now: "在等你回答"])?;
        flow_quiet(
            self.db(),
            item,
            Update {
                kind: "ask",
                text: question.into(),
                decision: Some(d.id),
                ..Default::default()
            },
        );
        self.st.pending.insert(s.id.clone(), Mark::Asked);
        settle(self.db(), item)
    }

    pub(crate) fn tool_request_approval(
        &mut self,
        s: &Session,
        action: &str,
        detail: &str,
    ) -> Result<()> {
        let Some(item) = s.item_id.as_deref() else {
            self.db().decision_create(NewDecision {
                workspace_id: s.workspace_id.clone(),
                kind: "approve".into(),
                subject_type: "action".into(),
                subject_id: s.id.clone(),
                item_id: None,
                session_id: Some(s.id.clone()),
                question: action.into(),
                draft: Some(detail.into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: Self::by(s),
            })?;
            self.st.pending.insert(s.id.clone(), Mark::Approval);
            return Ok(());
        };
        supersede(self.db(), item, &["ask", "fail"], "Bot 继续往下做了")?;
        let d = self.db().decision_create(NewDecision {
            workspace_id: s.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: "action".into(),
            subject_id: s.id.clone(),
            item_id: Some(item.into()),
            session_id: Some(s.id.clone()),
            question: action.into(),
            draft: Some(detail.into()),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: Self::by(s),
        })?;
        self.db().item_patch(item, patch![now: "在等你批准"])?;
        flow_quiet(
            self.db(),
            item,
            Update {
                kind: "ask",
                text: format!("要你批准：{action}"),
                decision: Some(d.id),
                ..Default::default()
            },
        );
        self.st.pending.insert(s.id.clone(), Mark::Approval);
        settle(self.db(), item)
    }

    pub(crate) fn tool_deliver(
        &mut self,
        s: &Session,
        summary: &str,
        outputs: &[Output],
        checks: &[Check],
        unfinished: &[String],
    ) -> Result<()> {
        let Some(item_id) = s.item_id.as_deref() else {
            return Ok(());
        };
        /* every plan step is ticked off or declared left open before anything is delivered */
        let item = self.item(item_id)?;
        let open: Vec<&str> = item
            .plan
            .iter()
            .filter(|p| !p.done && !unfinished.contains(&p.step))
            .map(|p| p.step.as_str())
            .collect();
        if !open.is_empty() {
            return Err(msg(format!(
                "Not delivered: these plan steps are not done yet: {}. Finish them and tick them off with steve_update, or list the steps you deliberately leave open in `unfinished`.",
                open.join("; ")
            )));
        }
        /* a new delivery also replaces an older one still waiting for acceptance */
        supersede(
            self.db(),
            item_id,
            &["ask", "fail", "accept"],
            "Bot 已经交付",
        )?;
        let plan: Vec<PlanStep> = item
            .plan
            .iter()
            .map(|p| PlanStep {
                left: !p.done,
                ..p.clone()
            })
            .collect();
        let left = plan_note(&plan);
        if plan != item.plan {
            self.db().item_patch(item_id, patch![plan: plan])?;
        }
        for o in outputs {
            self.db().artifact_add(
                item_id,
                &o.kind,
                &o.title,
                o.location.as_deref(),
                o.role.as_deref(),
            )?;
        }
        let check_text = if checks.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n检查：{}",
                checks
                    .iter()
                    .map(|c| format!("{} {}", if c.passed { "✓" } else { "✗" }, c.name))
                    .collect::<Vec<_>>()
                    .join("；")
            )
        };
        self.db()
            .timeline_add(item_id, &Self::by(s), "sys", "交付，等你验收", Some(&s.id))?;
        /* the acceptance policy: automatic needs evidence (checks, all passed) and nothing left open;
         * without it a human decides. Waived records that nobody confirmed. */
        let evidence: Vec<String> = checks
            .iter()
            .map(|c| format!("{} {}", if c.passed { "✓" } else { "✗" }, c.name))
            .collect();
        let policy = match self.accept_of(&item)?.as_str() {
            "waived" => crate::cycle::Policy::Waived,
            "auto" if !checks.is_empty() && checks.iter().all(|c| c.passed) && left.is_empty() => {
                crate::cycle::Policy::Automatic
            }
            _ => crate::cycle::Policy::Human,
        };
        self.st.pending.insert(s.id.clone(), Mark::Delivered);
        let (back, full) = crate::thread::flowback::delivery(self.db(), item_id, summary)?;
        if policy != crate::cycle::Policy::Human {
            flow_quiet(
                self.db(),
                item_id,
                Update {
                    kind: "deliver",
                    text: back.clone(),
                    full,
                    ..Default::default()
                },
            );
            self.mark_deliver(&item.workspace_id, item_id, &Self::by(s), None); // stats (ST3)
            let line = if policy == crate::cycle::Policy::Automatic {
                "自检全部通过，自动验收"
            } else {
                "免确认：不需要验收"
            };
            self.db()
                .timeline_add(item_id, "steve", "sys", line, None)?;
            /* the request returns at once; the close waits for this very turn and anything required */
            self.lc_request_complete(item_id, &Self::by(s), policy, true, line, None, evidence)?;
            return Ok(());
        }
        /* a delivery waiting for acceptance before this one is void */
        self.lc_invalidate(item_id, "有新的交付，上一次交付作废")?;
        let d = self.db().decision_create(NewDecision {
            workspace_id: item.workspace_id.clone(),
            kind: "accept".into(),
            subject_type: "item".into(),
            subject_id: item_id.into(),
            item_id: Some(item_id.into()),
            session_id: Some(s.id.clone()),
            question: "做完了，请验收".into(),
            draft: Some(format!("{summary}{check_text}{left}")),
            options: Some(vec!["pass".into(), "rework".into()]),
            requested_by: Self::by(s),
        })?;
        self.mark_deliver(&item.workspace_id, item_id, &Self::by(s), Some(&d.id)); // stats (ST3)
        self.lc_request_complete(
            item_id,
            &Self::by(s),
            crate::cycle::Policy::Human,
            false,
            "交付，等你验收",
            Some(d.id.clone()),
            evidence,
        )?;
        self.db().item_patch(
            item_id,
            patch![stage: "交付与验收", now: "交付了，等你验收"],
        )?;
        flow_quiet(
            self.db(),
            item_id,
            Update {
                kind: "deliver",
                text: back,
                decision: Some(d.id),
                full,
                ..Default::default()
            },
        );
        settle(self.db(), item_id)
    }

    pub(crate) fn tool_publish(&mut self, s: &Session, branch: &str, summary: &str) -> Result<()> {
        let Some(item_id) = s.item_id.as_deref() else {
            return Ok(());
        };
        let item = self.item(item_id)?;
        let p = self.db().project(item.project_id.as_deref())?;
        let Some(p) = p.filter(|p| p.release.is_some()) else {
            self.db().event_add(
                &s.id,
                "system",
                json!({"text": "这个项目没有配置发布动作，不能发布。"}),
            )?;
            return Ok(());
        };
        if self.nothing_to_release(&item)? {
            return self.publish_nothing(&item, s);
        }
        /* a project finished by its completion rule merges through its target's lane (one card per batch
         * there, not one release card per item here), when the lane can release it that way: merged is
         * released, or the repository cuts its own tagged releases (ops/cut-release.py) */
        let rule = p.release.as_ref().and_then(|r| r.done_when.as_deref());
        let cmd = p.release.as_ref().map(|r| r.cmd.trim()).unwrap_or_default();
        let cut = Path::new(&p.path).join("ops/cut-release.py");
        /* the lane runs no release command of the project's own: one it would skip keeps the old path */
        let lane_can = match rule {
            Some(steve_store::DONE_MERGE) => cmd.is_empty(),
            /* the repository's own cut-release, the one the lane runs in its batch: the same file, not
             * one that only looks like it */
            Some(steve_store::DONE_TAG) => {
                let same = |c: &str| {
                    let c = Path::new(&p.path).join(c);
                    c.canonicalize()
                        .ok()
                        .zip(cut.canonicalize().ok())
                        .is_some_and(|(a, b)| a == b)
                };
                cut.is_file() && (cmd.is_empty() || same(cmd))
            }
            _ => false,
        };
        if lane_can {
            let req = self.c.lane_enqueue_item(&item, &p.path)?;
            let where_ = req
                .unit
                .map(|u| format!("MQ-{u}"))
                .unwrap_or_else(|| "下一批".into());
            self.db().event_add(
                &s.id,
                "system",
                json!({"text": format!("已进入合并通道（{where_}）：Steve 会和同一目标的其他改动一起合并、检查、发布，完成后这件事项自动结束。结束这一回合即可。")}),
            )?;
            self.db()
                .item_patch(item_id, patch![now: format!("在合并通道里（{where_}）")])?;
            return settle(self.db(), item_id);
        }
        self.db().decision_create(NewDecision {
            workspace_id: s.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: "release".into(),
            subject_id: branch.into(),
            item_id: Some(item_id.into()),
            session_id: Some(s.id.clone()),
            question: format!("发布新版本（{} · 分支 {branch}）", p.name),
            draft: Some(format!("{summary}\n\n只改了前端时不重启，切到新前端后刷新页面即可；改到 Hub 时 Steve 会重启一次，大约一分钟。检查不通过会自动回到当前版本。")),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: Self::by(s),
        })?;
        self.db().item_patch(item_id, patch![now: "等你批准发布"])?;
        self.st.pending.insert(s.id.clone(), Mark::Approval);
        settle(self.db(), item_id)
    }

    /// steve_publish with nothing on the branch: no release decision. An accepted item is finished; one not yet
    /// accepted stays as it is (accepting it will finish it). The agent gets a polite refusal either way.
    fn publish_nothing(&mut self, item: &Item, s: &Session) -> Result<()> {
        let accepted = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .rev()
            .find(|d| d.kind == "accept")
            .is_some_and(|d| d.status == "resolved" && d.result["choice"] == "pass");
        if !accepted || item.is_settled() {
            return Err(msg(format!(
                "{NOTHING_TO_RELEASE}，不用发布。{}",
                if item.is_settled() {
                    "结束这一回合即可。"
                } else {
                    "交付后用户验收通过，事项会直接完成。"
                }
            )));
        }
        /* nothing on the branch is not proof that the accepted result is live: you are asked */
        self.db().timeline_add(
            &item.id,
            &Self::by(s),
            "sys",
            "请求发布，但分支上没有需要发布的改动",
            Some(&s.id),
        )?;
        self.st.pending.insert(s.id.clone(), Mark::Delivered);
        self.release_unproven(&item.id)?;
        Err(msg(format!(
            "{NOTHING_TO_RELEASE}，不用发布；已请用户确认它是否已经上线。结束这一回合即可。"
        )))
    }

    /// A turn ended: if the agent neither asked, delivered nor is waiting, it stalled — tell the user.
    pub(crate) fn on_turn_end(&mut self, t: TurnEnd) -> Result<()> {
        let Some(session) = self.db().session(&t.session_id)? else {
            return Ok(());
        };
        let Some(item_id) = session.item_id.as_deref() else {
            self.st.pending.remove(&session.id);
            return Ok(());
        };
        let mark = self.st.pending.remove(&session.id);
        let was_resume = self.st.resumed.remove(&session.id);
        /* the turn answered something forwarded from the conversation: its reply goes back there */
        if t.stop_reason != "cancelled"
            && let Some(r) = self.c.forwarded_take(item_id)
        {
            let last = self.last_agent_text(&session.id)?;
            if !last.is_empty() {
                flow_quiet(
                    self.db(),
                    item_id,
                    Update {
                        kind: "reply",
                        text: js_head(&last, 600).to_string(),
                        reference: Some(r),
                        ..Default::default()
                    },
                );
            }
        }
        let Some(item) = self.db().item(item_id)? else {
            return Ok(());
        };
        /* a closing waits for this turn (among others): look at it again; what it still waits on is said */
        if item.phase == crate::cycle::CLOSING {
            /* the turn may have merged or tagged: a completion rule is looked up afresh */
            self.st.probed.remove(item_id);
            if !self.lc_try_close(item_id)? {
                self.release_not_requested(item_id)?;
            }
            return Ok(());
        }
        if item.is_settled() {
            return Ok(());
        }
        let error = t.error.as_deref().filter(|e| !e.is_empty());
        if was_resume && t.stop_reason == "error" {
            return self.fail(
                item_id,
                &format!(
                    "{INTERRUPTED}；自动继续没有成功：{}",
                    error.unwrap_or("会话出错")
                ),
            );
        }
        if mark.is_some() {
            return settle(self.db(), item_id);
        }
        if t.stop_reason == "cancelled" {
            return self
                .db()
                .item_patch(item_id, patch![now: "你中断了这一回合"]);
        }
        let why = match error {
            Some(e) => format!("会话出错：{e}"),
            None => {
                let last = self.last_agent_text(&session.id)?;
                let tail = if last.is_empty() {
                    String::new()
                } else {
                    format!("：{}", js_head(&last, 160))
                };
                let why = format!("Agent 停下来了，没有交付也没有提问{tail}");
                /* Steve looks first: done by the project's rule, or worth another go */
                let stop = crate::takeover::Stop::Stopped { why, last };
                match self.takeover(item_id, &session.id, stop)? {
                    Some(why) => why,
                    None => return Ok(()),
                }
            }
        };
        self.fail(item_id, &why)
    }

    pub(crate) fn last_agent_text(&self, sid: &str) -> Result<String> {
        let evs = self.db().events_after(sid, 0)?;
        let mut tail: Vec<String> = vec![];
        let mut len = 0;
        for t in evs
            .iter()
            .rev()
            .filter_map(|e| steve_transcript::assistant_text(&e.kind, &e.data))
        {
            if len >= 400 {
                break;
            }
            let t = t.to_string();
            len += js_len(&t);
            tail.push(t);
        }
        tail.reverse();
        Ok(tail.concat().trim().to_string())
    }

    /* ── finishing and prerequisites ── */
    /// The required release of an accepted result has nothing to publish and no release of this cycle
    /// proves it live: a card asks you (already live / no release needed / look at it).
    pub(crate) fn release_unproven(&mut self, item_id: &str) -> Result<()> {
        if self
            .lc_open_op(item_id, crate::cycle::OP_RELEASE)?
            .is_none()
        {
            return Ok(());
        }
        let i = self.item(item_id)?;
        supersede(self.db(), item_id, &["fail"], "需要确认是否已上线")?;
        self.db().decision_create(NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "fail".into(),
            subject_type: "item".into(),
            subject_id: item_id.into(),
            item_id: Some(item_id.into()),
            question: "分支上没有需要发布的改动，也没有本周期包含这个结果的发布记录：它已经上线了，还是不需要发布？".into(),
            options: Some(vec!["release_live".into(), "release_waive".into(), "look".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        self.db()
            .item_patch(item_id, patch![now: "等你确认是否已上线"])?;
        Ok(())
    }

    /// The agent's turn ended and the accepted result still waits only for its release, which nobody
    /// requested (no release card, no release running, nothing queued for the agent): nothing would ever
    /// move it, so a card asks you whether it is still needed instead of the item waiting in silence.
    pub(crate) fn release_not_requested(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        let Some(c) = crate::cycle::closing_of(&i)
            .filter(|c| i.phase == crate::cycle::CLOSING && c.kind == crate::cycle::COMPLETE)
        else {
            return Ok(());
        };
        let Some(op) = self.lc_open_op(item_id, crate::cycle::OP_RELEASE)? else {
            return Ok(());
        };
        if op.status != "pending" || self.st.releasing.as_ref().is_some_and(|r| r.item == i.id) {
            return Ok(());
        }
        let g = self.lc_gate(&i, &c)?;
        if g.turn || g.input || g.decision || g.acceptance || g.changed {
            return Ok(());
        }
        if self
            .db()
            .decisions_of_item(item_id)?
            .iter()
            .any(|d| d.status == "open")
        {
            return Ok(());
        }
        self.db().decision_create(NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "fail".into(),
            subject_type: "item".into(),
            subject_id: item_id.into(),
            item_id: Some(item_id.into()),
            question: "验收已经通过，只差发布，但 Agent 结束了这一回合也没有请求发布：还要发布吗？"
                .into(),
            options: Some(vec![
                "release_waive".into(),
                "continue".into(),
                "look".into(),
            ]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        self.db()
            .item_patch(item_id, patch![now: "只差发布，等你决定是否还要发布"])?;
        Ok(())
    }

    /// A project with a release action: accepted is not finished; the item stays open until the release is live.
    pub(crate) fn release_pending(&self, i: &Item) -> Result<bool> {
        Ok(i.branch.as_deref().is_some_and(|b| !b.is_empty())
            && self
                .db()
                .project(i.project_id.as_deref())?
                .is_some_and(|p| p.release.is_some()))
    }

    pub(crate) fn await_release(&mut self, item_id: &str, to: &To) -> Result<()> {
        let i = self.item(item_id)?;
        self.db()
            .item_patch(item_id, patch![stage: "收尾", now: "等待发布"])?;
        settle(self.db(), item_id)?;
        self.send(
            to,
            &format!(
                "验收通过。确认改动都已提交到 {} 后，用 steve_publish 请求发布。如果用户说过不发布，不用再问，直接结束这一回合，Steve 会请用户确认。",
                i.branch.unwrap_or_default()
            ),
        )
    }

    /// A finished or dropped item gives back its worktree (only when clean; for a finished item only once its
    /// branch is merged) and its branch once it is merged into main; unmerged work stays.
    pub(crate) fn tidy_branch(&mut self, item_id: &str) -> Result<()> {
        self.tidy_branch_now(item_id).map(|_| ())
    }

    /// [`Self::tidy_branch`], telling whether it could decide: `false` while a session of the item is
    /// still busy (nothing was touched; ask again later). `true` also when there is nothing to tidy, or
    /// the worktree is kept on purpose (dirty, unmerged).
    pub(crate) fn tidy_branch_now(&mut self, item_id: &str) -> Result<bool> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(true);
        };
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(true);
        };
        let (Some(branch), Some(cwd)) = (
            i.branch.as_deref().filter(|b| !b.is_empty()),
            i.cwd.as_deref().filter(|c| !c.is_empty()),
        ) else {
            return Ok(true);
        };
        let prefix = format!("{}/", self.cfg().worktrees.to_string_lossy());
        if !i.is_finished() || !cwd.starts_with(&prefix) || !git::is_git(Path::new(&p.path)) {
            return Ok(true);
        }
        let ss = self.db().sessions_of_item(item_id)?;
        if ss.iter().any(|s| self.busy(&s.id)) {
            return Ok(false);
        }
        for s in &ss {
            self.rt().release(&s.id);
        }
        /* the caches go to the trash first (a rename): removing the worktree under the lock is then quick,
         * and they are deleted outside it */
        let shed = self.shed_closed_caches(item_id, Path::new(cwd));
        if !shed.is_empty() {
            self.c.purge_soon();
        }
        /* an archived item gives back a clean worktree even when its branch is not in main: the branch
         * stays, and the worktree can be made again from it */
        let (wt, br) = git::tidy_worktree(
            Path::new(&p.path),
            Path::new(cwd),
            branch,
            i.status == "done" && !i.archived,
            "main",
        );
        let mut said = vec![];
        if wt == git::WorktreeTidy::Removed {
            let head = git::git(
                Path::new(&p.path),
                &["rev-parse", "--short", &format!("refs/heads/{branch}")],
            );
            said.push(match (br, head) {
                (git::BranchTidy::Kept, Ok(h)) => {
                    format!("删除了工作区 {cwd}（分支 {branch} 保留在 {h}，继续时从分支重建）")
                }
                _ => format!("删除了工作区 {cwd}"),
            });
        }
        if br == git::BranchTidy::Deleted {
            said.push(format!("删除了已合并的分支 {branch}"));
        }
        if !said.is_empty() {
            self.db()
                .timeline_add(item_id, "steve", "sys", &said.join("；"), None)?;
        }
        Ok(true)
    }

    /// A removed worktree comes back (from the kept branch, or fresh from main) when the item continues.
    pub(crate) fn ensure_cwd(&self, item_id: &str) -> Result<()> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        if i.branch.as_deref().is_some_and(|b| !b.is_empty())
            && i.cwd
                .as_deref()
                .is_some_and(|c| !c.is_empty() && !Path::new(c).exists())
            && let Err(e) = self.prepare_cwd(&i)
        {
            tracing::error!("worktree {e}");
        }
        Ok(())
    }

    pub(crate) fn check_blocked(&mut self, item_id: &str) -> Result<()> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        if i.status != "block" || !deps_open(self.db(), item_id)?.is_empty() {
            return Ok(());
        }
        let auto = self
            .db()
            .setting(&i.workspace_id, "depsAfter", "自动开始")?
            == "自动开始";
        let note = if auto {
            "前置事项都已完成，Steve 自动开始"
        } else {
            "前置事项都已完成，等你开始"
        };
        self.db()
            .timeline_add(item_id, "steve", "sys", note, None)?;
        if !auto {
            self.db().decision_create(NewDecision {
                workspace_id: i.workspace_id.clone(),
                kind: "propose".into(),
                subject_type: "item".into(),
                subject_id: item_id.into(),
                item_id: Some(item_id.into()),
                question: "前置事项都已完成，现在开始？".into(),
                draft: Some(self.db().goal(item_id)?),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                ..Default::default()
            })?;
            return self
                .db()
                .item_patch(item_id, patch![status: "wait", now: "前置完成，等你开始"]);
        }
        let mut summary = vec![];
        for d in self.db().deps_of(item_id)? {
            let (title, now_) = self
                .db()
                .item(&d)?
                .map(|x| (x.title, x.now))
                .unwrap_or_default();
            summary.push(format!("{d}「{title}」：{now_}"));
        }
        self.db().item_patch(
            item_id,
            patch![status: "run", now: "前置事项都已完成，开始处理"],
        )?;
        let m = format!(
            "前置事项都已完成：\n{}\n现在开始这个事项。",
            summary.join("\n")
        );
        match self.active_session(item_id)? {
            Some(s) => self.prompt(&s.id, &m, "steve"),
            None => self.start_or_fail(item_id, Some(&m), None)?,
        }
        Ok(())
    }

    fn reaches(&self, from: &str, to: &str, seen: &mut HashSet<String>) -> Result<bool> {
        if !seen.insert(from.into()) {
            return Ok(false);
        }
        for d in self.db().deps_of(from)? {
            if d == to || self.reaches(&d, to, seen)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn add_dep(&mut self, item_id: &str, dep: &str) -> Result<()> {
        if item_id == dep || self.db().deps_of(item_id)?.iter().any(|d| d == dep) {
            return Ok(());
        }
        if self.reaches(dep, item_id, &mut HashSet::new())? {
            return Err(msg(steve_i18n::t!("core.item.dep_cycle")));
        }
        self.db().dep_add(item_id, dep)?;
        self.db()
            .timeline_add(item_id, "me", "sys", &format!("添加前置事项 {dep}"), None)?;
        let i = self.item(item_id)?;
        let busy = self
            .active_session(item_id)?
            .is_some_and(|s| self.busy(&s.id));
        if self.item(dep)?.status != "done" && status_in(&i, &["run", "prop", "pause"]) && !busy {
            self.db().item_patch(
                item_id,
                patch![status: "block", now: "等前置事项完成后自动开始"],
            )?;
        }
        Ok(())
    }

    pub(crate) fn rm_dep(&mut self, item_id: &str, dep: &str) -> Result<()> {
        self.db().dep_remove(item_id, dep)?;
        self.db()
            .timeline_add(item_id, "me", "sys", &format!("移除前置事项 {dep}"), None)?;
        self.check_blocked(item_id)
    }
    /* ── decisions ── */
    pub(crate) fn send(&mut self, to: &To, m: &str) -> Result<()> {
        match to {
            To::Session(sid) => self.prompt(sid, m, "steve"),
            To::Decision { live, item } => {
                if let Some(i) = item {
                    supersede(self.db(), i, &["ask", "fail"], "你已经回复，会话继续")?;
                    recover_quiet(self.db(), i);
                }
                match (live, item) {
                    (Some(s), _) => self.prompt(s, m, "steve"),
                    (None, Some(i)) => self.start_or_fail(i, Some(m), None)?,
                    (None, None) => {}
                }
            }
        }
        Ok(())
    }

    fn new_decision(&self, d: NewDecision) -> Result<Decision> {
        self.db().decision_create(d)
    }

    /// The timeline line of an answer: the user's own (`kind=me`), or for YOLO's a system line
    /// `YOLO：…` by Steve, so it never reads as something the user said.
    pub(crate) fn answer_line(
        &self,
        item: &str,
        actor: &str,
        text: &str,
        sid: Option<&str>,
    ) -> Result<()> {
        if actor == YOLO_ACTOR {
            self.db()
                .timeline_add(item, "steve", "sys", &format!("YOLO：{text}"), sid)
        } else {
            self.db().timeline_add(item, actor, "me", text, sid)
        }
    }

    pub(crate) fn resolve(
        &mut self,
        id: &str,
        choice: &str,
        text: &str,
        actor: &str,
    ) -> Result<Decision> {
        let db = self.db();
        let d = db
            .decision(id)?
            .filter(|d| d.status == "open")
            .ok_or_else(|| msg(steve_i18n::t!("core.decision.handled")))?;
        db.audit(
            actor,
            "decision.resolve",
            id,
            &json!({"choice": choice, "text": text}),
            &d.workspace_id,
        )?;
        let item_id = d.item_id.clone();
        let s = match (&d.session_id, &item_id) {
            (Some(sid), _) => db.session(sid)?,
            (None, Some(i)) => self.active_session(i)?,
            _ => None,
        };
        if item_id
            .as_deref()
            .map(|i| self.db().item(i))
            .transpose()?
            .flatten()
            .is_none()
        {
            return self.resolve_detached(&d, choice, text, s, actor);
        }
        let sid = s.as_ref().map(|s| s.id.clone());
        let to = To::Decision {
            live: s
                .as_ref()
                .filter(|s| s.status != "ended")
                .map(|s| s.id.clone()),
            item: item_id.clone(),
        };
        let iid = || {
            item_id
                .clone()
                .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))
        };
        let reget = |t: &Tx| t.db().decision(id).map(|d| d.expect("decision exists"));
        let tl = |t: &Tx, item: &str, text: &str| t.answer_line(item, actor, text, sid.as_deref());
        match (d.kind.as_str(), d.subject_type.as_str()) {
            ("accept", _) if choice == "pass" => {
                let i = iid()?;
                let before = self.item(&i)?;
                let undo = json!({"status": before.status, "now": before.now, "stage": before.stage, "plan": before.plan});
                self.db().decision_resolve(
                    id,
                    json!({"choice": choice}),
                    "resolved",
                    Some(undo),
                )?;
                self.answer_line(&i, actor, "验收通过", None)?;
                if before.phase == crate::cycle::CLOSING {
                    /* what this result still needs (a release, the modules' wrap-up) is recorded and
                     * started; the close commits once all of it is done and the work has stopped */
                    self.lc_accept(&i, crate::cycle::Policy::Human, Some("你验收通过"))?;
                    if text == "no-release"
                        && let Some(op) = self.lc_open_op(&i, crate::cycle::OP_RELEASE)?
                    {
                        self.lc_op_set(&op.id, "waived", json!({"note": "你选择不发布"}))?;
                    }
                }
                return reget(self);
            }
            ("accept", _) => {
                let i = iid()?;
                self.db().decision_resolve(
                    id,
                    json!({"choice": "rework", "text": text}),
                    "resolved",
                    None,
                )?;
                tl(self, &i, &format!("返工：{text}"))?;
                self.lc_invalidate(&i, "验收没有通过，返工")?;
                self.db()
                    .item_patch(&i, patch![stage: "执行", now: "按你的意见返工"])?;
                self.send(
                    &to,
                    &format!(
                        "验收没有通过，返工意见：{}\n改完再用 steve_deliver 交付。",
                        or(text, "请再检查一遍")
                    ),
                )?;
            }
            ("ask", _) => {
                let i = iid()?;
                /* YOLO answers with the plain answer as `choice` and the whole message for the agent as `text` */
                let yolo = actor == YOLO_ACTOR;
                let answer = if yolo {
                    or(choice, text)
                } else {
                    or(text, choice)
                }
                .to_string();
                self.db()
                    .decision_resolve(id, json!({"answer": answer}), "resolved", None)?;
                if yolo {
                    self.db().timeline_add(
                        &i,
                        "steve",
                        "sys",
                        &format!("YOLO 代答：{answer}"),
                        sid.as_deref(),
                    )?;
                } else {
                    tl(self, &i, &format!("回答：{answer}"))?;
                }
                /* an item without a project that asked which project it belongs to: move it there, a new session continues in it */
                let i0 = self.item(&i)?;
                let attach = if i0.project_id.is_none() {
                    self.db()
                        .projects(&i0.workspace_id)?
                        .into_iter()
                        .find(|p| p.name == answer.trim())
                } else {
                    None
                };
                if let Some(p) = attach {
                    self.db().item_patch(&i, patch![project_id: p.id.as_str(), cwd: Value::Null, branch: Value::Null, now: format!("转到项目 {}", p.name)])?;
                    self.db().timeline_add(
                        &i,
                        "steve",
                        "sys",
                        &format!("关联到项目 {}，开新会话", p.name),
                        None,
                    )?;
                    let last = match &s {
                        Some(s) => {
                            self.db().session_patch(&s.id, patch![status: "ended"])?;
                            self.rt().release(&s.id);
                            js_head(&self.last_agent_text(&s.id)?, 600).to_string()
                        }
                        None => String::new(),
                    };
                    let who = if yolo {
                        "Steve（YOLO，代用户）"
                    } else {
                        "用户"
                    };
                    self.start_or_fail(&i, Some(&format!("{who}把这个事项关联到了项目 {}。上一个会话的最后进展：{last}\n在这个项目里继续。", p.name)), None)?;
                } else {
                    self.db().item_patch(&i, patch![now: "收到回答，继续"])?;
                    if yolo && !text.is_empty() {
                        self.send(&to, text)?;
                    } else {
                        self.send(&to, &format!("用户的回答：{answer}"))?;
                    }
                }
            }
            ("approve", "tool_call") => {
                let opt = (choice != "deny").then_some(choice);
                /* answer first: resolve() only touches open decisions, so marking it resolved first would hide the expiry */
                if self.rt().answer_permission(id, opt) {
                    self.db()
                        .decision_resolve(id, json!({"optionId": opt}), "resolved", None)?;
                } else {
                    self.db()
                        .decision_resolve(id, json!({"expired": true}), "expired", None)?;
                    self.send(&to, "你之前请求权限的那次工具调用已经失效（会话被重启过）。如果还需要，请重新执行。")?;
                }
            }
            ("approve", "release") => {
                let i = iid()?;
                let yes = choice == "yes";
                self.db().decision_resolve(
                    id,
                    json!({"approved": yes, "text": text}),
                    "resolved",
                    None,
                )?;
                /* the note goes into the conversation exactly when it goes to the agent: with a "no" */
                tl(
                    self,
                    &i,
                    &match (yes, text) {
                        (true, _) => format!("批准发布 {}", d.subject_id),
                        (false, "") => format!("不发布 {}", d.subject_id),
                        (false, t) => format!("不发布 {}：{t}", d.subject_id),
                    },
                )?;
                if yes {
                    if self.rebase_for_release(&i, &d.subject_id, &to)? {
                        self.start_release(&d.subject_id, &i)?;
                    }
                } else if !text.is_empty() {
                    /* your no stands for this cycle (release_requirement); the agent first does what you said instead */
                    self.send(&to, &format!("用户没有批准发布：{text}。这个周期不再发布；按用户的意思还要做的（比如合并 PR）做完就结束这一回合，改了代码就用 steve_deliver 重新交付。"))?;
                } else if let Some(op) = self.lc_open_op(&i, crate::cycle::OP_RELEASE)? {
                    /* you decided this result needs no release: the requirement is waived, said so */
                    self.lc_op_set(&op.id, "waived", json!({"note": "你选择不发布"}))?;
                    return reget(self);
                }
            }
            ("approve", _) => {
                let i = iid()?;
                let yes = choice == "yes";
                self.db().decision_resolve(
                    id,
                    json!({"approved": yes, "text": text}),
                    "resolved",
                    None,
                )?;
                /* the conversation line is the agent's message minus its framing, note included */
                let said = match (yes, text) {
                    (_, "") => d.question.clone(),
                    (true, t) => format!("{}。补充：{t}", d.question),
                    (false, t) => format!("{}。原因：{t}", d.question),
                };
                tl(
                    self,
                    &i,
                    &format!("{}{said}", if yes { "批准：" } else { "拒绝：" }),
                )?;
                let m = if yes {
                    format!("用户批准了：{said}")
                } else {
                    format!("用户拒绝了：{said}。不要执行这个操作。")
                };
                self.send(&to, &m)?;
            }
            ("propose", _) => {
                let i = iid()?;
                let before = self.item(&i)?;
                if choice == "yes" {
                    self.db()
                        .decision_resolve(id, json!({"choice": choice}), "resolved", None)?;
                    self.answer_line(&i, actor, "同意开始", None)?;
                    self.db().item_patch(&i, patch![status: "run"])?;
                    match self.active_session(&i)? {
                        Some(a) => self.prompt(&a.id, "用户同意了，开始做。", "steve"),
                        // from a conversation: with what it would have started with right away
                        None => match self.c.thread_start(&before)? {
                            Some((opening, files)) => {
                                self.start_or_fail_files(&i, Some(&opening), None, &files)?;
                            }
                            None => self.start_or_fail(&i, None, None)?,
                        },
                    }
                } else {
                    self.db().decision_resolve(
                        id,
                        json!({"choice": choice}),
                        "resolved",
                        Some(json!({"status": before.status, "now": before.now})),
                    )?;
                    self.answer_line(&i, actor, "不需要", None)?;
                    self.lc_stop(&i, crate::cycle::CANCEL, actor, "你认为不需要", None)?;
                }
                return reget(self);
            }
            ("fail", _) => {
                let i = iid()?;
                let before = self.item(&i)?;
                match choice {
                    "continue" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice, "text": text}),
                            "resolved",
                            None,
                        )?;
                        self.db().item_patch(&i, patch![now: "让它继续"])?;
                        recover_quiet(self.db(), &i);
                        if actor == YOLO_ACTOR {
                            self.answer_line(&i, actor, "让它继续", sid.as_deref())?;
                        }
                        /* accepted and only the release left (release_not_requested): continuing is publishing */
                        let publish = before.phase == crate::cycle::CLOSING
                            && self.lc_open_op(&i, crate::cycle::OP_RELEASE)?.is_some();
                        let next = if publish {
                            format!(
                                "验收已经通过，只差发布。确认改动都已提交到 {} 后，用 steve_publish 请求发布。",
                                before.branch.as_deref().unwrap_or_default()
                            )
                        } else {
                            "请继续。完成后用 steve_deliver 交付；需要决定时用 steve_ask。".into()
                        };
                        self.send(&to, or(text, &next))?;
                    }
                    "delivered" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice}),
                            "resolved",
                            None,
                        )?;
                        let draft = self.last_agent_text(sid.as_deref().unwrap_or(""))?
                            + &plan_note(&self.item(&i)?.plan);
                        self.lc_invalidate(&i, "你说它其实做完了，重新验收")?;
                        let acc = self.new_decision(NewDecision {
                            workspace_id: d.workspace_id.clone(),
                            kind: "accept".into(),
                            subject_type: "item".into(),
                            subject_id: i.clone(),
                            item_id: Some(i.clone()),
                            session_id: sid.clone(),
                            question: "请验收".into(),
                            draft: Some(draft),
                            options: Some(vec!["pass".into(), "rework".into()]),
                            requested_by: "steve".into(),
                        })?;
                        self.lc_request_complete(
                            &i,
                            actor,
                            crate::cycle::Policy::Human,
                            false,
                            "你说它其实做完了",
                            Some(acc.id),
                            vec![],
                        )?;
                    }
                    /* a release whose result nobody knew, checked by you */
                    "release_live" | "release_retry" | "release_waive" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice}),
                            "resolved",
                            None,
                        )?;
                        let line = match choice {
                            "release_live" => "核对过：已经上线",
                            "release_retry" => "需要重新发布",
                            _ => "不需要发布",
                        };
                        self.answer_line(&i, actor, line, sid.as_deref())?;
                        /* every release of this cycle whose result nobody knew is settled by your check */
                        let checked = match choice {
                            "release_live" => "succeeded",
                            "release_retry" => "failed",
                            _ => "waived",
                        };
                        let required = self.lc_open_op(&i, crate::cycle::OP_RELEASE)?;
                        self.lc_reconcile(
                            &i,
                            crate::cycle::OP_RELEASE,
                            checked,
                            line,
                            required.as_ref().map(|o| o.id.as_str()),
                        )?;
                        let completing = self.db().item(&i)?.is_some_and(|x| {
                            crate::cycle::closing_of(&x)
                                .is_some_and(|c| c.kind == crate::cycle::COMPLETE)
                        });
                        if let Some(op) = required {
                            match choice {
                                "release_live" => {
                                    self.lc_op_set(
                                        &op.id,
                                        "succeeded",
                                        json!({"note": "你核对过已经上线"}),
                                    )?;
                                }
                                "release_waive" => {
                                    self.lc_op_set(
                                        &op.id,
                                        "waived",
                                        json!({"note": "你确认不需要发布"}),
                                    )?;
                                }
                                _ => {
                                    self.lc_op_set(
                                        &op.id,
                                        "pending",
                                        json!({"note": "需要重新发布"}),
                                    )?;
                                    if completing {
                                        self.await_release_for(&i)?;
                                    }
                                }
                            }
                        }
                    }
                    "look" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice}),
                            "resolved",
                            Some(json!({"status": before.status, "now": before.now})),
                        )?;
                        if let Some(sid) = &sid {
                            self.rt().cancel(sid);
                        }
                        self.db()
                            .item_patch(&i, patch![status: "pause", now: "你在查看，已暂停"])?;
                    }
                    "tosteve" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice}),
                            "resolved",
                            None,
                        )?;
                        recover_quiet(self.db(), &i);
                        self.reassign(&i, "steve", actor)?;
                    }
                    // the stall card (crate::stall): give the item up
                    "drop" => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice}),
                            "resolved",
                            None,
                        )?;
                        self.item_action(&i, ItemAction::Drop, actor)?;
                        return reget(self);
                    }
                    _ => {
                        self.db().decision_resolve(
                            id,
                            json!({"choice": choice, "text": text}),
                            "resolved",
                            None,
                        )?;
                    }
                }
            }
            _ => {
                self.db().decision_resolve(
                    id,
                    json!({"choice": choice, "text": text}),
                    "resolved",
                    None,
                )?;
            }
        }
        if let Some(i) = &item_id {
            settle(self.db(), i)?;
        }
        reget(self)
    }

    /// A decision with no item: the session never had one (forked, imported then continued) or the item was
    /// deleted since. Every kind is answered straight to its session, woken up if it had ended; nothing
    /// item-shaped is touched.
    fn resolve_detached(
        &mut self,
        d: &Decision,
        choice: &str,
        text: &str,
        s: Option<Session>,
        actor: &str,
    ) -> Result<Decision> {
        let id = d.id.as_str();
        let db = self.db().clone();
        let send = |t: &Tx, m: &str| -> Result<()> {
            let Some(s) = &s else { return Ok(()) };
            if s.status == "ended" {
                t.db().session_patch(&s.id, patch![status: "idle"])?;
            }
            t.prompt(&s.id, m, "steve");
            Ok(())
        };
        let note = |m: &str| -> Result<()> {
            if let Some(s) = &s {
                db.event_add(&s.id, "system", json!({"text": m}))?;
            }
            Ok(())
        };
        let with = |x: &str, pre: &str| {
            if text.is_empty() {
                String::new()
            } else {
                format!("{pre}{x}")
            }
        };
        match (d.kind.as_str(), d.subject_type.as_str()) {
            ("approve", "delivery_batch")
                if crate::lane::drive::parse_subject(&d.subject_id).is_some() =>
            {
                let (lane, unit) =
                    crate::lane::drive::parse_subject(&d.subject_id).expect("checked");
                let yes = choice == "yes";
                db.decision_resolve(id, json!({"approved": yes}), "resolved", None)?;
                /* git and checks run off the lock; the answer is the batch's, not one item's */
                let c = self.c.clone();
                crate::detach(move || c.lane_answered(&lane, unit, yes));
            }
            ("fail", "delivery_batch")
                if crate::lane::drive::parse_subject(&d.subject_id).is_some() =>
            {
                let (lane, unit) =
                    crate::lane::drive::parse_subject(&d.subject_id).expect("checked");
                let retry = choice == "retry";
                db.decision_resolve(id, json!({"choice": choice}), "resolved", None)?;
                let c = self.c.clone();
                crate::detach(move || c.lane_release_answered(&lane, unit, retry));
            }
            ("approve", "tool_call") => {
                let opt = (choice != "deny").then_some(choice);
                if self.rt().answer_permission(id, opt) {
                    db.decision_resolve(id, json!({"optionId": opt}), "resolved", None)?;
                } else {
                    db.decision_resolve(id, json!({"expired": true}), "expired", None)?;
                    send(
                        self,
                        "你之前请求权限的那次工具调用已经失效（会话被重启过）。如果还需要，请重新执行。",
                    )?;
                }
            }
            ("ask", _) if actor == YOLO_ACTOR => {
                let answer = or(choice, text);
                db.decision_resolve(id, json!({"answer": answer}), "resolved", None)?;
                note(&format!("YOLO 代答：{}", js_head(answer, 200)))?;
                send(self, if text.is_empty() { answer } else { text })?;
            }
            ("ask", _) => {
                let answer = or(text, choice);
                db.decision_resolve(id, json!({"answer": answer}), "resolved", None)?;
                note(&format!("你回答了：{}", js_head(answer, 200)))?;
                send(self, &format!("用户的回答：{answer}"))?;
            }
            ("approve", "release") => {
                /* a release belongs to an item's project; without the item there is nothing Steve can publish */
                let yes = choice == "yes";
                let mut r = json!({"approved": yes, "text": text, "detached": true});
                if yes {
                    r["outcome"] = json!("not_released");
                    r["reason"] = json!("会话没有关联的事项");
                }
                db.decision_resolve(id, r, "resolved", None)?;
                let m = if yes {
                    "这个会话没有关联的事项（或事项已删除），Steve 不能发布。需要发布的话，先把会话关联到事项。".to_string()
                } else {
                    format!("用户没有批准发布{}。", with(text, "："))
                };
                send(self, &m)?;
            }
            ("approve", _) => {
                let yes = choice == "yes";
                db.decision_resolve(id, json!({"approved": yes, "text": text}), "resolved", None)?;
                note(&format!(
                    "{}{}",
                    if yes {
                        "你批准了："
                    } else {
                        "你拒绝了："
                    },
                    d.question
                ))?;
                let m = if yes {
                    format!("用户批准了：{}{}", d.question, with(text, "。补充："))
                } else {
                    format!(
                        "用户拒绝了：{}{}。不要执行这个操作。",
                        d.question,
                        with(text, "。原因：")
                    )
                };
                send(self, &m)?;
            }
            ("accept", _) if choice == "pass" => {
                db.decision_resolve(id, json!({"choice": "pass"}), "resolved", None)?;
                note("你验收通过了")?;
                send(self, "验收通过。")?;
            }
            ("accept", _) => {
                db.decision_resolve(
                    id,
                    json!({"choice": "rework", "text": text}),
                    "resolved",
                    None,
                )?;
                send(
                    self,
                    &format!("验收没有通过，返工意见：{}", or(text, "请再检查一遍")),
                )?;
            }
            ("fail", _) => {
                db.decision_resolve(
                    id,
                    json!({"choice": choice, "text": text}),
                    "resolved",
                    None,
                )?;
                if choice == "continue" {
                    send(self, or(text, "请继续。"))?;
                } else if let (true, Some(s)) = (choice == "look", &s) {
                    self.rt().cancel(&s.id);
                }
            }
            _ => {
                db.decision_resolve(
                    id,
                    json!({"choice": choice, "text": text}),
                    "resolved",
                    None,
                )?;
            }
        }
        db.decision(id)?
            .ok_or_else(|| msg(steve_i18n::t!("core.decision.handled")))
    }

    /// Undo a decision within 24 hours, when nothing irreversible happened because of it.
    pub(crate) fn undo_decision(&mut self, id: &str, actor: &str) -> Result<Decision> {
        let d = self.db().decision(id)?;
        let undoable = |d: &Decision| {
            d.status == "resolved"
                && crate::gateway::truthy(&d.undo)
                && d.resolved.is_some_and(|r| r != 0 && now() - r <= DAY)
        };
        let Some(d) = d.filter(|d| undoable(d)) else {
            return Err(msg(steve_i18n::t!("core.decision.not_undoable")));
        };
        /* a closed cycle stays as it closed: nothing of it is taken back. The one exception is a
         * proposal you declined before anything started: taking that back is a new cycle, the declined
         * one stays as it was */
        if let Some(i) = d.item_id.as_deref()
            && let Some(item) = self.db().item(i)?.filter(|i| i.is_finished())
        {
            let declined = d.kind == "propose"
                && item.outcome.as_deref() == Some("cancelled")
                && item.revision == 0;
            if !declined {
                return Err(crate::cycle::conflict(steve_i18n::t!(
                    "core.decision.undo_closed"
                )));
            }
            self.lc_new_cycle(i, actor, None)?;
            self.db().item_patch(i, patch![stage: "规划"])?;
        }
        if d.kind == "accept"
            && let Some(i) = d.item_id.as_deref()
        {
            self.lc_unaccept(i)?;
        }
        if let Some(d) = self.module_undo(&d, actor, undoable)? {
            /* the module reopened it with its own side undone; the item follows as for core decisions */
            if let Some(i) = &d.item_id {
                let p = undo_patch(&d.undo);
                if !p.is_empty() {
                    self.db().item_patch(i, p)?;
                }
            }
        } else {
            if let Some(i) = &d.item_id {
                let u = &d.undo;
                self.db().item_patch(i, undo_patch(u))?;
            }
            self.db().decision_reopen(id)?;
            self.db()
                .audit(actor, "decision.undo", id, &json!({}), &d.workspace_id)?;
        }
        if let Some(i) = &d.item_id {
            self.db().timeline_add(i, actor, "me", "撤销了决定", None)?;
            settle(self.db(), i)?;
            if d.kind == "accept" {
                self.reblock(i, d.resolved.unwrap_or(0))?;
            }
        }
        self.db()
            .decision(id)?
            .ok_or_else(|| msg(steve_i18n::t!("core.decision.not_undoable")))
    }

    /// An acceptance was taken back: dependents it unblocked go back to waiting, unless they already started a turn.
    fn reblock(&mut self, item_id: &str, since: i64) -> Result<()> {
        for dep_id in self.db().dependents(item_id)? {
            let Some(x) = self.db().item(&dep_id)? else {
                continue;
            };
            if status_in(&x, &["done", "drop", "block"])
                || deps_open(self.db(), &dep_id)?.is_empty()
            {
                continue;
            }
            if !self
                .db()
                .timeline(&dep_id)?
                .iter()
                .any(|t| t.ts >= since && t.text.starts_with("前置事项都已完成"))
            {
                continue;
            }
            let mut ran = false;
            for s in self.db().sessions_of_item(&dep_id)? {
                if self.busy(&s.id) || self.db().has_events_since(&s.id, since)? {
                    ran = true;
                    break;
                }
            }
            if ran {
                self.db().timeline_add(
                    &dep_id,
                    "steve",
                    "sys",
                    &format!("前置事项 {item_id} 的验收被撤销了；这个事项已经开始，没有停下"),
                    None,
                )?;
                continue;
            }
            for o in self
                .db()
                .decisions_of_item(&dep_id)?
                .into_iter()
                .filter(|o| o.status == "open" && o.kind == "propose")
            {
                self.db().decision_resolve(
                    &o.id,
                    json!({"superseded": "前置事项的验收被撤销"}),
                    "expired",
                    None,
                )?;
            }
            self.db().item_patch(
                &dep_id,
                patch![status: "block", now: "等前置事项完成后自动开始"],
            )?;
            self.db().timeline_add(
                &dep_id,
                "steve",
                "sys",
                &format!("前置事项 {item_id} 的验收被撤销了，回到等前置"),
                None,
            )?;
        }
        Ok(())
    }

    /// Take a card out of 待处理 without answering it: nothing is sent to the agent. Undoable like other
    /// decisions, except a tool permission — dismissing that denies the call.
    pub(crate) fn dismiss_decision(&mut self, id: &str, actor: &str) -> Result<Decision> {
        let d = self
            .db()
            .decision(id)?
            .filter(|d| d.status == "open")
            .ok_or_else(|| msg(steve_i18n::t!("core.decision.handled")))?;
        self.db()
            .audit(actor, "decision.dismiss", id, &json!({}), &d.workspace_id)?;
        let i = match &d.item_id {
            Some(i) => self.db().item(i)?,
            None => None,
        };
        if d.kind == "approve" && d.subject_type == "tool_call" {
            self.rt().answer_permission(id, None);
            self.db()
                .decision_resolve(id, json!({"dismissed": true}), "resolved", None)?;
        } else {
            let undo = i
                .as_ref()
                .map(|i| json!({"status": i.status, "now": i.now}));
            self.db()
                .decision_resolve(id, json!({"dismissed": true}), "resolved", undo)?;
        }
        if let Some(i) = i {
            self.db().timeline_add(
                &i.id,
                actor,
                "me",
                &format!("撤回了待处理：{}", js_head(&d.question, 80)),
                None,
            )?;
            let busy = self
                .active_session(&i.id)?
                .is_some_and(|s| self.busy(&s.id));
            let open = self
                .db()
                .decisions_of_item(&i.id)?
                .iter()
                .any(|x| x.status == "open");
            /* nothing left to wait for and nothing running: park it, so 继续 picks it up again */
            if !open && !busy && status_in(&i, &["wait", "run"]) {
                self.db().item_patch(
                    &i.id,
                    patch![status: "pause", now: "你撤回了待处理，需要时点继续"],
                )?;
            } else {
                settle(self.db(), &i.id)?;
            }
        }
        self.db()
            .decision(id)?
            .ok_or_else(|| msg(steve_i18n::t!("core.decision.handled")))
    }

    /* ── user actions on items and sessions ── */
    /// Any new message means the item is in progress again, even if it was finished.
    /// New input for the item: a closed one refuses it (a new cycle is explicit), a closing for
    /// completion is void, a `ready` one starts its cycle, a paused one runs again.
    fn reactivate(&mut self, item_id: Option<&str>) -> Result<()> {
        let Some(i) = item_id.map(|i| self.db().item(i)).transpose()?.flatten() else {
            return Ok(());
        };
        self.lc_new_input(&i.id)?;
        let fresh = i.phase == crate::cycle::READY;
        if fresh {
            self.lc_start(&i.id)?;
        }
        let i = self.item(&i.id)?;
        if !i.is_settled() {
            /* a new cycle waiting for its first word, or a wait whose cards are all answered: the bot takes
             * it up now. A card still open keeps it waiting on you. */
            let open = self
                .db()
                .decisions_of_item(&i.id)?
                .iter()
                .any(|d| d.status == "open");
            if i.status == "wait" && !open {
                let now = if fresh {
                    "新周期，收到新消息，开始处理"
                } else {
                    "收到新消息，继续处理"
                };
                self.db()
                    .item_patch(&i.id, patch![status: "run", stage: "执行", now: now])?;
            }
            return Ok(());
        }
        self.ensure_cwd(&i.id)?;
        self.db().item_patch(
            &i.id,
            patch![status: "run", stage: "执行", now: "收到新消息，继续处理", archived: false],
        )?;
        settle(self.db(), &i.id)
    }

    pub(crate) fn supplement(
        &mut self,
        item_id: &str,
        text: &str,
        mode: SupplementMode,
        files: &[FileRef],
    ) -> Result<()> {
        self.lc_guard_open(item_id)?;
        let s = self.active_session(item_id)?;
        self.db().timeline_add_files(
            item_id,
            "me",
            "me",
            text,
            s.as_ref().map(|s| s.id.as_str()),
            files,
        )?;
        supersede(self.db(), item_id, &["ask", "fail"], "你直接补充了信息")?;
        recover_quiet(self.db(), item_id);
        self.reactivate(Some(item_id))?;
        let s = match s {
            Some(s) if mode != SupplementMode::New => s,
            _ => {
                self.start_session_files(
                    item_id,
                    Some(&format!(
                        "补充信息：{text}\n\n（这是事项 {item_id} 的新会话。）"
                    )),
                    None,
                    files,
                )?;
                return Ok(());
            }
        };
        /* past the handoff threshold the next message starts a fresh session with a handoff summary */
        let ws = self.item(item_id)?.workspace_id;
        let limit: f64 = self
            .db()
            .setting(&ws, "handoff", "80")?
            .parse()
            .unwrap_or(f64::NAN);
        if let (Some(used), Some(size)) = (s.used.filter(|u| *u != 0), s.size.filter(|z| *z != 0)) {
            let ratio = used as f64 / size as f64 * 100.0;
            if ratio >= limit && !self.busy(&s.id) {
                let pct = ratio.round() as i64;
                let last = self.last_agent_text(&s.id)?;
                let last = js_head(&last, 800);
                self.db().event_add(
                    &s.id,
                    "system",
                    json!({"text": format!("上下文已用 {pct}%，带交接摘要转到新会话")}),
                )?;
                self.db().session_patch(&s.id, patch![status: "ended"])?;
                self.rt().release(&s.id);
                self.db().timeline_add(
                    item_id,
                    "steve",
                    "sys",
                    &format!("上下文已用 {pct}%：开新会话"),
                    None,
                )?;
                self.start_session_files(item_id, Some(&format!("交接：上一个会话的上下文用到了 {pct}%。它的最后进展：{last}\n\n补充信息：{text}")), None, files)?;
                return Ok(());
            }
        }
        if mode == SupplementMode::Steer && self.busy(&s.id) {
            self.rt().cancel(&s.id);
        }
        self.rt_prompt(&s.id, &format!("补充信息：{text}"), "me", files)?;
        Ok(())
    }

    /// Deliver everything queued for the item's active session now. While a turn runs or the Hub
    /// holds for a release the queue is left as it is: taking it and prompting again would only queue
    /// it a second time (a duplicate `queued` entry, its `via` and attachments lost).
    pub(crate) fn deliver_queued(&mut self, item_id: &str) -> Result<()> {
        self.lc_guard_open(item_id)?;
        let Some(s) = self.active_session(item_id)? else {
            return Ok(());
        };
        if self.db().queued(&s.id)?.is_empty() || self.holding() || self.busy(&s.id) {
            return Ok(());
        }
        self.reactivate(Some(item_id))?;
        self.rt().flush(&s.id, None);
        Ok(())
    }

    /// End a session, its adapter released (it idles out if a turn is still finishing).
    pub(crate) fn end_and_release(&self, sid: &str) -> Result<()> {
        self.rt().cancel(sid);
        self.db().session_patch(sid, patch![status: "ended"])?;
        self.rt().release(sid);
        Ok(())
    }

    pub(crate) fn item_action(
        &mut self,
        id: &str,
        action: ItemAction,
        actor: &str,
    ) -> Result<Item> {
        let i = self.item(id)?;
        self.db().audit(
            actor,
            &format!("item.{}", action.as_str()),
            id,
            &json!({}),
            &i.workspace_id,
        )?;
        let s = self.active_session(id)?;
        match action {
            ItemAction::Pause => {
                if i.is_finished() {
                    return Err(crate::cycle::conflict(crate::cycle::closed_msg()));
                }
                if let Some(s) = &s {
                    self.rt().cancel(&s.id);
                }
                self.db()
                    .item_patch(id, patch![status: "pause", now: "你暂停了"])?;
            }
            ItemAction::Finish => {
                if i.is_finished() {
                    return Err(steve_modules::ApiError::conflict(steve_i18n::t!(
                        "core.item.ended"
                    ))
                    .into());
                }
                if crate::cycle::stopping(&i) {
                    return Err(crate::cycle::conflict(steve_i18n::t!("core.item.ending")));
                }
                let busy = s.as_ref().is_some_and(|s| self.busy(&s.id));
                self.st.marked_done.insert(id.into(), MarkedDone { busy });
                self.db()
                    .timeline_add(id, actor, "me", action.label(), None)?;
                /* you are the acceptor; the running turn is stopped (outside the lock) and the close
                 * waits until it has */
                if let Some(s) = s.as_ref().filter(|s| s.status != "ended") {
                    self.st.cancels.push((s.id.clone(), id.into()));
                }
                self.lc_request_complete(
                    id,
                    actor,
                    crate::cycle::Policy::Human,
                    true,
                    "你标记了完成",
                    None,
                    vec!["你标记了完成".into()],
                )?;
                return Ok(self.db().item(id)?.unwrap_or(i));
            }
            ItemAction::Unfinish => {
                if i.is_finished() {
                    self.st.marked_done.remove(id);
                    return Err(crate::cycle::conflict(steve_i18n::t!(
                        "core.item.ended_new_cycle"
                    )));
                }
                let m = self
                    .st
                    .marked_done
                    .get(id)
                    .filter(|_| i.phase == crate::cycle::CLOSING)
                    .cloned()
                    .ok_or_else(|| {
                        steve_modules::ApiError::conflict(steve_i18n::t!(
                            "core.item.unfinish_only_marked"
                        ))
                    })?;
                self.st.marked_done.remove(id);
                self.lc_invalidate(id, "你撤销了标记完成")?;
                self.db()
                    .timeline_add(id, actor, "me", action.label(), None)?;
                settle(self.db(), id)?;
                /* a turn cut off by the finish picks up where it was */
                let back = self.item(id)?;
                if m.busy && back.status == "run" {
                    match s.as_ref().filter(|s| s.status != "ended") {
                        Some(s) => self.prompt(&s.id, "继续这个事项。", "me"),
                        None => self.start_or_fail(id, None, None)?,
                    }
                }
                return Ok(back);
            }
            ItemAction::Drop => {
                self.db()
                    .timeline_add(id, actor, "me", action.label(), None)?;
                return self.lc_stop(id, crate::cycle::CANCEL, actor, "你放弃了", None);
            }
            ItemAction::Reopen if i.is_finished() => {
                /* reopening a closed item is an explicit new cycle; the cycle before stays as it closed */
                self.db()
                    .timeline_add(id, actor, "me", action.label(), None)?;
                return self.lc_new_cycle(id, actor, None);
            }
            ItemAction::Resume | ItemAction::Reopen => {
                if i.is_finished() {
                    return Err(crate::cycle::conflict(crate::cycle::closed_msg()));
                }
                if crate::cycle::stopping(&i) {
                    return Err(crate::cycle::conflict(steve_i18n::t!("core.item.ending")));
                }
                supersede(self.db(), id, &["fail"], "你让它继续了")?;
                recover_quiet(self.db(), id);
                self.ensure_cwd(id)?;
                if !deps_open(self.db(), id)?.is_empty() {
                    self.db()
                        .item_patch(id, patch![status: "block", now: "等前置事项完成后自动开始"])?;
                } else if self
                    .db()
                    .decisions_of_item(id)?
                    .iter()
                    .any(|d| d.status == "open")
                {
                    /* paused while a question or approval was open: back to waiting on it, don't push the agent past it */
                    self.db()
                        .item_patch(id, patch![status: "run", now: "继续处理"])?;
                    settle(self.db(), id)?;
                } else {
                    self.db()
                        .item_patch(id, patch![status: "run", now: "继续处理"])?;
                    match s.as_ref().filter(|s| s.status != "ended") {
                        Some(s) => self.prompt(&s.id, "继续这个事项。", "me"),
                        None => self.start_or_fail(id, None, None)?,
                    }
                }
            }
            ItemAction::Archive => self.db().item_patch(id, patch![archived: true])?,
            ItemAction::Unarchive => self.db().item_patch(id, patch![archived: false])?,
        }
        self.db()
            .timeline_add(id, actor, "me", action.label(), None)?;
        Ok(self.db().item(id)?.unwrap_or(i))
    }

    pub(crate) fn revise_goal(&mut self, id: &str, text: &str) -> Result<()> {
        self.lc_guard_open(id)?;
        let prev = self.db().goal(id)?;
        let v = self.db().add_goal(id, text)?;
        self.db()
            .timeline_add(id, "me", "sys", &format!("目标更新为 v{v}"), None)?;
        let Some(s) = self.active_session(id)? else {
            return Ok(());
        };
        /* a much bigger goal starts over with a handoff, a small change is told to the running session */
        if js_len(text) as f64 > js_len(&prev) as f64 * 1.6 {
            self.db().session_patch(&s.id, patch![status: "ended"])?;
            self.rt().release(&s.id);
            let last = self.last_agent_text(&s.id)?;
            self.start_or_fail(
                id,
                Some(&format!(
                    "目标改动较大（v{v}）：{text}\n上一个会话的最后进展：{}",
                    js_head(&last, 600)
                )),
                None,
            )?;
            self.db()
                .timeline_add(id, "steve", "sys", "目标改动较大：开新会话", None)?;
        } else {
            self.prompt(
                &s.id,
                &format!("目标更新了（v{v}）：{text}\n按新目标调整计划，并用 steve_update 更新。"),
                "me",
            );
        }
        Ok(())
    }

    /// Give the item to another bot: the old session ends, the new bot starts with a handoff.
    pub(crate) fn reassign(&mut self, id: &str, bot_id: &str, actor: &str) -> Result<Item> {
        let i = self.item(id)?;
        if i.bot_id.as_deref() == Some(bot_id) {
            return Ok(i);
        }
        let b = match self.db().bot(bot_id)? {
            Some(b) if self.db().bot_in_workspace(bot_id, &i.workspace_id)? => b,
            _ => return Err(msg(steve_i18n::t!("core.bot.not_in_workspace"))),
        };
        let s = self.active_session(id)?;
        let handoff = match &s {
            Some(s) => js_head(&self.last_agent_text(&s.id)?, 800).to_string(),
            None => String::new(),
        };
        if let Some(s) = &s {
            self.end_and_release(&s.id)?;
        }
        for d in self
            .db()
            .decisions_of_item(id)?
            .into_iter()
            .filter(|d| d.status == "open" && d.session_id.is_some())
        {
            self.db()
                .decision_resolve(&d.id, json!({"reassigned": true}), "expired", None)?;
        }
        self.db()
            .timeline_add(id, actor, "sys", &format!("换成{}", b.display()), None)?;
        let prev = if handoff.is_empty() {
            String::new()
        } else {
            format!("上一位的最后进展：{handoff}")
        };
        self.start_or_fail(
            id,
            Some(&format!("这个事项从别的 Bot 交接给你。{prev}")),
            Some(bot_id),
        )?;
        self.item(id)
    }

    pub(crate) fn session_message(
        &mut self,
        sid: &str,
        text: &str,
        interrupt: bool,
        files: &[FileRef],
    ) -> Result<Prompted> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        if let Some(i) = s.item_id.as_deref() {
            self.lc_guard_open(i)?;
            self.db()
                .timeline_add_files(i, "me", "me", text, Some(&s.id), files)?;
            supersede(self.db(), i, &["ask", "fail"], "你直接在会话里发了消息")?;
            recover_quiet(self.db(), i);
            self.reactivate(Some(i))?;
        }
        if s.status == "ended" {
            self.db().session_patch(sid, patch![status: "idle"])?;
        }
        if interrupt && self.busy(sid) {
            self.rt().cancel(sid);
        }
        self.rt_prompt(sid, text, "me", files)
    }

    pub(crate) fn queued_edit(&mut self, sid: &str, qid: &str, text: &str) -> Result<Queued> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        let (before, after) = self
            .db()
            .queue_edit(sid, qid, text)?
            .ok_or_else(already_sent)?;
        self.db().event_add(
            sid,
            "queued_edited",
            json!({"id": qid, "text": text, "was": before.text}),
        )?;
        if let Some(i) = s.item_id.as_deref() {
            self.db().timeline_add(
                i,
                "me",
                "sys",
                &format!("修改了排队的消息：{}", js_head(text, 200)),
                Some(sid),
            )?;
        }
        Ok(after)
    }

    pub(crate) fn queued_remove(&mut self, sid: &str, qid: &str) -> Result<Queued> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        let q = self.db().queue_remove(sid, qid)?.ok_or_else(already_sent)?;
        self.db()
            .event_add(sid, "queued_removed", json!({"id": qid, "text": q.text}))?;
        if let Some(i) = s.item_id.as_deref() {
            self.db().timeline_add(
                i,
                "me",
                "sys",
                &format!("撤回了排队的消息：{}", js_head(&q.text, 200)),
                Some(sid),
            )?;
        }
        Ok(q)
    }

    pub(crate) fn queued_send_now(&mut self, sid: &str, qid: &str) -> Result<SendNow> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        if let Some(i) = s.item_id.as_deref() {
            self.lc_guard_open(i)?;
        }
        let q = self.db().queue_front(sid, qid)?.ok_or_else(already_sent)?;
        if self.holding() {
            return Ok(SendNow::Waiting(steve_i18n::t!("core.queue.wait_release")));
        }
        if self.db().store().maintenance() {
            return Ok(SendNow::Waiting(steve_i18n::t!(
                "core.queue.wait_maintenance"
            )));
        }
        // moving on from a turn nobody can tell the end of is a decision of its own (中断), not
        // part of a reorder: the message stays first and goes once reconciliation settles it
        if self.rt().uncertain(sid) {
            return Ok(SendNow::Waiting(steve_i18n::t!(
                "core.queue.wait_uncertain"
            )));
        }
        let ev = |interrupted: bool| json!({"id": qid, "text": q.text, "interrupted": interrupted});
        // a cancel while the adapter is still starting goes right after its prompt; false means the
        // turn ended just now (and took the queue, or left it for the flush below)
        if self.busy(sid) && self.rt().cancel(sid) {
            self.db().event_add(sid, "queued_sent_now", ev(true))?;
            self.reactivate(s.item_id.as_deref())?;
            return Ok(SendNow::Interrupted);
        }
        if !cwd_usable(Path::new(&s.cwd)) {
            return Ok(SendNow::Waiting(steve_i18n::t!("core.queue.wait_cwd")));
        }
        if s.status == "ended" {
            self.db().session_patch(sid, patch![status: "idle"])?;
        }
        // false with the row gone means a turn took the queue in between: the message went anyway;
        // false with the row still queued means nothing took it (a session opening, say)
        let went = self.rt().flush(sid, None);
        if !went
            && self
                .db()
                .store()
                .queued()
                .get(qid)?
                .is_some_and(|q| q.session_id == sid)
        {
            return Ok(SendNow::Waiting(if self.busy(sid) {
                steve_i18n::t!("core.queue.wait_starting")
            } else {
                steve_i18n::t!("core.queue.wait_later")
            }));
        }
        self.db().event_add(sid, "queued_sent_now", ev(false))?;
        self.reactivate(s.item_id.as_deref())?;
        Ok(SendNow::Sent)
    }

    pub(crate) fn send_to_session(
        &mut self,
        item_id: Option<&str>,
        session_id: Option<&str>,
        text: &str,
    ) -> Result<()> {
        let s = match session_id {
            Some(id) => self.db().session(id)?,
            None => None,
        };
        let item = item_id
            .map(String::from)
            .or_else(|| s.as_ref().and_then(|s| s.item_id.clone()));
        let live = match (&s, item.as_deref()) {
            (Some(s), _) if s.status != "ended" => Some(s.clone()),
            (_, Some(i)) => self.active_session(i)?,
            _ => None,
        };
        if let Some(i) = item.as_deref() {
            self.reactivate(Some(i))?;
        }
        match (live, item.as_deref()) {
            (Some(s), _) => {
                self.rt_prompt(&s.id, text, "steve", &[])?;
            }
            (None, Some(i)) => {
                self.start_session_files(i, Some(text), None, &[])?;
            }
            (None, None) => {
                if let Some(s) = s {
                    self.db().session_patch(&s.id, patch![status: "idle"])?;
                    self.rt_prompt(&s.id, text, "steve", &[])?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn mark_awaiting_approval(&mut self, sid: &str) -> Result<()> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        self.st.pending.insert(s.id.clone(), Mark::Approval);
        if let Some(item) = s.item_id.as_deref() {
            supersede(self.db(), item, &["ask", "fail"], "Bot 继续往下做了")?;
            self.db().item_patch(item, patch![now: "在等你批准"])?;
            settle(self.db(), item)?;
        }
        Ok(())
    }

    pub(crate) fn end_session(&mut self, sid: &str) -> Result<()> {
        self.end_and_release(sid)?;
        self.db().event_add(
            sid,
            "system",
            json!({"text": "会话已结束，发送消息可以恢复"}),
        )?;
        Ok(())
    }

    /* ── fork: carry the conversation up to a message into a new item or a new session of the same item ── */
    fn transcript_up_to(&self, sid: &str, seq: i64) -> Result<String> {
        let mut out: Vec<String> = vec![];
        let mut agent = String::new();
        for e in self.db().events_up_to(sid, seq)? {
            if let Some(t) = steve_transcript::assistant_text(&e.kind, &e.data) {
                agent.push_str(t);
                continue;
            }
            if !agent.is_empty() {
                out.push(format!("Agent：{}", agent.trim()));
                agent.clear();
            }
            if e.kind == "user" {
                let who = if e.data.get("via").and_then(Value::as_str) == Some("steve") {
                    "Steve："
                } else {
                    "用户："
                };
                let t = match e.data.get("text") {
                    None | Some(Value::Null) => String::new(),
                    Some(v) => crate::gateway::jstr(v),
                };
                out.push(format!("{who}{}", js_head(&t, 1500)));
            }
        }
        if !agent.is_empty() {
            out.push(format!("Agent：{}", agent.trim()));
        }
        let text = out.join("\n\n");
        Ok(if js_len(&text) > 8000 {
            format!("…{}", js_tail(&text, 8000))
        } else {
            text
        })
    }

    pub(crate) fn fork(
        &mut self,
        ws: &str,
        sid: &str,
        seq: i64,
        goal: &str,
        as_session: bool,
        actor: &str,
    ) -> Result<Forked> {
        let s = self
            .db()
            .session(sid)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        let ctx = self.transcript_up_to(sid, seq)?;
        let g = or(goal, "从这里继续另一件事");
        let opening =
            format!("这是从另一个会话派生出来的。派生点之前的对话：\n\n{ctx}\n\n现在要做的：{g}");
        if let (true, Some(item)) = (as_session, s.item_id.as_deref()) {
            self.db().timeline_add(
                item,
                actor,
                "sys",
                &format!("从会话 #{} 派生出新会话", s.no),
                None,
            )?;
            let ns = self.start_session(item, Some(&opening), None)?;
            return Ok(Forked::Session(ns.id));
        }
        let parent = match s.item_id.as_deref() {
            Some(i) => self.db().item(i)?,
            None => None,
        };
        let bot = s
            .bot_id
            .clone()
            .or_else(|| parent.as_ref().and_then(|p| p.bot_id.clone()))
            .unwrap_or_else(|| "steve".into());
        let tag_ids = match &parent {
            Some(p) => self.db().tags_of_item(&p.id)?,
            None => vec![],
        };
        let item = self.create_item(CreateItem {
            ws: ws.into(),
            goal: g.into(),
            title: js_head(g, 24).into(),
            bot: Some(bot),
            project: Some(parent.as_ref().and_then(|p| p.project_id.clone())),
            source: Some(("派生".into(), "fork".into())),
            parent: Some(Parent {
                item: Some(s.item_id.clone()),
                session: Some(Some(s.id.clone())),
                at: Some(Some(seq)),
            }),
            tag_ids,
            propose: false,
            opening: Some(opening),
            actor: Some(actor.into()),
            files: vec![],
        })?;
        if let Some(pi) = s.item_id.as_deref() {
            self.db().timeline_add(
                pi,
                actor,
                "sys",
                &format!("派生出事项 {}「{}」", item.id, js_head(g, 20)),
                None,
            )?;
        }
        Ok(Forked::Item(item.id))
    }

    /* ── bots ── */
    pub(crate) fn delete_bot(&mut self, id: &str, to: &str, actor: &str) -> Result<Vec<String>> {
        let b = self
            .db()
            .bot(id)?
            .filter(|b| !b.main)
            .ok_or_else(|| msg(steve_i18n::t!("core.bot.main_undeletable")))?;
        let target0 = self
            .db()
            .bot(to)?
            .filter(|_| to != id)
            .ok_or_else(|| msg(steve_i18n::t!("core.bot.no_heir")))?;
        let mut moved = vec![];
        let mut heirs: Vec<(String, String)> = vec![];
        for w in self.db().bot_workspaces(id)? {
            /* the heir may not be in every workspace the bot served: there the main bot takes over */
            let target = if self.db().bot_in_workspace(to, &w)? {
                target0.clone()
            } else {
                self.db().main_bot()?
            };
            heirs.push((w.clone(), target.id.clone()));
            for i in self
                .db()
                .items(&w)?
                .into_iter()
                .filter(|x| x.bot_id.as_deref() == Some(id) && !x.is_finished())
            {
                self.db().timeline_add(
                    &i.id,
                    "steve",
                    "sys",
                    &format!("{} Bot 已删除，交给{}", b.name, target.display()),
                    None,
                )?;
                moved.push(i.id.clone());
                /* running items continue with the heir (a new session with a handoff); the rest just change hands */
                if i.status == "run" {
                    self.reassign(&i.id, &target.id, actor)?;
                    continue;
                }
                self.db()
                    .item_patch(&i.id, patch![bot_id: target.id.as_str()])?;
                if let Some(s) = self.active_session(&i.id)? {
                    self.end_and_release(&s.id)?;
                }
            }
        }
        self.db().bot_remove(id)?;
        /* the modules' references (personas, flow nodes) move to the same heir, one transaction */
        if let Some(m) = self.c.modules() {
            let now = now();
            m.store
                .transaction(|tx| {
                    for (w, heir) in &heirs {
                        m.registry.on_bot_deleted(tx, w, id, heir, now)?;
                    }
                    Ok::<_, steve_modules::ApiError>(())
                })
                .map_err(|e| msg(e.to_string()))?;
        }
        self.db().audit(
            actor,
            "bot.delete",
            id,
            &json!({"to": to, "moved": moved}),
            WS,
        )?;
        Ok(moved)
    }

    /* ── boot: a turn cut off by the restart continues once by itself, with whatever was queued for it; an item
     *  that waits on you (paused, or an open decision) is not poked; a repeat within the window, or a resume that
     *  fails, shows a fail card ── */
    pub(crate) fn waits_on_user(&self, item_id: Option<&str>) -> Result<bool> {
        let Some(i) = item_id.map(|i| self.db().item(i)).transpose()?.flatten() else {
            return Ok(false);
        };
        Ok(i.status == "pause"
            || self
                .db()
                .decisions_of_item(&i.id)?
                .iter()
                .any(|d| d.status == "open"))
    }

    pub(crate) fn recover(&mut self) -> Result<()> {
        self.report_release()?;
        /* closing intents survive the restart; their pending hook consultations are asked again (the
         * gate itself is only checked in `ready`, once the interrupted turns are accounted for) */
        self.lc_recover_ops()?;
        /* first the cards that no longer apply, so "waits on the user" below only counts live ones */
        for w in self.db().workspace_ids()? {
            for d in self.db().decisions_open(&w)? {
                if d.kind == "approve"
                    && d.subject_type == "tool_call"
                    && d.session_id
                        .as_deref()
                        .is_some_and(|sid| self.rt().recovers_itself(sid))
                {
                    /* a Node's card outlives the Hub: the Node expires it, or the attempt goes on */
                } else if d.kind == "approve" && d.subject_type == "tool_call" {
                    /* a tool permission dies with the adapter that asked; the resumed turn asks again if it still needs it */
                    self.db()
                        .decision_resolve(&d.id, json!({"expired": true}), "expired", None)?;
                    if let Some(sid) = &d.session_id {
                        self.db().event_add(sid, "system", json!({"text": "Hub 重启，原工具授权请求已失效。尚未授权的操作不会自动获准；继续时必须重新请求授权。", "approval_expired": d.id}))?;
                    }
                    if let Some(i) = &d.item_id {
                        self.db().timeline_add(
                            i,
                            "steve",
                            "sys",
                            "Hub 重启使工具授权失效，继续操作需要重新授权",
                            d.session_id.as_deref(),
                        )?;
                        settle(self.db(), i)?;
                    }
                } else if let Some(i) = d
                    .item_id
                    .as_deref()
                    .map(|i| self.db().item(i))
                    .transpose()?
                    .flatten()
                    && i.is_finished()
                {
                    /* reminders left behind on items that were finished or dropped before this was enforced */
                    let why = if i.status == "done" {
                        "finished"
                    } else {
                        "dropped"
                    };
                    self.db()
                        .decision_resolve(&d.id, json!({why: true}), "expired", None)?;
                }
            }
            /* cards from before the expiry rules: a question or failure older than a delivery or approval request */
            let mut seen = HashSet::new();
            for d in self.db().decisions_open(&w)? {
                if let Some(i) = d.item_id
                    && seen.insert(i.clone())
                {
                    tidy(self.db(), &i)?;
                }
            }
        }
        self.db().store().queued().recover_dispatches()?;
        let pending = self.db().store().queued().resume_ids()?;
        let mut resume = vec![];
        let store = self.db().store().clone();
        let oldest = now() - RESUME_MAX_AGE_MS;
        for s in self.db().sessions_all()? {
            if s.status != "running" && s.status != "starting" && !pending.contains(&s.id) {
                continue;
            }
            if s.status == "ended" {
                // nothing to continue: the intent goes
                store.queued().resume_done(&s.id)?;
                continue;
            }
            // its backend settles it (a Node's reconcile, or the Node it waits for); an intent an
            // earlier boot left goes without a word
            if self.rt().recovers_itself(&s.id) {
                store.queued().resume_done(&s.id)?;
                continue;
            }
            // Persist first, including sessions blocked on a pause or a human decision. Notices and
            // failure cards only when this boot found the interruption (not again on every restart).
            let fresh = store.queued().plan_resume(&s.id)?;
            let item = match s.item_id.as_deref() {
                Some(i) => self.db().item(i)?,
                None => None,
            };
            let sys = |t: &Tx, text: &str| -> Result<()> {
                if fresh {
                    t.db().event_add(&s.id, "system", json!({"text": text}))?;
                }
                Ok(())
            };
            // never continued by itself: the notice and card once, with the intent dropped (so no
            // later boot sees it again)
            let give_up = |t: &mut Tx, text: &str, fail: Option<String>| -> Result<()> {
                t.db().event_add(&s.id, "system", json!({"text": text}))?;
                if let (Some(i), Some(why)) = (&item, fail) {
                    t.fail(&i.id, &why)?;
                }
                t.db().store().queued().resume_done(&s.id)?;
                Ok(())
            };
            if item.as_ref().is_some_and(|i| i.is_finished()) {
                give_up(
                    self,
                    "Hub 重启，这一回合被中断。事项已结束，不自动继续。",
                    None,
                )?;
                continue;
            }
            // kept: continued when the pause or the decision ends (its next submit clears the intent)
            if item.as_ref().is_some_and(|i| i.status == "pause") {
                sys(self, "Hub 重启，这一回合被中断。事项已暂停，继续时接着做。")?;
                continue;
            }
            if !crate::lifecycle::drain_cut(self.db(), &s.id)?
                && self.waits_on_user(s.item_id.as_deref())?
            {
                sys(
                    self,
                    "Hub 重启，这一回合被中断。事项在等你处理，之后再继续。",
                )?;
                continue;
            }
            /* never wake an agent in a directory that isn't there (or can't be written), nor at all when resuming is
             * switched off (a copy of the real database: its sessions point at real worktrees and harness histories);
             * the intent goes too, so a later boot with resuming on does not revive it */
            if self.cfg().no_resume || !cwd_usable(Path::new(&s.cwd)) {
                give_up(
                    self,
                    "Hub 重启，这一回合被中断。没有自动继续，需要时让它继续。",
                    Some(INTERRUPTED.to_string()),
                )?;
                continue;
            }
            // an interruption older than a day is not continued by itself
            if store
                .queued()
                .resume_created(&s.id)?
                .is_some_and(|c| c < oldest)
            {
                give_up(
                    self,
                    "Hub 重启，这一回合中断已经超过一天，不自动继续，需要时让它继续。",
                    Some(INTERRUPTED.to_string()),
                )?;
                continue;
            }
            if !may_resume(self.db(), &s.id, now())? {
                give_up(
                    self,
                    "Hub 重启，这一回合被中断。半小时内已经自动继续过一次，这次等你决定。",
                    Some(format!("{INTERRUPTED}（已经自动继续过一次）")),
                )?;
                continue;
            }
            resume.push(s.id.clone());
        }
        /* after the listener is up, so the sessions can reach the Steve tools: the resumes (each with what was
         * queued for it, identical texts once), then prompts held for the restart */
        if self.cfg().no_resume {
            return Ok(());
        }
        self.st.resume_batch += 1;
        let batch = self.st.resume_batch;
        self.st.resumes.push((batch, resume));
        Ok(())
    }

    /// Listener-ready dispatch: `Some(batch)` runs that prepared batch, `None` every pending one.
    /// The database intent survives losing this in-memory batch before dispatch.
    pub(crate) fn resume(&mut self, batch: Option<u64>) {
        let (mut due, mut found) = (vec![], false);
        self.st.resumes.retain_mut(|(b, ids)| {
            if batch.is_some_and(|x| x != *b) {
                return true;
            }
            due.append(ids);
            found = true;
            false
        });
        if !found {
            return;
        }
        for id in &due {
            /* an item waiting to close (or closed) keeps the result it froze: its interrupted turn is
             * not resumed — the restart ended it — and the closing is looked at again in `ready` */
            let closing = self
                .db()
                .session(id)
                .ok()
                .flatten()
                .and_then(|s| s.item_id)
                .and_then(|i| self.db().item(&i).ok().flatten())
                .is_some_and(|i| {
                    matches!(
                        i.phase.as_str(),
                        crate::cycle::CLOSING | crate::cycle::CLOSED
                    )
                });
            if closing || self.rt().recovers_itself(id) {
                if let Err(e) = self.db().store().queued().resume_done(id) {
                    tracing::warn!("resume intent of {id}: {e}");
                }
                continue;
            }
            // the state may have moved between boot and the listener (paused, finished, a new card)
            let permitted = self.db().session(id).ok().flatten().is_some_and(|s| {
                let item = s
                    .item_id
                    .as_deref()
                    .and_then(|i| self.db().item(i).ok().flatten());
                (crate::lifecycle::drain_cut(self.db(), &s.id).unwrap_or(false)
                    || !self.waits_on_user(s.item_id.as_deref()).unwrap_or(true))
                    && s.status != "ended"
                    && !item
                        .as_ref()
                        .is_some_and(|i| i.is_finished() || i.status == "pause")
                    && cwd_usable(Path::new(&s.cwd))
            });
            if !permitted {
                continue;
            }
            self.st.resumed.insert(id.clone());
            if !self.rt().flush(id, Some((RESUME, "steve"))) {
                self.st.resumed.remove(id);
            } else if let Ok(Some(s)) = self.db().session(id)
                && let Some(i) = s.item_id
            {
                let _ = self.db().timeline_add(
                    &i,
                    "steve",
                    "sys",
                    "Hub 重启打断了会话，已自动继续",
                    Some(id),
                );
            }
        }
        if let Err(e) = self.flush_queued() {
            tracing::error!("flush queued {e}");
        }
    }

    /// Prompts held for a restart or a release, or queued behind a turn it cut off, go now (unless the item waits on the user).
    pub(crate) fn flush_queued(&mut self) -> Result<()> {
        for id in self.db().sessions_with_queue()? {
            let Some(s) = self.db().session(&id)? else {
                continue;
            };
            if !cwd_usable(Path::new(&s.cwd)) {
                tracing::warn!("not flushing {id}: cwd {} unusable", s.cwd);
                continue;
            }
            // a resume intent holds the queue for the resumed turn; a session that recovers
            // itself is never resumed, so a leftover intent must not hold it
            if !self.waits_on_user(s.item_id.as_deref())?
                && (self.rt().recovers_itself(&id)
                    || !self.db().store().queued().resume_ids()?.contains(&id))
            {
                self.rt().flush(&id, None);
            }
        }
        Ok(())
    }
}

/// How a new plan relates to the one before: `None` when it keeps every earlier step (the same round,
/// grown or ticked off), else the earlier round ends — `done` if all of it was done, `replaced` if not.
pub(crate) fn plan_turn(before: &[PlanStep], plan: &[PlanStep]) -> Option<&'static str> {
    if before.is_empty() || before.iter().all(|b| plan.iter().any(|p| p.step == b.step)) {
        return None;
    }
    Some(if before.iter().all(|b| b.done) {
        "done"
    } else {
        "replaced"
    })
}

/// The steps left open, for an acceptance card ("" when none).
fn plan_note(plan: &[PlanStep]) -> String {
    let open: Vec<&str> = plan
        .iter()
        .filter(|p| !p.done)
        .map(|p| p.step.as_str())
        .collect();
    if open.is_empty() {
        String::new()
    } else {
        format!("\n\n没做完的计划步骤：{}", open.join("；"))
    }
}
