//! The GitHub writes you approved (Y2): opening a PR, merging it, an agent's comment. The answer's
//! transaction inserts a `gh_effects` row ([`queue`]); the runner takes due rows after the commit,
//! so nothing is lost to a restart and nothing runs for an answer that was rolled back.
//!
//! ```text
//! pending → running → done            (GitHub confirmed it)
//!                   → failed          (GitHub refused, or it could not start: nothing happened)
//!                   → unknown         (sent, no answer: timed out, cut off, 5xx)
//! restart: running → unknown
//! unknown → reconcile with GitHub → done (it happened) | run again (it did not)
//! ```
//!
//! Every run after the first reconciles before it writes, without comparing clocks: an open PR from
//! the item's branch is the one (GitHub allows one per head and base), a merged PR is merged, and an
//! identical comment no other effect claimed is the comment. Attempts back off
//! ([`outbox::backoff`], first after a minute: longer than [`crate::api::REQUEST_TIMEOUT`], so a
//! request still on its way has landed); after [`outbox::MAX_ATTEMPTS`] it stays unknown and the
//! item's timeline says so — a person decides.

use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_runtime::{
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use steve_store::outbox;

use crate::api::GhError;
use crate::{GitHub, Inner};
use steve_store::github::EffectRow;

pub const OPEN: &str = "open";
pub const MERGE: &str = "merge";
pub const COMMENT: &str = "comment";

/// Fallback ceiling for the due loop (re-exported for callers inspecting the loop bound).
pub const EVERY: Duration = steve_runtime::due::FALLBACK;
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Effects taken per pass.
const BATCH: i64 = 20;

/// Records the write for `decision`, in the answer's transaction.
pub(crate) fn queue(
    tx: &mut steve_store::W<'_>,
    gh: &GitHub,
    ws: &str,
    decision: &str,
    kind: &str,
) -> Result<(), ApiError> {
    steve_store::github::insert_effect(tx, ws, decision, kind, gh.now())?;
    gh.0.effect_wake.notify_one();
    Ok(())
}

/// How a write that did not (certainly) happen stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// GitHub refused, or it could not start: nothing happened.
    Refused,
    /// Sent, no (readable) answer, or a local step after GitHub did it failed: it may have.
    Unknown,
    /// Nothing sent: the connection waits (paused, to be authorized again).
    Wait,
    /// It may have happened and GitHub gives no evidence either way.
    Stuck,
}

/// Why a write did not (certainly) happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fail {
    pub(crate) message: String,
    pub(crate) why: Why,
}

impl Fail {
    pub(crate) fn definite(message: impl Into<String>) -> Self {
        Self::new(message, Why::Refused)
    }

    pub(crate) fn unknown(message: impl Into<String>) -> Self {
        Self::new(message, Why::Unknown)
    }

    pub(crate) fn stuck(message: impl Into<String>) -> Self {
        Self::new(message, Why::Stuck)
    }

    fn new(message: impl Into<String>, why: Why) -> Self {
        Self {
            message: message.into(),
            why,
        }
    }

    /// It may have happened.
    pub(crate) fn maybe(&self) -> bool {
        matches!(self.why, Why::Unknown | Why::Stuck)
    }

    /// The run's outcome for anything but a refusal (the caller tells the item or session about
    /// those itself); `None` for a refusal.
    pub(crate) fn ran(&self) -> Option<Ran> {
        match self.why {
            Why::Refused => None,
            Why::Unknown => Some(Ran::Unknown(self.message.clone())),
            Why::Wait => Some(Ran::Wait(self.message.clone())),
            Why::Stuck => Some(Ran::Stuck(self.message.clone())),
        }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<GhError> for Fail {
    /// Unknown: sent without a (readable) answer, or GitHub's own error. Refused otherwise (it
    /// refused, or nothing was sent).
    fn from(e: GhError) -> Self {
        let why = if matches!(
            e,
            GhError::Network | GhError::Decode | GhError::Status { status: 500.., .. }
        ) {
            Why::Unknown
        } else {
            Why::Refused
        };
        Self::new(e.to_string(), why)
    }
}

impl From<String> for Fail {
    fn from(m: String) -> Self {
        Self::definite(m)
    }
}

impl GitHub {
    /// The client for an effect's write: a connection that waits is [`Why::Wait`] (tried again
    /// later), one that may not write is a refusal.
    pub(crate) fn effect_conn(
        &self,
        ws: &str,
        connection: &str,
    ) -> Result<crate::api::GhConn, Fail> {
        let waits = self
            .store()
            .read(|c| {
                steve_modules::accounts::connections::may_write_in(
                    c,
                    ws,
                    connection,
                    Some(crate::adapters::WRITE_CAP),
                )
            })
            .map_err(|e| Fail::unknown(e.to_string()))?;
        if let Err(steve_modules::accounts::connections::WriteRefusal::Later(m)) = waits {
            return Err(Fail::new(m, Why::Wait));
        }
        self.conn_for_write(ws, connection)
            .map_err(|e| Fail::definite(e.to_string()))
    }
}

/// What one run of an effect did.
pub(crate) enum Ran {
    /// Happened (or is known to have happened); `result` goes on the row.
    Done(Value),
    /// Did not happen; the run already told the item or session why.
    Failed(String),
    /// May have happened: reconciled on the next run.
    Unknown(String),
    /// Nothing sent: the connection is paused or to be authorized again. Tried again later.
    Wait(String),
    /// May have happened, and GitHub gives no evidence either way (a comment not found: deleted,
    /// or never posted): not tried again by itself, you are told.
    Stuck(String),
}

impl GitHub {
    /// Runs the due effects once; returns how many ran.
    pub async fn run_effects(&self) -> usize {
        let now = self.now();
        let due = match self
            .store()
            .read(|c| steve_store::github::due_effects(c, now, BATCH).map_err(ApiError::from))
        {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "github effects not read");
                return 0;
            }
        };
        let mut n = 0;
        for e in due {
            let claimed = self.store().write(|tx| {
                steve_store::github::claim_effect(tx, &e.id, e.attempts, now)
                    .map_err(ApiError::from)
            });
            if !matches!(claimed, Ok(true)) {
                continue;
            }
            self.run_effect_row(&e).await;
            n += 1;
        }
        n
    }

    async fn run_effect_row(&self, e: &EffectRow) {
        // a run after one that sent its write (unknown, a restart) reconciles first; one after a
        // wait (nothing sent) does not
        let reconcile = tried(&e.result);
        let d = self
            .store()
            .read(|c| steve_store::decision_repo::get(c, &e.decision_id).map_err(ApiError::from));
        let ran = match d {
            Ok(Some(d)) if d.status == steve_types::DecisionStatus::Resolved => {
                match e.kind.as_str() {
                    OPEN => self.open_pr(&d, e, reconcile).await,
                    MERGE => self.merge_pr(&d, reconcile).await,
                    COMMENT => self.comment_answered(&d, reconcile).await,
                    k => Ran::Failed(format!("不认识的 GitHub 动作 {k}")),
                }
            }
            Ok(_) => Ran::Failed("这个决定不再是批准状态".into()),
            Err(err) => Ran::Unknown(err.to_string()),
        };
        let now = self.now();
        let attempts = e.attempts + 1;
        // what an earlier step of this run wrote down (the commit it is about)
        let kept = self
            .store()
            .read(|c| steve_store::github::effect_of(c, &e.decision_id).map_err(ApiError::from))
            .ok()
            .flatten()
            .map(|r| r.result)
            .unwrap_or_else(|| e.result.clone());
        let (state, next, error, result) = match ran {
            Ran::Done(r) => ("done", None, None, merge_json(kept, r)),
            Ran::Failed(m) => ("failed", None, Some(m), kept),
            Ran::Unknown(m) => {
                let last = attempts >= outbox::MAX_ATTEMPTS;
                if attempts == 1 || last {
                    self.effect_note(e, &m, last).await;
                }
                let next = (!last).then(|| now + outbox::backoff(attempts));
                ("unknown", next, Some(m), kept)
            }
            Ran::Stuck(m) => {
                self.effect_note(e, &m, true).await;
                ("unknown", None, Some(m), kept)
            }
            // as it was before this run (nothing sent), later
            Ran::Wait(m) => {
                let back = if e.state == "unknown" {
                    "unknown"
                } else {
                    "pending"
                };
                (back, Some(now + outbox::backoff(attempts)), Some(m), kept)
            }
        };
        let r = self.store().write(|tx| {
            steve_store::github::settle_effect(
                tx,
                &e.id,
                state,
                next,
                error.as_deref(),
                &result,
                now,
            )
            .map_err(ApiError::from)
        });
        if let Err(err) = r {
            tracing::error!(effect = %e.id, error = %err, "github effect result not recorded");
        }
    }

    /// Tells the item (or the session) that a write is unconfirmed: on the first unknown, and when
    /// Steve stops trying by itself.
    async fn effect_note(&self, e: &EffectRow, why: &str, last: bool) {
        let what = match e.kind.as_str() {
            OPEN => "开 PR",
            MERGE => "合并 PR",
            _ => "发评论",
        };
        let text = if last {
            format!(
                "{what}一直没有得到 GitHub 的确认（{why}），已停止自动核对，请到 GitHub 上看一下"
            )
        } else {
            format!("{what}时 GitHub 没有回应（{why}），可能已经完成，稍后核对后再决定是否重做")
        };
        let d = self
            .store()
            .read(|c| steve_store::decision_repo::get(c, &e.decision_id).map_err(ApiError::from))
            .ok()
            .flatten();
        let (item, session) = d.map(|d| (d.item_id, d.session_id)).unwrap_or_default();
        self.tell_session(&e.workspace_id, item.as_deref(), session.as_deref(), &text)
            .await;
    }

    /// After a restart: running effects become unknown (reconciled on their next run).
    pub fn recover_effects(&self) -> usize {
        let now = self.now();
        self.store()
            .write(|tx| {
                let after = now + crate::api::REQUEST_TIMEOUT.as_millis() as i64;
                steve_store::github::interrupted_effects(tx, now, after).map_err(ApiError::from)
            })
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "github effects not recovered");
                0
            })
    }

    /// The runner: recovers once, then drives due effects via [`due_loop`], woken on every
    /// queued write. Holds the service weakly; stops when cancelled.
    pub async fn run_effects_job(self) {
        self.recover_effects();
        let clock = self.0.conns.modules().clock.clone();
        let wake = self.0.effect_wake.clone();
        let source = Arc::new(EffectsDueSource(Arc::downgrade(&self.0)));
        drop(self);
        let backoff = Backoff {
            base: RETRY_BASE,
            max: RETRY_MAX,
            factor: 2,
            jitter: Jitter::Equal,
            attempts: None,
            reset_after: None,
        };
        due_loop(source, clock, wake, backoff).await;
    }
}

struct EffectsDueSource(Weak<Inner>);

impl DueSource for EffectsDueSource {
    fn name(&self) -> &'static str {
        "github.effects"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let Some(inner) = self.0.upgrade() else {
            return Ok(None);
        };
        let gh = GitHub(inner);
        gh.store()
            .read(|r| steve_store::github::next_due_effect(r))
            .map_err(|e| DueError(e.to_string()))
    }

    async fn run_due(&self, _now: i64) -> Result<(), DueError> {
        let Some(inner) = self.0.upgrade() else {
            return Ok(());
        };
        let gh = GitHub(inner);
        gh.run_effects().await;
        Ok(())
    }
}

impl GitHub {
    /// Writes down what a run is about before it sends anything (the commit to push and open a PR
    /// from): a later run holds to it. Nothing sent yet if this fails: tried again.
    pub(crate) fn effect_intent(&self, decision: &str, intent: &Value) -> Result<(), Fail> {
        self.store()
            .write(|tx| {
                steve_store::github::effect_intent(tx, decision, intent).map_err(ApiError::from)
            })
            .map_err(|e| Fail::new(e.to_string(), Why::Wait))
    }

    /// Claims the outbound step under the same writer that closes the item. A closed
    /// cycle may still be reconciled by readback, but no PR step can be started for it.
    pub(crate) fn mark_pull_write(
        &self,
        d: &steve_store::Decision,
        github_api: bool,
    ) -> Result<(), Fail> {
        let recorded = self
            .store()
            .write(|tx| {
                if let Some(item) = d.item_id.as_deref() {
                    let Some(cycle) =
                        steve_store::item_repo::open_cycle(tx, &d.workspace_id, item)?
                    else {
                        return Ok(false);
                    };
                    let scope = steve_store::item_delivery::scope(tx, item)?;
                    let effect = steve_store::github::effect_of(tx, &d.id)?;
                    if !effect.is_some_and(|effect| {
                        effect.item_cycle == Some(cycle) && effect.item_scope == Some(scope)
                    }) {
                        return Ok(false);
                    }
                }
                let intent = if github_api {
                    json!({"tried": true})
                } else {
                    json!({"push_started": true})
                };
                steve_store::github::effect_intent(tx, &d.id, &intent)?;
                Ok::<_, steve_store::StoreError>(true)
            })
            .map_err(|error| Fail::new(error.to_string(), Why::Wait))?;
        if recorded {
            Ok(())
        } else {
            Err(Fail::definite("pull request approval is no longer current"))
        }
    }

    /// Right before the write goes to GitHub: from here on, a run that does not hear back
    /// reconciles before writing again.
    pub(crate) fn effect_sending(&self, decision: &str) -> Result<(), Fail> {
        self.effect_intent(decision, &json!({ "tried": true }))
    }
}

/// Whether an earlier run sent the write.
fn tried(result: &Value) -> bool {
    result["tried"].as_bool() == Some(true)
}

/// `a` with `b`'s keys over it.
fn merge_json(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Object(mut a), Value::Object(b)) => {
            a.extend(b);
            Value::Object(a)
        }
        (a, Value::Null) => a,
        (_, b) => b,
    }
}

/// `{"pr": n}`.
pub(crate) fn pr_result(number: i64) -> Value {
    json!({ "pr": number })
}
