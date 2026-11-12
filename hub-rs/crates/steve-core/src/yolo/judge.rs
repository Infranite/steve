//! `yolo::judge`: what YOLO would do with one decision, as a pure function
//! of what is known about it ([`Facts`]), the user's [`Policy`] and where Steve lives ([`Env`]).
//!
//! Order: out of scope → [`Verdict::Off`]; a hard limit ([`super::limits::check`]) → `Decline`; the user
//! is present → `Wait`; then the table, where a tool request under full access is judged by what full
//! access promises ([`super::full_guard::full`]) instead of the guard. There are no budgets. Nothing in
//! [`Policy`] reaches the hard limits; widening it only ever turns `Decline` into `Auto` for decisions
//! that passed them.
//!
//! The executor (Y2) gathers the facts from the database, runs this, and runs it again right before
//! acting; a different result then cancels the action.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{guard, limits};

/// Choices YOLO never makes, whatever the facts.
pub const NEVER: &[&str] = &["allow_always", "delivered", "tosteve", "continue_once"];

/// What the user can widen or narrow. The defaults are the spec's; [`Policy::widest`] turns
/// everything on, which the hard-limit tests use. Stored in the workspace setting `yolo.policy`; keys
/// left out keep their default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// `approve` / `action` that reads as local work.
    pub approve_actions: bool,
    /// `accept` / `item`: pass with passing checks, rework without.
    pub accept: bool,
    /// `approve` / `release` and `delivery_batch` of the user's own Steve deployment.
    pub release_self: bool,
    /// `fail` / `item` and `fail` / `stall`: continue a stopped agent.
    pub continue_stalls: bool,
    /// `ask` / `question`: answer for the user.
    pub answer_asks: bool,
    /// The lowest model confidence an answer may have (0.75).
    pub ask_min_confidence: f64,
    /// `propose` / `item`: start when the dependencies are done, or a plan from the user's own chat.
    pub propose: bool,
    /// Bot-identity channel replies to internal audiences.
    pub chan_bot_internal: bool,
    /// Messages and mail as the user, with the `[AI] ` mark.
    pub self_send: bool,
    /// …also when the audience includes people outside the organisation.
    pub self_send_external: bool,
    /// Programs the user lets the tool guard run on top of its own list. Never reaches the hard limits.
    pub extra_programs: Vec<String>,
    /// How long the user counts as present after their last message or answer.
    pub presence_s: u64,
    pub release_delay_s: u64,
    pub propose_chat_delay_s: u64,
    /// The cooling-off before a message or mail goes out as the user (`chan.self_ai`, `mail.self_ai`).
    pub chan_delay_s: u64,
    /// The cooling-off before a bot account's first send to an internal audience (`chan.bot_internal`):
    /// nobody reads it as the user, and it can still be recalled.
    pub chan_bot_delay_s: u64,
    pub chan_retry_delay_s: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            approve_actions: true,
            accept: true,
            release_self: true,
            continue_stalls: true,
            answer_asks: true,
            ask_min_confidence: 0.75,
            propose: true,
            chan_bot_internal: true,
            self_send: true,
            self_send_external: false,
            extra_programs: Vec::new(),
            presence_s: 180,
            release_delay_s: 180,
            propose_chat_delay_s: 120,
            chan_delay_s: 600,
            chan_bot_delay_s: 60,
            chan_retry_delay_s: 300,
        }
    }
}

impl Policy {
    /// Everything on, no confidence floor, no delays, and every program the guard knows of allowed.
    pub fn widest() -> Self {
        Policy {
            approve_actions: true,
            accept: true,
            release_self: true,
            continue_stalls: true,
            answer_asks: true,
            ask_min_confidence: 0.0,
            propose: true,
            chan_bot_internal: true,
            self_send: true,
            self_send_external: true,
            extra_programs: WIDE_PROGRAMS.iter().map(|s| s.to_string()).collect(),
            presence_s: 0,
            release_delay_s: 0,
            propose_chat_delay_s: 0,
            chan_delay_s: 0,
            chan_bot_delay_s: 0,
            chan_retry_delay_s: 0,
        }
    }
}

/// Programs a user could plausibly add to [`Policy::extra_programs`], the dangerous ones included.
pub const WIDE_PROGRAMS: &[&str] = &[
    "git",
    "gh",
    "curl",
    "wget",
    "rm",
    "mv",
    "cp",
    "rsync",
    "ssh",
    "scp",
    "npm",
    "pnpm",
    "yarn",
    "cargo",
    "docker",
    "bash",
    "sh",
    "zsh",
    "eval",
    "sudo",
    "launchctl",
    "kill",
    "pkill",
    "find",
    "xargs",
    "python3",
    "node",
    "make",
    "twine",
    "tee",
    "dd",
    "chmod",
    "open",
    "osascript",
    "security",
    "vercel",
    "aws",
    "kubectl",
    "tar",
    "unzip",
    "sed",
    "perl",
];

/// Where Steve lives on this machine.
#[derive(Debug, Clone)]
pub struct Env {
    /// `STEVE_HOME` (the release script of the user's own deployment lives under it).
    pub steve_home: PathBuf,
    /// The user's home directory (`~`).
    pub home: PathBuf,
    /// Shared temporary directories (`/tmp`, `/private/tmp`, `$TMPDIR`): files may be written there.
    pub tmp: Vec<PathBuf>,
}

impl Env {
    /// The temporary directories of this process: `/tmp`, `/private/tmp` and `$TMPDIR`.
    pub fn tmp_dirs() -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
        if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
            v.push(PathBuf::from(t));
        }
        v
    }
}

/// Outcome of the disclosure check on outbound text (steve-channels `disclosure::Verdict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disclosure {
    Pass,
    Rewritten,
    Leak,
    /// Not checked (yet).
    Unchecked,
}

impl Disclosure {
    pub fn passed(self) -> bool {
        matches!(self, Disclosure::Pass | Disclosure::Rewritten)
    }
}

/// Who a channel message speaks as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// A bot / persona account: does not impersonate the user.
    Bot,
    /// The user's own account (Feishu self, a personal mailbox).
    SelfAccount,
    /// A bot speaking for the user (`on_behalf=proxy`).
    Proxy,
}

/// An outbound message or mail.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outbound {
    pub identity: Identity,
    pub disclosure: Disclosure,
    /// Someone outside the organisation is in the audience.
    pub external: bool,
    /// The space is a customer group.
    pub customer: bool,
    /// The outbound path prefixes this message with `[AI] ` when YOLO approves it. Set by the
    /// adapter that sends it, never from the message text, so the model cannot fake or skip it.
    pub ai_prefix: bool,
    /// The option that sends (`yes`, `retry`, `send`…), as on the card.
    pub send_option: Option<String>,
    /// draft | leak_draft | send_failed | handoff
    pub stage: OutboundStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundStage {
    /// A draft waiting for approval.
    Draft,
    /// A draft the disclosure check turned out of a leak.
    LeakDraft,
    /// Sending failed (`retry` / `discard`).
    SendFailed,
    /// Only `discard` is left (the check blocked it).
    Blocked,
    /// Someone asked for the user in person.
    Handoff,
}

/// A tool permission request (`approve` / `tool_call`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    /// ACP `toolCall.kind`: read | edit | delete | move | search | execute | think | fetch | other.
    pub kind: Option<String>,
    pub title: String,
    /// The untruncated `rawInput`.
    pub raw: Value,
    /// `raw` is not the whole input (the stored draft was cut at 2000 characters).
    pub truncated: bool,
    /// `toolCall.locations[].path`.
    pub locations: Vec<String>,
    /// `(optionId, kind)` per option.
    pub options: Vec<(String, String)>,
    /// The item's own directory (worktree or scratch), if it has one. `None`: the agent works in a real
    /// directory, so nothing it writes is "inside".
    pub cwd: Option<PathBuf>,
    /// The project's main directory (read-only for the guard).
    pub project_dir: Option<PathBuf>,
    /// The item's own branch (`git branch -D` of it stays inside).
    pub branch: Option<String>,
    /// Processes the session's agent started (its descendants and its process group), looked up when
    /// the request came; `kill <pid>` is allowed for these only.
    pub own_pids: Vec<u32>,
    /// The item runs with full access (完全放开) while its harness still asked: a request left over
    /// from the mode it ran in before (the switch lands at the turn's end) or one its executor asks
    /// in any mode. Judged by what full access promises ([`super::full_guard::full`]), not the guard.
    pub full: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckRun {
    pub name: String,
    pub passed: bool,
}

/// A delivery waiting for acceptance (`accept` / `item`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Accept {
    pub checks: Vec<CheckRun>,
    pub unfinished: Vec<String>,
    /// `(clean, ahead of main)` when the item has a branch.
    pub branch: Option<(bool, bool)>,
    /// The item itself says `accept_mode=manual` (an inherited manual mode may be overridden).
    pub explicit_manual: bool,
    /// Automatic reworks in a row without progress.
    pub no_progress: u32,
}

/// A release or delivery batch waiting for approval.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Release {
    /// `delivery_batch` rather than a single item's release.
    pub batch: bool,
    /// `release.kind` of the project (`script`…).
    pub release_kind: String,
    /// `release.cmd` as configured.
    pub cmd: String,
    /// The branch released is the item's branch.
    pub branch_ok: bool,
    /// The item has been accepted.
    pub accepted: bool,
    /// Another release is running or the Hub holds for one.
    pub releasing: bool,
    /// Seconds until the last YOLO release is 15 minutes ago (the executor's spacing rule; 0: none).
    pub spacing_wait_s: u64,
    /// Times a rebase conflict was handed back to the bot for this item.
    pub rebase_returns: u32,
}

/// Why an item stopped (`fail` / `item`, `fail` / `stall`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StallCause {
    /// Ended a turn without delivering or asking.
    Stopped,
    /// The session failed; `restart` = resuming after a Hub restart failed.
    Error { message: String, restart: bool },
    /// A release failed and was rolled back.
    ReleaseFailed {
        prev: String,
        reason: String,
        log: String,
        /// Failed releases of this item so far, this one included.
        nth: u32,
    },
    /// The release could not start: `busy` = another release held the lock; `still_busy` = it still does.
    ReleaseRefused { busy: bool, still_busy: bool },
    /// Something else (no session could be opened…).
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stall {
    pub cause: StallCause,
    /// Automatic continues in a row without progress.
    pub no_progress: u32,
    /// Automatic continues after errors so far (for the back-off).
    pub attempts: u32,
    /// The agent's last words look like a question; the answer comes from [`Ask`].
    pub asked: Option<Ask>,
    /// The agent's last words wait for the user's own action (hard limit 7): what gave it away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own_action: Option<String>,
    /// Steve already continued it on its own (`crate::takeover`) and it still stopped.
    pub steve_tried: bool,
}

/// The model's answer for the user.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    /// `answer` | `ask_user`
    pub act: String,
    pub answer: String,
    pub option: Option<String>,
    pub confidence: f64,
    pub reason: String,
    /// The model only declined for lack of information (not an objection to the recommendation).
    pub only_lacks_info: bool,
}

/// A question (`ask` / `question`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ask {
    pub recommended: Option<String>,
    pub why: Option<String>,
    pub answer: Option<Answer>,
    /// The same question (normalised) was answered by YOLO before: the agent did not accept it.
    pub repeated: bool,
    /// Questions of this item YOLO answered in this window.
    pub count: u32,
    /// The normalised question (kept in the ledger to spot a repeat).
    pub key: String,
    /// The item runs with full access: the agent acts on an answer without the tool guard, so every
    /// outward word counts, not only going live and mail.
    pub full: bool,
}

/// The rule of a tool request judged under full access ([`Tool::full`]).
pub const FULL_RULE: &str = "tool.full";
/// …and its reason when answered.
const FULL_REASON: &str = "完全放开：执行器还在请求权限，按完全放开的边界放行";

/// Rebase conflicts handed back before YOLO leaves the next release card to you.
pub const REBASE_RETURNS: u32 = 3;

/// Answers YOLO gives per item and window.
pub const ASK_PER_ITEM: u32 = 10;

/// What kind of decision it is, with the evidence its row of the table needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Subject {
    Tool(Tool),
    /// `approve` / `action`; the text is the question and draft.
    Action,
    Accept(Accept),
    Release(Release),
    Stall(Stall),
    Ask(Ask),
    /// “前置事项都已完成，现在开始？”
    ProposeDeps,
    /// A plan asked for at creation; `from_chat` = the user's own conversation started it.
    ProposeCreate {
        from_chat: bool,
    },
    /// `approve` / `secret_use`: the item's template and sensitivity.
    SecretUse {
        template: String,
        sensitivity: String,
    },
    /// `approve` / `mail_draft`, `fail` / `mail_draft`.
    Mail(Outbound),
    /// `approve` / `fail` / `ask` on `chan_message`.
    Chan(Outbound),
    /// `gh_pr_open`, `gh_pr_merge`, `gh_comment`.
    Github,
    FlowStep,
    FlowRun {
        over_budget: bool,
    },
    Connection,
    /// `fail` / `delivery_batch`.
    BatchFailed,
    /// `fail` / `mq_batch`: a merge-queue batch whose checks gave no result (`no_result`; else
    /// its bisection reached the limit), run again `reruns` times of the `limit` YOLO may.
    QueueBatch {
        no_result: bool,
        reruns: u32,
        limit: u32,
    },
    /// Cards that block nothing: signals, new spaces, memory candidates, attribute confirmations, merges.
    Muted,
    /// Known kind, but the evidence could not be gathered (preview, a missing row).
    Unverified(String),
    Unknown,
}

/// Everything judged about one decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Facts {
    pub decision_id: String,
    pub kind: String,
    pub subject_type: String,
    pub question: String,
    pub draft: String,
    pub options: Vec<String>,
    /// YOLO is in effect for it (scope, not forced off, not paused).
    pub in_scope: bool,
    /// The user was active on it lately.
    pub present: bool,
    pub subject: Subject,
}

impl Facts {
    /// A tool request of a session that runs with full access ([`Tool::full`]).
    pub fn full_tool(&self) -> bool {
        matches!(&self.subject, Subject::Tool(t) if t.full)
    }
}

/// How a decided action can be undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Undo {
    Decision,
    Rollback,
    Recall,
    Reanswer,
    Pause,
}

/// What YOLO does with the decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Not in scope (or YOLO is off): nothing happens and nothing is recorded.
    Off,
    /// Answer it with `choice` (and `text`) after `delay_s`.
    Auto {
        choice: String,
        text: Option<String>,
        delay_s: u64,
        rule: String,
        reason: String,
        undo: Option<Undo>,
        checks: Value,
    },
    /// Judge again after `delay_s` (the user is around, a release is running).
    Wait {
        rule: String,
        reason: String,
        delay_s: u64,
    },
    /// Leave it to the user; the card says “YOLO 不代办：<reason>”.
    Decline { rule: String, reason: String },
    /// Leave it, without a push notification; it only appears in the digest.
    Mute { rule: String, reason: String },
}

impl Verdict {
    pub fn is_auto(&self) -> bool {
        matches!(self, Verdict::Auto { .. })
    }
    pub fn rule(&self) -> &str {
        match self {
            Verdict::Off => "",
            Verdict::Auto { rule, .. }
            | Verdict::Wait { rule, .. }
            | Verdict::Decline { rule, .. }
            | Verdict::Mute { rule, .. } => rule,
        }
    }
    /// Declined by a hard limit.
    pub fn is_hard_limit(&self) -> bool {
        matches!(self, Verdict::Decline { rule, .. } if rule.starts_with("limit."))
    }
}

fn decline(rule: &str, reason: impl Into<String>) -> Verdict {
    Verdict::Decline {
        rule: rule.into(),
        reason: reason.into(),
    }
}

fn auto(choice: &str, rule: &str, reason: impl Into<String>) -> Verdict {
    Verdict::Auto {
        choice: choice.into(),
        text: None,
        delay_s: 0,
        rule: rule.into(),
        reason: reason.into(),
        undo: None,
        checks: json!({}),
    }
}

trait AutoExt {
    fn text(self, t: String) -> Self;
    fn delay(self, s: u64) -> Self;
    fn undo(self, u: Undo) -> Self;
    fn checks(self, c: Value) -> Self;
}

impl AutoExt for Verdict {
    fn text(mut self, t: String) -> Self {
        if let Verdict::Auto { text, .. } = &mut self {
            *text = Some(t);
        }
        self
    }
    fn delay(mut self, s: u64) -> Self {
        if let Verdict::Auto { delay_s, .. } = &mut self {
            *delay_s = s;
        }
        self
    }
    fn undo(mut self, u: Undo) -> Self {
        if let Verdict::Auto { undo, .. } = &mut self {
            *undo = Some(u);
        }
        self
    }
    fn checks(mut self, c: Value) -> Self {
        if let Verdict::Auto { checks, .. } = &mut self {
            *checks = c;
        }
        self
    }
}

/// The texts sent to a session when YOLO continues it.
pub const CONTINUE_STALL: &str = "你上一回合没有交付也没有提问就停了。继续完成目标；做完用 steve_deliver 交付，真的需要用户才能决定时用 steve_ask（给出 recommended）。";

pub fn continue_error(message: &str) -> String {
    let head: String = message.chars().take(200).collect();
    format!("上一回合出错（{head}）。从你停下的地方继续。")
}

pub fn rework_text(failed: &[String]) -> String {
    let list = if failed.is_empty() {
        "没有列出检查".to_string()
    } else {
        failed
            .iter()
            .map(|n| format!("✗ {n}"))
            .collect::<Vec<_>>()
            .join("、")
    };
    format!("自动验收需要实际运行过的检查全部通过：{list}。修好、运行检查后重新交付。")
}

/// Sent when the release that blocked this one has ended.
pub const RELEASE_AGAIN: &str = "刚才另一个发布正在进行，所以没有发布；它已经结束了。确认分支已提交后，重新用 steve_publish 请求发布。";

pub fn release_failed_text(prev: &str, reason: &str, log: &str) -> String {
    format!("发布失败并已自动回到 {prev}：{reason}。日志 {log}。修好后重新交付。")
}

/// What the session receives when YOLO answers a question for the user.
pub fn answer_text(answer: &str, reason: &str) -> String {
    format!(
        "（YOLO：Steve 代你回答）{answer}。依据：{reason}。如果这个假设后来证明不对，在交付摘要里写明。"
    )
}

/// Back-off before continuing after a session error: 1 / 5 / 15 minutes, from 5 after a restart.
pub fn error_backoff_s(attempts: u32, restart: bool) -> u64 {
    const STEPS: [u64; 3] = [60, 300, 900];
    let i = attempts as usize + usize::from(restart);
    STEPS[i.min(STEPS.len() - 1)]
}

/// A stored reason (a ledger row's, a scope's) as the UI shows it, in the request's language. The reasons are
/// stored in Chinese and read back by rules and the digest; this translates the fixed ones and the fixed frame
/// of the ones with a value, and leaves the rest (a model's own words, a guard's detail) as it is.
pub fn reason_shown(reason: &str) -> String {
    let tail = |p: &str| reason.strip_prefix(p);
    if let Some(w) = tail(super::limits::REASON_PREFIX) {
        return steve_i18n::t!(
            "core.yolo.reason.limit",
            what = super::limits::what_shown(w)
        );
    }
    if let Some(w) = tail("看起来涉及工作区以外：") {
        return steve_i18n::t!("core.yolo.reason.outside", what = w);
    }
    if let Some(w) = tail("选了 Agent 推荐的：") {
        return steve_i18n::t!("core.yolo.reason.recommended", why = reason_shown(w));
    }
    if let Some(w) = tail("YOLO 没替你答：") {
        let why = if w == "拿不准" {
            steve_i18n::t!("core.yolo.reason.unsure")
        } else {
            w.to_string()
        };
        return steve_i18n::t!("core.yolo.reason.not_answered", why = why);
    }
    if let Some(w) = tail("执行前复查结果变了：") {
        return steve_i18n::t!(
            "core.yolo.reason.recheck_changed_why",
            why = reason_shown(w)
        );
    }
    if let Some(w) = tail("执行失败：") {
        return steve_i18n::t!("core.yolo.reason.failed", error = w);
    }
    if let Some(w) = tail("模型调用失败（").and_then(|w| w.strip_suffix('）')) {
        return steve_i18n::t!("core.yolo.reason.model_failed", error = w);
    }
    if let Some(c) = tail("YOLO 不会选「").and_then(|w| w.strip_suffix('」')) {
        return steve_i18n::t!("core.yolo.reason.never", choice = c);
    }
    if reason == format!("这件事项已经代答了 {ASK_PER_ITEM} 次，之后的提问留给你")
    {
        return steve_i18n::t!("core.yolo.reason.ask_count", n = ASK_PER_ITEM);
    }
    if let Some(what) = reason.strip_suffix("要在打开后由执行器核对") {
        match what {
            "检查结果" => return steve_i18n::t!("core.yolo.later.check_results"),
            "发布状态" => return steve_i18n::t!("core.yolo.later.release_state"),
            "停下的原因" => return steve_i18n::t!("core.yolo.later.stop_reason"),
            "代答" => return steve_i18n::t!("core.yolo.later.answer"),
            "事项来源" => return steve_i18n::t!("core.yolo.later.item_source"),
            "保险箱条目" => return steve_i18n::t!("core.yolo.later.vault_entry"),
            "披露检查" => return steve_i18n::t!("core.yolo.later.disclosure"),
            _ => {}
        }
    }
    match reason {
        "你刚在这件事项上操作过，先等你" => {
            steve_i18n::t!("core.yolo.reason.presence")
        }
        "这件事项没有自己的工作区，直接改的是真实目录" => {
            steve_i18n::t!("core.yolo.reason.tool_real_dir")
        }
        "工作区内的操作，守卫检查通过" => {
            steve_i18n::t!("core.yolo.reason.tool_worktree")
        }
        FULL_REASON => steve_i18n::t!("core.yolo.reason.tool_full"),
        "设置里没有让 YOLO 批准动作" => steve_i18n::t!("core.yolo.reason.action_off"),
        "工作区内、可以重做的动作" => steve_i18n::t!("core.yolo.reason.action_local"),
        "设置里没有让 YOLO 批准发布" => steve_i18n::t!("core.yolo.reason.release_off"),
        "另一个发布正在进行，等它结束再判" => {
            steve_i18n::t!("core.yolo.reason.release_busy")
        }
        "发布前的 rebase 已经交回 Bot 3 次" => {
            steve_i18n::t!("core.yolo.reason.release_rebase")
        }
        "分支不对或事项还没验收" => steve_i18n::t!("core.yolo.reason.release_state"),
        "两次发布至少隔 15 分钟，到时间再发" => {
            steve_i18n::t!("core.yolo.reason.release_rate")
        }
        "发布你自己的 Steve，健康检查失败会自动回滚" => {
            steve_i18n::t!("core.yolo.reason.release_self")
        }
        "设置里没有让 YOLO 开始事项" => steve_i18n::t!("core.yolo.reason.propose_off"),
        "前置事项都已完成" => steve_i18n::t!("core.yolo.reason.propose_deps"),
        "这件事项不是你在对话里建的" => {
            steve_i18n::t!("core.yolo.reason.propose_source")
        }
        "你在对话里让 Steve 建的事项" => steve_i18n::t!("core.yolo.reason.propose_chat"),
        "保险箱的使用留给你，需要的话给 Bot 限时授权" => {
            steve_i18n::t!("core.yolo.reason.secret")
        }
        "GitHub 上的操作留给你" => steve_i18n::t!("core.yolo.reason.github"),
        "流程里你自己设的关卡" => steve_i18n::t!("core.yolo.reason.flow_step"),
        "流程停在对外节点，重放会重复对外效果" => {
            steve_i18n::t!("core.yolo.reason.flow_run")
        }
        "需要你本人重新授权" => steve_i18n::t!("core.yolo.reason.connection"),
        "交付批次失败不自动重试" => steve_i18n::t!("core.yolo.reason.batch_failed"),
        "不挡任何事项，只进摘要" => steve_i18n::t!("core.yolo.reason.muted"),
        "YOLO 不认识这种卡" => steve_i18n::t!("core.yolo.reason.unknown"),
        "设置里没有让 YOLO 验收" => steve_i18n::t!("core.yolo.reason.accept_off"),
        "这件事项设成了手动验收" => steve_i18n::t!("core.yolo.reason.accept_manual"),
        "连续 3 次自动返工没有进展" => steve_i18n::t!("core.yolo.reason.stuck_rework"),
        "没有列出检查或有检查没过" => steve_i18n::t!("core.yolo.reason.accept_rework"),
        "还有 Agent 故意留着没做的步骤" => {
            steve_i18n::t!("core.yolo.reason.accept_unfinished")
        }
        "分支有没提交的改动，或没有新提交" => {
            steve_i18n::t!("core.yolo.reason.accept_branch")
        }
        "检查全部通过" => steve_i18n::t!("core.yolo.reason.accept_checks"),
        "设置里没有让 YOLO 让它继续" => steve_i18n::t!("core.yolo.reason.continue_off"),
        "连续 3 次自动继续没有进展" => steve_i18n::t!("core.yolo.reason.stuck_continue"),
        "Steve 已经让它继续过，仍然没有交付，留给你看" => {
            steve_i18n::t!("core.yolo.reason.continue_again")
        }
        "它停下了，没有交付也没有提问" => {
            steve_i18n::t!("core.yolo.reason.continue_stall")
        }
        "会话出错后自动继续" => steve_i18n::t!("core.yolo.reason.continue_error"),
        "这件事项第二次发布失败" => steve_i18n::t!("core.yolo.reason.continue_release"),
        "发布失败已回滚，交回 Bot 修" => {
            steve_i18n::t!("core.yolo.reason.release_rolled_back")
        }
        "另一个发布正在进行，结束后再让它重新请求" => {
            steve_i18n::t!("core.yolo.reason.release_busy_again")
        }
        "另一个发布已经结束，让它重新请求发布" => {
            steve_i18n::t!("core.yolo.reason.release_done_again")
        }
        "这种失败留给你看" => steve_i18n::t!("core.yolo.reason.continue_other"),
        "设置里没有让 YOLO 代答" => steve_i18n::t!("core.yolo.reason.ask_off"),
        "同一个问题又问了一次，Agent 没接受上次的答案" => {
            steve_i18n::t!("core.yolo.reason.ask_repeated")
        }
        "答案涉及硬限制" => steve_i18n::t!("core.yolo.reason.ask_limit"),
        "Agent 的推荐" => steve_i18n::t!("core.yolo.reason.agent_recommended"),
        "卡上没有发送选项" => steve_i18n::t!("core.yolo.reason.no_option"),
        "别人点名找的是你" => steve_i18n::t!("core.yolo.reason.handoff"),
        "披露检查没通过" => steve_i18n::t!("core.yolo.reason.blocked"),
        "没有通过披露检查" => steve_i18n::t!("core.yolo.reason.unchecked"),
        "客户群留给你" => steve_i18n::t!("core.yolo.reason.customer"),
        "设置里没有让 YOLO 代发" => steve_i18n::t!("core.yolo.reason.send_off"),
        "听众里有外部成员" => steve_i18n::t!("core.yolo.reason.external"),
        "设置里没有让 YOLO 以你的名义发" => {
            steve_i18n::t!("core.yolo.reason.self_send_off")
        }
        "披露检查通过，冷静期后发出" => {
            steve_i18n::t!("core.yolo.reason.send_after_cooldown")
        }
        "事项不在了" => steve_i18n::t!("core.yolo.reason.item_gone"),
        "项目没有配置发布" => steve_i18n::t!("core.yolo.reason.no_release_action"),
        "消息不在了" => steve_i18n::t!("core.yolo.reason.message_gone"),
        "邮件草稿不在了" => steve_i18n::t!("core.yolo.reason.mail_draft_gone"),
        "模型没有给出可用的答案" => steve_i18n::t!("core.yolo.reason.model_no_answer"),
        "你已经处理了这张卡" => steve_i18n::t!("core.yolo.reason.handled_by_you"),
        "权限请求已经失效" => steve_i18n::t!("core.yolo.reason.permission_expired"),
        "没有对应的决定" => steve_i18n::t!("core.yolo.reason.no_decision"),
        "YOLO 已经不再处理这件" => steve_i18n::t!("core.yolo.reason.no_longer"),
        "你选择自己来定" => steve_i18n::t!("core.yolo.reason.user_cancel"),
        "执行前复查结果变了" => steve_i18n::t!("core.yolo.reason.recheck_changed"),
        "你在 YOLO 摘要里暂停了这件" => {
            steve_i18n::t!("core.yolo.reason.paused_in_digest")
        }
        "没有 worktree，直接改真实目录" => steve_i18n::t!("core.yolo.reason.real_dir"),
        _ => reason.to_string(),
    }
}

/// Judges one decision. See the module docs for the order.
pub fn judge(f: &Facts, p: &Policy, env: &Env) -> Verdict {
    if !f.in_scope {
        return Verdict::Off;
    }
    if let Some(l) = limits::check(f, env) {
        return decline(&l.rule(), l.reason());
    }
    // a tool call inside the worktree is the guard's to judge: waiting for the user there only holds
    // the agent the user just told what to do (and a message queued behind the call waits for it)
    if f.present && !matches!(f.subject, Subject::Muted | Subject::Tool(_)) {
        return Verdict::Wait {
            rule: "presence".into(),
            reason: "你刚在这件事项上操作过，先等你".into(),
            delay_s: p.presence_s.max(1),
        };
    }
    let v = table(f, p, env);
    // the last word: never a choice from the forbidden list, never one the card does not offer
    if let Verdict::Auto { choice, .. } = &v {
        let offered = match &f.subject {
            Subject::Tool(t) => t
                .options
                .iter()
                .any(|(id, kind)| id == choice && kind == "allow_once"),
            // questions are answered with text; the option list is a suggestion
            Subject::Ask(_) => true,
            Subject::Stall(s) if s.asked.is_some() => has(f, choice),
            _ => has(f, choice),
        };
        if NEVER.contains(&choice.as_str()) || choice.starts_with("grant_") || !offered {
            return decline("never", format!("YOLO 不会选「{choice}」"));
        }
    }
    v
}

fn has(f: &Facts, choice: &str) -> bool {
    f.options
        .iter()
        .any(|o| o == choice || o.split('|').next() == Some(choice))
}

fn table(f: &Facts, p: &Policy, env: &Env) -> Verdict {
    match &f.subject {
        Subject::Tool(t) if t.full => match super::full_guard::full(t, env) {
            Ok(guard::Guard::Allow { option, checks }) => {
                auto(&option, FULL_RULE, FULL_REASON).checks(checks)
            }
            Ok(guard::Guard::Ask(why)) => decline(FULL_RULE, why),
            // a command the line runs inside another one hits a hard limit
            Err(l) => decline(&l.rule(), l.reason()),
        },
        Subject::Tool(t) => {
            if t.cwd.is_none() {
                return decline(
                    "tool.no_worktree",
                    "这件事项没有自己的工作区，直接改的是真实目录",
                );
            }
            match guard::check(t, &p.extra_programs, env) {
                guard::Guard::Allow { option, checks } => {
                    auto(&option, "tool.worktree", "工作区内的操作，守卫检查通过").checks(checks)
                }
                guard::Guard::Ask(why) => decline("tool.guard", why),
            }
        }
        Subject::Action => {
            if !p.approve_actions {
                return decline("action.off", "设置里没有让 YOLO 批准动作");
            }
            match limits::action_outside(&format!("{}\n{}", f.question, f.draft)) {
                Some(w) => decline("action.outside", format!("看起来涉及工作区以外：{w}")),
                None => auto("yes", "action.local", "工作区内、可以重做的动作"),
            }
        }
        Subject::Accept(a) => accept(a, p),
        Subject::Release(r) => {
            if !p.release_self {
                return decline("release.off", "设置里没有让 YOLO 批准发布");
            }
            if r.releasing {
                return Verdict::Wait {
                    rule: "release.wait".into(),
                    reason: "另一个发布正在进行，等它结束再判".into(),
                    delay_s: 60,
                };
            }
            if r.rebase_returns >= REBASE_RETURNS {
                return decline("release.rebase", super::ledger::REBASE_REASON);
            }
            if !r.branch_ok || !r.accepted && !r.batch {
                return decline("release.state", "分支不对或事项还没验收");
            }
            if r.spacing_wait_s > 0 {
                return Verdict::Wait {
                    rule: "release.spacing".into(),
                    reason: "两次发布至少隔 15 分钟，到时间再发".into(),
                    delay_s: r.spacing_wait_s,
                };
            }
            auto(
                "yes",
                "release.self",
                "发布你自己的 Steve，健康检查失败会自动回滚",
            )
            .delay(p.release_delay_s)
            .undo(Undo::Rollback)
        }
        Subject::Stall(s) => stall(s, p),
        Subject::Ask(a) => ask(a, p),
        Subject::ProposeDeps => {
            if !p.propose {
                return decline("propose.off", "设置里没有让 YOLO 开始事项");
            }
            auto("yes", "propose.deps", "前置事项都已完成").undo(Undo::Pause)
        }
        Subject::ProposeCreate { from_chat } => {
            if !p.propose {
                return decline("propose.off", "设置里没有让 YOLO 开始事项");
            }
            if !from_chat {
                return decline("propose.source", "这件事项不是你在对话里建的");
            }
            auto("yes", "propose.chat", "你在对话里让 Steve 建的事项").delay(p.propose_chat_delay_s)
        }
        Subject::SecretUse { .. } => {
            decline("secret", "保险箱的使用留给你，需要的话给 Bot 限时授权")
        }
        Subject::Mail(o) => outbound(o, p, "mail"),
        Subject::Chan(o) => outbound(o, p, "chan"),
        Subject::Github => decline("github", "GitHub 上的操作留给你"),
        Subject::FlowStep => decline("flow", "流程里你自己设的关卡"),
        Subject::FlowRun { .. } => decline("flow", "流程停在对外节点，重放会重复对外效果"),
        Subject::Connection => decline("connection", "需要你本人重新授权"),
        Subject::BatchFailed => decline("batch.failed", "交付批次失败不自动重试"),
        // checks have no outward effect: running them again is YOLO's, as many times as the
        // target's policy says (once by default); landing without them, or sending a batch back,
        // stays yours
        Subject::QueueBatch {
            no_result,
            reruns,
            limit,
        } => {
            if !no_result {
                return decline("mq.limit", "定位失败原因的轮数用完了，留给你");
            }
            if reruns >= limit {
                return decline("mq.rerun_used", "这一批已经重跑过检查，留给你");
            }
            auto(
                "rerun",
                "mq.rerun",
                "检查没有结果，重跑一次：检查没有对外效果",
            )
        }
        Subject::Muted => Verdict::Mute {
            rule: "mute".into(),
            reason: "不挡任何事项，只进摘要".into(),
        },
        Subject::Unverified(why) => decline("unverified", why.clone()),
        Subject::Unknown => decline("unknown", "YOLO 不认识这种卡"),
    }
}

fn accept(a: &Accept, p: &Policy) -> Verdict {
    if !p.accept {
        return decline("accept.off", "设置里没有让 YOLO 验收");
    }
    if a.explicit_manual {
        return decline("accept.manual", "这件事项设成了手动验收");
    }
    if a.no_progress >= 3 {
        return decline("accept.stuck", "连续 3 次自动返工没有进展");
    }
    let failed: Vec<String> = a
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name.clone())
        .collect();
    let checks = json!({ "checks": a.checks, "unfinished": a.unfinished, "branch": a.branch });
    if a.checks.is_empty() || !failed.is_empty() {
        return auto("rework", "accept.rework", "没有列出检查或有检查没过")
            .text(rework_text(&failed))
            .checks(checks);
    }
    if !a.unfinished.is_empty() {
        return decline("accept.unfinished", "还有 Agent 故意留着没做的步骤");
    }
    if let Some((clean, ahead)) = a.branch
        && !(clean && ahead)
    {
        return decline("accept.branch", "分支有没提交的改动，或没有新提交");
    }
    auto("pass", "accept.checks", "检查全部通过")
        .undo(Undo::Decision)
        .checks(checks)
}

fn stall(s: &Stall, p: &Policy) -> Verdict {
    if !p.continue_stalls {
        return decline("continue.off", "设置里没有让 YOLO 让它继续");
    }
    if s.no_progress >= 3 {
        return decline("continue.stuck", "连续 3 次自动继续没有进展");
    }
    if s.steve_tried {
        return decline(
            "continue.steve",
            "Steve 已经让它继续过，仍然没有交付，留给你看",
        );
    }
    match &s.cause {
        StallCause::Stopped => {
            if let Some(q) = &s.asked {
                // it stopped on a question: answer it the way `steve_ask` is answered
                return match ask(q, p) {
                    Verdict::Auto { text, reason, .. } => {
                        auto("continue", "continue.stall", reason).text(text.unwrap_or_default())
                    }
                    Verdict::Decline { reason, .. } => decline("continue.question", reason),
                    v => v,
                };
            }
            auto("continue", "continue.stall", "它停下了，没有交付也没有提问")
                .text(CONTINUE_STALL.into())
                .undo(Undo::Pause)
        }
        StallCause::Error { message, restart } => {
            auto("continue", "continue.error", "会话出错后自动继续")
                .text(continue_error(message))
                .delay(error_backoff_s(s.attempts, *restart))
                .undo(Undo::Pause)
        }
        StallCause::ReleaseFailed {
            prev,
            reason,
            log,
            nth,
        } => {
            if *nth >= 2 {
                return decline("continue.release", "这件事项第二次发布失败");
            }
            auto(
                "continue",
                "continue.release",
                "发布失败已回滚，交回 Bot 修",
            )
            .text(release_failed_text(prev, reason, log))
        }
        StallCause::ReleaseRefused {
            busy: true,
            still_busy: true,
        } => Verdict::Wait {
            rule: "continue.release".into(),
            reason: "另一个发布正在进行，结束后再让它重新请求".into(),
            delay_s: 60,
        },
        StallCause::ReleaseRefused {
            busy: true,
            still_busy: false,
        } => auto(
            "continue",
            "continue.release",
            "另一个发布已经结束，让它重新请求发布",
        )
        .text(RELEASE_AGAIN.into()),
        StallCause::ReleaseRefused { busy: false, .. } | StallCause::Other => {
            decline("continue.other", "这种失败留给你看")
        }
    }
}

fn ask(a: &Ask, p: &Policy) -> Verdict {
    if !p.answer_asks {
        return decline("ask.off", "设置里没有让 YOLO 代答");
    }
    if a.repeated {
        return decline(
            "ask.repeated",
            "同一个问题又问了一次，Agent 没接受上次的答案",
        );
    }
    if a.count >= ASK_PER_ITEM {
        return decline(
            "ask.count",
            format!("这件事项已经代答了 {ASK_PER_ITEM} 次，之后的提问留给你"),
        );
    }
    // without the model's view YOLO cannot tell whether the question is ambiguous: it stays with you
    let Some(ans) = &a.answer else {
        return decline("ask.unsure", "YOLO 没替你答：拿不准");
    };
    let checks = json!({ "question": a.key, "answer": ans, "recommended": a.recommended });
    if limits::money_or_outside_words(&ans.answer, a.full).is_some()
        || ans
            .option
            .as_deref()
            .is_some_and(|o| limits::money_or_outside_words(o, a.full).is_some())
    {
        return decline("ask.limit", "答案涉及硬限制");
    }
    if ans.act == "answer"
        && ans.confidence >= p.ask_min_confidence
        && !ans.answer.trim().is_empty()
    {
        let choice = ans
            .option
            .clone()
            .filter(|o| !o.trim().is_empty())
            .unwrap_or_else(|| ans.answer.trim().to_string());
        return auto(&choice, "ask.main", ans.reason.clone())
            .text(answer_text(&ans.answer, &ans.reason))
            .undo(Undo::Reanswer)
            .checks(checks);
    }
    // the model said nothing against the agent's pick: only “answer” with low confidence, or “ask the
    // user” for lack of information, counts as no objection
    let objected = !(ans.act == "answer" || ans.act == "ask_user" && ans.only_lacks_info);
    match &a.recommended {
        Some(rec)
            if !objected
                && !rec.trim().is_empty()
                && limits::money_or_outside_words(rec, a.full).is_none() =>
        {
            let why = a.why.clone().unwrap_or_else(|| "Agent 的推荐".into());
            auto(rec, "ask.recommended", format!("选了 Agent 推荐的：{why}"))
                .text(answer_text(rec, &why))
                .undo(Undo::Reanswer)
                .checks(checks)
        }
        _ => decline(
            "ask.unsure",
            format!(
                "YOLO 没替你答：{}",
                if ans.reason.trim().is_empty() {
                    "拿不准"
                } else {
                    ans.reason.trim()
                }
            ),
        ),
    }
}

fn outbound(o: &Outbound, p: &Policy, what: &str) -> Verdict {
    let Some(send) = o.send_option.clone() else {
        return decline(&format!("{what}.no_option"), "卡上没有发送选项");
    };
    match o.stage {
        OutboundStage::Handoff => decline(&format!("{what}.handoff"), "别人点名找的是你"),
        OutboundStage::LeakDraft | OutboundStage::Blocked => {
            decline(&format!("{what}.blocked"), "披露检查没通过")
        }
        OutboundStage::Draft | OutboundStage::SendFailed => {
            if !o.disclosure.passed() {
                return decline(&format!("{what}.unchecked"), "没有通过披露检查");
            }
            if o.customer {
                return decline(&format!("{what}.customer"), "客户群留给你");
            }
            let retry = o.stage == OutboundStage::SendFailed;
            let (rule, delay) = match o.identity {
                Identity::Bot => {
                    if !p.chan_bot_internal {
                        return decline(&format!("{what}.off"), "设置里没有让 YOLO 代发");
                    }
                    if o.external {
                        return decline(&format!("{what}.external"), "听众里有外部成员");
                    }
                    if retry {
                        ("chan.retry", p.chan_retry_delay_s)
                    } else {
                        ("chan.bot_internal", p.chan_bot_delay_s)
                    }
                }
                Identity::SelfAccount | Identity::Proxy => {
                    if !p.self_send {
                        return decline(&format!("{what}.off"), "设置里没有让 YOLO 以你的名义发");
                    }
                    if o.external && !p.self_send_external {
                        return decline(&format!("{what}.external"), "听众里有外部成员");
                    }
                    if retry {
                        (
                            if what == "mail" {
                                "mail.retry"
                            } else {
                                "chan.retry"
                            },
                            p.chan_retry_delay_s,
                        )
                    } else {
                        (
                            if what == "mail" {
                                "mail.self_ai"
                            } else {
                                "chan.self_ai"
                            },
                            p.chan_delay_s,
                        )
                    }
                }
            };
            let v = auto(&send, rule, "披露检查通过，冷静期后发出").delay(delay);
            if what == "chan" && !retry {
                v.undo(Undo::Recall)
            } else {
                v
            }
        }
    }
}

/// The parts of a decision row everyone can read, with `subject` left for the caller.
pub fn facts_of(d: &crate::repo::Decision, subject: Subject) -> Facts {
    Facts {
        decision_id: d.id.clone(),
        kind: d.kind.clone(),
        subject_type: d.subject_type.clone(),
        question: d.question.clone(),
        draft: d.draft.clone(),
        options: d.options.clone(),
        in_scope: true,
        present: false,
        subject,
    }
}

/// `<optionId>|<name>|<kind>` options of a permission card as `(optionId, kind)`.
pub fn tool_options(options: &[String]) -> Vec<(String, String)> {
    options
        .iter()
        .map(|o| {
            let mut it = o.split('|');
            let id = it.next().unwrap_or_default().to_string();
            let kind = o.rsplit('|').next().unwrap_or_default().to_string();
            (id, kind)
        })
        .collect()
}

/// The subject a decision row alone can tell: enough for the preview. Evidence only the
/// executor gathers (checks, release state, disclosure verdicts, the full tool input) is missing, so
/// those rows come out as [`Subject::Unverified`] and are declined; the tool input is read from the
/// stored draft, which is cut at 2000 characters, so a draft that long counts as truncated.
pub fn subject_from_row(
    d: &crate::repo::Decision,
    cwd: Option<PathBuf>,
    project_dir: Option<PathBuf>,
    branch: Option<String>,
) -> Subject {
    let later = |what: &str| Subject::Unverified(format!("{what}要在打开后由执行器核对"));
    match (d.kind.as_str(), d.subject_type.as_str()) {
        ("approve", "tool_call") => {
            let raw: Option<Value> = serde_json::from_str(&d.draft).ok();
            let truncated = raw.is_none() || d.draft.chars().count() >= 2000;
            let raw = raw.unwrap_or(Value::Null);
            let kind = if raw.get("command").is_some() {
                Some("execute".to_string())
            } else {
                None
            };
            Subject::Tool(Tool {
                kind,
                title: d.question.trim_start_matches("允许执行：").to_string(),
                raw,
                truncated,
                locations: Vec::new(),
                options: tool_options(&d.options),
                cwd,
                project_dir,
                branch,
                own_pids: Vec::new(),
                full: false,
            })
        }
        ("approve", "action") => Subject::Action,
        ("accept", "item") => later("检查结果"),
        ("approve", "release") | ("approve", "delivery_batch") => later("发布状态"),
        ("fail", "delivery_batch") => Subject::BatchFailed,
        ("fail", "mq_batch") => later("合并队列批次"),
        ("fail", "item") | ("fail", "stall") => later("停下的原因"),
        ("ask", "question") => later("代答"),
        ("propose", "item") if d.question.starts_with("前置事项都已完成") => {
            Subject::ProposeDeps
        }
        ("propose", "item") => later("事项来源"),
        ("approve", "secret_use") => later("保险箱条目"),
        (_, "mail_draft") | (_, "chan_message") => later("披露检查"),
        ("approve", "gh_pr_open" | "gh_pr_merge" | "gh_comment") => Subject::Github,
        (_, "flow_step") => Subject::FlowStep,
        (_, "flow_run") => Subject::FlowRun { over_budget: false },
        (_, "connection") => Subject::Connection,
        ("ask", "signal" | "space")
        | ("confirm", "mem" | "attr_value" | "attr_def" | "steward")
        | ("propose", "party_merge") => Subject::Muted,
        _ => Subject::Unknown,
    }
}
