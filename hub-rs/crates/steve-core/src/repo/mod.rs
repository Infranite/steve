//! The domain layer's view of the database: a thin adapter over `steve-store` with the call shapes the
//! orchestrator uses (one method per repo.ts call).

use crate::error::Result;
use rusqlite::OptionalExtension;
use serde_json::{Map, Value};

pub use steve_store::{
    Bot, ChangeEvent, Decision, Event, FileRef, FileRow, Goal, Item, Memory, Message, NewDecision,
    NewItem, NewSession, Parent, PlanStep, Project, Queued, Release, Session, Tag, WS, Workspace,
};
/// A timeline entry with its attachments (`timeline.list`).
pub type TimelineRow = steve_store::TimelineEntry;
/// The SSE payload union (bus.ts `HubEvent`).
pub type HubEvent = ChangeEvent;

/// `patch!{status: "run", now: "继续处理"}`: a JSON object of column → value, decoded into the store's patch types.
#[macro_export]
macro_rules! patch {
    ($($k:ident : $v:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut m = serde_json::Map::new();
        $(m.insert(stringify!($k).to_string(), serde_json::json!($v));)*
        m
    }};
}

/// Status shorthands of repo.ts call sites.
pub trait ItemExt {
    /// closed or paused: nothing runs by itself.
    fn is_settled(&self) -> bool;
    /// closed (any outcome).
    fn is_finished(&self) -> bool;
}
impl ItemExt for Item {
    fn is_settled(&self) -> bool {
        self.is_finished() || self.status == "pause"
    }
    /// The lifecycle's phase decides, not the legacy `status`.
    fn is_finished(&self) -> bool {
        self.phase == crate::cycle::CLOSED
    }
}

pub trait BotExt {
    /// "Steve" for the main bot, "<name> Bot" for the others (timeline texts).
    fn display(&self) -> String;
}
impl BotExt for Bot {
    fn display(&self) -> String {
        if self.main {
            self.name.clone()
        } else {
            format!("{} Bot", self.name)
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(m: Map<String, Value>) -> Result<T> {
    Ok(serde_json::from_value(Value::Object(m))?)
}

/// The store plus what the domain layer adds on top. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    s: steve_store::Store,
}

impl Db {
    pub fn new(s: steve_store::Store) -> Db {
        Db { s }
    }

    /// The underlying store (for the API and the runner).
    pub fn store(&self) -> &steve_store::Store {
        &self.s
    }

    pub fn publish(&self, e: HubEvent) {
        self.s.publish(e)
    }

    /// Raw SQL on the write connection (tests).
    pub fn exec(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<usize> {
        Ok(self.s.with_conn(|c| Ok(c.execute(sql, p)?))?)
    }

    pub fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        detail: &Value,
        ws: &str,
    ) -> Result<()> {
        Ok(self.s.audit(actor, action, target, detail, ws)?)
    }

    /* ── workspaces, settings ── */
    pub fn ensure_workspace(&self) -> Result<()> {
        Ok(self.s.ensure_workspace()?)
    }
    pub fn workspace(&self, id: &str) -> Result<Option<Workspace>> {
        Ok(self.s.workspaces().get(id)?)
    }
    pub fn workspace_ids(&self) -> Result<Vec<String>> {
        Ok(self.s.workspaces().ids()?)
    }
    pub fn workspace_patch(&self, id: &str, p: Map<String, Value>) -> Result<()> {
        self.s.workspaces().patch(id, &decode(p)?)?;
        Ok(())
    }
    pub fn settings_all(&self, ws: &str) -> Result<Map<String, Value>> {
        Ok(self.s.settings().all(ws)?)
    }
    /// A setting as text (they are stored as JSON strings), else `d`.
    pub fn setting(&self, ws: &str, key: &str, d: &str) -> Result<String> {
        Ok(self.s.settings().get::<String>(ws, key, d.to_string())?)
    }
    pub fn settings_set(&self, ws: &str, p: &Map<String, Value>, author: &str) -> Result<()> {
        Ok(self.s.settings().set(ws, p, author)?)
    }

    /* ── projects ── */
    pub fn projects(&self, ws: &str) -> Result<Vec<Project>> {
        Ok(self.s.projects().list(ws, false)?)
    }
    pub fn project(&self, id: Option<&str>) -> Result<Option<Project>> {
        match id {
            Some(id) => Ok(self.s.projects().get(id)?),
            None => Ok(None),
        }
    }
    pub fn project_by_path(&self, ws: &str, path: &str) -> Result<Option<Project>> {
        Ok(self.s.projects().by_path(ws, path)?)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn project_create(
        &self,
        ws: &str,
        name: &str,
        description: &str,
        path: &str,
        kind: &str,
        worktree: bool,
        default_bot: Option<&str>,
        release: Option<&Release>,
    ) -> Result<Project> {
        Ok(self.s.projects().create(
            ws,
            &steve_store::NewProject {
                name: name.into(),
                description: description.into(),
                device_id: "hub".into(),
                path: path.into(),
                kind: kind.into(),
                worktree,
                default_bot: default_bot.map(String::from),
                release: release.cloned(),
            },
        )?)
    }
    /// Patch with a JSON object of column → value (as the API and config drafts send it).
    pub fn project_patch(&self, id: &str, p: &Map<String, Value>, author: &str) -> Result<()> {
        self.s.projects().patch(id, &decode(p.clone())?, author)?;
        Ok(())
    }

    /* ── bots ── */
    pub fn bots(&self, ws: &str) -> Result<Vec<Bot>> {
        Ok(self.s.bots().list(Some(ws))?)
    }
    pub fn bot(&self, id: &str) -> Result<Option<Bot>> {
        Ok(self.s.bots().get(id)?)
    }
    pub fn main_bot(&self) -> Result<Bot> {
        self.s
            .bots()
            .main()?
            .ok_or_else(|| crate::error::msg(steve_i18n::t!("core.bot.no_main")))
    }
    pub fn bot_workspaces(&self, id: &str) -> Result<Vec<String>> {
        Ok(self
            .s
            .bots()
            .workspaces(id)?
            .into_iter()
            .map(|w| w.workspace_id)
            .collect())
    }
    pub fn bot_in_workspace(&self, id: &str, ws: &str) -> Result<bool> {
        Ok(self.s.bots().in_workspace(id, ws)?)
    }
    /// Seed-only insert (never overwrites what the user changed); `b` is a `NewBot` as JSON.
    pub fn bot_seed(&self, id: &str, main: bool, b: Value, ws: &str) -> Result<()> {
        Ok(self
            .s
            .bots()
            .seed(id, main, &serde_json::from_value(b)?, ws)?)
    }
    pub fn bot_seed_fixups(&self, steve_project: Option<&str>) -> Result<()> {
        Ok(self.s.bots().seed_fixups(steve_project)?)
    }
    /// A new bot in `ws`; `b` is a `NewBot` as JSON.
    pub fn bot_create(&self, ws: &str, b: Value, author: &str) -> Result<Bot> {
        self.s
            .bots()
            .create(ws, &serde_json::from_value(b)?, author)?
            .ok_or_else(|| crate::error::msg(steve_i18n::t!("core.bot.not_found")))
    }
    pub fn bot_patch(&self, id: &str, p: &Map<String, Value>, author: &str) -> Result<()> {
        self.s.bots().patch(id, &decode(p.clone())?, author)?;
        Ok(())
    }
    pub fn bot_remove(&self, id: &str) -> Result<()> {
        Ok(self.s.bots().remove(id)?)
    }
    pub fn memories(&self, bot: &str, ws: &str) -> Result<Vec<Memory>> {
        Ok(self.s.memories().list(bot, ws)?)
    }
    pub fn memory_add(&self, bot: &str, ws: &str, text: &str, source: &str) -> Result<String> {
        Ok(self.s.memories().add(bot, ws, text, source)?)
    }

    /* ── tags ── */
    pub fn tags(&self, ws: &str) -> Result<Vec<Tag>> {
        Ok(self.s.tags().list(ws)?)
    }
    pub fn tag(&self, id: &str) -> Result<Option<Tag>> {
        Ok(self.s.tags().get(id)?)
    }
    pub fn tag_by_name(&self, ws: &str, name: &str) -> Result<Option<Tag>> {
        Ok(self.s.tags().by_name(ws, name)?)
    }
    /// Create, or return the existing tag of that name.
    pub fn tag_create(
        &self,
        ws: &str,
        name: &str,
        cat: &str,
        description: &str,
        by: &str,
    ) -> Result<Tag> {
        let t = steve_store::NewTag {
            name: name.into(),
            cat: cat.into(),
            description: description.into(),
        };
        Ok(self.s.tags().create(ws, &t, by)?)
    }
    pub fn tag_set_description(&self, id: &str, description: &str) -> Result<()> {
        let p = steve_store::TagPatch {
            description: Some(description.into()),
            ..Default::default()
        };
        self.s.tags().patch(id, &p)?;
        Ok(())
    }
    pub fn tag_patch(&self, id: &str, p: &steve_store::TagPatch) -> Result<()> {
        self.s.tags().patch(id, p)?;
        Ok(())
    }
    /// How many items and other objects (contacts, teams, spaces…) carry the tag.
    pub fn tag_used(&self, id: &str) -> Result<i64> {
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT (SELECT count(*) FROM item_tags WHERE tag_id = ?1) + (SELECT count(*) FROM entity_tags WHERE tag_id = ?1)",
                [id],
                |r| r.get(0),
            )?)
        })?)
    }
    pub fn tag_remove(&self, id: &str) -> Result<()> {
        Ok(self.s.tags().remove(id)?)
    }
    pub fn tags_of_item(&self, item: &str) -> Result<Vec<String>> {
        Ok(self.s.tags().of_item(item)?)
    }
    /// Replaces an item's tags; a task item's must hold exactly one category. Returns the ids kept.
    pub fn set_item_tags(&self, item: &str, ids: &[String]) -> Result<Vec<String>> {
        Ok(self.s.tags().set_item(item, ids)?)
    }
    /// The six item categories of `ws` (seeded on first use).
    pub fn kinds(&self, ws: &str) -> Result<Vec<Tag>> {
        Ok(self.s.tags().kinds(ws)?)
    }

    /* ── items ── */
    /// Non-archived task items, most recently updated first.
    pub fn items(&self, ws: &str) -> Result<Vec<Item>> {
        Ok(self.s.items().list(ws, false)?)
    }
    /// All task items, archived included.
    pub fn items_all(&self, ws: &str) -> Result<Vec<Item>> {
        Ok(self.s.items().all(ws)?)
    }
    pub fn item(&self, id: &str) -> Result<Option<Item>> {
        Ok(self.s.items().get(id)?)
    }
    /// Ids of the task items of `ws` at work (`run`, active, not archived), most recently updated first.
    pub fn items_running(&self, ws: &str) -> Result<Vec<String>> {
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            let mut st = c.prepare(
                "SELECT id FROM items WHERE workspace_id = ? AND kind = 'task' AND status = 'run' AND archived = 0
                   AND phase = 'active' ORDER BY updated DESC",
            )?;
            let v = st.query_map([ws], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(v)
        })?)
    }
    pub fn item_create(&self, ws: &str, p: &NewItem) -> Result<Item> {
        self.s
            .items()
            .create(ws, p)?
            .ok_or_else(|| crate::error::msg(steve_i18n::t!("core.item.not_found")))
    }
    /// `items.patch` with a JSON object of column → value (also a stored decision `undo`).
    pub fn item_patch(&self, id: &str, p: Map<String, Value>) -> Result<()> {
        if p.is_empty() {
            return Ok(());
        }
        self.s.items().patch(id, &decode(p)?)?;
        Ok(())
    }
    /// Keeps a plan the agent is replacing; returns its round number.
    pub fn plan_round_add(
        &self,
        ws: &str,
        id: &str,
        steps: &[PlanStep],
        end: &str,
        author: &str,
    ) -> Result<i64> {
        Ok(self.s.items().add_plan_round(ws, id, steps, end, author)?)
    }
    pub fn plan_rounds(&self, id: &str) -> Result<Vec<steve_store::PlanRound>> {
        Ok(self.s.items().plan_rounds(id)?)
    }
    pub fn goals(&self, id: &str) -> Result<Vec<Goal>> {
        Ok(self.s.items().goals(id)?)
    }
    pub fn goal(&self, id: &str) -> Result<String> {
        Ok(self.s.items().goal(id)?)
    }
    pub fn add_goal(&self, id: &str, text: &str) -> Result<i64> {
        Ok(self.s.items().add_goal(id, text)?)
    }
    pub fn deps_of(&self, item: &str) -> Result<Vec<String>> {
        Ok(self.s.deps().of(item)?)
    }
    pub fn dependents(&self, item: &str) -> Result<Vec<String>> {
        Ok(self.s.deps().dependents(item)?)
    }
    pub fn dep_add(&self, item: &str, dep: &str) -> Result<()> {
        Ok(self.s.deps().add(item, dep)?)
    }
    pub fn dep_remove(&self, item: &str, dep: &str) -> Result<()> {
        Ok(self.s.deps().remove(item, dep)?)
    }
    pub fn timeline_add(
        &self,
        item: &str,
        actor: &str,
        kind: &str,
        text: &str,
        session: Option<&str>,
    ) -> Result<()> {
        self.timeline_add_files(item, actor, kind, text, session, &[])
    }
    /// A timeline entry with the attachments the user sent with it.
    pub fn timeline_add_files(
        &self,
        item: &str,
        actor: &str,
        kind: &str,
        text: &str,
        session: Option<&str>,
        files: &[FileRef],
    ) -> Result<()> {
        Ok(self
            .s
            .timeline()
            .add(item, actor, kind, text, session, files)?)
    }
    pub fn timeline(&self, item: &str) -> Result<Vec<TimelineRow>> {
        Ok(self.s.timeline().list(item)?)
    }
    pub fn artifact_add(
        &self,
        item: &str,
        kind: &str,
        title: &str,
        location: Option<&str>,
        role: Option<&str>,
    ) -> Result<()> {
        Ok(self
            .s
            .artifacts()
            .add_as(item, kind, title, location, role)?)
    }

    /* ── sessions, events, queue ── */
    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        Ok(self.s.sessions().get(id)?)
    }
    pub fn session_by_token(&self, token: &str) -> Result<Option<Session>> {
        Ok(self.s.sessions().by_token(token)?)
    }
    pub fn sessions_of_item(&self, item: &str) -> Result<Vec<Session>> {
        Ok(self.s.sessions().of_item(item)?)
    }
    /// Every workspace, most recently updated first (LIMIT 1000).
    pub fn sessions_all(&self) -> Result<Vec<Session>> {
        Ok(self.s.sessions().list(None)?)
    }
    pub fn session_create(&self, p: &NewSession) -> Result<Session> {
        Ok(self.s.sessions().create(p)?)
    }
    pub fn session_patch(&self, id: &str, p: Map<String, Value>) -> Result<()> {
        if p.is_empty() {
            return Ok(());
        }
        Ok(self.s.sessions().patch(id, &decode(p)?)?)
    }
    /// Store a new MCP token hash (orchestrator `rotateToken`).
    pub fn rotate_token(&self, id: &str, token: &str) -> Result<()> {
        Ok(self.s.sessions().rotate_token(id, token)?)
    }
    pub fn event_add(&self, session: &str, kind: &str, data: Value) -> Result<i64> {
        Ok(self.s.events().add(session, kind, data)?)
    }
    pub fn events_after(&self, session: &str, after: i64) -> Result<Vec<Event>> {
        Ok(self.s.events().after(session, after, 5000)?)
    }
    pub fn events_up_to(&self, session: &str, seq: i64) -> Result<Vec<Event>> {
        Ok(self.s.events().up_to(session, seq)?)
    }
    /// `ts` of the session's newest system event carrying `marker` (e.g. `resumed`, `drain_cut`): looked
    /// up directly, however many events the session has.
    pub fn last_marker_ts(&self, session: &str, marker: &str) -> Result<Option<i64>> {
        let path = format!("$.{marker}");
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE session_id = ? AND kind = 'system' AND json_extract(data, ?) = 1
                  ORDER BY seq DESC LIMIT 1",
                rusqlite::params![session, path],
                |r| r.get(0),
            )
            .optional()?)
        })?)
    }
    /// `ts` of the session's newest event.
    pub fn last_event_ts(&self, session: &str) -> Result<Option<i64>> {
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE session_id = ? ORDER BY seq DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?)
        })?)
    }
    /// The text of the session's newest `system` event.
    pub fn last_system_text(&self, session: &str) -> Result<Option<String>> {
        let data: Option<String> = self.s.reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT data FROM events WHERE session_id = ? AND kind = 'system' ORDER BY seq DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        Ok(data
            .and_then(|d| serde_json::from_str::<Value>(&d).ok())
            .and_then(|v| v.get("text").and_then(Value::as_str).map(String::from)))
    }
    /// Any event of the session at or after `ts`.
    pub fn has_events_since(&self, session: &str, ts: i64) -> Result<bool> {
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM events WHERE session_id = ? AND ts >= ? LIMIT 1",
                rusqlite::params![session, ts],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })?)
    }
    pub fn queued(&self, session: &str) -> Result<Vec<Queued>> {
        Ok(self.s.queued().list(session)?)
    }
    pub fn queue_add(&self, session: &str, text: &str, via: &str, files: &[FileRef]) -> Result<()> {
        Ok(self.s.queued().add(session, text, via, files)?)
    }
    /// Queue unless the same message is already waiting; false when nothing was added.
    pub fn queue_add_unless_waiting(
        &self,
        session: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<bool> {
        Ok(self
            .s
            .queued()
            .add_unless_waiting(session, text, via, files)?)
    }
    /// All queued rows of a session, deleted.
    pub fn queue_take(&self, session: &str) -> Result<Vec<Queued>> {
        Ok(self.s.queued().take(session)?)
    }
    /// `(before, after)` of an edited row still in the queue; `None` once a turn took it.
    pub fn queue_edit(
        &self,
        session: &str,
        id: &str,
        text: &str,
    ) -> Result<Option<(Queued, Queued)>> {
        Ok(self.s.queued().edit(session, id, text)?)
    }
    /// The removed row; `None` once a turn took it.
    pub fn queue_remove(&self, session: &str, id: &str) -> Result<Option<Queued>> {
        Ok(self.s.queued().remove(session, id)?)
    }
    /// [`Self::queue_remove`], a message a runner had delivered (`resumed:`) included.
    pub fn queue_remove_any(&self, session: &str, id: &str) -> Result<Option<Queued>> {
        Ok(self.s.queued().remove_any(session, id)?)
    }
    /// Moves a row to the head of the queue; `None` once a turn took it.
    pub fn queue_front(&self, session: &str, id: &str) -> Result<Option<Queued>> {
        Ok(self.s.queued().to_front(session, id)?)
    }
    /// Sessions that are not ended and have queued rows.
    pub fn sessions_with_queue(&self) -> Result<Vec<String>> {
        Ok(self.s.reader::<_, steve_store::StoreError>(|c| {
            let mut st = c.prepare(
                "SELECT DISTINCT q.session_id id FROM queued q JOIN sessions s ON s.id = q.session_id WHERE s.status != 'ended'",
            )?;
            let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(v)
        })?)
    }

    /* ── decisions ── */
    pub fn decisions_open(&self, ws: &str) -> Result<Vec<Decision>> {
        Ok(self.s.decisions().open(ws)?)
    }
    pub fn decision(&self, id: &str) -> Result<Option<Decision>> {
        Ok(self.s.decisions().get(id)?)
    }
    pub fn decisions_of_item(&self, item: &str) -> Result<Vec<Decision>> {
        Ok(self.s.decisions().of_item(item)?)
    }
    pub fn decision_create(&self, p: NewDecision) -> Result<Decision> {
        self.s
            .decisions()
            .create(&p)?
            .ok_or_else(|| crate::error::msg(steve_i18n::t!("core.decision.not_found")))
    }
    /// Resolve an open decision (no-op if it is not open); `undo` is the item patch that reverses it.
    pub fn decision_resolve(
        &self,
        id: &str,
        result: Value,
        status: &str,
        undo: Option<Value>,
    ) -> Result<Option<Decision>> {
        Ok(self
            .s
            .decisions()
            .resolve(id, &result, status, &undo.unwrap_or(Value::Null))?)
    }
    /// Replace the result of a settled decision (an approved release's outcome).
    pub fn decision_set_result(&self, id: &str, result: &Value) -> Result<()> {
        self.s.decisions().set_result(id, result)?;
        Ok(())
    }
    pub fn decision_reopen(&self, id: &str) -> Result<()> {
        self.s.decisions().reopen(id)?;
        Ok(())
    }

    /* ── attachments ── */
    pub fn file(&self, id: &str) -> Result<Option<FileRow>> {
        Ok(self.s.files().get(id)?)
    }
    /// Insert a file row (the bytes are written by the caller); `created` is set by the store.
    pub fn file_create(&self, f: &FileRow) -> Result<()> {
        self.s.files().create(&steve_store::NewFile {
            id: f.id.clone(),
            workspace_id: f.workspace_id.clone(),
            sha256: f.sha256.clone(),
            name: f.name.clone(),
            mime: f.mime.clone(),
            size: f.size,
            created_by: f.created_by.clone(),
        })?;
        Ok(())
    }

    /* ── the main thread ── */
    /// The last 200 messages of the workspace's thread, oldest first.
    pub fn messages(&self, ws: &str) -> Result<Vec<Message>> {
        Ok(self.s.messages().list(ws, "steve", 200)?)
    }
    /// The latest `limit` messages of a space (`steve` or `notify`), oldest first.
    pub fn messages_in(&self, ws: &str, space: &str, limit: i64) -> Result<Vec<Message>> {
        Ok(self.s.messages().list(ws, space, limit)?)
    }
    pub fn message_add_in(
        &self,
        ws: &str,
        space: &str,
        author: &str,
        text: &str,
        item: Option<&str>,
        meta: &Value,
    ) -> Result<String> {
        Ok(self.s.messages().add(ws, author, text, item, meta, space)?)
    }
    pub fn message(&self, id: &str) -> Result<Option<Message>> {
        Ok(self.s.messages().get(id)?)
    }
    pub fn message_add(
        &self,
        ws: &str,
        author: &str,
        text: &str,
        item: Option<&str>,
        meta: Option<&Value>,
    ) -> Result<String> {
        Ok(self.s.messages().add(
            ws,
            author,
            text,
            item,
            meta.unwrap_or(&Value::Null),
            "steve",
        )?)
    }
    pub fn message_patch_meta(&self, id: &str, meta: &Value) -> Result<()> {
        Ok(self.s.messages().patch_meta(id, meta)?)
    }
}
