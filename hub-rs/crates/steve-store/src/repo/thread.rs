//! Thread routing (migration 016): one `route_log` row per routed user
//! message, `thread_runs` for inline research runs, and `thread_items` linking an item to the conversation
//! message it came from. None of these publish events: the messages they describe do.

use rusqlite::{Row, params};
use serde::Serialize;
use serde_json::Value;

use super::{Cached, all, must, one};
use crate::{Result, Store, now, uid};

impl Store {
    pub fn thread(&self) -> Thread<'_> {
        Thread(self)
    }
}

pub struct Thread<'a>(&'a Store);

/// How one message was routed: what the model said, what the guards made of it, and later corrections.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteLog {
    pub id: String,
    pub workspace_id: String,
    pub message_id: String,
    pub reply_id: Option<String>,
    pub model: Option<String>,
    /// the kind the model gave
    pub model_kind: String,
    /// the kind after `decide`
    pub kind: String,
    pub confidence: Option<f64>,
    pub signals: Vec<String>,
    /// which guard rule changed the kind (`None`: none did)
    pub guard: Option<String>,
    pub fallback: bool,
    pub item_id: Option<String>,
    pub ms: i64,
    /// to_inline | to_item | to_supplement
    #[serde(rename = "override")]
    pub override_: Option<String>,
    pub override_at: Option<i64>,
    /// offer_taken | offer_dismissed | suggest_here | suggest_item | suggest_expired
    pub outcome: Option<String>,
    pub created: i64,
}

impl RouteLog {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let signals: String = r.get("signals")?;
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            message_id: r.get("message_id")?,
            reply_id: r.get("reply_id")?,
            model: r.get("model")?,
            model_kind: r.get("model_kind")?,
            kind: r.get("kind")?,
            confidence: r.get("confidence")?,
            signals: serde_json::from_str(&signals).unwrap_or_default(),
            guard: r.get("guard")?,
            fallback: r.get::<_, i64>("fallback")? != 0,
            item_id: r.get("item_id")?,
            ms: r.get("ms")?,
            override_: r.get("override")?,
            override_at: r.get("override_at")?,
            outcome: r.get("outcome")?,
            created: r.get("created")?,
        })
    }
}

/// Fields of a new `route_log` row.
#[derive(Debug, Clone, Default)]
pub struct NewRouteLog {
    pub workspace_id: String,
    pub message_id: String,
    pub reply_id: Option<String>,
    pub model: Option<String>,
    pub model_kind: String,
    pub kind: String,
    pub confidence: Option<f64>,
    pub signals: Vec<String>,
    pub guard: Option<String>,
    pub fallback: bool,
    pub item_id: Option<String>,
    pub ms: i64,
}

/// One inline research run hanging off the main conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadRun {
    pub id: String,
    pub workspace_id: String,
    pub message_id: String,
    pub reply_id: Option<String>,
    pub prev_run: Option<String>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub model: Option<String>,
    /// The harness adapter's session id (to continue the run with) and the adapter's id.
    pub session: Option<String>,
    pub harness: Option<String>,
    /// queued | running | done | stopped | budget | violated | failed | interrupted
    pub status: String,
    #[serde(skip)]
    pub token_hash: Option<String>,
    pub tools: i64,
    pub turns: i64,
    pub first_text_ms: Option<i64>,
    pub error: Option<String>,
    pub created: i64,
    pub started: Option<i64>,
    pub ended: Option<i64>,
}

impl ThreadRun {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            message_id: r.get("message_id")?,
            reply_id: r.get("reply_id")?,
            prev_run: r.get("prev_run")?,
            user_id: r.get("user_id")?,
            project_id: r.get("project_id")?,
            model: r.get("model")?,
            session: r.get("session")?,
            harness: r.get("harness")?,
            status: r.get("status")?,
            token_hash: r.get("token_hash")?,
            tools: r.get("tools")?,
            turns: r.get("turns")?,
            first_text_ms: r.get("first_text_ms")?,
            error: r.get("error")?,
            created: r.get("created")?,
            started: r.get("started")?,
            ended: r.get("ended")?,
        })
    }

    /// Still waiting or still reading.
    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

/// Fields of a new `thread_runs` row (status `queued`).
#[derive(Debug, Clone, Default)]
pub struct NewThreadRun {
    pub workspace_id: String,
    pub message_id: String,
    pub reply_id: Option<String>,
    pub prev_run: Option<String>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
}

/// A change to a run; `None` leaves the column as it is.
#[derive(Debug, Clone, Default)]
pub struct RunPatch {
    pub status: Option<String>,
    pub reply_id: Option<String>,
    pub model: Option<String>,
    pub session: Option<String>,
    pub harness: Option<String>,
    /// `Some(None)` clears the token (the run ended)
    pub token_hash: Option<Option<String>>,
    pub tools: Option<i64>,
    pub turns: Option<i64>,
    pub first_text_ms: Option<i64>,
    pub error: Option<String>,
    pub started: Option<i64>,
    pub ended: Option<i64>,
}

/// An item that came from the main conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadItem {
    pub item_id: String,
    pub workspace_id: String,
    /// the user's message it started from
    pub origin_msg: String,
    /// Steve's message announcing it
    pub reply_msg: Option<String>,
    pub from_question: bool,
    pub follow: bool,
    pub last_post: Option<i64>,
    pub created: i64,
}

impl ThreadItem {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            item_id: r.get("item_id")?,
            workspace_id: r.get("workspace_id")?,
            origin_msg: r.get("origin_msg")?,
            reply_msg: r.get("reply_msg")?,
            from_question: r.get::<_, i64>("from_question")? != 0,
            follow: r.get::<_, i64>("follow")? != 0,
            last_post: r.get("last_post")?,
            created: r.get("created")?,
        })
    }
}

const ROUTE_BY_MSG: &str = "SELECT * FROM route_log WHERE message_id = ?";
const RUN: &str = "SELECT * FROM thread_runs WHERE id = ?";
const THREAD_ITEM: &str = "SELECT * FROM thread_items WHERE item_id = ?";
/// The settings key that marks the one-time backfill as done.
pub const BACKFILL_KEY: &str = "thread.backfill";

impl Thread<'_> {
    // ── route_log ──

    /// Records how a message was routed. A message is routed once: a second call returns the first row.
    pub fn route_add(&self, n: &NewRouteLog) -> Result<RouteLog> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO route_log (id, workspace_id, message_id, reply_id, model, model_kind, kind, confidence, signals, guard, fallback, item_id, ms, created)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(message_id) DO NOTHING",
                params![
                    uid("rt"),
                    n.workspace_id,
                    n.message_id,
                    n.reply_id,
                    n.model,
                    n.model_kind,
                    n.kind,
                    n.confidence,
                    serde_json::to_string(&n.signals)?,
                    n.guard,
                    n.fallback as i64,
                    n.item_id,
                    n.ms,
                    now()
                ],
            )?;
            must(one(w.c, ROUTE_BY_MSG, [&n.message_id], RouteLog::from_row)?)
        })
    }

    pub fn route_of(&self, message_id: &str) -> Result<Option<RouteLog>> {
        self.0
            .read(|c| one(c, ROUTE_BY_MSG, [message_id], RouteLog::from_row))
    }

    /// Steve's reply to a routed message, and the item it made if any.
    pub fn route_reply(&self, message_id: &str, reply: &str, item: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE route_log SET reply_id = ?, item_id = coalesce(?, item_id) WHERE message_id = ?",
                params![reply, item, message_id],
            )?;
            Ok(())
        })
    }

    /// The user corrected the routing. Only the first correction counts; returns whether this one did.
    pub fn route_override(&self, message_id: &str, to: &str, item: Option<&str>) -> Result<bool> {
        self.0.write(|w| {
            Ok(w.c.run(
                "UPDATE route_log SET override = ?, override_at = ?, item_id = coalesce(?, item_id) WHERE message_id = ? AND override IS NULL",
                params![to, now(), item, message_id],
            )? > 0)
        })
    }

    /// What became of an offer or a suggestion card (not a correction).
    pub fn route_outcome(&self, message_id: &str, outcome: &str, item: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE route_log SET outcome = ?, item_id = coalesce(?, item_id) WHERE message_id = ?",
                params![outcome, item, message_id],
            )?;
            Ok(())
        })
    }

    /// The routing of a workspace in `[from, to)`, oldest first.
    pub fn routes(&self, ws: &str, from: i64, to: i64) -> Result<Vec<RouteLog>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM route_log WHERE workspace_id = ? AND created >= ? AND created < ? ORDER BY created, rowid",
                params![ws, from, to],
                RouteLog::from_row,
            )
        })
    }

    // ── thread_runs ──

    pub fn run_add(&self, n: &NewThreadRun) -> Result<ThreadRun> {
        let id = uid("tr");
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO thread_runs (id, workspace_id, message_id, reply_id, prev_run, user_id, project_id, status, created)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?)",
                params![
                    id,
                    n.workspace_id,
                    n.message_id,
                    n.reply_id,
                    n.prev_run,
                    n.user_id,
                    n.project_id,
                    now()
                ],
            )?;
            must(one(w.c, RUN, [&id], ThreadRun::from_row)?)
        })
    }

    pub fn run(&self, id: &str) -> Result<Option<ThreadRun>> {
        self.0.read(|c| one(c, RUN, [id], ThreadRun::from_row))
    }

    pub fn run_patch(&self, id: &str, p: &RunPatch) -> Result<Option<ThreadRun>> {
        let mut s = super::Sets::default();
        s.val("status", p.status.clone())
            .val("reply_id", p.reply_id.clone())
            .val("model", p.model.clone())
            .val("session", p.session.clone())
            .val("harness", p.harness.clone())
            .null("token_hash", p.token_hash.clone())
            .val("tools", p.tools)
            .val("turns", p.turns)
            .val("first_text_ms", p.first_text_ms)
            .val("error", p.error.clone())
            .val("started", p.started)
            .val("ended", p.ended);
        self.0.write(|w| {
            if !s.is_empty() {
                w.c.run(
                    &format!("UPDATE thread_runs SET {} WHERE id = ?", s.sql()),
                    rusqlite::params_from_iter(s.params([id.to_string().into()])),
                )?;
            }
            one(w.c, RUN, [id], ThreadRun::from_row)
        })
    }

    /// Queued and running runs of a workspace, oldest first.
    pub fn runs_active(&self, ws: &str) -> Result<Vec<ThreadRun>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM thread_runs WHERE workspace_id = ? AND status IN ('queued', 'running') ORDER BY created, rowid",
                [ws],
                ThreadRun::from_row,
            )
        })
    }

    /// The running run whose MCP token is `token` (`token_hash = sha256(token)`, cleared when it ends).
    pub fn run_by_token(&self, token: &str) -> Result<Option<ThreadRun>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM thread_runs WHERE token_hash = ? AND status = 'running'",
                [crate::sha(token)],
                ThreadRun::from_row,
            )
        })
    }

    /// The workspace's latest finished run (`done`), the one a follow-up may `--resume`.
    pub fn run_last_done(&self, ws: &str) -> Result<Option<ThreadRun>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM thread_runs WHERE workspace_id = ? AND status = 'done' ORDER BY ended DESC, rowid DESC LIMIT 1",
                [ws],
                ThreadRun::from_row,
            )
        })
    }

    /// The runs answering one message (a research reply continued after a suggestion card, a re-answer).
    pub fn runs_of_message(&self, message_id: &str) -> Result<Vec<ThreadRun>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM thread_runs WHERE message_id = ? ORDER BY created, rowid",
                [message_id],
                ThreadRun::from_row,
            )
        })
    }

    /// At boot: every run still queued or running did not finish. Returns them, now `interrupted`.
    /// `resume`: record a resume intent for the local runs (off under `STEVE_NO_RESUME`, so a later
    /// normal boot does not revive them).
    pub fn runs_interrupt(&self, resume: bool) -> Result<Vec<ThreadRun>> {
        self.0.write(|w| {
            let tx = w.tx()?;
            // Reply persistence is the completion receipt even if the process died before the
            // subsequent run_patch. Do not generate a second local answer on the next boot.
            tx.run("UPDATE thread_runs SET status='done',token_hash=NULL,ended=? WHERE status IN ('queued','running') AND EXISTS (SELECT 1 FROM messages m WHERE json_valid(m.meta) AND json_extract(m.meta,'$.run')=thread_runs.id AND json_extract(m.meta,'$.run_receipt')=1 AND m.author='steve')", [now()])?;
            let ids: Vec<String> = all(
                &tx,
                "SELECT id FROM thread_runs WHERE status IN ('queued', 'running') ORDER BY created, rowid",
                [],
                |r| r.get(0),
            )?;
            let t = now();
            for id in &ids {
                // Only the local, read-only conversation is safe to restart automatically.
                // Guest answers can deliver externally and retain their explicit rerun decision.
                if resume {
                    tx.run("INSERT OR IGNORE INTO thread_resume (run_id, created) SELECT r.id,? FROM thread_runs r JOIN messages m ON m.id=r.message_id WHERE r.id=? AND m.space='steve'", params![t,id])?;
                }
                tx.run(
                    "UPDATE thread_runs SET status = 'interrupted', ended = ?, token_hash = NULL WHERE id = ?",
                    params![t, id],
                )?;
            }
            let mut out = Vec::with_capacity(ids.len());
            for id in &ids {
                out.push(must(one(&tx, RUN, [id], ThreadRun::from_row)?)?);
            }
            tx.commit()?;
            Ok(out)
        })
    }

    /// Interrupted local runs with an intent first recorded after `since`, and the automatic
    /// re-answers each already had.
    pub fn runs_to_resume(&self, since: i64) -> Result<Vec<(ThreadRun, i64)>> {
        self.0.read(|c| {
            let mut st = c.prepare_cached("SELECT r.*, p.attempts AS resume_attempts FROM thread_runs r JOIN thread_resume p ON p.run_id=r.id WHERE r.status='interrupted' AND p.created > ? ORDER BY r.created")?;
            let rows = st
                .query_map([since], |r| Ok((ThreadRun::from_row(r)?, r.get::<_, i64>("resume_attempts")?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The re-answers already started for `run` (0 without an intent).
    pub fn resume_attempts(&self, run: &str) -> Result<i64> {
        self.0.read(|c| {
            Ok(super::one(
                c,
                "SELECT attempts FROM thread_resume WHERE run_id = ?",
                [run],
                |r| r.get(0),
            )?
            .unwrap_or(0))
        })
    }

    /// Counts a re-answer, before it starts (a crash during it then counts).
    pub fn resume_attempt(&self, run: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE thread_resume SET attempts = attempts + 1 WHERE run_id = ?",
                [run],
            )?;
            Ok(())
        })
    }

    /// Drops the intent: it will not be re-answered by itself (an explicit rerun still can).
    pub fn resume_drop(&self, run: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run("DELETE FROM thread_resume WHERE run_id = ?", [run])?;
            Ok(())
        })
    }

    /// Your messages in the main conversation since `since` that were stored but never routed (the
    /// routing lane is in memory: a stop before routing leaves no route, no run, no reply), without
    /// a notice yet. Guarded pastes are never routed and are skipped. The core checks whether a
    /// candidate has a usable durable routing Job; valid JSON alone cannot establish that.
    pub fn unrouted_since(&self, since: i64) -> Result<Vec<(String, String)>> {
        self.0.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT m.workspace_id, m.id FROM messages m
                  WHERE m.space = 'steve' AND m.author = 'me' AND m.created > ?
                    AND NOT (m.meta IS NOT NULL AND json_valid(m.meta) AND json_extract(m.meta, '$.guarded') IS 1)
                    AND NOT EXISTS (SELECT 1 FROM route_log l WHERE l.message_id = m.id)
                    AND NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = m.id)
                    AND NOT EXISTS (SELECT 1 FROM messages x WHERE x.workspace_id = m.workspace_id AND x.space = 'steve'
                                     AND json_valid(x.meta) AND (json_extract(x.meta, '$.origin') = m.id OR json_extract(x.meta, '$.rerun') = m.id))
                  ORDER BY m.created",
            )?;
            let rows = st
                .query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // ── thread_items ──

    /// Links an item to the message it came from; an item keeps its first origin.
    pub fn item_add(
        &self,
        item: &str,
        ws: &str,
        origin: &str,
        reply: Option<&str>,
        from_question: bool,
    ) -> Result<ThreadItem> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO thread_items (item_id, workspace_id, origin_msg, reply_msg, from_question, created) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(item_id) DO NOTHING",
                params![item, ws, origin, reply, from_question as i64, now()],
            )?;
            must(one(w.c, THREAD_ITEM, [item], ThreadItem::from_row)?)
        })
    }

    pub fn item(&self, item: &str) -> Result<Option<ThreadItem>> {
        self.0
            .read(|c| one(c, THREAD_ITEM, [item], ThreadItem::from_row))
    }

    /// Steve's announcing message, once it is written.
    pub fn item_reply(&self, item: &str, reply: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE thread_items SET reply_msg = ? WHERE item_id = ?",
                params![reply, item],
            )?;
            Ok(())
        })
    }

    pub fn item_follow(&self, item: &str, follow: bool) -> Result<bool> {
        self.0.write(|w| {
            Ok(w.c.run(
                "UPDATE thread_items SET follow = ? WHERE item_id = ?",
                params![follow as i64, item],
            )? > 0)
        })
    }

    pub fn item_posted(&self, item: &str, at: i64) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE thread_items SET last_post = ? WHERE item_id = ?",
                params![at, item],
            )?;
            Ok(())
        })
    }

    /// The conversation's items of a workspace, newest first.
    pub fn items(&self, ws: &str) -> Result<Vec<ThreadItem>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM thread_items WHERE workspace_id = ? ORDER BY created DESC, rowid DESC",
                [ws],
                ThreadItem::from_row,
            )
        })
    }

    /// The one-time backfill: unfinished items made from the conversation get their `thread_items`
    /// row from the message announcing them and the user message before it. Runs once per workspace
    /// (settings `thread.backfill = "v1"`); running it again changes nothing. Returns the items linked.
    pub fn backfill(&self, ws: &str) -> Result<Vec<String>> {
        self.0.write(|w| {
            let tx = w.tx()?;
            let done: Option<String> = one(
                &tx,
                "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
                [ws, BACKFILL_KEY],
                |r| r.get(0),
            )?;
            if done.is_some() {
                return Ok(vec![]);
            }
            let items: Vec<(String, String)> = all(
                &tx,
                "SELECT id, source FROM items WHERE workspace_id = ? AND status NOT IN ('done', 'drop') ORDER BY created, rowid",
                [ws],
                |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())),
            )?;
            let mut linked = vec![];
            for (id, source) in items {
                let src: Value = serde_json::from_str(&source).unwrap_or(Value::Null);
                if src != serde_json::json!(["对话", "chat"]) {
                    continue;
                }
                let reply: Option<(String, i64)> = one(
                    &tx,
                    "SELECT id, created FROM messages WHERE workspace_id = ? AND space = 'steve' AND author = 'steve' AND item_id = ? ORDER BY created, rowid LIMIT 1",
                    params![ws, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let Some((reply, at)) = reply else { continue };
                let origin: Option<String> = one(
                    &tx,
                    "SELECT id FROM messages WHERE workspace_id = ? AND space = 'steve' AND author = 'me' AND created <= ? ORDER BY created DESC, rowid DESC LIMIT 1",
                    params![ws, at],
                    |r| r.get(0),
                )?;
                let Some(origin) = origin else { continue };
                if tx.run(
                    "INSERT INTO thread_items (item_id, workspace_id, origin_msg, reply_msg, from_question, created) VALUES (?, ?, ?, ?, 0, ?)
                     ON CONFLICT(item_id) DO NOTHING",
                    params![id, ws, origin, reply, now()],
                )? > 0
                {
                    linked.push(id);
                }
            }
            tx.run(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, '\"v1\"', ?) ON CONFLICT(workspace_id, key) DO NOTHING",
                params![ws, BACKFILL_KEY, now()],
            )?;
            tx.commit()?;
            Ok(linked)
        })
    }
}
