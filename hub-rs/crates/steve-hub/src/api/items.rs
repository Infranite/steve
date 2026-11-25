//! `/api/items*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use super::{
    ApiResult, AppState, Ctx, Noun, body, objs, ok, ok_true, opt_obj, own, str_of, summary,
};
use crate::backend::{CreateItem, Obj};
use crate::error::ApiError;
use crate::js;
use crate::validate::{Check, Num, Str};

/// `own(c, items.get(id), Noun::Item)` on the read pool.
async fn own_item(st: &AppState, ws: &str, id: &str) -> Result<Obj, ApiError> {
    let (id, ws) = (id.to_string(), ws.to_string());
    st.read(move |r| Ok(own(r.item_get(&id)?, &ws, Noun::Item)?))
        .await
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let goal = k.str("goal", Str::min(1));
    let title = k.opt_str("title", Str::ANY);
    let bot = k.opt_str("bot", Str::ANY);
    let team = k.opt_str("team", Str::len(1, 64));
    let node = k.opt_str("node", Str::len(1, 80));
    let required_specialties = k
        .opt_str_array_of("required_specialties", Str::len(1, 48), None, Some(12))
        .unwrap_or_default();
    let deps = k
        .opt_str_array_of("deps", Str::len(1, 80), None, Some(200))
        .unwrap_or_default();
    let project = k.opt_nullable_str("project", Str::ANY);
    let mode = k
        .opt_enum("mode", &["start", "propose"])
        .unwrap_or_else(|| "start".into());
    let client_id = k.opt_str("client_id", Str::len(1, 80));
    k.finish()?;
    // a retry of the same form (`client_id`) gets the item the first request made; the creation
    // runs on even if this request goes away, so it needs its own state
    let (st2, c2) = (st.clone(), c.clone());
    let item = st
        .dedupe
        .once(&c.ws, &c.me, "items", client_id, move || async move {
            create_now(
                &st2,
                &c2,
                goal,
                title,
                bot,
                project,
                mode,
                team,
                deps,
                node,
                required_specialties,
            )
            .await
        })
        .await?;
    ok(item)
}

async fn create_now(
    st: &AppState,
    c: &Ctx,
    goal: String,
    title: Option<String>,
    bot: Option<String>,
    project: Option<Option<String>>,
    mode: String,
    team: Option<String>,
    deps: Vec<String>,
    node: Option<String>,
    required_specialties: Vec<String>,
) -> Result<Value, ApiError> {
    for dep in &deps {
        own_item(st, &c.ws, dep).await?;
    }
    if let Some(id) = team.as_deref() {
        let store =
            super::bot_dispatch::store(st).ok_or_else(|| ApiError::not_found("Bot Team"))?;
        store
            .read(|db| steve_store::bot_dispatch::selected(db, &c.ws, Some(id)))
            .map_err(|e| match e {
                steve_store::StoreError::NotFound(_) => {
                    ApiError::not_found(steve_i18n::t!("hub.dispatch.not_found"))
                }
                e => ApiError::internal(e.to_string()),
            })?;
    }
    // whatever the user left open (bot, project, title, tags) Steve decides, the same way as in the conversation
    let needs_route = project.is_none() || title.as_deref().is_none_or(str::is_empty);
    let r = if needs_route {
        Some(
            st.domain
                .route_for_team(&c.ws, &goal, team.as_deref())
                .await?,
        )
    } else {
        None
    };
    let mut tag_ids = vec![];
    if let Some(r) = &r {
        let (ws, new) = (c.ws.clone(), r.new_tags.clone());
        let made = st
            .write(move |w| {
                new.iter()
                    .map(|t| {
                        Ok(str_of(
                            &w.tag_create(&ws, &t.name, &t.cat, &t.description, "Steve")?,
                            "id",
                        ))
                    })
                    .collect::<Result<Vec<_>, ApiError>>()
            })
            .await?;
        tag_ids = r.tag_ids.iter().cloned().chain(made).collect();
    }
    let title = title
        .or_else(|| r.as_ref().map(|r| r.title.clone()))
        .unwrap_or_else(|| js::slice(&goal, 24));
    let project = match project {
        Some(p) => Some(p),
        None => Some(r.as_ref().and_then(|r| r.project.clone())),
    };
    let item = st
        .domain
        .create_item(CreateItem {
            ws: c.ws.clone(),
            goal,
            title,
            bot,
            team,
            node,
            required_specialties,
            deps,
            project,
            tag_ids,
            source: ["手动新建".into(), "manual".into()],
            mode,
            actor: c.me.clone(),
            opening: None,
        })
        .await?;
    Ok(Value::Object(item))
}

/// The fixed reasons of a dependency wait view (`steve_core`'s, stored nowhere: written in zh-CN).
const DEP_WAIT_KEYS: &[&str] = &[
    "hub.dep_wait.add_first",
    "hub.dep_wait.no_resume",
    "hub.dep_wait.holding",
    "hub.dep_wait.fallback",
    "hub.dep_wait.item_ending",
    "hub.dep_wait.no_session",
];

/// A dependency wait view with its fixed reasons in the request's language.
fn dependency_shown(mut v: Value) -> Value {
    for k in [
        "register_blocker",
        "resume_blocker",
        "resume_fallback_reason",
    ] {
        if let Some(Value::String(s)) = v.get(k)
            && let Some(t) = steve_i18n::reword!(s, DEP_WAIT_KEYS)
        {
            v[k] = Value::String(t);
        }
    }
    v
}

pub async fn get_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let lifecycle = st.domain.lifecycle_view(&id).await?;
    let dependency_wait = dependency_shown(st.domain.dependency_get(&id).await?);
    let dispatch = super::bot_dispatch::store(&st).and_then(|s| {
        super::bot_dispatch::item_dispatch(s, &id, &c.ws)
            .inspect_err(|e| tracing::warn!(item = %id, "item dispatch detail unavailable: {e}"))
            .ok()
    });
    st.read(move |r| {
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        let mut out = summary(r, i)?;
        let text = r
            .item_text(&c.ws, &id)?
            .ok_or_else(|| ApiError::not_found(Noun::Item.missing()))?;
        out.extend(text);
        if let Some(dispatch) = dispatch {
            out.insert("dispatch".into(), dispatch);
        }
        if let Some(l) = lifecycle {
            out.insert("lifecycle".into(), l);
        }
        out.insert("dependency_wait".into(), dependency_wait);
        out.insert("cycles".into(), objs(r.item_cycles(&id)?));
        out.insert("planRounds".into(), objs(r.item_plan_rounds(&id)?));
        out.insert("timeline".into(), objs(r.timeline_list(&id)?));
        out.insert("artifacts".into(), objs(r.artifacts_list(&id)?));
        out.insert(
            "artifactRoles".into(),
            Value::Object(r.artifact_roles(&id)?),
        );
        out.insert("annotations".into(), objs(r.annotations_list(&id)?));
        out.insert(
            "decisions".into(),
            serde_json::to_value(r.decision_views_of_item(&c.ws, &id)?)
                .map_err(|e| ApiError::internal(e.to_string()))?,
        );
        out.insert("dependents".into(), json!(r.deps_dependents(&id)?));
        out.insert("thread".into(), r.thread_item(&id)?.unwrap_or(Value::Null));
        ok(Value::Object(out))
    })
    .await
}

pub async fn dependency_get(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    ok(dependency_shown(st.domain.dependency_get(&id).await?))
}

pub async fn dependency_wait(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let cycle = k.num(
        "expected_cycle",
        Num {
            int: true,
            min: Some(1.0),
            max: None,
            gt: None,
        },
    ) as i64;
    let resume = k.enum_("resume", &["automatic", "notify", "workspace"]);
    let reason = k.str("reason", Str::len(1, 2000));
    k.finish()?;
    ok(st
        .domain
        .dependency_wait(&id, cycle, &resume, &reason)
        .await?)
}

pub async fn actions(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let action = k.enum_(
        "action",
        &["pause", "resume", "drop", "archive", "unarchive"],
    );
    k.finish()?;
    ok(Value::Object(
        st.domain.item_action(&id, &action, &c.me).await?,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BulkOp {
    Pause,
    Resume,
    Drop,
    Archive,
}

impl BulkOp {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "drop" => Some(Self::Drop),
            "archive" => Some(Self::Archive),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Drop => "drop",
            Self::Archive => "archive",
        }
    }

    /// Whether `state` (and `archived`) allows this bulk operation under the W3.2 lifecycle.
    fn applies(self, state: &steve_types::ItemState, archived: bool) -> bool {
        use steve_types::{CloseKind, ItemState};
        match self {
            Self::Pause => matches!(
                state,
                ItemState::Ready
                    | ItemState::Active
                    | ItemState::Closing {
                        request: CloseKind::Complete,
                        ..
                    }
            ),
            Self::Resume => matches!(state, ItemState::Paused),
            Self::Drop => matches!(
                state,
                ItemState::Ready
                    | ItemState::Active
                    | ItemState::Paused
                    | ItemState::Closing {
                        request: CloseKind::Complete,
                        ..
                    }
            ),
            Self::Archive => !archived,
        }
    }
}

pub async fn bulk(State(st): State<AppState>, Extension(c): Extension<Ctx>, b: Bytes) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let ids = k.str_array("ids", Some(1), Some(200));
    let op_raw = k.enum_("op", &["pause", "resume", "drop", "archive"]);
    k.finish()?;
    let op = BulkOp::parse(&op_raw).ok_or_else(|| ApiError::Invalid(vec!["op".into()]))?;
    let mut n = 0;
    for id in ids {
        let (ws, i) = (c.ws.clone(), id.clone());
        let Some(item) = st.read(move |r| Ok(r.item_view(&ws, &i)?)).await? else {
            continue;
        };
        if !op.applies(&item.state, item.archived) {
            continue;
        }
        st.domain.item_action(&id, op.as_str(), &c.me).await?;
        n += 1;
    }
    ok(json!({ "n": n }))
}

pub async fn goal(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::min(1));
    k.finish()?;
    st.domain.revise_goal(&id, &text, &c.me).await?;
    ok_true()
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let mut rest = Obj::new();
    if let Some(t) = k.opt_str("title", Str::min(1)) {
        rest.insert("title".into(), json!(t));
    }
    if let Some(p) = k.opt_enum("priority", &["高", "中", "低"]) {
        rest.insert("priority".into(), json!(p));
    }
    if let Some(d) = k.opt_nullable_str("due", Str::ANY) {
        rest.insert("due".into(), json!(d));
    }
    if let Some(s) = k.opt_bool("star") {
        rest.insert("star".into(), json!(s));
    }
    if let Some(a) = k.opt_enum("accept_mode", &["inherit", "manual", "auto", "waived"]) {
        rest.insert("accept_mode".into(), json!(a));
    }
    if let Some(p) = k.opt_nullable_str("project_id", Str::ANY) {
        rest.insert("project_id".into(), json!(p));
    }
    let tags = k.opt_str_array("tags", None, None);
    k.finish()?;

    st.write(move |r| {
        // re-read under the lock: the TS handler ran all of this synchronously
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        let me = c.me.as_str();
        let project_id = rest.get("project_id").cloned();
        if let Some(Value::String(p)) = &project_id
            && !p.is_empty()
        {
            own(r.project_get(p)?, &c.ws, Noun::Project)?;
        }
        // tags first, before anything is written: the store drops other workspaces' tags and refuses
        // (409) a set without exactly one category, so removing the only one or adding a second fails
        let tags = match tags {
            Some(t) => {
                let before = r.tags_of_item(&id)?;
                Some((before, r.tags_set_item(&id, &t)?))
            }
            None => None,
        };
        if let Some(Value::String(t)) = rest.get("title")
            && i.get("title").and_then(Value::as_str) != Some(t)
        {
            r.timeline_add(&id, me, "sys", &format!("标题改为「{t}」"), None)?;
        }
        if let Some(due) = rest.get("due")
            && Some(due) != i.get("due")
        {
            let text = match due.as_str() {
                Some(d) if !d.is_empty() => format!("截止时间设为 {d}"),
                _ => "移除截止时间".into(),
            };
            r.timeline_add(&id, me, "sys", &text, None)?;
        }
        let project_changed = project_id
            .as_ref()
            .is_some_and(|p| Some(p) != i.get("project_id"));
        if let Some(p) = project_id.as_ref().filter(|_| project_changed) {
            let name = match p.as_str() {
                Some(pid) if !pid.is_empty() => r.project_get(pid)?.map(|x| str_of(&x, "name")),
                _ => None,
            };
            r.timeline_add(
                &id,
                me,
                "sys",
                &format!(
                    "项目改为 {}，下一个会话起生效",
                    name.as_deref().unwrap_or("无")
                ),
                None,
            )?;
        }
        if let Some((before, kept)) = &tags {
            note_tag_changes(r, &id, me, before, kept)?;
        }
        // a different project only applies from the next session: forget the old directory
        let mut patch = rest;
        if project_changed {
            patch.insert("cwd".into(), Value::Null);
            patch.insert("branch".into(), Value::Null);
        }
        ok(opt_obj(r.item_patch(&id, &patch)?))
    })
    .await
}

/// Describe the validated tag delta after the field/project change notes, in its original order.
fn note_tag_changes(
    r: &dyn crate::backend::Repo,
    id: &str,
    me: &str,
    before: &[String],
    kept: &[String],
) -> Result<(), ApiError> {
    // name and whether it is the category (`类型`); a missing name reads as ""
    let tag = |x: &String| -> Result<(String, bool), ApiError> {
        Ok(r.tag_get(x)?
            .map(|g| (str_of(&g, "name"), str_of(&g, "cat") == "类型"))
            .unwrap_or_default())
    };
    let mut add = vec![];
    let mut rm = vec![];
    let (mut kind_from, mut kind_to) = (None, None);
    for x in kept.iter().filter(|x| !before.contains(x)) {
        match tag(x)? {
            (n, true) => kind_to = Some(n),
            (n, false) => add.push(n),
        }
    }
    for x in before.iter().filter(|x| !kept.contains(x)) {
        match tag(x)? {
            (n, true) => kind_from = Some(n),
            (n, false) => rm.push(n),
        }
    }
    if let Some(to) = kind_to {
        let text = match kind_from {
            Some(from) => format!("分类改为 {to}（原来是 {from}）"),
            None => format!("分类设为 {to}"),
        };
        r.timeline_add(id, me, "me", &text, None)?;
    }
    if !add.is_empty() {
        r.timeline_add(
            &id,
            me,
            "me",
            &format!("添加标签：{}", add.join("、")),
            None,
        )?;
    }
    if !rm.is_empty() {
        r.timeline_add(id, me, "me", &format!("移除标签：{}", rm.join("、")), None)?;
    }
    Ok(())
}

/// `PUT /items/{id}/design-gate {on}` (Q1): a delivery of the item waits for the owner to confirm the
/// design before any implementation. Only the instance's owner switches it, from the page or the API;
/// no Bot, MCP tool or YOLO has a way to write it.
pub async fn design_gate(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    if c.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.item.design_gate_owner_only"),
        ));
    }
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let on = k.opt_bool("on");
    k.finish()?;
    let Some(on) = on else {
        return Err(ApiError::Invalid(vec!["on".into()]));
    };
    st.write(move |r| {
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        if i.get("design_gate").and_then(Value::as_bool) != Some(on) {
            let line = if on {
                "开启设计确认：交付后先由你确认设计，再决定是否实现"
            } else {
                "关闭设计确认"
            };
            r.timeline_add(&id, &c.me, "me", line, None)?;
        }
        ok(opt_obj(r.item_set_design_gate(&id, on)?))
    })
    .await
}

pub async fn delete_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        if !js::truthy(i.get("archived")) {
            return Err(ApiError::bad_request(steve_i18n::t!(
                "hub.item.archive_first"
            )));
        }
        r.item_remove(&id)?;
        r.audit(
            &c.me,
            "item.delete",
            &id,
            &json!({}),
            &str_of(&i, "workspace_id"),
        )?;
        ok_true()
    })
    .await
}

pub async fn supplement(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::min(1));
    let mode = k
        .opt_enum("mode", &["auto", "steer", "new"])
        .unwrap_or_else(|| "auto".into());
    let files = super::files::file_ids(&mut k);
    k.finish()?;
    let refs = super::files::refs(&st, &c.ws, files).await?;
    st.domain.supplement(&id, &text, &mode, refs, &c.me).await?;
    ok_true()
}

pub async fn deliver(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    st.domain.deliver_queued(&id).await?;
    ok_true()
}

pub async fn reassign(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let bot = k.str("bot", Str::ANY);
    k.finish()?;
    ok(Value::Object(st.domain.reassign(&id, &bot, &c.me).await?))
}

pub async fn handoff(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let target = parse_handoff_body(&b)?;
    ok(st.domain.handoff_item(&id, target, &c.me).await?)
}

pub(super) fn parse_handoff_body(b: &Bytes) -> Result<steve_core::HandoffTarget, ApiError> {
    if b.iter().all(|c| c.is_ascii_whitespace()) {
        return Ok(steve_core::HandoffTarget::default());
    }
    let v = body(b)?;
    let mut k = Check::object(&v);
    let bot = k.opt_nullable_str("bot", Str::len(0, 128)).flatten();
    let harness = k.opt_nullable_str("harness", Str::len(0, 64)).flatten();
    let model = k.opt_nullable_str("model", Str::len(0, 200)).flatten();
    let effort = k.opt_nullable_str("effort", Str::len(0, 32)).flatten();
    let make_bot_default = k.opt_bool("make_bot_default").unwrap_or(false);
    k.finish()?;
    let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    Ok(steve_core::HandoffTarget {
        bot: clean(bot),
        harness: clean(harness),
        model: clean(model),
        effort: clean(effort),
        make_bot_default,
    })
}

pub async fn add_dep(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let dep = k.str("dep", Str::ANY);
    k.finish()?;
    own_item(&st, &c.ws, &dep).await?;
    st.domain.add_dep(&id, &dep, &c.me).await?;
    ok_true()
}

pub async fn rm_dep(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, dep)): Path<(String, String)>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    st.domain.rm_dep(&id, &dep, &c.me).await?;
    ok_true()
}

pub async fn snooze(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let label = k.nullable_str("label", Str::ANY);
    let until = k.nullable_num("until", Num::ANY);
    k.finish()?;
    st.write(move |r| {
        let snooze = match (&label, until) {
            (Some(l), Some(u)) if !l.is_empty() && u != 0.0 => {
                json!({ "label": l, "until": super::misc::js_num(u) })
            }
            _ => Value::Null,
        };
        let mut p = Obj::new();
        p.insert("snooze".into(), snooze);
        r.item_patch(&id, &p)?;
        if let Some(l) = label.filter(|l| !l.is_empty()) {
            r.timeline_add(&id, &c.me, "me", &format!("稍后提醒：{l}"), None)?;
        }
        ok_true()
    })
    .await
}

/// `GET /api/items/:id/panels`: every module's sidebar panels for the item (`[]` without modules).
pub async fn panels(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let Some(m) = st.modules.clone() else {
        return ok(json!([]));
    };
    let r = m
        .blocking(move |m| steve_modules::panels::panels(m, &c.ws, &c.me, &id))
        .await;
    Ok(match r {
        Ok(p) => {
            crate::error::json_response(200, &serde_json::to_value(p).unwrap_or_else(|_| json!([])))
        }
        Err(e) => axum::response::IntoResponse::into_response(e),
    })
}

/// `POST /api/items/:id/panels/:panel/actions/:action`: the panel's provider runs the action.
pub async fn panel_action(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, panel, action)): Path<(String, String, String)>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let Some(m) = st.modules.clone() else {
        return Err(ApiError::not_found(steve_i18n::t!("hub.not_found.panel")));
    };
    let caller = steve_modules::Caller {
        ws: c.ws.clone(),
        me: c.me.clone(),
    };
    Ok(
        match steve_modules::panels::action(&m, &caller, &id, &panel, &action).await {
            Ok(o) => {
                crate::error::json_response(200, &serde_json::to_value(o).unwrap_or(Value::Null))
            }
            Err(e) => axum::response::IntoResponse::into_response(e),
        },
    )
}

/// `POST /items/:id/lifecycle {command, expected_cycle, expected_revision, reason?, goal?}`:
/// `200 {item}`; `409 {error, lifecycle}` when the item changed or the phase
/// does not allow the command.
pub async fn lifecycle(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let command = k.enum_(
        "command",
        &["accept", "finish", "cancel", "fail", "new_cycle"],
    );
    let int = |min: f64| Num {
        int: true,
        min: Some(min),
        max: None,
        gt: None,
    };
    let cycle = k.num("expected_cycle", int(1.0)) as i64;
    let revision = k.num("expected_revision", int(0.0)) as i64;
    let reason = k.opt_str("reason", Str::ANY);
    let goal = k.opt_str("goal", Str::ANY);
    k.finish()?;
    if command == "finish" && c.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.item.finish_owner_only"),
        ));
    }
    match st
        .domain
        .lifecycle_command(&id, &command, cycle, revision, reason, goal, &c.me)
        .await
    {
        Ok(_) => {}
        Err(e) if e.status == 409 => {
            let l = st.domain.lifecycle_view(&id).await?.unwrap_or(Value::Null);
            return Ok(crate::error::json_response(
                409,
                &json!({"error": e.message, "lifecycle": l}),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let lifecycle = st.domain.lifecycle_view(&id).await?;
    st.read(move |r| {
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        let mut out = summary(r, i)?;
        if let Some(l) = lifecycle {
            out.insert("lifecycle".into(), l);
        }
        ok(json!({ "item": Value::Object(out) }))
    })
    .await
}

#[cfg(test)]
mod dependency_text_tests {
    use super::*;

    #[test]
    fn the_reasons_by_language() {
        let v = json!({"register_blocker": "请先添加前置事项", "resume_blocker": "实例已禁用自动续跑", "reason": "等 ST-1"});
        let en = steve_i18n::with(steve_i18n::EN, || dependency_shown(v.clone()));
        assert_eq!(en["register_blocker"], "Add a prerequisite first");
        assert_eq!(
            en["resume_blocker"],
            "This instance has automatic resume turned off"
        );
        assert_eq!(en["reason"], "等 ST-1");
        assert_eq!(
            steve_i18n::with(steve_i18n::ZH_CN, || dependency_shown(v.clone())),
            v
        );
    }

    #[test]
    fn bulk_op_matches_typed_item_state() {
        use steve_types::{CloseKind, ClosedBy, ItemOutcome, ItemState};

        let cases = [
            (ItemState::Ready, (true, false, true)),
            (ItemState::Active, (true, false, true)),
            (ItemState::Paused, (false, true, true)),
            (
                ItemState::Closing {
                    request: CloseKind::Complete,
                    waits: vec![],
                },
                (true, false, true),
            ),
            (
                ItemState::Closing {
                    request: CloseKind::Cancel,
                    waits: vec![],
                },
                (false, false, false),
            ),
            (
                ItemState::Closing {
                    request: CloseKind::Fail,
                    waits: vec![],
                },
                (false, false, false),
            ),
            (
                ItemState::Closed {
                    outcome: ItemOutcome::Finished,
                    by: ClosedBy::Owner,
                },
                (false, false, false),
            ),
            (
                ItemState::Closed {
                    outcome: ItemOutcome::Cancelled,
                    by: ClosedBy::Owner,
                },
                (false, false, false),
            ),
            (
                ItemState::Closed {
                    outcome: ItemOutcome::Failed,
                    by: ClosedBy::Rule,
                },
                (false, false, false),
            ),
        ];
        for (state, (pause, resume, drop)) in cases {
            assert_eq!(BulkOp::Pause.applies(&state, false), pause, "{state:?}");
            assert_eq!(BulkOp::Resume.applies(&state, false), resume, "{state:?}");
            assert_eq!(BulkOp::Drop.applies(&state, false), drop, "{state:?}");
            assert!(BulkOp::Archive.applies(&state, false));
            assert!(!BulkOp::Archive.applies(&state, true));
        }
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/items", post(items::create))
        .route("/items/bulk", post(items::bulk))
        .route(
            "/items/{id}",
            get(items::get_one)
                .patch(items::patch_one)
                .delete(items::delete_one),
        )
        .route("/items/{id}/actions", post(items::actions))
        .route("/items/{id}/lifecycle", post(items::lifecycle))
        .route(
            "/items/{id}/dependency-wait",
            get(items::dependency_get).post(items::dependency_wait),
        )
        .route("/items/{id}/design-gate", put(items::design_gate))
        .route("/items/{id}/goal", post(items::goal))
        .route("/items/{id}/supplement", post(items::supplement))
        .route("/items/{id}/deliver", post(items::deliver))
        .route("/items/{id}/reassign", post(items::reassign))
        .route("/items/{id}/handoff", post(items::handoff))
        .route("/items/{id}/deps", post(items::add_dep))
        .route("/items/{id}/deps/{dep}", delete(items::rm_dep))
        .route("/items/{id}/snooze", post(items::snooze))
        .route("/items/{id}/panels", get(items::panels))
        .route(
            "/items/{id}/panels/{panel}/actions/{action}",
            post(items::panel_action),
        )
}
