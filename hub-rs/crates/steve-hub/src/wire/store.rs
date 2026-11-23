//! [`Repo`] on steve-store: every method is one store call; rows go out through serde, which keeps the
//! repo.ts shapes (steve-store's types are tested against them).

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use steve_store as s;

use std::collections::HashMap;

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

/// Forwards one committed typed batch without changing its order or revision boundary.
pub struct BusPublisher(pub Bus);

impl s::Publisher for BusPublisher {
    fn publish_batch(&self, events: Vec<s::BusEvent>) {
        self.0.publish_batch(events);
    }
}

/// The Hub adapter for storage reads and writes.
#[derive(Clone)]
pub struct StoreBackend(pub s::Store);

impl Backend for StoreBackend {
    fn reader(&self) -> BResult<Box<dyn Repo + '_>> {
        Ok(Box::new(self.clone()))
    }
    fn writer(&self) -> BResult<Box<dyn Repo + '_>> {
        Ok(Box::new(self.clone()))
    }
}

fn mcp_value(server: s::state_batches::McpSummary) -> BResult<Value> {
    serde_json::to_value(server).map_err(|e| BackendError::msg(e.to_string()))
}

fn str_field(o: &Obj, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_string)
}

impl Repo for StoreBackend {
    fn item_extras(&self, ws: &str) -> BResult<Option<HashMap<String, ItemExtras>>> {
        self.0
            .read(|r| {
                s::state_batches::item_extras(r, ws, steve_core::pull::Closeout::from_evidence)
            })
            .map(Some)
            .map_err(err)
    }

    fn item_closeout(&self, id: &str) -> BResult<Option<steve_core::pull::Closeout>> {
        self.0
            .read(|r| {
                s::state_batches::item_closeout(r, id, steve_core::pull::Closeout::from_evidence)
            })
            .map_err(err)
    }
    fn queued_by_session(&self, ws: &str) -> BResult<Option<HashMap<String, Vec<Obj>>>> {
        let grouped = self
            .0
            .read(|r| s::state_batches::queued_by_session(r, ws))
            .map_err(err)?;
        grouped
            .into_iter()
            .map(|(sid, rows)| Ok((sid, objs(rows)?)))
            .collect::<BResult<_>>()
            .map(Some)
    }

    fn events_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        self.events_page_raw(session, after, each)
    }

    fn events_page_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        self.0
            .events()
            .visit_page(session, after, |e| {
                // Value serialization is infallible; use the repository's transparent hydration.
                each((
                    e.seq,
                    e.ts,
                    e.kind,
                    serde_json::to_string(&e.data).expect("JSON event"),
                ));
            })
            .map_err(err)?;
        Ok(true)
    }

    fn bot_extras(&self, ws: &str) -> BResult<Option<BotExtras>> {
        let extras = self
            .0
            .read(|r| s::state_batches::bot_extras(r, ws))
            .map_err(err)?;
        let mcp = extras
            .mcp
            .into_iter()
            .map(|(bot, servers)| {
                let values = servers.into_iter().map(mcp_value).collect::<BResult<_>>()?;
                Ok((bot, values))
            })
            .collect::<BResult<_>>()?;
        Ok(Some(BotExtras {
            workspaces: extras.workspaces,
            memories: extras.memories,
            mcp,
        }))
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
    fn audit_page(
        &self,
        ws: &str,
        reader: &str,
        before: Option<i64>,
        limit: i64,
    ) -> BResult<Value> {
        let page = self.0.audit_page(ws, reader, before, limit).map_err(err)?;
        serde_json::to_value(page).map_err(|e| BackendError::msg(e.to_string()))
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
    fn bot_mcp_servers(&self, id: &str) -> BResult<Vec<Value>> {
        self.0
            .read(|r| s::state_batches::bot_mcp(r, id))
            .map_err(err)?
            .into_iter()
            .map(mcp_value)
            .collect()
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
                .unwrap_or_else(|| steve_harness_spec::DEFAULT_ADAPTER.0.into()),
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
    fn bot_patch_checked(
        &self,
        id: &str,
        p: &Obj,
        author: &str,
        harnesses: &[steve_core::harnesses::Harness],
    ) -> BResult<Obj> {
        self.0
            .atomic(|| -> anyhow::Result<Obj> {
                let b = self.bot_get(id)?.ok_or_else(|| gone("bot"))?;
                steve_core::models::validate_bot_patch(Some(&b), p, harnesses).map_err(
                    |message| BackendError {
                        status: 400,
                        message,
                    },
                )?;
                Ok(self.bot_patch(id, p, author)?)
            })
            .map_err(|e| match e.downcast::<BackendError>() {
                Ok(e) => e,
                Err(e) => BackendError::msg(e.to_string()),
            })
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
    fn item_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.items().get(id).map_err(err)?)
    }
    fn item_patch(&self, id: &str, p: &Obj) -> BResult<Option<Obj>> {
        opt(self.0.items().patch(id, &patch(p)?).map_err(err)?)
    }
    fn item_set_design_gate(&self, id: &str, on: bool) -> BResult<Option<Obj>> {
        opt(self.0.items().set_design_gate(id, on).map_err(err)?)
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
        let mut out = vec![];
        self.0
            .events()
            .visit_page(session, after, |e| out.push(e))
            .map_err(err)?;
        objs(out)
    }
    fn queued_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.queued().get(id).map_err(err)?)
    }
    fn queued_list(&self, session: &str) -> BResult<Vec<Obj>> {
        objs(self.0.queued().list(session).map_err(err)?)
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
        self.0
            .read(|r| steve_store::WorkspaceSettings(r).get(ws, key))
            .map_err(err)
    }
    fn node_env_list(&self, me: &str, ws: &str, node: &str, principal: &str) -> BResult<Value> {
        use super::node_env as ne;
        use steve_store::node_environment as env;
        self.0
            .read::<_, steve_store::StoreError>(|c| {
                if let Some(why) = ne::refusal(c, me, ws, node, principal)? {
                    return Ok(Err(why));
                }
                Ok(Ok(env::list(c, node, principal)?))
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
        use steve_store::node_environment as env;
        if let Some(v) = value
            && let Some(why) = ne::value_refusal(key, v)
        {
            return Err(BackendError {
                status: 400,
                message: why,
            });
        }
        self.0
            .write::<_, steve_store::StoreError>(|c| {
                if let Some(why) = ne::refusal(c, me, ws, node, principal)? {
                    return Ok(Err(why));
                }
                match value {
                    Some(v) => env::put(
                        c,
                        env::Change {
                            node,
                            principal,
                            key,
                            value: v,
                            note,
                            actor: me,
                        },
                        steve_store::now(),
                    )?,
                    None => {
                        env::delete(c, node, principal, key)?;
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
        self.0
            .write(|w| steve_store::WorkspaceSettings(w).set(ws, key, value, steve_store::now()))
            .map_err(err)
    }
    fn devices_list(&self) -> BResult<Vec<Obj>> {
        use steve_store::runtime::{HUB_DEVICE, HUB_NODE, session_room};
        let mut out = objs(self.0.devices().list().map_err(err)?)?;
        // the Hub's machine runs the Hub's own Node: how many of its turns run and wait, against
        // the limit on this row
        let room = self.0.reader(|c| session_room(c, HUB_NODE)).map_err(err)?;
        for d in out
            .iter_mut()
            .filter(|d| d.get("id").and_then(Value::as_str) == Some(HUB_DEVICE))
        {
            d.insert("running".into(), serde_json::json!(room.running));
            d.insert("waiting".into(), serde_json::json!(room.waiting));
        }
        Ok(out)
    }
    fn waiting_for_room(&self) -> BResult<HashMap<String, Value>> {
        let rows = self
            .0
            .reader(steve_store::runtime::sessions_waiting_for_room)
            .map_err(err)?;
        rows.into_iter()
            .map(|(s, r)| {
                serde_json::to_value(r)
                    .map(|v| (s, v))
                    .map_err(|e| BackendError::msg(e.to_string()))
            })
            .collect()
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
    fn transcript_entries_are_transparent_and_all_readers_use_bounded_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = s::Store::open(dir.path().join("steve.sqlite3"), s::NoPublisher).unwrap();
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
                .add(
                    &id,
                    "text",
                    json!({"role":"assistant", "text":i.to_string()}),
                )
                .unwrap();
        }
        let backend = StoreBackend(store.clone());
        let mut raw = Vec::new();
        assert!(backend.events_raw(&id, 0, &mut |e| raw.push(e)).unwrap());
        assert_eq!(raw.len(), 500);
        let mut bounded = Vec::new();
        assert!(
            backend
                .events_page_raw(&id, 0, &mut |e| bounded.push(e))
                .unwrap()
        );
        assert_eq!(bounded.len(), 500);
        assert_eq!(raw, bounded);
        let decoded = backend.events_after(&id, 0).unwrap();
        assert_eq!(decoded.len(), bounded.len());
        assert_eq!(decoded[0]["kind"], bounded[0].2);
        assert_eq!(
            decoded[0]["data"],
            serde_json::from_str::<Value>(&bounded[0].3).unwrap()
        );
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
    fn unsupported_formats_are_visible_and_keep_the_page_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = s::Store::open(dir.path().join("steve.sqlite3"), s::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let id = store
            .sessions()
            .create(&s::NewSession {
                workspace_id: s::WS.into(),
                harness: "fake".into(),
                cwd: "/w".into(),
                title: "formats".into(),
                token: "t".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        s::testkit::hub::state::unsupported_events(&store, &id, &["legacy.acp", "future.v2"])
            .unwrap();
        let last = store
            .events()
            .add(&id, "text", json!({"role":"assistant", "text":"current"}))
            .unwrap();
        let backend = StoreBackend(store);
        let entries = backend.events_after(&id, 0).unwrap();
        for (entry, format) in entries.iter().zip(["legacy.acp", "future.v2"]) {
            assert_eq!(entry["kind"], "system");
            assert_eq!(entry["data"]["code"], "unsupported_format");
            assert_eq!(entry["data"]["format"], format);
            assert!(!entry["data"].to_string().contains("not read"));
        }
        let mut tail = vec![];
        backend
            .events_page_raw(&id, entries[1]["seq"].as_i64().unwrap(), &mut |e| {
                tail.push(e)
            })
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].0, last);
        assert_eq!(tail[0].2, "text");
        assert!(backend.events_after(&id, last).unwrap().is_empty());
    }
}

#[cfg(test)]
mod workflow_summary_tests {
    use super::*;
    use s::testkit::hub::state as fixture;

    #[test]
    fn state_extras_identify_paused_current_graph_and_ignore_old_cycle_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steve.sqlite3");
        let store = s::Store::open(&path, s::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        fixture::failed_workflow(&store).unwrap();
        let backend = StoreBackend(store);
        let read = || {
            backend
                .item_extras(s::WS)
                .unwrap()
                .unwrap()
                .remove("ST-1")
                .unwrap()
        };
        let current = read();
        assert_eq!(current.workflow_paused, Some(true));
        assert_eq!(current.workflow_cycle, Some(1));
        assert_eq!(current.workflow_failed, ["ST-2"]);

        fixture::set_item_cycle(&backend.0, "ST-1", 2).unwrap();
        let previous = read();
        assert_eq!(previous.workflow_cycle, Some(1));
        assert!(previous.workflow_failed.is_empty());
    }
}
