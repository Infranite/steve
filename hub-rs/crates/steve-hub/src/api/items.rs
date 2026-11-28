//! `/api/items*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use super::{
    ApiResult, AppState, Ctx, Noun, Patch, check_file_ids, check_len, json_body, objs, ok, ok_true,
    opt_obj, own, str_of, summary,
};
use crate::error::ApiError;
use crate::js;
use crate::wire::core::CreateItem;
use crate::wire::store::{Obj, StoreBackend};

#[derive(serde::Deserialize)]
struct CreateBody {
    goal: String,
    title: Option<String>,
    bot: Option<String>,
    team: Option<String>,
    node: Option<String>,
    #[serde(default)]
    required_specialties: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    project: Patch<String>,
    mode: Option<String>,
    client_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct DepWaitBody {
    expected_cycle: i64,
    resume: String,
    reason: String,
}

#[derive(serde::Deserialize)]
struct ActionBody {
    action: String,
}

#[derive(serde::Deserialize)]
struct BulkBody {
    ids: Vec<String>,
    op: String,
}

#[derive(serde::Deserialize)]
struct TextBody {
    text: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum Priority {
    #[serde(rename = "高")]
    High,
    #[serde(rename = "中")]
    Medium,
    #[serde(rename = "低")]
    Low,
}

#[derive(serde::Deserialize)]
struct PatchBody {
    title: Option<String>,
    priority: Option<Priority>,
    #[serde(default)]
    due: Patch<String>,
    star: Option<bool>,
    accept_mode: Option<String>,
    #[serde(default)]
    project_id: Patch<String>,
    tags: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct DesignGateBody {
    on: bool,
}

#[derive(serde::Deserialize)]
struct SupplementBody {
    text: String,
    mode: Option<String>,
    files: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct ReassignBody {
    bot: String,
}

#[derive(serde::Deserialize)]
struct AddDepBody {
    dep: String,
}

#[derive(serde::Deserialize)]
struct SnoozeBody {
    label: Patch<String>,
    until: Patch<f64>,
}

#[derive(serde::Deserialize)]
struct LifecycleBody {
    command: String,
    expected_cycle: i64,
    expected_revision: i64,
    reason: Option<String>,
    goal: Option<String>,
    #[serde(default)]
    result: Option<Value>,
}

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
    let req: CreateBody = json_body(&b)?;
    let mut bad = Vec::new();
    if req.goal.is_empty() {
        bad.push("goal".into());
    }
    if req.team.as_deref().is_some_and(|t| !check_len(t, 1, 64)) {
        bad.push("team".into());
    }
    if req.node.as_deref().is_some_and(|n| !check_len(n, 1, 80)) {
        bad.push("node".into());
    }
    if req.required_specialties.len() > 12
        || req
            .required_specialties
            .iter()
            .any(|s| !check_len(s, 1, 48))
    {
        bad.push("required_specialties".into());
    }
    if req.deps.len() > 200 || req.deps.iter().any(|s| !check_len(s, 1, 80)) {
        bad.push("deps".into());
    }
    let mode = req.mode.unwrap_or_else(|| "start".into());
    if !matches!(mode.as_str(), "start" | "propose") {
        bad.push("mode".into());
    }
    if req
        .client_id
        .as_deref()
        .is_some_and(|id| !check_len(id, 1, 80))
    {
        bad.push("client_id".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let CreateBody {
        goal,
        title,
        bot,
        team,
        node,
        required_specialties,
        deps,
        project,
        client_id,
        ..
    } = req;
    let project = project.into_option();
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
        st.domain
            .route_for_team(&c.ws, &goal, team.as_deref())
            .await?
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
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            r.as_ref()
                .map(|r| r.title.clone())
                .filter(|t| !t.trim().is_empty())
        })
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
    let req: DepWaitBody = json_body(&b)?;
    let mut bad = Vec::new();
    if req.expected_cycle < 1 {
        bad.push("expected_cycle".into());
    }
    if !matches!(req.resume.as_str(), "automatic" | "notify" | "workspace") {
        bad.push("resume".into());
    }
    if !check_len(&req.reason, 1, 2000) {
        bad.push("reason".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    ok(st
        .domain
        .dependency_wait(&id, req.expected_cycle, &req.resume, &req.reason)
        .await?)
}

pub async fn actions(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let req: ActionBody = json_body(&b)?;
    if !matches!(
        req.action.as_str(),
        "pause" | "resume" | "drop" | "archive" | "unarchive"
    ) {
        return Err(ApiError::Invalid(vec!["action".into()]));
    }
    ok(Value::Object(
        st.domain.item_action(&id, &req.action, &c.me).await?,
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
    let req: BulkBody = json_body(&b)?;
    let mut bad = Vec::new();
    if !(1..=200).contains(&req.ids.len()) {
        bad.push("ids".into());
    }
    let op = BulkOp::parse(&req.op);
    if op.is_none() {
        bad.push("op".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let ids = req.ids;
    let op = op.unwrap();
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
    let req: TextBody = json_body(&b)?;
    if req.text.is_empty() {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    st.domain.revise_goal(&id, &req.text, &c.me).await?;
    ok_true()
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let req: PatchBody = json_body(&b)?;
    let mut bad = Vec::new();
    let mut rest = Obj::new();
    if let Some(t) = req.title {
        if t.is_empty() {
            bad.push("title".into());
        } else {
            rest.insert("title".into(), json!(t));
        }
    }
    if let Some(p) = req.priority {
        rest.insert("priority".into(), json!(p));
    }
    if let Some(d) = req.due.into_option() {
        rest.insert("due".into(), json!(d));
    }
    if let Some(s) = req.star {
        rest.insert("star".into(), json!(s));
    }
    if let Some(a) = req.accept_mode {
        if matches!(a.as_str(), "inherit" | "manual" | "auto" | "waived") {
            rest.insert("accept_mode".into(), json!(a));
        } else {
            bad.push("accept_mode".into());
        }
    }
    if let Some(p) = req.project_id.into_option() {
        rest.insert("project_id".into(), json!(p));
    }
    let tags = req.tags;
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }

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
    r: &StoreBackend,
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
/// no Bot, MCP tool, or relaxed permission level has a way to write it.
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
    let req: DesignGateBody = json_body(&b)?;
    let on = req.on;
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
    let req: SupplementBody = json_body(&b)?;
    let mut bad = Vec::new();
    if req.text.is_empty() {
        bad.push("text".into());
    }
    let mode = req.mode.unwrap_or_else(|| "auto".into());
    if !matches!(mode.as_str(), "auto" | "steer" | "new") {
        bad.push("mode".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    check_file_ids(&req.files)?;
    let refs = super::files::refs(&st, &c.ws, req.files).await?;
    st.domain
        .supplement(&id, &req.text, &mode, refs, &c.me)
        .await?;
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
    let req: ReassignBody = json_body(&b)?;
    ok(Value::Object(
        st.domain.reassign(&id, &req.bot, &c.me).await?,
    ))
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

#[derive(serde::Deserialize)]
struct HandoffBody {
    bot: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    make_bot_default: bool,
}

pub(super) fn parse_handoff_body(b: &Bytes) -> Result<steve_core::HandoffTarget, ApiError> {
    if b.iter().all(|c| c.is_ascii_whitespace()) {
        return Ok(steve_core::HandoffTarget::default());
    }
    let req: HandoffBody = json_body(b)?;
    let mut bad = Vec::new();
    if req.bot.as_deref().is_some_and(|s| !check_len(s, 0, 128)) {
        bad.push("bot".into());
    }
    if req.harness.as_deref().is_some_and(|s| !check_len(s, 0, 64)) {
        bad.push("harness".into());
    }
    if req.model.as_deref().is_some_and(|s| !check_len(s, 0, 200)) {
        bad.push("model".into());
    }
    if req.effort.as_deref().is_some_and(|s| !check_len(s, 0, 32)) {
        bad.push("effort".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let effort = match clean(req.effort) {
        None => None,
        Some(raw) => match steve_types::ReasoningEffort::parse(&raw) {
            Some(e) => Some(e),
            None => return Err(ApiError::Invalid(vec!["effort".into()])),
        },
    };
    Ok(steve_core::HandoffTarget {
        bot: clean(req.bot),
        harness: clean(req.harness),
        model: clean(req.model),
        effort,
        make_bot_default: req.make_bot_default,
    })
}

pub async fn add_dep(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let req: AddDepBody = json_body(&b)?;
    own_item(&st, &c.ws, &req.dep).await?;
    st.domain.add_dep(&id, &req.dep, &c.me).await?;
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
    let req: SnoozeBody = json_body(&b)?;
    let label = req.label.into_option().flatten();
    let until = req.until.into_option().flatten();
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
    let req: LifecycleBody = json_body(&b)?;
    let mut bad = Vec::new();
    if !matches!(
        req.command.as_str(),
        "accept" | "finish" | "cancel" | "fail" | "new_cycle"
    ) {
        bad.push("command".into());
    }
    if req.expected_cycle < 1 {
        bad.push("expected_cycle".into());
    }
    if req.expected_revision < 0 {
        bad.push("expected_revision".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let LifecycleBody {
        command,
        expected_cycle: cycle,
        expected_revision: revision,
        reason,
        goal,
        result,
    } = req;
    if command == "finish" && c.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.item.finish_owner_only"),
        ));
    }
    match st
        .domain
        .lifecycle_command(&id, &command, cycle, revision, reason, goal, result, &c.me)
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
