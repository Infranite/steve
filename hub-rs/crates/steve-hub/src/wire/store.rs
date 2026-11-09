//! [`Repo`] on steve-store: every method is one store call; rows go out through serde, which keeps the
//! repo.ts shapes (steve-store's types are tested against them).

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use steve_store as s;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::backend::RawEvent;
use crate::backend::{
    BResult, Backend, BackendError, BotExtras, ImportedSession, ItemExtras, NewProject, Obj, Repo,
};
use crate::bus::Bus;
use crate::files::NewFile;

/// Store errors keep their user-facing text (`没有这个 Bot`, SQLite's `UNIQUE constraint failed: …`) as a 500.
fn err(e: s::StoreError) -> BackendError {
    match e {
        // a refused write (an item without its one category, a category deleted): the user's conflict
        s::StoreError::Conflict(message) => BackendError {
            status: 409,
            message,
        },
        e => BackendError::msg(e.to_string()),
    }
}

fn obj<T: Serialize>(x: T) -> BResult<Obj> {
    match serde_json::to_value(x).map_err(|e| BackendError::msg(e.to_string()))? {
        Value::Object(o) => Ok(o),
        other => Err(BackendError::msg(format!(
            "expected an object, got {other}"
        ))),
    }
}

fn objs<T: Serialize>(xs: Vec<T>) -> BResult<Vec<Obj>> {
    xs.into_iter().map(obj).collect()
}

fn opt<T: Serialize>(x: Option<T>) -> BResult<Option<Obj>> {
    x.map(obj).transpose()
}

fn patch<T: DeserializeOwned>(p: &Obj) -> BResult<T> {
    serde_json::from_value(Value::Object(p.clone()))
        .map_err(|e| BackendError::msg(format!("patch: {e}")))
}

/// `没有这个工作空间` / `没有这个 Bot` (500), in the request's language.
fn gone(what: &str) -> BackendError {
    BackendError::msg(match what {
        "bot" => crate::api::Noun::Bot.missing(),
        _ => steve_i18n::t!("hub.not_found.workspace"),
    })
}

/// Forwards store change events to the Hub bus (the two enums serialize identically).
pub struct BusPublisher(pub Bus);

impl s::Publisher for BusPublisher {
    fn publish(&self, e: s::ChangeEvent) {
        if let Ok(v) = serde_json::to_value(&e) {
            self.0.publish_value(v);
        }
    }
}

/// The store as the Hub's storage backend. The store locks its write connection per call and serves reads
/// from its pool, so reader and writer are the same handle; steve-core's lock wraps domain operations.
/// `batch` is one extra read-only connection for the few whole-workspace queries that replace the N+1 of
/// `/api/state` (TODO(steve-store): move these into the store's read pool).
#[derive(Clone)]
pub struct StoreBackend(pub s::Store, Arc<Mutex<Vec<rusqlite::Connection>>>);

impl StoreBackend {
    /// A transcript reader for a page of `session` from `after`: clients see Steve's own entries,
    /// the old ACP rows read into them (with what the page's tool updates need from before it).
    fn transcript(&self, session: &str, after: i64) -> BResult<steve_transcript::LegacyReader> {
        let cwd = self
            .0
            .sessions()
            .get(session)
            .map_err(err)?
            .map(|s| s.cwd)
            .unwrap_or_default();
        let mut reader = steve_transcript::LegacyReader::new(&cwd);
        for e in self.0.events().tool_context(session, after).map_err(err)? {
            reader.prime(&e.kind, &e.data);
        }
        Ok(reader)
    }

    pub fn new(store: s::Store) -> BResult<Self> {
        let b = StoreBackend(store, Arc::new(Mutex::new(vec![])));
        // open one now so a broken path fails at boot, not on the first /api/state
        let c = b.open()?;
        b.1.lock().unwrap_or_else(|e| e.into_inner()).push(c);
        Ok(b)
    }

    fn open(&self) -> BResult<rusqlite::Connection> {
        let c = rusqlite::Connection::open_with_flags(
            self.0.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| BackendError::msg(e.to_string()))?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| BackendError::msg(e.to_string()))?;
        Ok(c)
    }

    /// A read-only connection of the batch pool for one closure (grows with concurrency, keeps up to 8).
    fn batch<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> BResult<T> {
        let pooled = self.1.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let c = match pooled {
            Some(c) => c,
            None => self.open()?,
        };
        let r = f(&c).map_err(|e| BackendError::msg(e.to_string()));
        let mut pool = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < 8 {
            pool.push(c);
        }
        r
    }
}

/// Runs `sql` with one text parameter and calls `row` per result row.
fn each(
    c: &rusqlite::Connection,
    sql: &str,
    p: &str,
    mut row: impl FnMut(&rusqlite::Row) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached(sql)?;
    let mut rows = st.query([p])?;
    while let Some(r) = rows.next()? {
        row(r)?;
    }
    Ok(())
}

impl Backend for StoreBackend {
    fn reader(&self) -> BResult<Box<dyn Repo + '_>> {
        Ok(Box::new(self.clone()))
    }
    fn writer(&self) -> BResult<Box<dyn Repo + '_>> {
        Ok(Box::new(self.clone()))
    }
}

fn str_field(o: &Obj, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_string)
}

impl Repo for StoreBackend {
    fn item_extras(&self, ws: &str) -> BResult<Option<HashMap<String, ItemExtras>>> {
        self.batch(|c| {
            let mut m: HashMap<String, ItemExtras> = HashMap::new();
            // same orders as the per-item queries of repo.ts (index order for the PK lookups)
            const ITEMS: &str = "SELECT id FROM items WHERE workspace_id = ?1 AND kind = 'task'";
            each(c, &format!("SELECT item_id, text FROM item_goals WHERE item_id IN ({ITEMS}) ORDER BY item_id, version"), ws, |r| {
                let x = m.entry(r.get(0)?).or_default();
                x.goal = r.get(1)?;
                x.goals += 1;
                Ok(())
            })?;
            each(c, &format!("SELECT it.item_id, it.tag_id FROM item_tags it LEFT JOIN tags t ON t.id = it.tag_id WHERE it.item_id IN ({ITEMS}) ORDER BY it.item_id, coalesce(t.cat = '类型', 0) DESC, it.rowid"), ws, |r| {
                m.entry(r.get(0)?).or_default().tags.push(r.get(1)?);
                Ok(())
            })?;
            each(c, &format!("SELECT item_id, dep_id FROM item_deps WHERE item_id IN ({ITEMS}) ORDER BY item_id, created, dep_id"), ws, |r| {
                m.entry(r.get(0)?).or_default().deps.push(r.get(1)?);
                Ok(())
            })?;
            each(c, &format!("SELECT item_id, id, status FROM sessions WHERE item_id IN ({ITEMS}) ORDER BY item_id, no, rowid"), ws, |r| {
                m.entry(r.get(0)?).or_default().sessions.push((r.get(1)?, r.get(2)?));
                Ok(())
            })?;
            each(c, &format!("SELECT item_id, id FROM decisions WHERE status = 'open' AND item_id IN ({ITEMS}) ORDER BY item_id, created, rowid"), ws, |r| {
                m.entry(r.get(0)?).or_default().open.push(r.get(1)?);
                Ok(())
            })?;
            let mut counts: HashMap<String, usize> = HashMap::new();
            each(c, &format!("SELECT q.session_id, count(*) FROM queued q JOIN sessions s ON s.id = q.session_id WHERE s.item_id IN ({ITEMS}) GROUP BY q.session_id"), ws, |r| {
                counts.insert(r.get(0)?, r.get::<_, i64>(1)? as usize);
                Ok(())
            })?;
            for x in m.values_mut() {
                if let Some((sid, _)) = x.sessions.iter().rev().find(|(_, st)| st != "ended") {
                    x.queued = counts.get(sid).copied().unwrap_or(0);
                }
            }
            Ok(Some(m))
        })
    }
    fn queued_by_session(&self, ws: &str) -> BResult<Option<HashMap<String, Vec<Obj>>>> {
        self.batch(|c| {
            let mut m: HashMap<String, Vec<Obj>> = HashMap::new();
            // the columns of steve-store's `Queued`, in its order (`files` JSON, default [])
            each(
                c,
                "SELECT id, session_id, text, via, created, files FROM queued WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id = ?1 ORDER BY updated DESC LIMIT 300) ORDER BY created, rowid",
                ws,
                |r| {
                    let sid: String = r.get(1)?;
                    let mut o = Obj::new();
                    o.insert("id".into(), Value::from(r.get::<_, String>(0)?));
                    o.insert("session_id".into(), Value::from(sid.clone()));
                    o.insert("text".into(), Value::from(r.get::<_, String>(2)?));
                    o.insert("via".into(), Value::from(r.get::<_, String>(3)?));
                    o.insert("created".into(), Value::from(r.get::<_, i64>(4)?));
                    let files = r
                        .get::<_, Option<String>>(5)?
                        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                        .filter(|v| v.is_array())
                        .unwrap_or_else(|| Value::Array(vec![]));
                    o.insert("files".into(), files);
                    m.entry(sid).or_default().push(o);
                    Ok(())
                },
            )?;
            Ok(Some(m))
        })
    }
    fn events_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        let mut reader = self.transcript(session, after)?;
        self.0
            .events()
            .visit_legacy_page(session, after, |e| {
                // Value serialization is infallible; use the repository's transparent hydration.
                let x = reader.read_row(&e.kind, &e.data);
                each((
                    e.seq,
                    e.ts,
                    x.kind,
                    serde_json::to_string(&x.data).expect("JSON event"),
                ));
            })
            .map_err(err)?;
        Ok(true)
    }

    fn events_page_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        let mut reader = self.transcript(session, after)?;
        self.0
            .events()
            .visit_page(session, after, |e| {
                // Value serialization is infallible; use the repository's transparent hydration.
                let x = reader.read_row(&e.kind, &e.data);
                each((
                    e.seq,
                    e.ts,
                    x.kind,
                    serde_json::to_string(&x.data).expect("JSON event"),
                ));
            })
            .map_err(err)?;
        Ok(true)
    }

    fn bot_extras(&self, ws: &str) -> BResult<Option<BotExtras>> {
        self.batch(|c| {
            let mut x = BotExtras::default();
            let mut st = c.prepare_cached(
                "SELECT bot_id, workspace_id FROM bot_workspaces ORDER BY bot_id, workspace_id",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                x.workspaces.entry(r.get(0)?).or_default().push(r.get(1)?);
            }
            each(
                c,
                "SELECT bot_id, count(*) FROM bot_memories WHERE workspace_id = ?1 GROUP BY bot_id",
                ws,
                |r| {
                    x.memories.insert(r.get(0)?, r.get(1)?);
                    Ok(())
                },
            )?;
            Ok(Some(x))
        })
    }

    fn ensure_workspace(&self) -> BResult<()> {
        self.0.ensure_workspace().map_err(err)
    }
    fn workspace_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.workspaces().get(id).map_err(err)?)
    }
    fn workspaces_of_user(&self, user: &str) -> BResult<Vec<Obj>> {
        objs(self.0.workspaces().of_user(user).map_err(err)?)
    }
    fn workspaces_all(&self) -> BResult<Vec<Obj>> {
        objs(self.0.workspaces().all().map_err(err)?)
    }
    fn workspace_create(
        &self,
        name: &str,
        description: Option<&str>,
        item_prefix: Option<&str>,
        owner: &str,
    ) -> BResult<Obj> {
        let p = s::NewWorkspace {
            name: name.into(),
            description: description.map(Into::into),
            item_prefix: item_prefix.map(Into::into),
        };
        obj(self.0.workspaces().create(&p, owner).map_err(err)?)
    }
    fn workspace_patch(&self, id: &str, p: &Obj) -> BResult<Obj> {
        self.0
            .workspaces()
            .patch(id, &patch(p)?)
            .map_err(err)?
            .map(obj)
            .unwrap_or_else(|| Err(gone("workspace")))
    }
    fn workspace_is_member(&self, id: &str, user: &str) -> BResult<bool> {
        self.0.workspaces().is_member(id, user).map_err(err)
    }
    fn user_by_session(&self, token: &str) -> BResult<Option<Obj>> {
        opt(self.0.auth_sessions().user(token).map_err(err)?)
    }
    fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        detail: &Value,
        ws: &str,
    ) -> BResult<()> {
        self.0.audit(actor, action, target, detail, ws).map_err(err)
    }

    fn settings_all(&self, ws: &str) -> BResult<Obj> {
        self.0.settings().all(ws).map_err(err)
    }
    fn settings_get(&self, ws: &str, key: &str) -> BResult<Option<Value>> {
        self.0
            .settings()
            .get::<Option<Value>>(ws, key, None)
            .map_err(err)
    }
    fn settings_set(&self, ws: &str, values: &Obj, author: &str) -> BResult<()> {
        self.0.settings().set(ws, values, author).map_err(err)
    }

    fn projects_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.projects().list(ws, false).map_err(err)?)
    }
    fn project_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.projects().get(id).map_err(err)?)
    }
    fn project_by_path(&self, ws: &str, path: &str) -> BResult<Option<Obj>> {
        opt(self.0.projects().by_path(ws, path).map_err(err)?)
    }
    fn project_create(&self, ws: &str, p: &NewProject) -> BResult<Obj> {
        let release = p
            .release
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| BackendError::msg(e.to_string()))?;
        let np = s::NewProject {
            name: p.name.clone(),
            description: p.description.clone(),
            device_id: p.device_id.clone(),
            path: p.path.clone(),
            kind: p.kind.clone(),
            worktree: p.worktree,
            default_bot: p.default_bot.clone(),
            release,
        };
        obj(self.0.projects().create(ws, &np).map_err(err)?)
    }
    fn project_patch(&self, id: &str, p: &Obj, author: &str) -> BResult<Obj> {
        obj(self
            .0
            .projects()
            .patch(id, &patch(p)?, author)
            .map_err(err)?)
    }

    fn bots_list(&self, ws: Option<&str>) -> BResult<Vec<Obj>> {
        objs(self.0.bots().list(ws).map_err(err)?)
    }
    fn bot_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.bots().get(id).map_err(err)?)
    }
    fn bot_workspaces(&self, id: &str) -> BResult<Vec<String>> {
        Ok(self
            .0
            .bots()
            .workspaces(id)
            .map_err(err)?
            .into_iter()
            .map(|w| w.workspace_id)
            .collect())
    }
    fn bot_in_workspace(&self, id: &str, ws: &str) -> BResult<bool> {
        self.0.bots().in_workspace(id, ws).map_err(err)
    }
    fn bot_set_workspaces(&self, id: &str, ws: &[String]) -> BResult<()> {
        self.0.bots().set_workspaces(id, ws).map_err(err)
    }
    fn bot_create(&self, ws: &str, f: &Obj, author: &str) -> BResult<Obj> {
        let get = |k: &str| f.get(k).filter(|v| !v.is_null()).cloned();
        let nb = s::NewBot {
            name: str_field(f, "name").unwrap_or_default(),
            harness: str_field(f, "harness")
                .unwrap_or_else(|| steve_acp::adapters::DEFAULT_ADAPTER.0.into()),
            role: str_field(f, "role"),
            model: str_field(f, "model"),
            reasoning_effort: str_field(f, "reasoning_effort"),
            rules: str_field(f, "rules"),
            persona: str_field(f, "persona"),
            proactive: f.get("proactive").and_then(Value::as_i64),
            accept_mode: str_field(f, "accept_mode"),
            perm_mode: str_field(f, "perm_mode"),
            avatar: get("avatar")
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| BackendError::msg(e.to_string()))?,
            default_project: str_field(f, "default_project"),
        };
        self.0
            .bots()
            .create(ws, &nb, author)
            .map_err(err)?
            .map(obj)
            .unwrap_or_else(|| Err(gone("bot")))
    }
    fn bot_patch(&self, id: &str, p: &Obj, author: &str) -> BResult<Obj> {
        self.0
            .bots()
            .patch(id, &patch(p)?, author)
            .map_err(err)?
            .map(obj)
            .unwrap_or_else(|| Err(gone("bot")))
    }
    fn bot_restore(&self, id: &str) -> BResult<()> {
        self.0.bots().restore(id).map_err(err)
    }

    fn memory_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.memories().get(id).map_err(err)?)
    }
    fn memories_list(&self, bot: &str, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.memories().list(bot, ws).map_err(err)?)
    }
    fn memories_count(&self, bot: &str, ws: &str) -> BResult<i64> {
        self.0.memories().count(bot, ws).map_err(err)
    }
    fn memory_add(&self, bot: &str, ws: &str, text: &str, source: &str) -> BResult<String> {
        self.0.memories().add(bot, ws, text, source).map_err(err)
    }
    fn memory_edit(&self, id: &str, text: &str) -> BResult<()> {
        self.0.memories().edit(id, text).map_err(err)
    }
    fn memory_remove(&self, id: &str) -> BResult<()> {
        self.0.memories().remove(id).map_err(err)
    }
    fn memories_clear(&self, ws: &str) -> BResult<()> {
        self.0.memories().clear(ws).map_err(err)
    }
    fn memories_delete_of_bot(&self, bot: &str) -> BResult<()> {
        self.0.memories().remove_of_bot(bot).map_err(err)
    }

    fn tags_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.tags().list(ws).map_err(err)?)
    }
    fn tag_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.tags().get(id).map_err(err)?)
    }
    fn tag_by_name(&self, ws: &str, name: &str) -> BResult<Option<Obj>> {
        opt(self.0.tags().by_name(ws, name).map_err(err)?)
    }
    fn tag_create(
        &self,
        ws: &str,
        name: &str,
        cat: &str,
        description: &str,
        by: &str,
    ) -> BResult<Obj> {
        let t = s::NewTag {
            name: name.into(),
            cat: cat.into(),
            description: description.into(),
        };
        obj(self.0.tags().create(ws, &t, by).map_err(err)?)
    }
    fn tag_patch(&self, id: &str, p: &Obj) -> BResult<Option<Obj>> {
        opt(self.0.tags().patch(id, &patch(p)?).map_err(err)?)
    }
    fn tag_remove(&self, id: &str) -> BResult<()> {
        self.0.tags().remove(id).map_err(err)
    }
    fn tag_merge(&self, from: &str, into: &str) -> BResult<()> {
        self.0.tags().merge(from, into).map_err(err)
    }
    fn tag_uses(&self, ws: &str) -> BResult<Obj> {
        Ok(self
            .0
            .tags()
            .uses(ws)
            .map_err(err)?
            .into_iter()
            .map(|(k, n)| (k, Value::from(n)))
            .collect::<Map<_, _>>())
    }
    fn tags_of_item(&self, item: &str) -> BResult<Vec<String>> {
        self.0.tags().of_item(item).map_err(err)
    }
    fn tags_set_item(&self, item: &str, tags: &[String]) -> BResult<Vec<String>> {
        self.0.tags().set_item(item, tags).map_err(err)
    }

    fn items_all(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().all(ws).map_err(err)?)
    }
    fn items_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().list(ws, false).map_err(err)?)
    }
    fn item_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.items().get(id).map_err(err)?)
    }
    fn item_patch(&self, id: &str, p: &Obj) -> BResult<Option<Obj>> {
        opt(self.0.items().patch(id, &patch(p)?).map_err(err)?)
    }
    fn item_remove(&self, id: &str) -> BResult<()> {
        self.0.items().remove(id).map_err(err)
    }
    fn item_goals(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().goals(id).map_err(err)?)
    }
    fn item_plan_rounds(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().plan_rounds(id).map_err(err)?)
    }
    fn item_cycles(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().cycles(id).map_err(err)?)
    }
    fn item_goal(&self, id: &str) -> BResult<String> {
        self.0.items().goal(id).map_err(err)
    }
    fn deps_of(&self, item: &str) -> BResult<Vec<String>> {
        self.0.deps().of(item).map_err(err)
    }
    fn deps_dependents(&self, item: &str) -> BResult<Vec<String>> {
        self.0.deps().dependents(item).map_err(err)
    }
    fn thread_item(&self, item: &str) -> BResult<Option<Value>> {
        Ok(self
            .0
            .thread()
            .item(item)
            .map_err(err)?
            .map(|t| serde_json::json!({"follow": t.follow, "origin_msg": t.origin_msg})))
    }
    fn timeline_add(
        &self,
        item: &str,
        actor: &str,
        kind: &str,
        text: &str,
        session: Option<&str>,
    ) -> BResult<()> {
        self.0
            .timeline()
            .add(item, actor, kind, text, session, &[])
            .map_err(err)
    }
    fn timeline_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.timeline().list(item).map_err(err)?)
    }
    fn artifacts_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.artifacts().list(item).map_err(err)?)
    }
    fn artifact_roles(&self, item: &str) -> BResult<Obj> {
        Ok(self
            .0
            .artifacts()
            .roles(item)
            .map_err(err)?
            .into_iter()
            .map(|(id, role)| (id, Value::String(role)))
            .collect())
    }
    fn annotations_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.annotations().list(item).map_err(err)?)
    }
    fn annotation_add(
        &self,
        item: &str,
        art: &str,
        path: &str,
        anchor: &Value,
        note: &str,
    ) -> BResult<Obj> {
        obj(self
            .0
            .annotations()
            .add(item, art, path, anchor, note)
            .map_err(err)?)
    }
    fn annotation_edit(&self, item: &str, id: &str, note: &str) -> BResult<Option<Obj>> {
        opt(self.0.annotations().edit(item, id, note).map_err(err)?)
    }
    fn annotation_remove(&self, item: &str, id: &str) -> BResult<bool> {
        self.0.annotations().remove(item, id).map_err(err)
    }
    fn annotations_claim(
        &self,
        item: &str,
        ids: &[String],
        key: &str,
    ) -> BResult<(bool, Vec<Obj>)> {
        let c = self.0.annotations().claim(item, ids, key).map_err(err)?;
        Ok((c.again, objs(c.taken)?))
    }
    fn annotations_settle(
        &self,
        item: &str,
        key: &str,
        sent: bool,
        timeline_id: Option<i64>,
    ) -> BResult<()> {
        self.0
            .annotations()
            .settle(item, key, sent, timeline_id)
            .map_err(err)
    }

    fn session_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.sessions().get(id).map_err(err)?)
    }
    fn sessions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.sessions().of_item(item).map_err(err)?)
    }
    fn sessions_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.sessions().list(Some(ws)).map_err(err)?)
    }
    fn session_patch(&self, id: &str, p: &Obj) -> BResult<()> {
        self.0.sessions().patch(id, &patch(p)?).map_err(err)
    }
    fn sessions_search(&self, ws: &str, q: &str) -> BResult<Vec<String>> {
        self.0.sessions().search(ws, q).map_err(err)
    }
    fn events_add(&self, session: &str, kind: &str, data: &Value) -> BResult<i64> {
        self.0
            .events()
            .add(session, kind, data.clone())
            .map_err(err)
    }
    fn events_after(&self, session: &str, after: i64) -> BResult<Vec<Obj>> {
        let mut reader = self.transcript(session, after)?;
        let mut out = vec![];
        for e in self.0.events().after(session, after, 5000).map_err(err)? {
            let x = reader.read_row(&e.kind, &e.data);
            out.push(steve_store::Event {
                seq: e.seq,
                ts: e.ts,
                kind: x.kind,
                data: x.data,
            });
        }
        objs(out)
    }
    fn queued_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.queued().get(id).map_err(err)?)
    }
    fn queued_list(&self, session: &str) -> BResult<Vec<Obj>> {
        objs(self.0.queued().list(session).map_err(err)?)
    }
    fn queued_drop(&self, id: &str) -> BResult<()> {
        self.0.queued().drop(id).map_err(err)
    }
    fn import_keys(&self) -> BResult<Vec<String>> {
        Ok(self
            .0
            .sessions()
            .import_keys()
            .map_err(err)?
            .into_iter()
            .collect())
    }
    fn session_insert_imported(&self, p: &ImportedSession) -> BResult<String> {
        let x = s::ImportedSession {
            workspace_id: p.workspace_id.clone(),
            harness: p.harness.clone(),
            cwd: p.cwd.clone(),
            title: p.title.clone(),
            source: p.source.clone(),
            import_key: p.import_key.clone(),
            // TODO(steve-store): Node stores mtimeMs with its fraction; the store column type is i64 here
            created: p.created as i64,
        };
        self.0.sessions().insert_imported(&x).map_err(err)
    }
    fn session_delete(&self, id: &str) -> BResult<()> {
        self.0.sessions().remove(id).map_err(err)
    }

    fn file_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.files().get(id).map_err(err)?)
    }
    fn file_same(&self, ws: &str, sha256: &str, name: &str) -> BResult<Option<Obj>> {
        opt(self.0.files().same(ws, sha256, name).map_err(err)?)
    }
    fn file_create(&self, f: &NewFile) -> BResult<Obj> {
        let n = s::NewFile {
            id: f.id.clone(),
            workspace_id: f.workspace_id.clone(),
            sha256: f.sha256.clone(),
            name: f.name.clone(),
            mime: f.mime.clone(),
            size: f.size as i64,
            created_by: f.created_by.clone(),
        };
        obj(self.0.files().create(&n).map_err(err)?)
    }

    fn decisions_open(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.decisions().open(ws).map_err(err)?)
    }
    fn decisions_recent(&self, ws: &str, since: i64) -> BResult<Vec<Obj>> {
        objs(self.0.decisions().recent(ws, since).map_err(err)?)
    }
    fn decision_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.decisions().get(id).map_err(err)?)
    }
    fn decisions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.decisions().of_item(item).map_err(err)?)
    }
    fn messages_in(&self, ws: &str, space: &str, limit: i64) -> BResult<Vec<Obj>> {
        objs(self.0.messages().list(ws, space, limit).map_err(err)?)
    }
    fn messages_before(
        &self,
        ws: &str,
        space: &str,
        before: i64,
        after: i64,
        limit: i64,
    ) -> BResult<Vec<Obj>> {
        objs(
            self.0
                .messages()
                .list_before(ws, space, before, after, limit)
                .map_err(err)?,
        )
    }
    fn messages_search(&self, ws: &str, space: &str, q: &str, limit: i64) -> BResult<Vec<Obj>> {
        objs(self.0.messages().search(ws, space, q, limit).map_err(err)?)
    }
    fn message_add(
        &self,
        ws: &str,
        author: &str,
        text: &str,
        item: Option<&str>,
        meta: &Value,
        space: &str,
    ) -> BResult<String> {
        self.0
            .messages()
            .add(ws, author, text, item, meta, space)
            .map_err(err)
    }
    fn setting_raw(&self, ws: &str, key: &str) -> BResult<Option<String>> {
        use rusqlite::OptionalExtension;
        self.0
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
                    [ws, key],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
            })
            .map_err(err)
    }
    fn node_env_list(&self, me: &str, ws: &str, node: &str, principal: &str) -> BResult<Value> {
        use super::node_env as ne;
        self.0
            .with_conn(|c| {
                if let Some(why) = ne::refusal(c, me, ws, node, principal)? {
                    return Ok(Err(why));
                }
                Ok(Ok(ne::list(c, node, principal)?))
            })
            .map_err(err)?
            .map(|rows| serde_json::json!({ "node": node, "principal": principal, "env": rows }))
            .map_err(|why| BackendError {
                status: 403,
                message: why,
            })
    }
    fn node_env_put(
        &self,
        me: &str,
        ws: &str,
        node: &str,
        principal: &str,
        key: &str,
        value: Option<&str>,
        note: &str,
    ) -> BResult<()> {
        use super::node_env as ne;
        if let Some(v) = value
            && let Some(why) = ne::value_refusal(key, v)
        {
            return Err(BackendError {
                status: 400,
                message: why,
            });
        }
        self.0
            .with_conn(|c| {
                if let Some(why) = ne::refusal(c, me, ws, node, principal)? {
                    return Ok(Err(why));
                }
                match value {
                    Some(v) => ne::put(c, node, principal, key, v, note, me)?,
                    None => {
                        ne::delete(c, node, principal, key)?;
                    }
                }
                Ok(Ok(()))
            })
            .map_err(err)?
            .map_err(|why| BackendError {
                status: 403,
                message: why,
            })
    }
    fn setting_put_raw(&self, ws: &str, key: &str, value: &str) -> BResult<()> {
        let t = steve_store::now();
        self.0
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                    rusqlite::params![ws, key, value, t],
                )?;
                Ok(())
            })
            .map_err(err)
    }
    fn devices_list(&self) -> BResult<Vec<Obj>> {
        objs(self.0.devices().list().map_err(err)?)
    }
    fn device_upsert(
        &self,
        id: &str,
        name: &str,
        roles: &[String],
        os: &str,
        version: &str,
    ) -> BResult<()> {
        let d = s::DeviceInfo {
            id: id.into(),
            name: name.into(),
            roles: roles.to_vec(),
            os: os.into(),
            version: version.into(),
        };
        self.0.devices().upsert(&d).map_err(err)
    }
    fn device_patch(&self, id: &str, p: &Obj) -> BResult<()> {
        self.0.devices().patch(id, &patch(p)?).map_err(err)
    }
}

#[cfg(test)]
mod event_page_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalized_events_are_transparent_and_bounded_pages_are_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = s::Store::open(dir.path().join("test.sqlite3"), s::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let id = store
            .sessions()
            .create(&s::NewSession {
                workspace_id: s::WS.into(),
                harness: "fake".into(),
                cwd: "/tmp".into(),
                title: "paging".into(),
                token: "fake-token".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        for i in 0..501 {
            store
                .events()
                .add(&id, "agent_message_chunk", json!({"text":i.to_string()}))
                .unwrap();
        }
        let backend = StoreBackend::new(store.clone()).unwrap();
        let mut legacy = Vec::new();
        assert!(backend.events_raw(&id, 0, &mut |e| legacy.push(e)).unwrap());
        assert_eq!(legacy.len(), 501);
        let mut bounded = Vec::new();
        assert!(
            backend
                .events_page_raw(&id, 0, &mut |e| bounded.push(e))
                .unwrap()
        );
        assert_eq!(bounded.len(), 500);
        let mut tail = Vec::new();
        backend
            .events_page_raw(&id, bounded.last().unwrap().0, &mut |e| tail.push(e))
            .unwrap();
        assert_eq!(tail.len(), 1);
        let data = json!({"content":[{"type":"image","data":"base64".repeat(20_000)}]});
        store
            .events()
            .add(&id, "tool_call_update", data.clone())
            .unwrap();
        let mut full = Vec::new();
        backend
            .events_raw(&id, tail[0].0, &mut |e| full.push(e))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&full[0].3).unwrap(), data);
    }

    #[test]
    fn old_acp_rows_read_as_transcript_entries_even_from_the_middle() {
        let dir = tempfile::tempdir().unwrap();
        let store = s::Store::open(dir.path().join("test.sqlite3"), s::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let id = store
            .sessions()
            .create(&s::NewSession {
                workspace_id: s::WS.into(),
                harness: "claude-code".into(),
                cwd: "/w".into(),
                title: "old".into(),
                token: "t".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let ev = |kind: &str, data: Value| store.events().add(&id, kind, data).unwrap();
        ev("user", json!({"text": "hi", "via": "me"}));
        ev(
            "agent_message_chunk",
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}}),
        );
        let call = ev(
            "tool_call",
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read /w/a.rs", "kind": "read", "status": "pending",
                "rawInput": {"file_path": "/w/a.rs"}, "_meta": {"claudeCode": {"toolName": "Read"}}}),
        );
        ev(
            "tool_call_update",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}),
        );
        let backend = StoreBackend::new(store.clone()).unwrap();
        let read = |after: i64| {
            let mut out = Vec::new();
            backend
                .events_raw(&id, after, &mut |e| out.push(e))
                .unwrap();
            out.into_iter()
                .map(|(_, _, k, d)| (k, serde_json::from_str::<Value>(&d).unwrap()))
                .collect::<Vec<_>>()
        };
        let all = read(0);
        let kinds: Vec<&str> = all.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["user", "text", "tool", "tool"]);
        assert_eq!(all[1].1["role"], "assistant");
        assert_eq!(all[3].1["tool"]["name"], "Read");
        assert_eq!(all[3].1["tool"]["target"], "a.rs");
        assert!(
            !all.iter()
                .any(|(_, d)| d.to_string().contains("sessionUpdate"))
        );
        // a page that starts after the call still knows the tool
        let tail = read(call);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].1["tool"]["name"], "Read");
        assert_eq!(tail[0].1["tool"]["phase"], "succeeded");
    }
}
