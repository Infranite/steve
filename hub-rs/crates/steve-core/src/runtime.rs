//! What the domain layer needs from the agent runtime (the Hub routes it to the Nodes) and gives to it and to the MCP server.
//!
//! Every method is called while the core lock is held, so implementations must return promptly and must
//! never call back into [`crate::Core`] synchronously: turns run on their own tasks and report through
//! [`crate::Core::on_turn_end`] later.

use crate::error::Result;
use crate::repo::FileRef;

/// The result of [`AgentRuntime::prompt`]: started a turn now, or queued behind the running one.
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

/// The turn a session runs now, frozen when it was looked at: a stop aimed at it reaches that turn
/// and never the next one (a gateway retry of it is the same turn, on every backend). `attempt` is
/// opaque and only means something to the backend that minted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub session: String,
    pub attempt: String,
}

/// What [`AgentRuntime::stop`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The turn was still running: it is being interrupted.
    Requested,
    /// That turn is over (or another one runs now): nothing was done.
    NotRunning,
}

/// What [`AgentRuntime::flush`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flushed {
    /// A turn started: this one.
    Started(Running),
    /// The session is in a turn already, or not ready for one (opening, say): it waits.
    Busy,
    /// New turns are held (release, maintenance), or the backend could not take it now.
    Held,
    /// Nothing to send.
    Empty,
}

impl Flushed {
    pub fn started(&self) -> bool {
        matches!(self, Flushed::Started(_))
    }
}

/// A permission answer is tied to the version of the card the caller acted on.
#[derive(Clone, Copy)]
pub struct PermissionAnswer<'a> {
    pub decision_id: &'a str,
    pub revision: i64,
    pub actor: &'a str,
    pub action: PermissionAction<'a>,
}

#[derive(Clone, Copy)]
pub enum PermissionAction<'a> {
    Resolve { choice: &'a str, text: &'a str },
    Dismiss,
}

impl<'a> PermissionAnswer<'a> {
    pub fn resolve(
        decision_id: &'a str,
        revision: i64,
        actor: &'a str,
        choice: &'a str,
        text: &'a str,
    ) -> Self {
        Self {
            decision_id,
            revision,
            actor,
            action: PermissionAction::Resolve { choice, text },
        }
    }

    pub fn option(self) -> Option<&'a str> {
        match self.action {
            PermissionAction::Resolve {
                choice: "reject" | "deny",
                ..
            }
            | PermissionAction::Dismiss => None,
            PermissionAction::Resolve { choice, .. } => Some(choice),
        }
    }
}

/// What [`AgentRuntime::answer_permission`] did with an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The adapter that asked has it.
    Applied,
    /// The Hub committed the card and its answer command together.
    Committed,
    /// It is on its way to the Node that asked (a queued answer command); the Node's receipt later
    /// marks it applied, or closes the card unapplied.
    Pending,
    /// Nobody waits for it any more (the adapter is gone, the request answered or expired).
    Gone,
}

/// Where a session's permission mode stands on its backend ([`AgentRuntime::mode_state`]): what
/// its harness was confirmed to run in, what waits or is on its way, and what was refused. Mode
/// names are Steve's (`plan`, `ask`, `accept_edits`, `auto`, `full_access`).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ModeState {
    /// The mode the backend confirmed the harness runs in (its last open or configure that
    /// finished with success). `None`: none confirmed yet.
    pub applied: Option<String>,
    /// A mode waiting for the session's next idle boundary or open, or on its way to the harness.
    pub desired: Option<String>,
    /// `desired` is on its way now (sent, not confirmed); else it waits.
    pub sending: bool,
    /// A turn of the session is not over: a waiting mode goes when it ends.
    pub busy: bool,
    /// No harness runs for the session: its next open carries the mode.
    pub released: bool,
    /// The last change did not take: what the backend or the harness said (its capability, a
    /// Node owner's ceiling, a mode the adapter does not offer).
    pub refused: Option<String>,
    /// The outcome of the last change is unknown: the session is held until it is known.
    pub unknown: bool,
}

/// What [`AgentRuntime::release`] did with a session's harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Released {
    /// Nothing runs for the session any more (it was idle and is stopped, or nothing was open).
    Gone,
    /// It is being let go: the operation named here does it (on a Node, a queued `release_session`;
    /// for a session the business side ended during a turn, the one queued for when that turn is
    /// over). Work that must not overlap the harness waits for it.
    Pending(String),
    /// A turn runs: nothing was done (a session that is not ended keeps its harness).
    Busy,
}

/// Hub-normalized provider/transport interruption. Its runtime has already applied its own
/// bounded backoff; it must never spend the no-delivery continue budget at turn end.
pub const MODEL_STREAM_INTERRUPTED: &str = "model_stream_interrupted";

/// A provider capacity outage, after the runtime's bounded, recovery-aware retry policy.
pub const MODEL_SERVICE_UNAVAILABLE: &str = "model_service_unavailable";

pub fn is_model_service_failure(stop: &str) -> bool {
    matches!(stop, MODEL_STREAM_INTERRUPTED | MODEL_SERVICE_UNAVAILABLE)
}

/// A finished turn, as the runtime hands it to the orchestrator's turn-end handler.
#[derive(Debug, Clone)]
pub struct TurnEnd {
    pub session_id: String,
    /// The turn that ended, as [`AgentRuntime::running`] named it while it ran (after a gateway
    /// retry or queued follow-ups, the last of them). `None` when the backend cannot tell.
    pub attempt: Option<String>,
    /// `end_turn`, `cancelled`, `error`, …
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Where sessions run (the Hub routes them to their Nodes): per-session harnesses, the prompt
/// queue, permissions, and the release hold.
pub trait AgentRuntime: Send + Sync {
    /// Assess one authorized binding against the current Node and its capability report.
    fn assess_placement_node(
        &self,
        _workspace: &str,
        _project: Option<&steve_store::Project>,
        _node: &str,
        _choice: &steve_node_api::HarnessChoice,
        _mode: &str,
    ) -> Result<Option<steve_types::PlacementRejection>> {
        Ok(None)
    }

    /// Propose a new Bot on a verified route when automatic placement found no eligible executor.
    fn propose_bot(
        &self,
        _req: &crate::CreateItem,
        _blocked: &steve_types::PlacementBlocked,
    ) -> Result<Option<steve_types::BotProposal>> {
        Ok(None)
    }

    /// Confirm a previously proposed Bot as the workspace owner and return its created Bot ID.
    fn confirm_bot_proposal(
        &self,
        _ws: &str,
        _actor: &str,
        _id: &str,
        _edit: steve_types::ConfirmBotProposal,
    ) -> Result<String> {
        Err(crate::error::msg("当前运行环境不支持确认新建 Bot 提案。"))
    }

    /// Whether `bot` (of `harness`) could run `item_id`'s work at all, before a session is made
    /// for it or the item is given to the Bot: its harness installed, ready and signed in on the
    /// Nodes. `Err` says why and where to fix it (stored text, zh-CN); nothing is made then, and
    /// nothing tries again by itself. Deterministic: no model, no Node is asked. The default lets
    /// everything run.
    fn can_run(&self, item_id: &str, bot_id: &str, harness: &str) -> Result<()> {
        let _ = (item_id, bot_id, harness);
        Ok(())
    }
    /// Like [`Self::can_run`], checking an explicit [`steve_node_api::HarnessChoice`] (e.g. a
    /// handoff overriding harness or model) against the Bot's authorized Nodes before ending an
    /// existing session. Defaults to [`Self::can_run`].
    fn can_run_choice(
        &self,
        item_id: &str,
        bot_id: &str,
        choice: &steve_node_api::HarnessChoice,
    ) -> Result<()> {
        self.can_run(item_id, bot_id, &choice.harness)
    }
    /// A session was just created (before its first prompt): the runtime may fix its backend now.
    /// Only new sessions are ever bound here; existing ones keep the backend they have. `after`: it
    /// takes over from a session whose release is still on its way ([`Released::Pending`]); its
    /// harness does not start before that release is done (two harnesses never share a worktree).
    fn created(&self, session_id: &str, after: Option<&str>) {
        let _ = (session_id, after);
    }
    /// Like [`Self::created`], carrying the W3.1 [`steve_types::PlacementExplanation`] recorded by
    /// placement selection when a session switches Bot, harness, or model.
    fn created_with_placement(
        &self,
        session_id: &str,
        after: Option<&str>,
        explanation: Option<&steve_types::PlacementExplanation>,
    ) {
        let _ = explanation;
        self.created(session_id, after);
    }
    /// Why nothing can run `session_id` (stored text, zh-CN), asked once [`Self::created`] had its
    /// say: no backend takes it, and there is no in-process runner to fall back on (B7). The core
    /// then ends it at once, said in it and on its item, instead of leaving it waiting for good.
    /// `None`: a backend runs it.
    fn refusal(&self, session_id: &str) -> Option<String> {
        let _ = session_id;
        None
    }
    /// Send now or queue behind the running turn (or behind the release hold). Writes the `queued`
    /// row/event itself (with the files). Attachments are copied into the session cwd and named in the
    /// prompt (see [`crate::attachments::for_agent`]). Errors only when the session does not exist (`no session`).
    fn prompt(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<Prompted>;
    /// Queue an owner response in the decision's write transaction. The runtime plans delivery
    /// from the committed queue.
    fn stage_decision_prompt(
        &self,
        _w: &mut steve_store::W<'_>,
        session_id: &str,
        text: &str,
    ) -> Result<()> {
        self.prompt(session_id, text, "steve", &[])?;
        Ok(())
    }
    /// The turn running now (or its pending gateway retry), if any.
    fn running(&self, session_id: &str) -> Option<Running>;
    /// Interrupt that turn, only while it is still the one running; [`Stop::NotRunning`] otherwise.
    /// Errors when the backend cannot tell (its store failed, its Node is stopped).
    fn stop(&self, turn: &Running) -> Result<Stop>;
    /// Interrupt whatever runs now ([`Self::running`] then [`Self::stop`]).
    fn cancel(&self, session_id: &str) -> Result<Stop> {
        match self.running(session_id) {
            Some(r) => self.stop(&r),
            None => Ok(Stop::NotRunning),
        }
    }
    /// The user's Stop: [`Self::cancel`], and a backend may also retire what holds the session
    /// (a Node's control whose outcome is unknown). The lifecycle's own cancels never do.
    fn user_stop(&self, session_id: &str) -> Result<Stop> {
        self.cancel(session_id)
    }
    /// A turn is in flight, including adapter start and the gateway-retry wait.
    fn is_busy(&self, session_id: &str) -> bool;
    /// Stop the adapter process if it is idle; the next prompt starts it again with `session/load`.
    /// A session the business side ended while a turn runs is let go once that turn is over.
    /// Errors when the backend cannot tell (its store failed): nothing is known to be let go.
    fn release(&self, session_id: &str) -> Result<Released>;
    /// Human retry of a blocked session open. Background continues must never call this.
    fn retry_session(
        &self,
        session_id: &str,
        choice: &steve_node_api::HarnessChoice,
    ) -> Result<()> {
        let _ = (session_id, choice);
        Err(crate::error::msg("当前执行后端不支持重试启动。"))
    }
    /// Answer a pending tool permission (`None` = cancelled). Errors when the backend cannot tell
    /// (its store failed): the answer was not taken and the card stays open.
    fn answer_permission(&self, answer: PermissionAnswer<'_>) -> Result<Answered>;
    /// Release hand-off: while holding, no new turn starts and prompts queue (they survive a restart).
    fn hold(&self, on: bool);
    fn is_holding(&self) -> bool;
    /// Turns in flight (including ones waiting for a permission answer or a retry).
    fn running_turns(&self) -> usize;
    /// Run what is queued for a session as one turn, optionally led by Steve's own message;
    /// identical texts are sent once. The turn it started, or why none did.
    fn flush(&self, session_id: &str, lead: Option<(&str, &str)>) -> Flushed;
    /// `a` and `b` name the same turn (a gateway retry is a new attempt of the same turn).
    fn same_turn(&self, a: &Running, b: &Running) -> bool {
        a == b
    }
    /// The session runs on a Node that has no link now: why, as far as is known (shown on what
    /// waits for it). `None`: it is linked, or runs elsewhere.
    fn node_offline(&self, session_id: &str) -> Option<String> {
        let _ = session_id;
        None
    }
    /// Where the session's permission mode stands on its backend (`None`: the backend cannot
    /// tell, or nothing runs it).
    fn mode_state(&self, session_id: &str) -> Option<ModeState> {
        let _ = session_id;
        None
    }
    /// The session's last attempt has an outcome nobody can tell yet (reconciliation decides).
    fn uncertain(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// The session's backend recovers it after a restart by itself (a Node's reconcile settles its
    /// attempts and expires its own cards; a parked one waits for its Node): boot recovery leaves it
    /// alone, with no notice, no failure card and no resume.
    fn recovers_itself(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// Work on a Node that an upgrade drain waits for: open turns (also one waiting for a
    /// permission answer until it is answered or expires, or one with an outcome nobody can tell
    /// yet) and harnesses opening, configuring or releasing. A restart would cut it with nothing to
    /// resume it, so the drain never seals while there is any.
    fn node_working(&self) -> usize {
        0
    }
    /// New sessions of items on a git worktree may be bound to a backend that makes the worktree
    /// itself (the local Node prepares it off the core's lock, before the session opens): the
    /// core then only names where it goes ([`crate::Core::workspace_ready`] says when it is there).
    fn makes_worktrees(&self) -> bool {
        false
    }
    /// [`Self::created`] would bind `session_id` where its worktree is made (asked before it, so
    /// that the core makes the worktree first for a session bound otherwise, e.g. to a directory
    /// the backend only opens).
    fn would_make_workspace(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// `session_id`'s backend makes the worktree it works in (once [`Self::created`] bound it),
    /// and makes it again when the session opens after a tidy.
    fn makes_workspace(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// Gives back the worktree `session_id` works in through its backend; `keep_branch`: its
    /// branch stays. `Ok(true)`: on its way ([`crate::Core::workspace_tidied`] tells what came of
    /// it). `Ok(false)`: the backend has no worktree of it now; nothing was done.
    fn tidy(&self, session_id: &str, keep_branch: bool) -> Result<bool> {
        let _ = (session_id, keep_branch);
        Ok(false)
    }
}

/// The MCP endpoint handed to an adapter when it starts (orchestrator `mcpEndpoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
}

/// One step of the plan reported by `steve_update`.
pub use crate::repo::PlanStep;

/// An output reported by `steve_deliver`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Output {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// `record`: documents the work (a self-check screenshot, a log, an intermediate file) rather than
    /// being its result; shown with the item's work record instead of its outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// A check reported by `steve_deliver`.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    /// Something the agent actually did on the running thing, not a build or a test ([`crate::proof`]).
    #[serde(default)]
    pub operated: bool,
    /// `unit` (default), `baseline` (pre-existing unrelated failure), or `external` (upstream service outage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Check {
    pub fn effective_scope(&self) -> &str {
        self.scope.as_deref().unwrap_or("unit")
    }

    pub fn valid_scope(&self) -> bool {
        matches!(self.effective_scope(), "unit" | "baseline" | "external")
    }

    pub fn is_unit_scope(&self) -> bool {
        self.effective_scope() == "unit"
    }
}
