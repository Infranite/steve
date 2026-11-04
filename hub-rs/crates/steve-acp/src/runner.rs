//! The ACP runner (acp.ts): one adapter process per live session; start / load, the prompt queue,
//! cancel, permission requests, idle release, gateway retry. Knows nothing about items or the database:
//! everything it persists goes through [`Host`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};

use crate::attachments::{FileRef, StoredFile, for_agent};
use crate::codex::{codex_args, codex_mode};
use crate::config::AcpConfig;
use crate::rpc::{Conn, Incoming, RpcError, Spawn};
use crate::util::{coalesce, js_slice, js_str, json_indent1, truthy};

fn effort_option(config: &Value) -> Option<&Value> {
    config.get("configOptions")?.as_array()?.iter().find(|o| {
        o.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| ["thought_level", "reasoning_effort", "effort"].contains(&id))
            || o.get("category").and_then(Value::as_str) == Some("thought_level")
    })
}

fn option_supports(options: &Value, effort: &str) -> bool {
    options.as_array().is_some_and(|os| {
        os.iter().any(|o| {
            o.get("value").and_then(Value::as_str) == Some(effort)
                || o.get("options").is_some_and(|v| option_supports(v, effort))
        })
    })
}

/// The session fields the runner reads (a `sessions` row plus the bot's `perm_mode`).
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub id: String,
    /// `claude-code` or `codex`.
    pub harness: String,
    pub cwd: String,
    pub acp_session_id: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    /// `bots.perm_mode` of the session's bot; `None` when the session has no bot.
    pub perm_mode: Option<String>,
    /// `sessions.status`; only `starting` matters here (a held first prompt resets it to `idle`).
    pub status: String,
}

/// One user/steve message of a turn; queued rows come back as these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub via: String,
    /// attachments; they wait in the queue with the message (also across a restart)
    pub files: Vec<FileRef>,
}

impl Message {
    /// `{text, via}` plus `files` when there are any: the `user` / `queued` event data.
    fn event(&self) -> Value {
        let mut v = json!({ "text": self.text, "via": self.via });
        if !self.files.is_empty() {
            v["files"] = Value::Array(self.files.iter().map(FileRef::to_json).collect());
        }
        v
    }
}

/// From a `usage_update`: `used`/`size` are written as given (null included); `cost` only when present.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub used: Option<i64>,
    pub size: Option<i64>,
    pub cost: Option<f64>,
}

/// Where the session's adapter reaches Steve's MCP tools.
#[derive(Debug, Clone, Default)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
}

/// A tool permission request that was not auto-allowed. The host turns it into a decision
/// `{kind:"approve", subject_type:"tool_call", item_id, session_id, requested_by: bot ‖ "agent", …these fields}`.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// `toolCall.toolCallId ?? String(rpcId)`.
    pub subject_id: String,
    /// `允许执行：<title>`.
    pub question: String,
    /// `JSON.stringify(rawInput ?? {}, null, 1)[:2000]`.
    pub draft: String,
    /// `<optionId>|<name>|<kind>` per option.
    pub options: Vec<String>,
    /// `toolCall.title ?? toolCall.kind ?? "工具调用"`.
    pub title: String,
    /// The whole `rawInput` (`{}` when absent), for the YOLO tool guard: `draft` is cut at 2000 characters.
    pub raw: Value,
    /// `toolCall.kind` (read | edit | delete | move | search | execute | think | fetch | other).
    pub kind: Option<String>,
    /// `toolCall.locations[].path`.
    pub locations: Vec<String>,
}

/// Passed to [`Host::turn_end`] once a turn finishes with nothing queued behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnd {
    pub session_id: String,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Passed to [`Host::turn_begin`] before every `session/prompt`: the first try, each gateway retry and
/// each queued follow-up get their own turn (stats.md §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBegin {
    pub session_id: String,
    /// 0 = first try, k = k-th automatic retry.
    pub attempt: u32,
    /// How many `user` events this turn just wrote (0 for retries): the host maps them to `user_seq`.
    pub user_events: usize,
    /// Unix ms.
    pub started: i64,
}

/// One row of a turn's token figures, as the adapter reported them (converted by the host).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsage {
    /// The model that answered; `None` = unknown (the host falls back to the session's / bot's model).
    pub model: Option<String>,
    /// The adapter's usage object (`PromptResponse.usage` or a `model_usage[].token_count`).
    pub raw: Value,
    /// `all` = `_meta.quota.model_usage` (subagents and compaction included, the accounting figure);
    /// `main` = `usage`, the main loop only.
    pub scope: &'static str,
}

/// Passed to [`Host::turn_finish`] once the prompt answered (or failed).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnFinish {
    /// What [`Host::turn_begin`] returned.
    pub turn_id: String,
    pub session_id: String,
    /// Unix ms.
    pub ended: i64,
    /// end_turn | cancelled | max_tokens | refusal | error | …
    pub stop_reason: String,
    pub error: Option<String>,
    /// The error is a transient gateway one and the runner retries.
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    /// Calls per bucket: the ACP `kind` (`execute`, `edit`…), `mcp:<tool>` for Steve's own MCP tools;
    /// besides those, `tool:<name>` per Claude Code tool name (a second dimension, not part of `tools`).
    pub tool_kinds: BTreeMap<String, i64>,
    /// Permission requests that needed the user (Steve's own tools are auto-allowed and not counted).
    pub permissions: i64,
    /// Empty when the adapter reported nothing (an RPC error, an adapter that does not report).
    pub usage: Vec<TurnUsage>,
    /// The model of the last `usage_update` in this turn (`_meta["_claude/model"]`).
    pub model: Option<String>,
    /// This turn's share of the adapter's own dollar figure: the process total now minus what earlier
    /// turns already booked (the total itself when it went down, i.e. the adapter restarted).
    /// `None` when the adapter never reported a cost.
    pub cost: Option<f64>,
}

/// A cost increment of work the adapter started on its own (Claude Code's background cycles), reported
/// by a `usage_update` with `_meta["_claude/origin"]` while no turn of ours runs. No token tally.
#[derive(Debug, Clone, PartialEq)]
pub struct AutonomousUsage {
    pub session_id: String,
    pub ts: i64,
    pub model: Option<String>,
    pub origin: String,
    pub cost: f64,
}

/// What the runner needs from the rest of the Hub. Calls are synchronous and should be quick (SQLite
/// writes). `queue_add`/`queue_take` run under the runner's lock so that queueing and draining never
/// race; they must not call back into [`Acp`]. `turn_end` runs with no lock held and may call anything.
pub trait Host: Send + Sync + 'static {
    fn session(&self, session_id: &str) -> Option<SessionInfo>;
    fn set_status(&self, session_id: &str, status: &str);
    fn set_acp_session_id(&self, session_id: &str, acp_session_id: &str);
    fn set_model(&self, session_id: &str, model: &str);
    fn set_reasoning_effort(&self, _session_id: &str, _effort: &str) {}
    fn set_usage(&self, session_id: &str, usage: Usage);
    /// Store an event and publish it (SSE).
    fn add_event(&self, session_id: &str, kind: &str, data: Value);
    /// Queue a message; false when the same message (text and attachments) is already waiting, so
    /// nothing was added and no second `queued` event is written.
    fn queue_add(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<bool, String>;
    /// Atomically claim queued rows in order. Durable hosts retain them until a dispatch receipt.
    fn queue_take(&self, session_id: &str) -> Vec<Message>;
    /// Durable local receipt; fail closed if the storage write fails.
    fn dispatch_submitted(&self, _session_id: &str) -> bool {
        true
    }
    fn dispatch_finished(&self, _session_id: &str, _state: &str) {}
    /// Restore an unsubmitted claim to the user-visible queue after a receipt write failure.
    fn dispatch_deferred(&self, _session_id: &str) {}
    fn maintenance(&self) -> bool {
        false
    }
    /// Number of queued rows (TS `queued.list(id).length`).
    fn queue_len(&self, session_id: &str) -> usize;
    /// The stored attachment (row + blob path), or `None` when it is gone.
    fn file(&self, file_id: &str) -> Option<StoredFile>;
    fn mcp_endpoint(&self, session: &SessionInfo) -> McpEndpoint;
    /// Create the approval decision; returns its id, or `None` if it could not be created.
    fn permission_decision(&self, session_id: &str, req: PermissionRequest) -> Option<String>;
    /// Permission cards that can no longer be answered (their turn ended, or the adapter that asked
    /// is gone): close them, so nobody answers a question nobody is waiting for.
    fn permissions_expired(&self, _session_id: &str, _decision_ids: &[String]) {}
    fn turn_end(&self, t: TurnEnd);
    /// A prompt is about to be sent; returns the turn id to finish it with, `None` to not record it.
    fn turn_begin(&self, _b: TurnBegin) -> Option<String> {
        None
    }
    /// The turn begun with [`Self::turn_begin`] is over (called before [`Self::turn_end`]).
    fn turn_finish(&self, _f: TurnFinish) {}
    /// Cost the adapter reported for work outside our turns.
    fn autonomous_usage(&self, _u: AutonomousUsage) {}
}

/// What a running turn has seen so far (tool calls, permission requests, the model in use).
#[derive(Debug, Default)]
struct Tally {
    tools: i64,
    tool_errors: i64,
    tool_kinds: BTreeMap<String, i64>,
    permissions: i64,
    model: Option<String>,
}

/// The adapter process's running `total_cost_usd` and how much of it turns have booked.
#[derive(Debug, Clone, Copy)]
struct Cost {
    generation: u64,
    now: f64,
    booked: f64,
}

impl Cost {
    /// What is not booked yet, booking it. A total below the booked one means the count restarted.
    fn take(&mut self) -> f64 {
        let d = if self.now >= self.booked {
            self.now - self.booked
        } else {
            self.now
        };
        self.booked = self.now;
        d
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The token rows of a `session/prompt` response: per model from `_meta.quota.model_usage` when there
/// are any (scope `all`), else the main-loop `usage` under `model` (scope `main`).
fn turn_usage(resp: &Value, model: Option<&str>) -> Vec<TurnUsage> {
    let per_model: Vec<TurnUsage> = resp
        .pointer("/_meta/quota/model_usage")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let raw = r.get("token_count").filter(|v| v.is_object())?;
                    Some(TurnUsage {
                        model: r
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|m| !m.is_empty())
                            .map(str::to_owned),
                        raw: raw.clone(),
                        scope: "all",
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if !per_model.is_empty() {
        return per_model;
    }
    match resp.get("usage").filter(|v| v.is_object()) {
        Some(u) => vec![TurnUsage {
            model: model.map(str::to_owned),
            raw: u.clone(),
            scope: "main",
        }],
        None => vec![],
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("no session")]
    NoSession,
    #[error("{0}")]
    Other(String),
}

/// Outcome of [`Acp::prompt`]; serialises to the TS return values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompted {
    Sent,
    Queued,
}

impl Prompted {
    pub fn as_str(self) -> &'static str {
        match self {
            Prompted::Sent => "sent",
            Prompted::Queued => "queued",
        }
    }
}

struct Live {
    conn: Arc<Conn>,
    generation: u64,
    acp_id: String,
    busy: bool,
    /// the current turn's `session/prompt` is written: a `session/cancel` now reaches it
    prompting: bool,
    idle: Option<u64>,
    /// `initialize` said `promptCapabilities.image`
    image: bool,
}

#[derive(Default)]
struct State {
    live: HashMap<String, Live>,
    /// sessions with a turn in flight, also while the adapter is still starting or a retry waits
    turning: HashSet<String>,
    retrying: HashMap<String, u64>,
    /// decision id → (session id, rpc id). Like the TS, entries outlive the process that asked.
    /// decision id → (session, the asking request's rpc id, the adapter process generation that asked,
    /// the prompt it asked in)
    waits: HashMap<String, (String, Value, u64, u64)>,
    /// every spawned adapter, including ones still starting, so shutdown leaves no orphans
    procs: HashMap<u64, Arc<Conn>>,
    drain_groups: HashSet<u32>,
    next_token: u64,
    closed: bool,
    /// a release is about to restart the Hub (see [`Acp::hold`])
    holding: bool,
    /// cancels that came before the turn's prompt was written (adapter still starting): sent right after it
    cancel_pending: HashSet<String>,
    /// the prompt each session is running now (a token per `session/prompt`, set before it is written,
    /// gone once it returned): permission cards belong to one prompt
    prompts: HashMap<String, u64>,
    /// prompts being cancelled: a permission request of one is answered `cancelled` at once
    cancelled: HashSet<u64>,
    /// per session, the counters of the turn being run (stats)
    tallies: HashMap<String, Tally>,
    /// per session, the live adapter's cost total (stats)
    costs: HashMap<String, Cost>,
}

impl State {
    /// The session's turn is over (or never started): it leaves `turning`, and a cancel meant for it is moot.
    fn end_turning(&mut self, sid: &str) {
        self.turning.remove(sid);
        self.cancel_pending.remove(sid);
    }
}

impl State {
    /// Mark a prompt cancelled and take the permission requests it still waits on: ACP has the
    /// cancelling client answer each of them `cancelled`, so the agent can end the turn.
    fn cancel_prompt(&mut self, prompt: u64) -> Vec<(String, Value)> {
        self.cancelled.insert(prompt);
        let ids: Vec<String> = self
            .waits
            .iter()
            .filter(|(_, w)| w.3 == prompt)
            .map(|(d, _)| d.clone())
            .collect();
        ids.into_iter()
            .filter_map(|d| self.waits.remove(&d).map(|w| (d, w.1)))
            .collect()
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }
}

struct Inner {
    cfg: AcpConfig,
    host: Arc<dyn Host>,
    st: Mutex<State>,
}

/// The runner. Cheap to clone; all clones share state. Must be used inside a tokio runtime.
#[derive(Clone)]
pub struct Acp(Arc<Inner>);

/// transient gateway errors (503, overloaded, rate limits) retry the same prompt on the same session a
/// bounded number of times. JS `\b` is ASCII-only, hence `(?-u:\b)`.
/// The adapter is alive but the agent behind its session is gone (the Claude Code process was killed or
/// crashed): every later prompt on that adapter fails with "Resource not found". The adapter must be
/// dropped so the next turn starts a fresh one that loads the session from the harness's history.
static DEAD_AGENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)process exited unexpectedly|start a new session|resource not found|agent process died")
        .expect("static regex")
});

/// What a gateway retry sends instead of the same message when the failed attempt had already run tools:
/// their effects happened, so the agent picks up where it stopped instead of doing the work again.
pub const RESUME_AFTER_TOOLS: &str = "刚才推理网关中断了，这一轮你已经调用过工具，那些操作已经生效。请先核对已经完成的部分，再从中断的地方继续，不要重复已经做过的操作。";

static TRANSIENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?-u:\b)(429|502|503|504|529)(?-u:\b)|temporarily|overloaded|try again|rate.?limit",
    )
    .expect("static regex")
});
static CC_TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^mcp__steve__([0-9A-Za-z_]+)$").expect("static regex"));
static CODEX_TOOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^Allow the steve MCP server to run tool "([0-9A-Za-z_]+)"\?$"#)
        .expect("static regex")
});

/// every tool in mcp.ts only reports to Steve or asks the user; none has outside effects by itself.
/// The knowledge, contacts, mail, GitHub-status and event tools read what the session's user may
/// see, save a candidate the user still confirms, or (`steve_mail_draft`) only draft and open an
/// approval; none reaches outside Steve. Not here: `steve_secret_use` (runs commands) and the
/// GitHub comment / open-PR tools.
const OWN_TOOLS: [&str; 19] = [
    "steve_update",
    "steve_ask",
    "steve_deliver",
    "steve_request_approval",
    "steve_publish",
    "steve_knowledge_search",
    "steve_knowledge_read",
    "steve_remember",
    "steve_contacts_find",
    "steve_contacts_remember",
    "steve_whois",
    "steve_person_holdings",
    "steve_steward_propose",
    "steve_mail_thread",
    "steve_mail_attachment",
    "steve_mail_draft",
    "steve_github_status",
    "steve_github_ci_log",
    "steve_event_get",
];

/// The Steve MCP tool a permission request is for, read from the structured request rather than a
/// free-text title (a shell command's title is the command itself): Claude Code names the tool
/// `mcp__steve__<tool>`; Codex sends an MCP approval form that names the server and the tool.
pub fn steve_tool(params: &Value) -> Option<String> {
    let tc = params.get("toolCall").filter(|v| !v.is_null());
    let name = coalesce([
        tc.and_then(|t| t.pointer("/_meta/claudeCode/toolName")),
        tc.and_then(|t| t.get("name")),
    ]);
    let name = match name {
        Some(v) => js_str(Some(v)),
        None => String::new(),
    };
    if let Some(c) = CC_TOOL.captures(&name) {
        return Some(c[1].to_owned());
    }
    let ri = tc.and_then(|t| t.get("rawInput"));
    let req = ri.and_then(|r| r.get("request"));
    if ri
        .and_then(|r| r.get("server_name"))
        .and_then(Value::as_str)
        == Some("steve")
        && req
            .and_then(|r| r.pointer("/_meta/codex_approval_kind"))
            .and_then(Value::as_str)
            == Some("mcp_tool_call")
    {
        let msg = match coalesce([req.and_then(|r| r.get("message"))]) {
            Some(v) => js_str(Some(v)),
            None => String::new(),
        };
        if let Some(c) = CODEX_TOOL.captures(&msg) {
            return Some(c[1].to_owned());
        }
    }
    None
}

/// The `allow_once` option to answer with when the request is for one of Steve's own tools.
/// Never `allow_always`: nothing is persisted into the harness's settings.
pub fn auto_allow(params: &Value) -> Option<String> {
    let tool = steve_tool(params)?;
    if !OWN_TOOLS.contains(&tool.as_str()) {
        return None;
    }
    let once = params
        .get("options")?
        .as_array()?
        .iter()
        .find(|o| o.get("kind").and_then(Value::as_str) == Some("allow_once"))?;
    Some(js_str(once.get("optionId")))
}

/// The decision fields for a permission request that needs the user.
pub fn permission_request(rpc_id: &Value, params: &Value) -> PermissionRequest {
    let tc = params.get("toolCall").filter(|v| !v.is_null());
    let title = match coalesce([
        tc.and_then(|t| t.get("title")),
        tc.and_then(|t| t.get("kind")),
    ]) {
        Some(v) => js_str(Some(v)),
        None => "工具调用".to_owned(),
    };
    let subject_id = match coalesce([tc.and_then(|t| t.get("toolCallId"))]) {
        Some(v) => js_str(Some(v)),
        None => js_str(Some(rpc_id)),
    };
    let empty = json!({});
    let raw = coalesce([tc.and_then(|t| t.get("rawInput"))]).unwrap_or(&empty);
    let draft = js_slice(&json_indent1(raw), 2000).to_owned();
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|os| {
            os.iter()
                .map(|o| {
                    format!(
                        "{}|{}|{}",
                        js_str(o.get("optionId")),
                        js_str(o.get("name")),
                        js_str(o.get("kind"))
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let kind = tc
        .and_then(|t| t.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let locations = tc
        .and_then(|t| t.get("locations"))
        .and_then(Value::as_array)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| l.get("path").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    PermissionRequest {
        subject_id,
        question: format!("允许执行：{title}"),
        draft,
        options,
        title,
        raw: raw.clone(),
        kind,
        locations,
    }
}

fn selected(option_id: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

impl Acp {
    pub fn new(cfg: AcpConfig, host: Arc<dyn Host>) -> Self {
        Acp(Arc::new(Inner {
            cfg,
            host,
            st: Mutex::new(State::default()),
        }))
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn host(&self) -> &dyn Host {
        &*self.0.host
    }

    /// Send a prompt now, or queue it behind the running turn (or behind the hold before a restart).
    pub async fn prompt(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
    ) -> Result<Prompted, AcpError> {
        self.prompt_files(session_id, text, via, vec![]).await
    }

    /// [`Self::prompt`] with attachments.
    pub async fn prompt_files(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: Vec<FileRef>,
    ) -> Result<Prompted, AcpError> {
        let msg = Message {
            text: text.to_owned(),
            via: via.to_owned(),
            files,
        };
        let Some(s0) = self.host().session(session_id) else {
            return Err(AcpError::NoSession);
        };
        {
            let mut st = self.st();
            if st.turning.contains(session_id) || st.holding || self.host().maintenance() {
                // a session that never ran is not "interrupted" by the restart: its first prompt just waits
                if st.holding && !st.turning.contains(session_id) && s0.status == "starting" {
                    self.host().set_status(session_id, "idle");
                }
                let added = self
                    .host()
                    .queue_add(session_id, text, via, &msg.files)
                    .map_err(AcpError::Other)?;
                drop(st);
                if added {
                    self.host().add_event(session_id, "queued", msg.event());
                }
                return Ok(Prompted::Queued);
            }
            // Persist even the immediate path before giving the caller a sent receipt.
            self.host()
                .queue_add(session_id, text, via, &msg.files)
                .map_err(AcpError::Other)?;
            let msgs = self.host().queue_take(session_id);
            if msgs.is_empty() {
                return Err(AcpError::Other("无法持久保存消息，请重试".into()));
            }
            st.turning.insert(session_id.to_owned());
            drop(st);
            self.start_turn(session_id.to_owned(), msgs);
        }
        Ok(Prompted::Sent)
    }

    /// A release is about to restart the Hub: no new turn starts, prompts wait in the queue (in the
    /// database, so they survive the restart) and run when the hold is lifted or after the restart;
    /// running turns finish normally.
    pub fn hold(&self, on: bool) {
        self.st().holding = on;
    }

    pub fn is_holding(&self) -> bool {
        self.st().holding
    }

    /// Turns in flight, including ones waiting for a permission answer or a retry.
    pub fn running_turns(&self) -> usize {
        self.st().turning.len()
    }

    /// Run what is queued for a session as one turn, optionally led by Steve's own message;
    /// identical messages (text and attachment ids) are sent once. False when a turn is running, while holding, or with nothing to run.
    pub fn flush(&self, session_id: &str, lead: Option<Message>) -> bool {
        let msgs = {
            let mut st = self.st();
            if st.turning.contains(session_id) || st.holding || self.host().maintenance() {
                return false;
            }
            if let Some(m) = &lead
                && let Err(e) = self.host().queue_add(session_id, &m.text, &m.via, &m.files)
            {
                tracing::error!("queue {session_id}: {e}");
                return false;
            }
            let mut seen = HashSet::new();
            let claimed = self.host().queue_take(session_id);
            if claimed.is_empty() {
                return false;
            }
            let msgs: Vec<Message> = lead
                .into_iter()
                .chain(claimed)
                .filter(|m| {
                    let ids: Vec<&str> = m.files.iter().map(|f| f.id.as_str()).collect();
                    seen.insert(format!("{}\u{0}{}", m.text, ids.join(",")))
                })
                .collect();
            if msgs.is_empty() {
                return false;
            }
            st.turning.insert(session_id.to_owned());
            msgs
        };
        self.start_turn(session_id.to_owned(), msgs);
        true
    }

    /// The synchronous start of a turn (TS `runTurn` up to its first await): `user` events on the first
    /// attempt and the status. `None` (and out of `turning`) when the session row is gone.
    fn begin(&self, sid: &str, msgs: &[Message], attempt: usize) -> Option<SessionInfo> {
        let Some(s) = self.host().session(sid) else {
            self.st().end_turning(sid);
            return None;
        };
        if attempt == 0 {
            for m in msgs {
                self.host().add_event(sid, "user", m.event());
            }
        }
        self.host().set_status(
            sid,
            if s.acp_session_id.is_some() {
                "running"
            } else {
                "starting"
            },
        );
        Some(s)
    }

    /// Begins a turn now and runs the rest on a task. The session is already in `turning`.
    fn start_turn(&self, sid: String, msgs: Vec<Message>) {
        if let Some(s) = self.begin(&sid, &msgs, 0) {
            let me = self.clone();
            tokio::spawn(async move { me.run_turn(sid, msgs, s).await });
        }
    }

    /// A turn (and the turns queued behind it), after [`Self::begin`].
    async fn run_turn(self, sid: String, mut msgs: Vec<Message>, first: SessionInfo) {
        let mut attempt = 0usize;
        let mut next_session = Some(first);
        loop {
            let s = match next_session.take() {
                Some(s) => s,
                None => match self.begin(&sid, &msgs, attempt) {
                    Some(s) => s,
                    None => return,
                },
            };
            let turn = self.turn_open(&sid, attempt, if attempt == 0 { msgs.len() } else { 0 });
            let (conn, generation, acp_id, image) = match self.ensure(&s).await {
                Ok(x) => x,
                Err(e) => {
                    self.turn_close(&sid, turn, None, None, "error", Some(e.clone()), false);
                    self.st().end_turning(&sid);
                    self.host().set_status(&sid, "error");
                    self.host().add_event(
                        &sid,
                        "system",
                        json!({ "text": format!("无法启动 {}：{e}", s.harness) }),
                    );
                    self.host().turn_end(TurnEnd {
                        session_id: sid,
                        stop_reason: "error".into(),
                        error: Some(e),
                    });
                    return;
                }
            };
            self.host().set_status(&sid, "running");
            let started = now_ms();
            let prompt = self.prompt_blocks(&s, &msgs, image);
            if !self.host().dispatch_submitted(&sid) {
                self.host().dispatch_deferred(&sid);
                self.st().end_turning(&sid);
                self.host().set_status(&sid, "error");
                self.host().add_event(
                    &sid,
                    "system",
                    json!({"text":"无法保存发送收据；消息已保留，尚未提交给智能体。"}),
                );
                return;
            }
            let ptoken = {
                let mut st = self.st();
                let t = st.token();
                st.prompts.insert(sid.clone(), t);
                t
            };
            let req = conn.request(
                "session/prompt",
                json!({ "sessionId": acp_id, "prompt": prompt }),
                None,
            );
            let mut req = std::pin::pin!(req);
            // the first poll writes the prompt; only then can a cancel reach this turn
            let mut early = None;
            std::future::poll_fn(|cx| {
                if let std::task::Poll::Ready(v) = req.as_mut().poll(cx) {
                    early = Some(v);
                }
                std::task::Poll::Ready(())
            })
            .await;
            let deferred = {
                let mut st = self.st();
                if let Some(l) = st.live.get_mut(&sid).filter(|l| l.generation == generation) {
                    l.prompting = true;
                }
                st.cancel_pending
                    .remove(&sid)
                    .then(|| st.cancel_prompt(ptoken))
            };
            if let Some(waits) = deferred {
                conn.notify("session/cancel", json!({ "sessionId": acp_id }));
                let ids: Vec<String> = waits
                    .into_iter()
                    .map(|(d, rpc_id)| {
                        conn.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } }));
                        d
                    })
                    .collect();
                if !ids.is_empty() {
                    self.host().permissions_expired(&sid, &ids);
                }
            }
            let r = match early {
                Some(r) => r,
                None => req.await,
            };
            let (stop_reason, error, resp) = match r {
                Ok(r) => (
                    r.get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("end_turn")
                        .to_owned(),
                    None,
                    Some(r),
                ),
                Err(e) => {
                    self.host().add_event(
                        &sid,
                        "system",
                        json!({ "text": format!("这一回合出错：{}", e.message) }),
                    );
                    ("error".to_owned(), Some(e.message), None)
                }
            };
            // the prompt returned: whatever it still waited on can't be answered, a cancel of it is over
            let stale: Vec<String> = {
                let mut st = self.st();
                if st.prompts.get(&sid) == Some(&ptoken) {
                    st.prompts.remove(&sid);
                }
                st.cancelled.remove(&ptoken);
                let ids: Vec<String> = st
                    .waits
                    .iter()
                    .filter(|(_, w)| w.3 == ptoken)
                    .map(|(d, _)| d.clone())
                    .collect();
                for d in &ids {
                    st.waits.remove(d);
                }
                ids
            };
            if !stale.is_empty() {
                self.host().permissions_expired(&sid, &stale);
            }
            let retry = error
                .as_deref()
                .is_some_and(|e| TRANSIENT.is_match(e) && attempt < self.0.cfg.retry_delays.len());
            // tools already ran in this attempt: their effects happened, the message must not be acted on again
            let acted = self.st().tallies.get(&sid).is_some_and(|t| t.tools > 0);
            // codex-acp reports no tokens; Codex's own rollout file has them
            let resp = match resp {
                Some(r) if r.get("usage").is_some_and(Value::is_object) => Some(r),
                r if s.harness == "codex" => {
                    let u = crate::rollout::turn_usage(
                        &self.0.cfg.codex_home,
                        &acp_id,
                        started,
                        now_ms(),
                        Duration::from_secs(3),
                    )
                    .await;
                    match (r, u) {
                        (Some(mut r), Some(u)) => {
                            if let Some(o) = r.as_object_mut() {
                                o.insert("usage".into(), u);
                            }
                            Some(r)
                        }
                        (None, Some(u)) => Some(json!({ "usage": u })),
                        (r, None) => r,
                    }
                }
                r => r,
            };
            self.turn_close(
                &sid,
                turn,
                Some(generation),
                resp.as_ref(),
                &stop_reason,
                error.clone(),
                retry,
            );
            if let Some(l) = self
                .st()
                .live
                .get_mut(&sid)
                .filter(|l| l.generation == generation)
            {
                l.busy = false;
                l.prompting = false;
            }
            if error.as_deref().is_some_and(|e| DEAD_AGENT.is_match(e)) {
                let dead = {
                    let mut st = self.st();
                    match st.live.get(&sid) {
                        Some(l) if l.generation == generation => st.live.remove(&sid),
                        _ => None,
                    }
                };
                if let Some(l) = dead {
                    l.conn.kill();
                    tracing::warn!(session = %sid, "agent behind the adapter is gone; the next turn starts a fresh adapter");
                }
            }

            // the session stays "in a turn" while waiting, so new messages queue behind the retry; cancel drops it
            if retry {
                let wait = self.0.cfg.retry_delays[attempt];
                self.host().add_event(
                    &sid,
                    "system",
                    json!({ "text": format!("推理网关暂时不可用，{} 秒后自动重试（第 {} 次）", wait.as_secs(), attempt + 1) }),
                );
                let token = {
                    let mut st = self.st();
                    let t = st.token();
                    st.retrying.insert(sid.clone(), t);
                    t
                };
                tokio::time::sleep(wait).await;
                let mine = {
                    let mut st = self.st();
                    if st.retrying.get(&sid) == Some(&token) {
                        st.retrying.remove(&sid);
                        true
                    } else {
                        false
                    }
                };
                if !mine {
                    return; // cancelled while waiting; cancel() finished the turn
                }
                if acted {
                    msgs = vec![Message {
                        text: RESUME_AFTER_TOOLS.into(),
                        via: "retry".into(),
                        files: vec![],
                    }];
                }
                attempt += 1;
                continue;
            }

            self.host().dispatch_finished(
                &sid,
                if stop_reason == "error" {
                    "error"
                } else if stop_reason == "cancelled" {
                    "cancelled"
                } else {
                    "completed"
                },
            );
            self.host().set_status(
                &sid,
                if stop_reason == "error" {
                    "error"
                } else {
                    "idle"
                },
            );
            self.host()
                .add_event(&sid, "turn_end", json!({ "stopReason": stop_reason }));
            let (next, held) = {
                let mut st = self.st();
                let next = if st.holding || self.host().maintenance() {
                    vec![]
                } else {
                    self.host().queue_take(&sid)
                };
                // a cancel that came after the prompt returned was for this turn, not the next
                st.cancel_pending.remove(&sid);
                if next.is_empty() {
                    st.turning.remove(&sid);
                    let token = st.token();
                    if let Some(l) = st.live.get_mut(&sid) {
                        l.idle = Some(token);
                        self.arm_idle(sid.clone(), token);
                    }
                }
                let held = st.holding && self.host().queue_len(&sid) > 0;
                (next, held)
            };
            if !next.is_empty() {
                // a queued follow-up replaces this turn's end callback
                msgs = next;
                attempt = 0;
                continue;
            }
            // held messages will run after the hold: the item's turn is not over yet
            if held {
                return;
            }
            self.expire_waits(&sid, None);
            self.host().turn_end(TurnEnd {
                session_id: sid,
                stop_reason,
                error,
            });
            return;
        }
    }

    /// Closes the permission cards of `sid` still waiting (of adapter `generation` only, when given):
    /// their turn is over or their adapter is gone. The host is told without the lock held.
    fn expire_waits(&self, sid: &str, generation: Option<u64>) {
        let gone: Vec<String> = {
            let mut st = self.st();
            let ids: Vec<String> = st
                .waits
                .iter()
                .filter(|(_, (s, _, g, _))| s == sid && generation.is_none_or(|x| x == *g))
                .map(|(d, _)| d.clone())
                .collect();
            for d in &ids {
                st.waits.remove(d);
            }
            ids
        };
        if !gone.is_empty() {
            self.host().permissions_expired(sid, &gone);
        }
    }

    /// Opens the stats tally of a turn and asks the host for a turn id.
    fn turn_open(&self, sid: &str, attempt: usize, user_events: usize) -> Option<String> {
        self.st().tallies.insert(sid.to_owned(), Tally::default());
        self.host().turn_begin(TurnBegin {
            session_id: sid.to_owned(),
            attempt: attempt as u32,
            user_events,
            started: now_ms(),
        })
    }

    /// Closes the turn's tally and reports it with the response's token figures and the cost increment
    /// of the adapter process `generation` (`None`: the adapter never started).
    #[allow(clippy::too_many_arguments)]
    fn turn_close(
        &self,
        sid: &str,
        turn: Option<String>,
        generation: Option<u64>,
        resp: Option<&Value>,
        stop_reason: &str,
        error: Option<String>,
        transient: bool,
    ) {
        let (tally, cost) = {
            let mut st = self.st();
            let tally = st.tallies.remove(sid).unwrap_or_default();
            let cost = match (generation, st.costs.get_mut(sid)) {
                (Some(g), Some(c)) if c.generation == g => Some(c.take()),
                _ => None,
            };
            (tally, cost)
        };
        let Some(turn_id) = turn else { return };
        let usage = resp
            .map(|r| turn_usage(r, tally.model.as_deref()))
            .unwrap_or_default();
        self.host().turn_finish(TurnFinish {
            turn_id,
            session_id: sid.to_owned(),
            ended: now_ms(),
            stop_reason: stop_reason.to_owned(),
            error,
            transient,
            tools: tally.tools,
            tool_errors: tally.tool_errors,
            tool_kinds: tally.tool_kinds,
            permissions: tally.permissions,
            usage,
            model: tally.model,
            cost,
        });
    }

    /// Stats side of an update: tool counts and the model while a turn runs, the cost total always.
    fn tally_update(&self, sid: &str, generation: u64, kind: &str, u: &Value) {
        let mut autonomous = None;
        {
            let mut st = self.st();
            let busy = st
                .live
                .get(sid)
                .is_some_and(|l| l.busy && l.generation == generation);
            match kind {
                "usage_update" => {
                    let model = u
                        .pointer("/_meta/_claude~1model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .map(str::to_owned);
                    let in_turn = busy && st.tallies.contains_key(sid);
                    if in_turn
                        && let Some(t) = st.tallies.get_mut(sid)
                        && model.is_some()
                    {
                        t.model = model.clone();
                    }
                    // between our turns; while one runs (or waits to retry) the cost goes into it
                    let outside = !st.turning.contains(sid);
                    if let Some(amount) = u.pointer("/cost/amount").and_then(Value::as_f64) {
                        let c = st.costs.entry(sid.to_owned()).or_insert(Cost {
                            generation,
                            now: 0.0,
                            booked: 0.0,
                        });
                        if c.generation != generation {
                            // a new adapter process counts from zero
                            *c = Cost {
                                generation,
                                now: 0.0,
                                booked: 0.0,
                            };
                        }
                        c.now = amount;
                        let origin = u
                            .pointer("/_meta/_claude~1origin")
                            .filter(|v| !v.is_null())
                            .map(|v| js_str(Some(v)));
                        if outside && let Some(origin) = origin {
                            let cost = c.take();
                            autonomous = Some(AutonomousUsage {
                                session_id: sid.to_owned(),
                                ts: now_ms(),
                                model,
                                origin,
                                cost,
                            });
                        }
                    }
                }
                "tool_call" | "tool_call_update" if busy => {
                    let Some(t) = st.tallies.get_mut(sid) else {
                        return;
                    };
                    let failed = u.get("status").and_then(Value::as_str) == Some("failed");
                    if failed {
                        t.tool_errors += 1;
                    }
                    if kind == "tool_call" {
                        t.tools += 1;
                        let name = u
                            .pointer("/_meta/claudeCode/toolName")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let bucket = match CC_TOOL.captures(name) {
                            Some(c) => format!("mcp:{}", &c[1]),
                            None => u
                                .get("kind")
                                .and_then(Value::as_str)
                                .filter(|k| !k.is_empty())
                                .unwrap_or("other")
                                .to_owned(),
                        };
                        *t.tool_kinds.entry(bucket).or_default() += 1;
                        if !name.is_empty() && !CC_TOOL.is_match(name) {
                            *t.tool_kinds.entry(format!("tool:{name}")).or_default() += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(a) = autonomous {
            self.host().autonomous_usage(a);
        }
    }

    fn arm_idle(&self, sid: String, token: u64) {
        let me = Arc::downgrade(&self.0);
        let wait = self.0.cfg.idle_release;
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let Some(inner) = me.upgrade() else { return };
            let acp = Acp(inner);
            let mut st = acp.st();
            if st
                .live
                .get(&sid)
                .is_some_and(|l| l.idle == Some(token) && !l.busy)
                && let Some(l) = st.live.remove(&sid)
            {
                l.conn.kill();
            }
        });
    }

    /// The prompt as ACP content: one text block (attachment paths appended to each message), then the images.
    fn prompt_blocks(&self, s: &SessionInfo, msgs: &[Message], image: bool) -> Value {
        let mut imgs = vec![];
        let text = msgs
            .iter()
            .map(|m| {
                if m.files.is_empty() {
                    return m.text.clone();
                }
                match for_agent(&s.cwd, &m.files, image, |id| self.host().file(id)) {
                    Ok(a) => {
                        imgs.extend(a.images);
                        m.text.clone() + &a.note
                    }
                    Err(e) => format!(
                        "{}\n\n（用户附带了 {} 个附件，但没能放进工作目录：{e}）",
                        m.text,
                        m.files.len()
                    ),
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Value::Array(
            std::iter::once(json!({ "type": "text", "text": text }))
                .chain(imgs)
                .collect(),
        )
    }

    /// The live process for the session, started if needed, marked busy with its idle timer cleared.
    async fn ensure(&self, s: &SessionInfo) -> Result<(Arc<Conn>, u64, String, bool), String> {
        {
            let mut st = self.st();
            if st.closed {
                return Err("hub is shutting down".into());
            }
            if let Some(l) = st.live.get_mut(&s.id) {
                l.idle = None;
                l.busy = true;
                return Ok((l.conn.clone(), l.generation, l.acp_id.clone(), l.image));
            }
        }
        let (conn, generation, acp_id, image) = self.start_process(s).await?;
        let mut st = self.st();
        if conn.has_exited() {
            return Err("adapter exited (null)".into());
        }
        st.live.insert(
            s.id.clone(),
            Live {
                conn: conn.clone(),
                generation,
                acp_id: acp_id.clone(),
                busy: true,
                prompting: false,
                idle: None,
                image,
            },
        );
        Ok((conn, generation, acp_id, image))
    }

    fn adapter_command(
        &self,
        harness: &str,
    ) -> Result<(std::path::PathBuf, Vec<OsString>), String> {
        let cfg = &self.0.cfg;
        let cmd = cfg
            .adapters
            .get(harness)
            .ok_or_else(|| format!("no adapter for harness {harness}"))?;
        let extra: Vec<OsString> = if harness == "codex" {
            codex_args(&cfg.codex_home, &cfg.steve_home, &cfg.user_home)
                .into_iter()
                .map(OsString::from)
                .collect()
        } else {
            vec![]
        };
        Ok(if cmd.extension().is_some_and(|e| e == "js") {
            (
                cfg.node.clone(),
                std::iter::once(cmd.clone().into_os_string())
                    .chain(extra)
                    .collect(),
            )
        } else {
            (cmd.clone(), extra)
        })
    }

    async fn start_process(
        &self,
        s: &SessionInfo,
    ) -> Result<(Arc<Conn>, u64, String, bool), String> {
        let (program, args) = self.adapter_command(&s.harness)?;
        let generation = self.st().token();
        let weak: Weak<Inner> = Arc::downgrade(&self.0);
        let (w1, sid1, sid2) = (weak.clone(), s.id.clone(), s.id.clone());
        let conn = Conn::spawn(
            Spawn {
                program,
                args,
                cwd: s.cwd.clone().into(),
                env: vec![(
                    "CLAUDE_CODE_EXECUTABLE".into(),
                    self.0.cfg.claude_executable.clone().into_os_string(),
                )],
                log_tag: format!("[acp {}]", s.id),
            },
            move |c, inc| {
                if let Some(i) = w1.upgrade() {
                    Acp(i).on_incoming(&sid1, generation, c, inc);
                }
            },
            move || {
                if let Some(i) = weak.upgrade() {
                    {
                        let mut st = i.st.lock().unwrap_or_else(|e| e.into_inner());
                        st.procs.remove(&generation);
                        if st
                            .live
                            .get(&sid2)
                            .is_some_and(|l| l.generation == generation)
                        {
                            st.live.remove(&sid2);
                        }
                    }
                    // the questions that adapter asked can't be answered any more
                    Acp(i).expire_waits(&sid2, Some(generation));
                }
            },
        )
        .map_err(|e| e.message)?;
        {
            let mut st = self.st();
            if conn.has_exited() {
                return Err("adapter exited (null)".into());
            }
            st.procs.insert(generation, conn.clone());
            if st.closed {
                conn.kill();
            }
        }
        // a failed start must not leave the adapter running (the TS leaked it until it exited by itself)
        match self.handshake(&conn, s).await {
            Ok((acp_id, image)) => Ok((conn, generation, acp_id, image)),
            Err(e) => {
                conn.kill();
                Err(e.message)
            }
        }
    }

    /// initialize + session/new|load + mode + model; returns the ACP session id and whether the
    /// adapter takes image blocks.
    async fn handshake(&self, l: &Conn, s: &SessionInfo) -> Result<(String, bool), RpcError> {
        let host = self.host();
        let init = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "steve", "version": self.0.cfg.version },
        });
        let init = l
            .request("initialize", init, Some(Duration::from_secs(60)))
            .await?;
        // image content blocks only go to adapters that say they take them; attachments always also go by path
        let image = init
            .pointer("/agentCapabilities/promptCapabilities/image")
            .is_some_and(truthy);
        let mcp = host.mcp_endpoint(s);
        let mcp_servers = json!([{ "type": "http", "name": "steve", "url": mcp.url,
            "headers": [{ "name": "Authorization", "value": format!("Bearer {}", mcp.token) }] }]);
        let (r, acp_id) = match &s.acp_session_id {
            Some(id) => {
                let r = l
                    .request(
                        "session/load",
                        json!({ "sessionId": id, "cwd": s.cwd, "mcpServers": mcp_servers }),
                        Some(Duration::from_secs(120)),
                    )
                    .await?;
                (r, id.clone())
            }
            None => {
                let r = l
                    .request(
                        "session/new",
                        json!({ "cwd": s.cwd, "mcpServers": mcp_servers }),
                        Some(Duration::from_secs(120)),
                    )
                    .await?;
                let id = js_str(r.get("sessionId"));
                host.set_acp_session_id(&s.id, &id);
                (r, id)
            }
        };
        // Claude Code reports models in `models`; Codex only as the "model" config option
        let model_opt = r
            .get("configOptions")
            .and_then(Value::as_array)
            .and_then(|os| {
                os.iter()
                    .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))
            });
        let models = r.get("models").filter(|v| !v.is_null());
        let reported = coalesce([
            models.and_then(|m| m.get("currentModelId")),
            model_opt.and_then(|o| o.get("currentValue")),
        ])
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty());
        let model = s.model.as_deref().filter(|m| !m.is_empty());
        if let Some(rep) = reported {
            host.set_model(&s.id, rep);
        }
        // the mode does not survive session/load, so it is set every time the process starts
        let mode = if s.harness == "codex" {
            Some(codex_mode(s.perm_mode.as_deref()).to_owned())
        } else {
            s.perm_mode
                .clone()
                .filter(|m| !m.is_empty() && m != "default")
        };
        if let Some(mode) = mode
            && let Err(e) = l
                .request(
                    "session/set_mode",
                    json!({ "sessionId": acp_id, "modeId": mode }),
                    Some(Duration::from_secs(30)),
                )
                .await
        {
            tracing::error!("set_mode {}", e.message);
        }
        // `model` is the snapshot taken when the turn started, like the TS `s.model`
        let mut config = r.clone();
        if let Some(model) = model
            && let Some(updated) = self
                .apply_model(l, &acp_id, model, model_opt.is_some())
                .await
        {
            if let Some(actual) = updated
                .pointer("/models/currentModelId")
                .and_then(Value::as_str)
                .or_else(|| {
                    updated
                        .get("configOptions")
                        .and_then(Value::as_array)
                        .and_then(|os| os.iter().find(|o| o["id"] == "model"))
                        .and_then(|o| o["currentValue"].as_str())
                })
            {
                host.set_model(&s.id, actual);
            }
            if updated.get("configOptions").is_some() {
                config = updated;
            }
        }
        let effort_opt = effort_option(&config);
        if let Some(effort) = s.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
            let option = effort_opt.ok_or_else(|| RpcError {
                message: format!("当前 harness 未报告可设置的思考强度，无法应用 {effort}"),
                data: None,
            })?;
            if !option
                .get("options")
                .is_some_and(|v| option_supports(v, effort))
            {
                return Err(RpcError {
                    message: format!("当前模型不支持思考强度 {effort}"),
                    data: None,
                });
            }
            let updated = l
                .request(
                    "session/set_config_option",
                    json!({"sessionId": acp_id, "configId": option["id"], "value": effort}),
                    Some(Duration::from_secs(30)),
                )
                .await?;
            let actual = effort_option(&updated).and_then(|o| o["currentValue"].as_str());
            if let Some(actual) = actual {
                host.set_reasoning_effort(&s.id, actual);
            }
            if actual != Some(effort) {
                return Err(RpcError {
                    message: format!(
                        "思考强度未生效：请求 {effort}，实际 {}",
                        actual.unwrap_or("未知")
                    ),
                    data: None,
                });
            }
        } else if let Some(actual) = effort_opt.and_then(|o| o["currentValue"].as_str()) {
            host.set_reasoning_effort(&s.id, actual);
        }
        Ok((acp_id, image))
    }

    /// Switch the session's model. claude-agent-acp 0.99.1 and codex-acp only take the "model" config
    /// option (`session/set_model` is -32601 there) and the model does not survive `session/load`, so
    /// this runs on every start. `session/set_model` is the fallback for adapters without the option.
    /// Failures and substitutions are logged as warnings: the session then runs on another model.
    async fn apply_model(
        &self,
        l: &Conn,
        acp_id: &str,
        model: &str,
        has_option: bool,
    ) -> Option<Value> {
        let t = Some(Duration::from_secs(30));
        let by_option = || {
            l.request(
                "session/set_config_option",
                json!({ "sessionId": acp_id, "configId": "model", "value": model }),
                t,
            )
        };
        let by_set_model = || {
            l.request(
                "session/set_model",
                json!({ "sessionId": acp_id, "modelId": model }),
                t,
            )
        };
        let not_found = |e: &RpcError| {
            e.data
                .as_ref()
                .and_then(|d| d.get("code"))
                .and_then(Value::as_i64)
                == Some(-32601)
        };
        let r = if has_option {
            by_option().await
        } else {
            match by_set_model().await {
                Err(e) if not_found(&e) => by_option().await,
                r => r,
            }
        };
        match r {
            Err(e) => {
                tracing::warn!("set model {model} {}", e.message);
                None
            }
            Ok(r) => {
                // the adapter may map an id it does not list to a near one (kiro/claude-haiku-5-5 → claude-haiku-5-5)
                let current = r
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .and_then(|os| {
                        os.iter()
                            .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))
                    })
                    .and_then(|o| o.get("currentValue"))
                    .and_then(Value::as_str);
                if let Some(cur) = current.filter(|c| *c != model) {
                    tracing::warn!("set model {model}: the adapter runs {cur}");
                }
                Some(r)
            }
        }
    }

    fn on_incoming(&self, sid: &str, generation: u64, conn: &Conn, inc: Incoming) {
        match inc {
            Incoming::Notification { method, params } if method == "session/update" => {
                if let Some(u) = params.get("update") {
                    if let Some(kind) = u.get("sessionUpdate").and_then(Value::as_str) {
                        self.tally_update(sid, generation, kind, u);
                    }
                    self.on_update(sid, u);
                }
            }
            Incoming::Notification { .. } => {}
            Incoming::Request { id, method, params } if method == "session/request_permission" => {
                self.on_permission(sid, generation, conn, id, &params);
            }
            Incoming::Request { id, method, .. } => {
                conn.respond_error(id, -32601, &format!("{method} not supported by Steve"));
            }
        }
    }

    /// replayed history arrives as updates during session/load; only updates during a live turn are stored
    fn on_update(&self, sid: &str, u: &Value) {
        let kind = u
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "usage_update" => {
                let int = |k: &str| {
                    u.get(k)
                        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                };
                let cost = u.pointer("/cost/amount").and_then(Value::as_f64);
                self.host().set_usage(
                    sid,
                    Usage {
                        used: int("used"),
                        size: int("size"),
                        cost,
                    },
                );
            }
            "config_option_update" => {
                if let Some(actual) = effort_option(u).and_then(|o| o["currentValue"].as_str()) {
                    self.host().set_reasoning_effort(sid, actual);
                }
                if let Some(actual) = u
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .and_then(|os| os.iter().find(|o| o["id"] == "model"))
                    .and_then(|o| o["currentValue"].as_str())
                {
                    self.host().set_model(sid, actual);
                }
            }
            "available_commands_update" | "current_mode_update" | "" => {}
            _ => {
                let busy = self.st().live.get(sid).is_some_and(|l| l.busy);
                if busy {
                    self.host().add_event(sid, kind, u.clone());
                }
            }
        }
    }

    /// A permission request of adapter process `generation`. It waits as a card only while that process
    /// runs the session's prompt and the prompt is not being cancelled; otherwise it is answered
    /// `cancelled` at once (ACP), also when that changes while the card is being made.
    fn on_permission(
        &self,
        sid: &str,
        generation: u64,
        conn: &Conn,
        rpc_id: Value,
        params: &Value,
    ) {
        let cancelled = json!({ "outcome": { "outcome": "cancelled" } });
        // the prompt it belongs to: the one this process runs now, unless that is being cancelled
        let current = |st: &State| {
            st.live
                .get(sid)
                .filter(|l| l.generation == generation)
                .and_then(|_| st.prompts.get(sid).copied())
                .filter(|t| !st.cancelled.contains(t))
        };
        let prompt = {
            let st = self.st();
            if !st.live.contains_key(sid) {
                return; // asked during start: left unanswered, as in the TS
            }
            match current(&st) {
                Some(t) => t,
                None => {
                    drop(st);
                    conn.respond(rpc_id, cancelled);
                    return;
                }
            }
        };
        // Steve's own tools are always allowed, once per call
        if let Some(opt) = auto_allow(params) {
            conn.respond(rpc_id, selected(&opt));
            return;
        }
        let req = permission_request(&rpc_id, params);
        let title = req.title.clone();
        if let Some(t) = self.st().tallies.get_mut(sid) {
            t.permissions += 1;
        }
        let Some(dec) = self.host().permission_decision(sid, req) else {
            return;
        };
        let waiting = {
            let mut st = self.st();
            // still the prompt it was asked in: one that ended meanwhile took its waits already
            if current(&st) == Some(prompt) {
                st.waits
                    .insert(dec.clone(), (sid.to_owned(), rpc_id.clone(), generation, prompt));
                true
            } else {
                false
            }
        };
        if !waiting {
            conn.respond(rpc_id, cancelled);
            self.host()
                .permissions_expired(sid, std::slice::from_ref(&dec));
            return;
        }
        self.host().add_event(
            sid,
            "permission",
            json!({ "decision": dec, "title": title }),
        );
    }

    /// Answer a pending tool permission (`None` = deny); false when the adapter that asked is gone.
    pub fn answer_permission(&self, decision_id: &str, option_id: Option<&str>) -> bool {
        let mut st = self.st();
        let Some((sid, rpc_id, generation, _)) = st.waits.remove(decision_id) else {
            return false;
        };
        // only the adapter process that asked: a restarted one numbers its requests anew
        let Some(l) = st.live.get(&sid).filter(|l| l.generation == generation) else {
            return false;
        };
        let result = match option_id {
            Some(o) => selected(o),
            None => json!({ "outcome": { "outcome": "cancelled" } }),
        };
        l.conn.respond(rpc_id, result);
        true
    }

    /// Cancel the running turn or a pending gateway retry. While the adapter is still starting (the
    /// prompt not written yet) the cancel waits and goes right after the prompt, so the agent has seen
    /// the message it interrupts. False when no turn is in flight.
    pub fn cancel(&self, session_id: &str) -> bool {
        let mut st = self.st();
        if st.retrying.remove(session_id).is_some() {
            let stale: Vec<String> = st
                .waits
                .iter()
                .filter(|(_, (s, _, _, _))| s == session_id)
                .map(|(d, _)| d.clone())
                .collect();
            for d in &stale {
                st.waits.remove(d);
            }
            self.host().dispatch_finished(session_id, "cancelled");
            // like a cancelled live turn: whatever was queued behind it goes next
            let next = if st.holding || self.host().maintenance() {
                vec![]
            } else {
                self.host().queue_take(session_id)
            };
            if next.is_empty() {
                st.end_turning(session_id);
            }
            drop(st);
            let h = self.host();
            if !stale.is_empty() {
                h.permissions_expired(session_id, &stale);
            }
            h.set_status(session_id, "idle");
            h.add_event(
                session_id,
                "system",
                json!({ "text": "你中断了当前回合，不再自动重试" }),
            );
            h.add_event(session_id, "turn_end", json!({ "stopReason": "cancelled" }));
            if next.is_empty() {
                h.turn_end(TurnEnd {
                    session_id: session_id.to_owned(),
                    stop_reason: "cancelled".into(),
                    error: None,
                });
            } else {
                self.start_turn(session_id.to_owned(), next);
            }
            return true;
        }
        let mut answered = Vec::new();
        match st.live.get(session_id).filter(|l| l.busy && l.prompting) {
            Some(l) => {
                let conn = l.conn.clone();
                conn.notify("session/cancel", json!({ "sessionId": l.acp_id }));
                if let Some(t) = st.prompts.get(session_id).copied() {
                    for (d, rpc_id) in st.cancel_prompt(t) {
                        conn.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } }));
                        answered.push(d);
                    }
                }
            }
            None if st.turning.contains(session_id) => {
                st.cancel_pending.insert(session_id.to_owned());
            }
            None => return false,
        }
        drop(st);
        if !answered.is_empty() {
            self.host().permissions_expired(session_id, &answered);
        }
        self.host()
            .add_event(session_id, "system", json!({ "text": "你中断了当前回合" }));
        true
    }

    /// Switch a live session's permission mode (`session/set_mode`, mapped for Codex like at start).
    /// False when no adapter runs for it: the next start reads the mode from the host anyway.
    pub fn set_mode(&self, session_id: &str, mode: &str) -> bool {
        let Some(s) = self.host().session(session_id) else {
            return false;
        };
        let mode = if s.harness == "codex" {
            codex_mode(Some(mode)).to_owned()
        } else {
            mode.to_owned()
        };
        let (conn, acp_id) = {
            let st = self.st();
            match st.live.get(session_id) {
                Some(l) => (l.conn.clone(), l.acp_id.clone()),
                None => return false,
            }
        };
        let Ok(h) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        h.spawn(async move {
            if let Err(e) = conn
                .request(
                    "session/set_mode",
                    json!({ "sessionId": acp_id, "modeId": mode }),
                    Some(Duration::from_secs(30)),
                )
                .await
            {
                tracing::error!("set_mode {}", e.message);
            }
        });
        true
    }

    /// Free the adapter process when idle; the next prompt resumes with `session/load`.
    pub fn release(&self, session_id: &str) {
        let mut st = self.st();
        if st.live.get(session_id).is_some_and(|l| !l.busy)
            && let Some(l) = st.live.remove(session_id)
        {
            l.conn.kill();
        }
    }

    /// A turn is in flight: adapter start, prompt, or gateway-retry wait.
    pub fn is_busy(&self, session_id: &str) -> bool {
        self.st().turning.contains(session_id)
    }

    /// pid of the session's live adapter (also its process group id), for diagnostics.
    pub fn adapter_pid(&self, session_id: &str) -> Option<u32> {
        self.st().live.get(session_id).map(|l| l.conn.pid())
    }

    /// Reversible upgrade drain. Only idle adapters may be stopped; verify their independent
    /// process groups disappeared, not just their leaders. A failed verification is not readiness.
    pub async fn quiesce(&self) -> bool {
        {
            let mut st = self.st();
            st.holding = true;
            if !st.turning.is_empty() {
                return false;
            }
            let pids: Vec<_> = st.procs.values().map(|c| c.pid()).collect();
            st.drain_groups.extend(pids);
        }
        self.shutdown(Duration::from_millis(500)).await;
        for _ in 0..20 {
            {
                let mut st = self.st();
                st.drain_groups.retain(|pid| {
                    // SAFETY: probe a process group created by this runner. EPERM is still alive.
                    let alive = unsafe { libc::kill(-(*pid as libc::pid_t), 0) } == 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
                    if alive {
                        unsafe {
                            libc::kill(-(*pid as libc::pid_t), libc::SIGKILL);
                        }
                    }
                    alive
                });
                if st.drain_groups.is_empty() {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
    pub fn reopen(&self) -> bool {
        let mut st = self.st();
        if !st.drain_groups.is_empty() {
            return false;
        }
        st.closed = false;
        st.holding = false;
        true
    }

    /// SIGTERM every adapter's process group (live or still starting), wait up to `grace`, then
    /// SIGKILL the groups. No new adapters start afterwards.
    pub async fn shutdown(&self, grace: Duration) {
        let procs: Vec<Arc<Conn>> = {
            let mut st = self.st();
            st.closed = true;
            st.live.clear();
            st.procs.values().cloned().collect()
        };
        for c in &procs {
            c.kill();
        }
        let all = async {
            for c in &procs {
                c.wait_exit().await;
            }
        };
        let _ = tokio::time::timeout(grace, all).await;
        // grandchildren that ignored SIGTERM, or a leader that is still alive
        for c in &procs {
            c.signal(libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(tool: &str) -> Value {
        json!({ "toolCall": { "toolCallId": "t1", "title": "mcp__steve__x", "_meta": { "claudeCode": { "toolName": tool } } },
            "options": [{ "optionId": "a", "name": "Always", "kind": "allow_always" }, { "optionId": "o", "name": "Once", "kind": "allow_once" }] })
    }

    #[test]
    fn claude_code_own_tools_are_allowed_once() {
        assert_eq!(
            auto_allow(&cc("mcp__steve__steve_deliver")).as_deref(),
            Some("o")
        );
        assert_eq!(auto_allow(&cc("mcp__steve__steve_other")), None);
        for t in [
            "steve_secret_use",
            "steve_github_comment",
            "steve_github_open_pr",
        ] {
            assert_eq!(auto_allow(&cc(&format!("mcp__steve__{t}"))), None, "{t}");
        }
        for t in [
            "steve_knowledge_search",
            "steve_knowledge_read",
            "steve_remember",
        ] {
            assert_eq!(
                auto_allow(&cc(&format!("mcp__steve__{t}"))).as_deref(),
                Some("o")
            );
        }
        assert_eq!(auto_allow(&cc("mcp__other__steve_deliver")), None);
        assert_eq!(auto_allow(&cc("Bash")), None);
        // name is only consulted when toolName is absent
        let p = json!({ "toolCall": { "name": "mcp__steve__steve_ask" }, "options": [{ "optionId": "o", "kind": "allow_once" }] });
        assert_eq!(auto_allow(&p).as_deref(), Some("o"));
        let p = json!({ "toolCall": { "name": "mcp__steve__steve_ask", "_meta": { "claudeCode": { "toolName": "Bash" } } }, "options": [{ "optionId": "o", "kind": "allow_once" }] });
        assert_eq!(auto_allow(&p), None);
        // no allow_once → a decision like any other
        let p = json!({ "toolCall": { "name": "mcp__steve__steve_ask" }, "options": [{ "optionId": "a", "kind": "allow_always" }] });
        assert_eq!(auto_allow(&p), None);
    }

    #[test]
    fn titles_never_count() {
        let p = json!({ "toolCall": { "title": "mcp__steve__steve_deliver", "kind": "execute", "rawInput": { "command": "echo mcp__steve__steve_deliver" } },
            "options": [{ "optionId": "o", "kind": "allow_once" }] });
        assert_eq!(auto_allow(&p), None);
        let p = json!({ "toolCall": { "title": "Allow the steve MCP server to run tool \"steve_ask\"?" }, "options": [{ "optionId": "o", "kind": "allow_once" }] });
        assert_eq!(auto_allow(&p), None);
    }

    #[test]
    fn codex_approval_form() {
        let form = |server: &str, kind: &str, msg: &str| {
            json!({ "toolCall": { "toolCallId": "c1", "title": "Approve", "rawInput": { "server_name": server,
                "request": { "message": msg, "_meta": { "codex_approval_kind": kind } } } },
                "options": [{ "optionId": "approved", "name": "Allow", "kind": "allow_once" }] })
        };
        let ok = "Allow the steve MCP server to run tool \"steve_update\"?";
        assert_eq!(
            auto_allow(&form("steve", "mcp_tool_call", ok)).as_deref(),
            Some("approved")
        );
        assert_eq!(auto_allow(&form("other", "mcp_tool_call", ok)), None);
        assert_eq!(auto_allow(&form("steve", "exec", ok)), None);
        assert_eq!(
            auto_allow(&form(
                "steve",
                "mcp_tool_call",
                "Allow the steve MCP server to run tool \"rm\"?"
            )),
            None
        );
        assert_eq!(
            auto_allow(&form("steve", "mcp_tool_call", &format!("{ok} "))),
            None
        );
    }

    #[test]
    fn dead_agent_errors() {
        for m in [
            "Internal error: The Claude Agent process exited unexpectedly. Please start a new session.",
            "Resource not found",
        ] {
            assert!(DEAD_AGENT.is_match(m), "{m}");
        }
        assert!(!DEAD_AGENT.is_match("503 no eligible upstream account"));
    }

    #[test]
    fn decision_fields() {
        let p = json!({ "toolCall": { "kind": "edit", "rawInput": { "file_path": "/x/a.txt" } },
            "options": [{ "optionId": "o", "name": "允许", "kind": "allow_once" }, { "optionId": "r", "kind": "reject_once" }] });
        let r = permission_request(&json!(7), &p);
        assert_eq!(r.subject_id, "7");
        assert_eq!(r.title, "edit");
        assert_eq!(r.question, "允许执行：edit");
        assert_eq!(r.draft, "{\n \"file_path\": \"/x/a.txt\"\n}");
        assert_eq!(
            r.options,
            vec!["o|允许|allow_once", "r|undefined|reject_once"]
        );
        assert_eq!(r.raw, json!({ "file_path": "/x/a.txt" }));
        assert_eq!(r.kind.as_deref(), Some("edit"));
        assert!(r.locations.is_empty());
        let long = "x".repeat(3000);
        let p = json!({ "toolCall": { "kind": "execute", "rawInput": { "command": long },
            "locations": [{ "path": "/w/a" }, { "line": 3 }, { "path": "/w/b" }] } });
        let r = permission_request(&json!(1), &p);
        assert_eq!(r.draft.chars().count(), 2000);
        assert_eq!(r.raw["command"].as_str().map(str::len), Some(3000));
        assert_eq!(r.locations, vec!["/w/a", "/w/b"]);
        let r = permission_request(&json!("x"), &json!({}));
        assert_eq!(
            (r.title.as_str(), r.subject_id.as_str(), r.draft.as_str()),
            ("工具调用", "x", "{}")
        );
        assert!(r.options.is_empty());
    }

    #[test]
    fn transient_errors() {
        for m in [
            "API Error: 503 Service Unavailable",
            "Overloaded",
            "rate limit reached",
            "ratelimit",
            "429",
            "please try again",
            "temporarily down",
            "错误503",
        ] {
            assert!(TRANSIENT.is_match(m), "{m}");
        }
        for m in [
            "5030",
            "invalid api key",
            "adapter exited (1)",
            "timeout session/prompt",
        ] {
            assert!(!TRANSIENT.is_match(m), "{m}");
        }
    }
}
