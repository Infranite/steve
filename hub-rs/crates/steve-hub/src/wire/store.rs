//! Hub storage operations on [`steve_store::Store`].

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use steve_store as s;

use std::collections::HashMap;

use crate::bus::Bus;
use crate::error::{BResult, BackendError};
use crate::files::NewFile;

/// A JSON object produced by store serializers.
pub type Obj = Map<String, Value>;
pub type ItemExtras = steve_store::state_batches::ItemExtras;

#[derive(Debug, Clone, PartialEq)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    pub device_id: String,
    pub path: String,
    /// `git` or `dir`.
    pub kind: String,
    pub worktree: bool,
    pub default_bot: Option<String>,
    pub release: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSession {
    pub workspace_id: String,
    pub harness: String,
    pub cwd: String,
    pub title: String,
    pub source: String,
    pub import_key: String,
    pub created: f64,
}

#[derive(Default)]
pub struct SummaryFacts {
    pub workspace_name: String,
    pub pending: bool,
    pub devices_online: usize,
    pub devices_total: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BotExtras {
    pub workspaces: HashMap<String, Vec<String>>,
    pub default_projects: HashMap<(String, String), String>,
    pub mcp: HashMap<String, Vec<Value>>,
    pub fallback_surfaces: HashMap<String, Vec<Value>>,
}

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

/// The Hub handle for storage reads and writes.
#[derive(Clone)]
pub struct StoreBackend(pub s::Store);

fn mcp_value(server: s::state_batches::McpSummary) -> BResult<Value> {
    serde_json::to_value(server).map_err(|e| BackendError::msg(e.to_string()))
}

fn str_field(o: &Obj, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_string)
}

impl StoreBackend {
    pub fn bot_extras(&self) -> BResult<BotExtras> {
        let extras = self
            .0
            .read(|r| s::state_batches::bot_extras(r))
            .map_err(err)?;
        let mcp = extras
            .mcp
            .into_iter()
            .map(|(bot, servers)| {
                let values = servers.into_iter().map(mcp_value).collect::<BResult<_>>()?;
                Ok((bot, values))
            })
            .collect::<BResult<_>>()?;
        let fallback_surfaces = extras
            .fallback_surfaces
            .into_iter()
            .map(|(bot, surfaces)| {
                let values = surfaces
                    .into_iter()
                    .map(|s| serde_json::to_value(s).map_err(|e| BackendError::msg(e.to_string())))
                    .collect::<BResult<_>>()?;
                Ok((bot, values))
            })
            .collect::<BResult<_>>()?;
        Ok(BotExtras {
            workspaces: extras.workspaces,
            default_projects: extras.default_projects,
            mcp,
            fallback_surfaces,
        })
    }

    pub fn ensure_workspace(&self) -> BResult<()> {
        self.0.ensure_workspace().map_err(err)
    }
    pub fn workspace_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.workspaces().get(id).map_err(err)?)
    }
    pub fn workspaces_of_user(&self, user: &str) -> BResult<Vec<Obj>> {
        objs(self.0.workspaces().of_user(user).map_err(err)?)
    }
    pub fn workspace_create(
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
    pub fn workspace_patch(&self, id: &str, p: &Obj) -> BResult<Obj> {
        self.0
            .workspaces()
            .patch(id, &patch(p)?)
            .map_err(err)?
            .map(obj)
            .unwrap_or_else(|| Err(gone("workspace")))
    }
    pub fn workspace_is_member(&self, id: &str, user: &str) -> BResult<bool> {
        self.0.workspaces().is_member(id, user).map_err(err)
    }
    pub fn user_by_session(&self, token: &str) -> BResult<Option<Obj>> {
        opt(self.0.auth_sessions().user(token).map_err(err)?)
    }
    pub fn member_identities(&self, ws: &str) -> BResult<Vec<steve_types::MemberIdentity>> {
        self.0.users().member_identities(ws).map_err(err)
    }
    pub fn member_identity(
        &self,
        ws: &str,
        user: &str,
    ) -> BResult<Option<steve_types::MemberIdentity>> {
        self.0.users().member_identity(ws, user).map_err(err)
    }
    pub fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        detail: &Value,
        ws: &str,
    ) -> BResult<()> {
        self.0.audit(actor, action, target, detail, ws).map_err(err)
    }
    pub fn audit_page(
        &self,
        ws: &str,
        reader: &str,
        before: Option<i64>,
        limit: i64,
    ) -> BResult<Value> {
        let page = self.0.audit_page(ws, reader, before, limit).map_err(err)?;
        serde_json::to_value(page).map_err(|e| BackendError::msg(e.to_string()))
    }

    pub fn settings_all(&self, ws: &str) -> BResult<Obj> {
        self.0.settings().all(ws).map_err(err)
    }
    pub fn settings_set(&self, ws: &str, values: &Obj, author: &str) -> BResult<()> {
        self.0.settings().set(ws, values, author).map_err(err)
    }

    pub fn projects_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.projects().list(ws, false).map_err(err)?)
    }
    pub fn project_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.projects().get(id).map_err(err)?)
    }
    pub fn project_by_path(&self, ws: &str, path: &str) -> BResult<Option<Obj>> {
        opt(self.0.projects().by_path(ws, path).map_err(err)?)
    }
    pub fn project_create(&self, ws: &str, p: &NewProject) -> BResult<Obj> {
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
    pub fn project_patch(&self, id: &str, p: &Obj, author: &str) -> BResult<Obj> {
        obj(self
            .0
            .projects()
            .patch(id, &patch(p)?, author)
            .map_err(err)?)
    }

    pub fn bots_list(&self, ws: Option<&str>) -> BResult<Vec<Obj>> {
        objs(self.0.bots().list(ws).map_err(err)?)
    }
    pub fn bot_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.bots().get(id).map_err(err)?)
    }
    pub fn bot_owner(&self, id: &str) -> BResult<Option<Option<String>>> {
        self.0.bots().owner(id).map_err(err)
    }
    pub fn bot_workspaces(&self, id: &str) -> BResult<Vec<String>> {
        Ok(self
            .0
            .bots()
            .workspaces(id)
            .map_err(err)?
            .into_iter()
            .map(|w| w.workspace_id)
            .collect())
    }
    pub fn bot_mcp_servers(&self, id: &str) -> BResult<Vec<Value>> {
        self.0
            .read(|r| s::state_batches::bot_mcp(r, id))
            .map_err(err)?
            .into_iter()
            .map(mcp_value)
            .collect()
    }
    pub fn bot_fallback_surfaces(&self, id: &str) -> BResult<Vec<Value>> {
        self.0
            .bots()
            .fallback_surfaces(id)
            .map_err(err)?
            .into_iter()
            .map(|surface| {
                serde_json::to_value(surface).map_err(|e| BackendError::msg(e.to_string()))
            })
            .collect()
    }
    pub fn bot_in_workspace(&self, id: &str, ws: &str) -> BResult<bool> {
        self.0.bots().in_workspace(id, ws).map_err(err)
    }
    pub fn bot_default_project(&self, id: &str, ws: &str) -> BResult<Option<String>> {
        self.0.bots().default_project(id, ws).map_err(err)
    }
    pub fn bot_set_workspaces(&self, id: &str, ws: &[String]) -> BResult<()> {
        self.0.bots().set_workspaces(id, ws).map_err(err)
    }
    pub fn bot_create(&self, ws: &str, f: &Obj, author: &str) -> BResult<Obj> {
        let get = |k: &str| f.get(k).filter(|v| !v.is_null()).cloned();
        let nb = s::NewBot {
            name: str_field(f, "name").unwrap_or_default(),
            harness: str_field(f, "harness")
                .unwrap_or_else(|| steve_harness_spec::DEFAULT_ADAPTER.0.into()),
            role: str_field(f, "role"),
            specialties: get("specialties")
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| BackendError::msg(e.to_string()))?,
            model: str_field(f, "model"),
            reasoning_effort: str_field(f, "reasoning_effort")
                .as_deref()
                .and_then(steve_types::ReasoningEffort::parse)
                .unwrap_or_default(),
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
            enabled: f.get("enabled").and_then(Value::as_bool),
            fallback_surfaces: get("fallback_surfaces")
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| BackendError::msg(e.to_string()))?,
        };
        let bot = self
            .0
            .write(|tx| steve_modules::contacts::stewards::create_bot(tx, ws, &nb, author))
            .map_err(|e| BackendError::msg(e.to_string()))?;
        obj(bot)
    }
    pub fn bot_patch(&self, ws: &str, id: &str, p: &Obj, author: &str) -> BResult<Obj> {
        self.0
            .bots()
            .patch_in(ws, id, &patch(p)?, author)
            .map_err(err)?
            .map(obj)
            .unwrap_or_else(|| Err(gone("bot")))
    }
    pub fn bot_patch_checked(
        &self,
        ws: &str,
        id: &str,
        p: &Obj,
        author: &str,
        harnesses: &[steve_core::harnesses::Harness],
    ) -> BResult<Obj> {
        self.0
            .write(|_| -> anyhow::Result<Obj> {
                let mut b = self.bot_get(id)?.ok_or_else(|| gone("bot"))?;
                b.insert(
                    "default_project".into(),
                    serde_json::json!(self.bot_default_project(id, ws)?),
                );
                steve_core::models::validate_bot_patch(Some(&b), p, harnesses).map_err(
                    |message| BackendError {
                        status: 400,
                        message,
                    },
                )?;
                Ok(self.bot_patch(ws, id, p, author)?)
            })
            .map_err(|e| match e.downcast::<BackendError>() {
                Ok(e) => e,
                Err(e) => BackendError::msg(e.to_string()),
            })
    }
    pub fn bot_restore(&self, id: &str) -> BResult<()> {
        self.0.bots().restore(id).map_err(err)
    }

    pub fn memory_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.memories().get(id).map_err(err)?)
    }
    pub fn memories_list(&self, bot: &str, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.memories().list(bot, ws).map_err(err)?)
    }
    pub fn memories_count(&self, bot: &str, ws: &str) -> BResult<i64> {
        self.0.memories().count(bot, ws).map_err(err)
    }
    pub fn memory_add(&self, bot: &str, ws: &str, text: &str, source: &str) -> BResult<String> {
        self.0.memories().add(bot, ws, text, source).map_err(err)
    }
    pub fn memory_edit(&self, id: &str, text: &str) -> BResult<()> {
        self.0.memories().edit(id, text).map_err(err)
    }
    pub fn memory_remove(&self, id: &str) -> BResult<()> {
        self.0.memories().remove(id).map_err(err)
    }
    pub fn memories_clear(&self, ws: &str) -> BResult<()> {
        self.0.memories().clear(ws).map_err(err)
    }
    pub fn memories_delete_of_bot(&self, bot: &str) -> BResult<()> {
        self.0.memories().remove_of_bot(bot).map_err(err)
    }
    pub fn memories_delete_of_bot_in_workspace(&self, bot: &str, ws: &str) -> BResult<()> {
        self.0
            .memories()
            .remove_of_bot_in_workspace(bot, ws)
            .map_err(err)
    }

    pub fn tags_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        objs(self.0.tags().list(ws).map_err(err)?)
    }
    pub fn tag_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.tags().get(id).map_err(err)?)
    }
    pub fn tag_by_name(&self, ws: &str, name: &str) -> BResult<Option<Obj>> {
        opt(self.0.tags().by_name(ws, name).map_err(err)?)
    }
    pub fn tag_create(
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
    pub fn tag_patch(&self, id: &str, p: &Obj) -> BResult<Option<Obj>> {
        opt(self.0.tags().patch(id, &patch(p)?).map_err(err)?)
    }
    pub fn tag_remove(&self, id: &str) -> BResult<()> {
        self.0.tags().remove(id).map_err(err)
    }
    pub fn tag_merge(&self, from: &str, into: &str) -> BResult<()> {
        self.0.tags().merge(from, into).map_err(err)
    }
    pub fn tag_uses(&self, ws: &str) -> BResult<Obj> {
        Ok(self
            .0
            .tags()
            .uses(ws)
            .map_err(err)?
            .into_iter()
            .map(|(k, n)| (k, Value::from(n)))
            .collect::<Map<_, _>>())
    }
    pub fn tags_of_item(&self, item: &str) -> BResult<Vec<String>> {
        self.0.tags().of_item(item).map_err(err)
    }
    pub fn tags_set_item(&self, item: &str, tags: &[String]) -> BResult<Vec<String>> {
        self.0.tags().set_item(item, tags).map_err(err)
    }

    pub fn item_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.items().get(id).map_err(err)?)
    }
    pub fn item_view(&self, ws: &str, id: &str) -> BResult<Option<steve_types::ItemView>> {
        use s::Views;
        let id = id.to_string();
        Ok(self
            .0
            .read(|r| r.item_views().of(ws, std::slice::from_ref(&id)))
            .map_err(err)?
            .into_iter()
            .next())
    }
    pub fn item_patch(&self, id: &str, p: &Obj) -> BResult<Option<Obj>> {
        opt(self.0.items().patch(id, &patch(p)?).map_err(err)?)
    }
    pub fn item_set_design_gate(&self, id: &str, on: bool) -> BResult<Option<Obj>> {
        opt(self.0.items().set_design_gate(id, on).map_err(err)?)
    }
    pub fn item_remove(&self, id: &str) -> BResult<()> {
        self.0.items().remove(id).map_err(err)
    }
    pub fn item_text(&self, ws: &str, id: &str) -> BResult<Option<Obj>> {
        use steve_store::Views;
        opt(self.0.read(|r| r.item_views().text(ws, id)).map_err(err)?)
    }
    pub fn item_goals(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().goals(id).map_err(err)?)
    }
    pub fn item_plan_rounds(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().plan_rounds(id).map_err(err)?)
    }
    pub fn item_cycles(&self, id: &str) -> BResult<Vec<Obj>> {
        objs(self.0.items().cycles(id).map_err(err)?)
    }
    pub fn item_goal(&self, id: &str) -> BResult<String> {
        self.0.items().goal(id).map_err(err)
    }
    pub fn deps_of(&self, item: &str) -> BResult<Vec<String>> {
        self.0.deps().of(item).map_err(err)
    }
    pub fn deps_dependents(&self, item: &str) -> BResult<Vec<String>> {
        self.0.deps().dependents(item).map_err(err)
    }
    pub fn thread_item(&self, item: &str) -> BResult<Option<Value>> {
        Ok(self
            .0
            .read(|r| s::thread::dispatch_by_item(r, item))
            .map_err(err)?
            .map(|t| serde_json::json!({"follow": t.follow, "origin_msg": t.origin_message_id})))
    }
    pub fn timeline_add(
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
    pub fn timeline_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.timeline().list(item).map_err(err)?)
    }
    pub fn artifacts_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.artifacts().list(item).map_err(err)?)
    }
    pub fn artifact_roles(&self, item: &str) -> BResult<Obj> {
        Ok(self
            .0
            .artifacts()
            .roles(item)
            .map_err(err)?
            .into_iter()
            .map(|(id, role)| (id, Value::String(role)))
            .collect())
    }
    pub fn annotations_list(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.annotations().list(item).map_err(err)?)
    }
    pub fn annotation_add(
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
    pub fn annotation_edit(&self, item: &str, id: &str, note: &str) -> BResult<Option<Obj>> {
        opt(self.0.annotations().edit(item, id, note).map_err(err)?)
    }
    pub fn annotation_remove(&self, item: &str, id: &str) -> BResult<bool> {
        self.0.annotations().remove(item, id).map_err(err)
    }
    pub fn annotations_claim(
        &self,
        item: &str,
        ids: &[String],
        key: &str,
    ) -> BResult<(bool, Vec<Obj>)> {
        let c = self.0.annotations().claim(item, ids, key).map_err(err)?;
        Ok((c.again, objs(c.taken)?))
    }
    pub fn annotations_settle(
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

    pub fn session_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.sessions().get(id).map_err(err)?)
    }
    pub fn sessions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.sessions().of_item(item).map_err(err)?)
    }
    pub fn session_patch(&self, id: &str, p: &Obj) -> BResult<()> {
        self.0.sessions().patch(id, &patch(p)?).map_err(err)
    }
    pub fn sessions_search(&self, ws: &str, q: &str) -> BResult<Vec<String>> {
        self.0.sessions().search(ws, q).map_err(err)
    }
    pub fn events_add(&self, session: &str, kind: &str, data: &Value) -> BResult<i64> {
        self.0
            .events()
            .add(session, kind, data.clone())
            .map_err(err)
    }
    pub fn queued_list(&self, session: &str) -> BResult<Vec<Obj>> {
        objs(self.0.queued().list(session).map_err(err)?)
    }
    pub fn import_keys(&self) -> BResult<Vec<String>> {
        Ok(self
            .0
            .sessions()
            .import_keys()
            .map_err(err)?
            .into_iter()
            .collect())
    }
    pub fn session_insert_imported(&self, p: &ImportedSession) -> BResult<String> {
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
    pub fn session_delete(&self, id: &str) -> BResult<()> {
        self.0.sessions().remove(id).map_err(err)
    }

    pub fn file_get(&self, id: &str) -> BResult<Option<Obj>> {
        opt(self.0.files().get(id).map_err(err)?)
    }
    pub fn file_same(&self, ws: &str, sha256: &str, name: &str) -> BResult<Option<Obj>> {
        opt(self.0.files().same(ws, sha256, name).map_err(err)?)
    }
    pub fn file_create(&self, f: &NewFile) -> BResult<Obj> {
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

    pub fn decisions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        objs(self.0.decisions().of_item(item).map_err(err)?)
    }
    pub fn decision_views_of_item(
        &self,
        ws: &str,
        item: &str,
    ) -> BResult<Vec<steve_types::DecisionView>> {
        use s::Views;
        let ds = self
            .0
            .read(|r| r.decision_views().of_item(ws, item))
            .map_err(err)?;
        Ok(ds)
    }
    pub fn messages_in(&self, ws: &str, space: &str, limit: i64) -> BResult<Vec<Obj>> {
        objs(self.0.messages().list(ws, space, limit).map_err(err)?)
    }
    pub fn messages_search(&self, ws: &str, space: &str, q: &str, limit: i64) -> BResult<Vec<Obj>> {
        objs(self.0.messages().search(ws, space, q, limit).map_err(err)?)
    }
    pub fn message_add(
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
    pub fn setting_raw(&self, ws: &str, key: &str) -> BResult<Option<String>> {
        self.0
            .read(|r| steve_store::WorkspaceSettings(r).get(ws, key))
            .map_err(err)
    }
    pub fn node_env_list(&self, me: &str, ws: &str, node: &str, principal: &str) -> BResult<Value> {
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
    pub fn node_env_put(
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
    pub fn setting_put_raw(&self, ws: &str, key: &str, value: &str) -> BResult<()> {
        self.0
            .write(|w| steve_store::WorkspaceSettings(w).set(ws, key, value, steve_store::now()))
            .map_err(err)
    }
    pub fn notices_post(
        &self,
        ws: &str,
        kind: &str,
        level: &str,
        text: &str,
        item: Option<&str>,
        post_host_line: bool,
    ) -> BResult<String> {
        self.0
            .write(|w| {
                s::thread::post_notice(
                    w,
                    ws,
                    kind,
                    level,
                    text,
                    item,
                    post_host_line,
                    steve_store::now(),
                )
            })
            .map(|n| n.id)
            .map_err(err)
    }
    pub fn notices_dismiss(
        &self,
        ws: &str,
        id_or_host_msg: &str,
        by: Option<&str>,
    ) -> BResult<bool> {
        self.0
            .write(|w| s::thread::close_notice(w, ws, id_or_host_msg, by, steve_store::now()))
            .map_err(err)
    }
    pub fn notices_list(&self, ws: &str, limit: i64) -> BResult<Vec<s::ThreadNotice>> {
        self.0
            .read(|r| s::thread::list_notices(r, ws, limit))
            .map_err(err)
    }
    pub fn notices_dismissed(&self, ws: &str) -> BResult<Vec<String>> {
        self.0
            .read(|r| s::thread::dismissed_alert_ids(r, ws))
            .map_err(err)
    }
    pub fn summary_facts(&self, ws: &str) -> BResult<SummaryFacts> {
        self.0
            .read(|r| -> steve_store::Result<_> {
                let workspace_name = s::Workspaces(r).name(ws)?.unwrap_or_default();
                let (devices_online, devices_total) = s::state_batches::device_counts(r)?;
                Ok(SummaryFacts {
                    workspace_name,
                    pending: s::thread::has_active_or_queued(r, ws, steve_core::thread::MAIN)?,
                    devices_online: devices_online as usize,
                    devices_total: devices_total as usize,
                })
            })
            .map_err(err)
    }
    pub fn devices_list(&self) -> BResult<Vec<Obj>> {
        use steve_store::runtime::{HUB_DEVICE, HUB_NODE, session_room};
        let mut out = objs(self.0.devices().list().map_err(err)?)?;
        // the Hub's machine runs the Hub's own Node: how many of its turns run and wait, against
        // the limit on this row
        let room = self.0.read(|c| session_room(c, HUB_NODE)).map_err(err)?;
        for d in out
            .iter_mut()
            .filter(|d| d.get("id").and_then(Value::as_str) == Some(HUB_DEVICE))
        {
            d.insert("running".into(), serde_json::json!(room.running));
            d.insert("waiting".into(), serde_json::json!(room.waiting));
        }
        Ok(out)
    }
    pub fn waiting_for_room(&self) -> BResult<HashMap<String, Value>> {
        let rows = self
            .0
            .read(|r| steve_store::runtime::sessions_waiting_for_room(r))
            .map_err(err)?;
        rows.into_iter()
            .map(|(s, r)| {
                serde_json::to_value(r)
                    .map(|v| (s, v))
                    .map_err(|e| BackendError::msg(e.to_string()))
            })
            .collect()
    }
    pub fn device_upsert(
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
}
