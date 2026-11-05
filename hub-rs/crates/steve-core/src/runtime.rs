//! What the domain layer needs from the agent runner (steve-acp) and gives to it and to the MCP server.
//!
//! Every method is called while the core lock is held, so implementations must return promptly and must
//! never call back into [`crate::Core`] synchronously: turns run on their own tasks and report through
//! [`crate::Core::on_turn_end`] later, exactly like the async turn loop in acp.ts.

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

/// What [`AgentRuntime::answer_permission`] did with an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The adapter that asked has it.
    Applied,
    /// It is on its way to the Node that asked (a queued answer command); the Node's receipt later
    /// marks it applied, or closes the card unapplied.
    Pending,
    /// Nobody waits for it any more (the adapter is gone, the request answered or expired).
    Gone,
}

/// What [`AgentRuntime::set_mode`] did with a permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Configured {
    /// The live session has it (or had it already).
    Applied,
    /// It is on its way, or waits for the session's next idle boundary.
    Pending,
    /// No adapter runs for the session: its next start applies the mode anyway.
    NotRunning,
    /// The backend cannot express the mode for this session (a Node path without it): unchanged,
    /// and asking again would not help.
    Unsupported,
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

/// A finished turn, as acp.ts hands it to the orchestrator's turn-end handler.
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

/// The ACP runner (acp.ts): per-session adapter processes, the prompt queue, permissions, and the release hold.
pub trait AgentRuntime: Send + Sync {
    /// A session was just created (before its first prompt): the runtime may fix its backend now.
    /// Only new sessions are ever bound here; existing ones keep the backend they have. `after`: it
    /// takes over from a session whose release is still on its way ([`Released::Pending`]); its
    /// harness does not start before that release is done (two harnesses never share a worktree).
    fn created(&self, session_id: &str, after: Option<&str>) {
        let _ = (session_id, after);
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
    /// Answer a pending tool permission (`None` = cancelled). Errors when the backend cannot tell
    /// (its store failed): the answer was not taken and the card stays open.
    fn answer_permission(&self, decision_id: &str, option_id: Option<&str>) -> Result<Answered>;
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
    /// Switch a live session's permission mode (`session/set_mode`). Errors when the change could
    /// not be taken this time (a store error, say): nothing changed, and asking again may work.
    fn set_mode(&self, session_id: &str, mode: &str) -> Result<Configured> {
        let _ = (session_id, mode);
        Ok(Configured::NotRunning)
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
    /// resume it, so the drain never seals while there is any. Legacy turns are not counted here.
    fn node_working(&self) -> usize {
        0
    }
    /// The pid of the session's live agent adapter (its process group leader), if one runs.
    fn agent_pid(&self, session_id: &str) -> Option<u32> {
        let _ = session_id;
        None
    }
    /// New sessions of items on a git worktree may be bound to a backend that makes the worktree
    /// itself (the local Node prepares it off the core's lock, before the session opens): the
    /// core then only names where it goes ([`crate::Core::workspace_ready`] says when it is there).
    fn makes_worktrees(&self) -> bool {
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
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Check {
    pub name: String,
    pub passed: bool,
}
