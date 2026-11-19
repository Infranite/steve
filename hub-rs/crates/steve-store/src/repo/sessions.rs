//! Sessions, their event log and queued messages.

use super::Cached;
use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Sets, all, exists, must, one, to_json};
use crate::types::{double, json_or};
use crate::{Event, FileRef, Queued, Result, Session, SessionStop, Store, now, sha, uid};

impl Store {
    pub fn sessions(&self) -> Sessions<'_> {
        Sessions(self)
    }
    pub fn events(&self) -> Events<'_> {
        Events(self)
    }
    pub fn queued(&self) -> QueuedRepo<'_> {
        QueuedRepo(self)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewSession {
    pub workspace_id: String,
    pub item_id: Option<String>,
    pub bot_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub cwd: String,
    pub title: String,
    /// The MCP bearer token; only its hash is stored.
    pub token: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub acp_session_id: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub used: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub size: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub cost: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub item_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no: Option<i64>,
}

/// A local Claude Code / Codex session copied in read-only (system.ts `importLocal`).
#[derive(Debug, Clone, Default)]
pub struct ImportedSession {
    pub workspace_id: String,
    pub harness: String,
    pub cwd: String,
    pub title: String,
    /// e.g. `本机 Claude Code`
    pub source: String,
    pub import_key: String,
    /// when the local session happened (becomes `created`)
    pub created: i64,
}

pub struct Sessions<'a>(&'a Store);

const SESSION: &str = "SELECT * FROM sessions WHERE id = ?";

impl Sessions<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Session>> {
        self.0.read(|c| one(c, SESSION, [id], Session::from_row))
    }

    /// The session whose MCP token this is.
    pub fn by_token(&self, token: &str) -> Result<Option<Session>> {
        self.0.read(|c| {
            one(
                c,
                // an ended session's token opens nothing: whatever still holds it is a leftover
                "SELECT * FROM sessions WHERE token_hash = ? AND status <> 'ended'",
                [sha(token)],
                Session::from_row,
            )
        })
    }

    pub fn of_item(&self, item_id: &str) -> Result<Vec<Session>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM sessions WHERE item_id = ? ORDER BY no",
                [item_id],
                Session::from_row,
            )
        })
    }

    /// Most recently updated first: 300 per workspace, or 1000 across all.
    pub fn list(&self, ws: Option<&str>) -> Result<Vec<Session>> {
        self.0.read(|c| match ws {
            Some(ws) => all(
                c,
                "SELECT * FROM sessions WHERE workspace_id = ? ORDER BY updated DESC LIMIT 300",
                [ws],
                Session::from_row,
            ),
            None => all(
                c,
                "SELECT * FROM sessions ORDER BY updated DESC LIMIT 1000",
                [],
                Session::from_row,
            ),
        })
    }

    /// Status `starting`; `no` is the next number among the item's sessions (1 without an item).
    pub fn create(&self, p: &NewSession) -> Result<Session> {
        self.0.write(|w| {
            let (id, t) = (uid("ses"), now());
            let no = match &p.item_id {
                Some(item) => {
                    let n: i64 = w.c.row("SELECT count(*) FROM sessions WHERE item_id = ?", [item], |r| r.get(0))?;
                    n + 1
                }
                None => 1,
            };
            w.c.run(
                "INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, model, reasoning_effort, cwd, status, token_hash, title, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?, ?, ?)",
                params![id, p.workspace_id, p.item_id, no, p.bot_id, p.harness, p.model, p.reasoning_effort, p.cwd, sha(&p.token), p.title, t, t],
            )?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Session {
                    ws: p.workspace_id.clone().into(),
                    id: id.clone().into(),
                    item_id: {
                        let value: Option<Option<String>> = Some(p.item_id.clone());
                        value.map(|value| value.map(Into::into))
                    },
                },
            ));
            must(one(w.c, SESSION, [&id], Session::from_row)?)
        })
    }

    /// Sets the given fields and `updated`; an empty patch does nothing.
    pub fn patch(&self, id: &str, p: &SessionPatch) -> Result<()> {
        self.0.atomic(|| {
            self.0.write(|w| {
                let mut s = Sets::default();
                s.val("status", p.status.clone())
                    .null("acp_session_id", p.acp_session_id.clone())
                    .null("used", p.used)
                    .null("size", p.size)
                    .null("cost", p.cost)
                    .null("model", p.model.clone())
                    .null("reasoning_effort", p.reasoning_effort.clone())
                    .val("title", p.title.clone())
                    .null("item_id", p.item_id.clone())
                    .val("no", p.no);
                if s.is_empty() {
                    return Ok(());
                }
                w.c.prepare_cached(&format!(
                    "UPDATE sessions SET {}, updated = ? WHERE id = ?",
                    s.sql()
                ))?
                .execute(rusqlite::params_from_iter(
                    s.params([now().into(), id.to_string().into()]),
                ))?;
                if p.status.as_deref() == Some("ended") {
                    self.retire_queue(id)?;
                }
                let item_id = one(
                    w.c,
                    "SELECT item_id FROM sessions WHERE id = ?",
                    [id],
                    |r| r.get::<_, Option<String>>(0),
                )?;
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM sessions WHERE id = ?",
                    [id],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Session {
                            ws: ws.into(),
                            id: id.into(),
                            item_id: {
                                let value: Option<Option<String>> = item_id;
                                value.map(|value| value.map(Into::into))
                            },
                        },
                    ));
                }
                Ok(())
            })
        })
    }

    /// End/handoff discards unsent input, including the initial protocol. A successor receives
    /// a fresh protocol; replaying this queue would give it obsolete Bot/session instructions.
    /// Also used to repair queues left on ended sessions by earlier builds.
    pub fn retire_queue(&self, id: &str) -> Result<()> {
        self.0.atomic(|| {
            if self.get(id)?.is_none_or(|s| s.status != "ended") {
                return Err(crate::StoreError::Conflict("只能清理已结束会话的旧队列。".into()));
            }
            let n = self.0.write(|w| Ok(w.c.execute("DELETE FROM queued WHERE session_id=?", [id])?))?;
            if n > 0 {
                self.0.events().add(id, "system", serde_json::json!({
                    "text": format!("会话已结束，已丢弃 {n} 条未发送消息；新会话使用新的任务说明。"),
                    "discarded_queued": n
                }))?;
            }
            let cards = self.0.read(|c| all(c, "SELECT * FROM decisions WHERE session_id=?", [id], crate::Decision::from_row))?;
            for d in cards.into_iter().filter(|d| d.status == "open" && d.subject_type == crate::SESSION_START_FAILURE) {
                self.0.decisions().resolve(&d.id, &serde_json::json!({"superseded":"会话已结束"}), "expired", &Value::Null)?;
            }
            Ok(())
        })
    }

    /// Sessions in `ws` with an event whose JSON contains `q` (unescaped LIKE, as in TS), at most 200.
    pub fn search(&self, ws: &str, q: &str) -> Result<Vec<String>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT s.id FROM sessions s WHERE s.workspace_id = ?1 AND (EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.data LIKE ?2) OR EXISTS (SELECT 1 FROM event_payloads p WHERE p.session_id = s.id AND p.data LIKE ?2)) LIMIT 200",
                [ws.to_string(), format!("%{q}%")],
                |r| r.get(0),
            )
        })
    }

    /// Replaces the MCP token hash (orchestrator `rotateToken`). No event.
    pub fn rotate_token(&self, id: &str, token: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE sessions SET token_hash = ? WHERE id = ?",
                [sha(token), id.to_string()],
            )?;
            Ok(())
        })
    }

    /// How the session's last turn ended and whether you stopped it (`session_stops`).
    pub fn stop(&self, id: &str) -> Result<Option<SessionStop>> {
        self.0.read(|c| {
            Ok(c.query_row(
                "SELECT attempt, stop_reason, error, yours, stop_asked, ended, seq, asked_seq FROM session_stops WHERE session_id = ?",
                [id],
                |r| {
                    Ok(SessionStop {
                        attempt: r.get(0)?,
                        stop_reason: r.get(1)?,
                        error: r.get(2)?,
                        yours: r.get::<_, i64>(3)? != 0,
                        stop_asked: r.get(4)?,
                        ended: r.get(5)?,
                        seq: r.get(6)?,
                        asked_seq: r.get(7)?,
                    })
                },
            )
            .optional()?)
        })
    }

    /// You asked to stop the session's turn `attempt` (`""`: none named; `None`: nothing was
    /// stopped after all). Kept until a turn ends, with the session's last event then.
    pub fn stop_asked(&self, id: &str, attempt: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO session_stops (session_id, stop_asked, asked_seq, updated) VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1), ?3) ON CONFLICT(session_id) DO UPDATE SET stop_asked = ?2, asked_seq = excluded.asked_seq, updated = ?3",
                params![id, attempt, now()],
            )?;
            Ok(())
        })
    }

    /// A turn of the session ended: how, and whether it was the one you stopped (a Stop asked for
    /// it is used up either way).
    pub fn stop_ended(
        &self,
        id: &str,
        attempt: Option<&str>,
        stop_reason: &str,
        error: Option<&str>,
        yours: bool,
    ) -> Result<()> {
        let at = now();
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO session_stops (session_id, attempt, stop_reason, error, yours, stop_asked, ended, seq, updated) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, (SELECT MAX(seq) FROM events WHERE session_id = ?1), ?6) ON CONFLICT(session_id) DO UPDATE SET attempt = ?2, stop_reason = ?3, error = ?4, yours = ?5, stop_asked = NULL, asked_seq = NULL, ended = ?6, seq = excluded.seq, updated = ?6 WHERE session_stops.stop_reason IS NOT 'session_start' OR ?3='session_start'",
                params![id, attempt, stop_reason, error, yours as i64, at],
            )?;
            Ok(())
        })
    }

    /// The end of the session's turn `attempt` came (`session_turns`): `true` the first time,
    /// `false` when it was recorded already (the same end told again, also after a restart).
    pub fn turn_ended(&self, id: &str, attempt: &str, stop_reason: &str) -> Result<bool> {
        let at = now();
        self.0.write(|w| {
            let n = w.c.execute(
                "INSERT INTO session_turns (session_id, attempt, stop_reason, ended, updated) VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT(session_id, attempt) DO UPDATE SET stop_reason = ?3, ended = ?4, updated = ?4 WHERE ended IS NULL",
                params![id, attempt, stop_reason, at],
            )?;
            prune_turns(w.c, id)?;
            Ok(n > 0)
        })
    }

    /// The graph stopped the session's turn `attempt` (a pause): kept until that turn's end comes.
    pub fn turn_paused(&self, id: &str, attempt: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.execute(
                "INSERT INTO session_turns (session_id, attempt, paused, updated) VALUES (?1, ?2, 1, ?3) ON CONFLICT(session_id, attempt) DO UPDATE SET paused = 1, updated = ?3",
                params![id, attempt, now()],
            )?;
            prune_turns(w.c, id)?;
            Ok(())
        })
    }

    /// The session's turns a pause stopped whose mark still stands.
    pub fn turns_paused(&self, id: &str) -> Result<Vec<String>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT attempt FROM session_turns WHERE session_id = ? AND paused = 1 ORDER BY updated, attempt",
                [id],
                |r| r.get(0),
            )
        })
    }

    /// A pause's mark of the session's turn `attempt` goes (`None`: every turn's).
    pub fn turn_unpaused(&self, id: &str, attempt: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            w.c.execute(
                "UPDATE session_turns SET paused = 0, updated = ?3 WHERE session_id = ?1 AND paused = 1 AND (?2 IS NULL OR attempt = ?2)",
                params![id, attempt, now()],
            )?;
            Ok(())
        })
    }

    /// Import keys already used (system.ts `localSessions`).
    pub fn import_keys(&self) -> Result<HashSet<String>> {
        self.0.read(|c| {
            let v: Vec<String> = all(
                c,
                "SELECT import_key FROM sessions WHERE import_key IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            Ok(v.into_iter().collect())
        })
    }

    pub fn is_imported(&self, key: &str) -> Result<bool> {
        self.0
            .read(|c| exists(c, "SELECT 1 FROM sessions WHERE import_key = ?", [key]))
    }

    /// Inserts an ended, read-only, unlinked session and returns its id. No event: the caller adds the
    /// events and then publishes `session` (without `item_id`), as system.ts does.
    pub fn insert_imported(&self, p: &ImportedSession) -> Result<String> {
        self.0.write(|w| {
            let id = uid("ses");
            w.c.run(
                "INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, model, cwd, status, token_hash, title, source, readonly, import_key, created, updated) VALUES (?, ?, NULL, 0, NULL, ?, NULL, ?, 'ended', ?, ?, ?, 1, ?, ?, ?)",
                params![id, p.workspace_id, p.harness, p.cwd, sha(&uid("ro")), p.title, p.source, p.import_key, p.created, now()],
            )?;
            Ok(id)
        })
    }

    /// Deletes a session with its events and queue (system.ts `removeImported`, after its readonly check).
    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            let tx = w.tx()?;
            tx.run("DELETE FROM events WHERE session_id = ?", [id])?;
            tx.run("DELETE FROM queued WHERE session_id = ?", [id])?;
            // its view in shadow names other directories of the machine: it goes with it
            tx.run("DELETE FROM view_shadow WHERE session_id = ?", [id])?;
            let ws = one(
                &tx,
                "SELECT workspace_id FROM sessions WHERE id = ?",
                [id],
                |r| r.get::<_, String>(0),
            )?;
            tx.run("DELETE FROM sessions WHERE id = ?", [id])?;
            tx.commit()?;
            if let Some(ws) = ws {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Session {
                        ws: ws.into(),
                        id: id.into(),
                        item_id: {
                            let value: Option<Option<String>> = None;
                            value.map(|value| value.map(Into::into))
                        },
                    },
                ));
            }

            Ok(())
        })
    }
}

fn ensure_open(c: &rusqlite::Connection, session: &str) -> Result<()> {
    let ended: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=? AND status='ended')",
        [session],
        |r| r.get(0),
    )?;
    if ended {
        return Err(crate::StoreError::Conflict(
            "已结束的会话不能接收新消息，请使用当前会话。".into(),
        ));
    }
    Ok(())
}

pub struct Events<'a>(&'a Store);

/// Events [`Events::compact`] reads and rewrites per transaction.
const COMPACT_PAGE: i64 = 200;

/// Only a session's newest turns are kept (an end told again comes soon after the first).
fn prune_turns(c: &rusqlite::Connection, id: &str) -> Result<()> {
    c.execute(
        "DELETE FROM session_turns WHERE session_id = ?1 AND rowid NOT IN (SELECT rowid FROM session_turns WHERE session_id = ?1 ORDER BY updated DESC, rowid DESC LIMIT 64)",
        [id],
    )?;
    Ok(())
}

fn event_row(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        seq: r.get(0)?,
        ts: r.get(1)?,
        kind: r.get(2)?,
        data: json_or(r.get(3)?, Value::Null),
    })
}

fn hydrate_event(c: &rusqlite::Connection, session: &str, e: &mut Event) -> Result<()> {
    // Only these event kinds are normalized; high-frequency small text chunks
    // do not incur a second SQL statement just to discover they have no payload.
    if matches!(e.kind.as_str(), "tool_call" | "tool_call_update") {
        crate::event_payload::hydrate(c, session, e.seq, &mut e.data)?;
    }
    Ok(())
}

/// Decode a transcript page row, leaving unsupported formats visible at their stored cursor.
fn transcript_row(c: &rusqlite::Connection, session: &str, row: &rusqlite::Row) -> Result<Event> {
    let format: String = row.get(5)?;
    if format != "steve.v1" {
        return Ok(Event {
            seq: row.get(0)?,
            ts: row.get(1)?,
            kind: "system".into(),
            data: serde_json::json!({
                "code": "unsupported_format",
                "format": format,
                "text": format!("Unsupported transcript format: {format}"),
            }),
        });
    }
    let mut event = event_row(row)?;
    hydrate_event(c, session, &mut event)?;
    Ok(event)
}

impl Events<'_> {
    /// Appends an event and returns its `seq`. The published event carries `data` as stored.
    ///
    /// The one place session events are written (transcript entries, Steve tools, core), always in
    /// Steve's own terms (`steve.v1`).
    pub fn add(&self, session_id: &str, kind: &str, data: Value) -> Result<i64> {
        let (stored, payloads) = crate::event_payload::split(kind, &data)?;
        let stored = to_json(&stored)?;
        let ts = now();
        self.0.write(|w| {
            let tx = w.tx()?;
            tx.prepare_cached(
                "INSERT INTO events (session_id, ts, kind, data, format) VALUES (?, ?, ?, ?, 'steve.v1')",
            )?
            .execute(params![session_id, ts, kind, stored])?;
            let seq = tx.last_insert_rowid();
            super::stats::first_tool_in(&tx, session_id, kind, ts)?;
            crate::event_payload::save(&tx, session_id, seq, &payloads)?;
            tx.commit()?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM sessions WHERE id = ?",
                [session_id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::LiveEvent::from(
                    steve_types::WorkLiveEvent::Event {
                        ws: ws.into(),
                        session_id: session_id.into(),
                        seq: steve_types::Seq(seq),
                        kind: kind.into(),
                        data: {
                            let value: serde_json::Value = data;
                            steve_types::RawJson::from_str(&serde_json::to_string(&value).expect("JSON value"))
                                .expect("validated JSON")
                        },
                    },
                ));
            }
            Ok(seq)
        })
    }

    /// Events with `seq > after`, at most `limit` (TS defaults: 0 and 5000).
    pub fn after(&self, session_id: &str, after: i64, limit: i64) -> Result<Vec<Event>> {
        self.0.read(|c| {
            // Keep inline rows and their payload refs in the same read snapshot.
            crate::read_snapshot(c, |tx| {
                let mut events: Vec<Event> = all(
                    tx,
                    "SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq > ? ORDER BY seq LIMIT ?",
                    params![session_id, after, limit],
                    event_row,
                )?;
                for e in &mut events { hydrate_event(tx, session_id, e)?; }
                Ok(events)
            })
        })
    }

    /// Visit at most 500 rows in sequence order within a 2 MiB byte budget, without retaining
    /// all decoded rows. The byte budget
    /// accounts for full referenced strings, not just previews. A single oversize row is
    /// returned alone to guarantee cursor progress; this is not a per-event size limit.
    /// Callers continue from the last returned seq until an empty page (never infer EOF
    /// from a short page). Internal `after`/`up_to` keep their complete-data semantics.
    pub fn visit_page(
        &self,
        session_id: &str,
        after: i64,
        mut each: impl FnMut(Event),
    ) -> Result<()> {
        self.0.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT e.seq, e.ts, e.kind, e.data, length(CAST(e.data AS BLOB)) + COALESCE((SELECT SUM(length(CAST(p.data AS BLOB))) FROM event_payload_refs r JOIN event_payloads p ON p.session_id = r.session_id AND p.hash = r.hash WHERE r.event_seq = e.seq), 0), e.format FROM events e WHERE e.session_id = ? AND e.seq > ? ORDER BY e.seq LIMIT ?"
            )?;
            let mut rows = st.query(params![session_id, after, 500i64])?;
            let (mut count, mut bytes) = (0, 0usize);
            while let Some(row) = rows.next()? {
                let size = row.get::<_, i64>(4)?.max(0) as usize;
                if count > 0 && bytes.saturating_add(size) > 2 * 1024 * 1024 { break; }
                let event = transcript_row(c, session_id, row)?;
                each(event);
                bytes = bytes.saturating_add(size);
                count += 1;
                if bytes >= 2 * 1024 * 1024 { break; }
            }
            Ok(())
        })
    }

    /// Compacts the event log of session `session_id` ([`crate::compact`]: images to placeholders, a
    /// tool's big outputs to head and tail) from where the last compaction stopped up to `upto` (the
    /// last seq when the session was found due; events appended later are left whole), in pages of
    /// [`COMPACT_PAGE`] events, each its own transaction, and records `upto`. Lossy, and only for
    /// archived items whose workspace asked for it. Every page is written, and `upto` recorded, while
    /// holding what `still_due` returns: `None` (the item was taken back up meanwhile) stops the pass
    /// before it writes anything more, and the pass returns `None`. Otherwise it returns the events
    /// changed.
    pub fn compact<G>(
        &self,
        session_id: &str,
        upto: i64,
        mut still_due: impl FnMut() -> Option<G>,
    ) -> Result<Option<usize>> {
        let mut after = self.compacted_upto(session_id)?;
        let mut changed = 0;
        loop {
            let page: Vec<Event> = self.0.read(|c| {
                crate::read_snapshot(c, |tx| {
                    let mut events: Vec<Event> = all(
                        tx,
                        "SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq > ? AND seq <= ? ORDER BY seq LIMIT ?",
                        params![session_id, after, upto, COMPACT_PAGE],
                        event_row,
                    )?;
                    for e in &mut events {
                        hydrate_event(tx, session_id, e)?;
                    }
                    Ok(events)
                })
            })?;
            let Some(last) = page.last().map(|e| e.seq) else {
                break;
            };
            after = last;
            let mut todo = vec![];
            for mut e in page {
                if crate::compact::compact(&e.kind, &mut e.data) {
                    let (stored, payloads) = crate::event_payload::split(&e.kind, &e.data)?;
                    todo.push((e.seq, to_json(&stored)?, payloads));
                }
            }
            if todo.is_empty() {
                continue;
            }
            let Some(_due) = still_due() else {
                return Ok(None);
            };
            changed += todo.len();
            self.0.write(|w| {
                let tx = w.tx()?;
                for (seq, data, payloads) in &todo {
                    // the refs go first: the trigger drops a payload no other event points at
                    tx.run("DELETE FROM event_payload_refs WHERE event_seq = ?", [seq])?;
                    tx.run(
                        "UPDATE events SET data = ? WHERE seq = ? AND session_id = ?",
                        params![data, seq, session_id],
                    )?;
                    crate::event_payload::save(&tx, session_id, *seq, payloads)?;
                }
                tx.commit()?;
                Ok(())
            })?;
        }
        let Some(_due) = still_due() else {
            return Ok(None);
        };
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO session_compactions (session_id, upto, events, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (session_id) DO UPDATE SET upto = max(upto, ?2), events = events + ?3, at = ?4",
                params![session_id, upto, changed as i64, now()],
            )?;
            Ok(())
        })?;
        Ok(Some(changed))
    }

    /// The last seq of session `session_id` compacted already (0: never compacted).
    pub fn compacted_upto(&self, session_id: &str) -> Result<i64> {
        self.0.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(upto), 0) FROM session_compactions WHERE session_id = ?",
                [session_id],
                |r| r.get(0),
            )?)
        })
    }

    /// The seq of session `session_id`'s last event (0: none).
    pub fn last_seq(&self, session_id: &str) -> Result<i64> {
        self.0.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?",
                [session_id],
                |r| r.get(0),
            )?)
        })
    }

    /// Up to `limit` events with `seq < before`, newest first (`before <= 0`: from the last one):
    /// a reader of the latest words pages back with it however long the session is.
    pub fn before(&self, session_id: &str, before: i64, limit: i64) -> Result<Vec<Event>> {
        let before = if before <= 0 { i64::MAX } else { before };
        self.0.read(|c| {
            crate::read_snapshot(c, |tx| {
                let mut events: Vec<Event> = all(
                    tx,
                    "SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq < ? ORDER BY seq DESC LIMIT ?",
                    params![session_id, before, limit],
                    event_row,
                )?;
                for e in &mut events { hydrate_event(tx, session_id, e)?; }
                Ok(events)
            })
        })
    }

    /// Events with `seq <= seq` (the history a fork starts from).
    pub fn up_to(&self, session_id: &str, seq: i64) -> Result<Vec<Event>> {
        self.0.read(|c| {
            crate::read_snapshot(c, |tx| {
                let mut events: Vec<Event> = all(tx, "SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq <= ? ORDER BY seq", params![session_id, seq], event_row)?;
                for e in &mut events { hydrate_event(tx, session_id, e)?; }
                Ok(events)
            })
        })
    }
}

/// Messages waiting for the session's current turn to end.
pub struct QueuedRepo<'a>(&'a Store);

impl QueuedRepo<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Queued>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM queued WHERE id = ?",
                [id],
                Queued::from_row,
            )
        })
    }

    pub fn add(&self, session_id: &str, text: &str, via: &str, files: &[FileRef]) -> Result<()> {
        self.0.write(|w| {
            ensure_open(w.c, session_id)?;
            w.c.run(
                "INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)",
                params![uid("q"), session_id, text, via, now(), super::file_refs(files)?],
            )?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM sessions WHERE id = ?",
                [session_id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Session {
                        ws: ws.into(),
                        id: session_id.into(),
                        item_id: {
                            let value: Option<Option<String>> = None;
                            value.map(|value| value.map(Into::into))
                        },
                    },
                ));
            }
            Ok(())
        })
    }

    /// [`Self::add`] unless the same message (text and attachments) is already waiting in this
    /// session's queue: a second copy would only be merged away when the turn starts, after showing up
    /// twice. False when nothing was added.
    pub fn add_unless_waiting(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<bool> {
        let refs = super::file_refs(files)?;
        self.0.write(|w| {
            ensure_open(w.c, session_id)?;
            let waiting: Option<i64> = one(
                w.c,
                "SELECT 1 FROM queued WHERE session_id = ? AND text = ? AND coalesce(files, '') = coalesce(?, '')",
                params![session_id, text, refs],
                |r| r.get(0),
            )?;
            if waiting.is_some() {
                return Ok(false);
            }
            w.c.run(
                "INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)",
                params![uid("q"), session_id, text, via, now(), refs],
            )?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM sessions WHERE id = ?",
                [session_id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Session {
                        ws: ws.into(),
                        id: session_id.into(),
                        item_id: {
                            let value: Option<Option<String>> = None;
                            value.map(|value| value.map(Into::into))
                        },
                    },
                ));
            }
            Ok(true)
        })
    }

    pub fn list(&self, session_id: &str) -> Result<Vec<Queued>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM queued WHERE session_id = ? ORDER BY created, rowid",
                [session_id],
                Queued::from_row,
            )
        })
    }

    /// Returns and deletes the whole queue. No event.
    pub fn take(&self, session_id: &str) -> Result<Vec<Queued>> {
        self.0.write(|w| {
            let xs = all(
                w.c,
                "SELECT * FROM queued WHERE session_id = ? ORDER BY created, rowid",
                [session_id],
                Queued::from_row,
            )?;
            w.c.run("DELETE FROM queued WHERE session_id = ?", [session_id])?;
            Ok(xs)
        })
    }

    /// Sets the text of a row still waiting in `session_id`'s queue; returns `(before, after)`, or
    /// `None` when a turn already took it (or it never was there). Runs in one write, so it is either
    /// before the take (the turn sends the new text) or after it (nothing changes).
    pub fn edit(&self, session_id: &str, id: &str, text: &str) -> Result<Option<(Queued, Queued)>> {
        self.0.write(|w| {
            let Some(before) = one(
                w.c,
                "SELECT * FROM queued WHERE id = ? AND session_id = ?",
                [id, session_id],
                Queued::from_row,
            )?
            else {
                return Ok(None);
            };
            w.c.run("UPDATE queued SET text = ? WHERE id = ?", params![text, id])?;
            let after = Queued {
                text: text.to_owned(),
                ..before.clone()
            };
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM sessions WHERE id = ?",
                [session_id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Session {
                        ws: ws.into(),
                        id: session_id.into(),
                        item_id: {
                            let value: Option<Option<String>> = None;
                            value.map(|value| value.map(Into::into))
                        },
                    },
                ));
            }
            Ok(Some((before, after)))
        })
    }

    /// Deletes one row of `session_id`'s queue and returns it; `None` when a turn already took it.
    pub fn remove(&self, session_id: &str, id: &str) -> Result<Option<Queued>> {
        self.0.write(|w| {
            let q = one(
                w.c,
                "DELETE FROM queued WHERE id = ? AND session_id = ? RETURNING *",
                [id, session_id],
                Queued::from_row,
            )?;
            if q.is_some() {
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM sessions WHERE id = ?",
                    [session_id],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Session {
                            ws: ws.into(),
                            id: session_id.into(),
                            item_id: {
                                let value: Option<Option<String>> = None;
                                value.map(|value| value.map(Into::into))
                            },
                        },
                    ));
                }
            }
            Ok(q)
        })
    }

    /// Moves a row to the head of `session_id`'s queue (the rest keep their order) and returns it;
    /// `None` when a turn already took it.
    pub fn to_front(&self, session_id: &str, id: &str) -> Result<Option<Queued>> {
        self.0.write(|w| {
            let q = one(
                w.c,
                "UPDATE queued SET created = (SELECT min(created) - 1 FROM queued WHERE session_id = ?1 AND id != ?2)
                 WHERE id = ?2 AND session_id = ?1 AND EXISTS (SELECT 1 FROM queued WHERE session_id = ?1 AND id != ?2 AND created <= (SELECT created FROM queued WHERE id = ?2))
                 RETURNING *",
                [session_id, id],
                Queued::from_row,
            )?;
            if q.is_some() {
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM sessions WHERE id = ?",
                    [session_id],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Session {
                            ws: ws.into(),
                            id: session_id.into(),
                            item_id: {
                                let value: Option<Option<String>> = None;
                                value.map(|value| value.map(Into::into))
                            },
                        },
                    ));
                }
                return Ok(q);
            }
            // already first (or alone), or gone
            one(
                w.c,
                "SELECT * FROM queued WHERE id = ? AND session_id = ?",
                [id, session_id],
                Queued::from_row,
            )
        })
    }

    pub fn drop(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            let s: Option<String> = one(
                w.c,
                "DELETE FROM queued WHERE id = ? RETURNING session_id",
                [id],
                |r| r.get(0),
            )?;
            if let Some(s) = s {
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM sessions WHERE id = ?",
                    [&s],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Session {
                            ws: ws.into(),
                            id: s.into(),
                            item_id: {
                                let value: Option<Option<String>> = None;
                                value.map(|value| value.map(Into::into))
                            },
                        },
                    ));
                }
            }
            Ok(())
        })
    }
}
