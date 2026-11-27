//! Session/bootstrap, workspaces, conversation, decisions, tags, settings, config drafts, devices.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use serde_json::{Value, json};

use super::{
    ApiResult, AppState, Ctx, Noun, body, check_file_ids, check_len, from_body, json_body, objs,
    ok, ok_true, opt_obj, own, set_cookie, str_of,
};
use crate::error::ApiError;
use crate::wire::core::Tier;
use crate::wire::store::Obj;

fn valid_ws_prefix(s: &str) -> bool {
    (1..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase())
}

#[derive(serde::Deserialize)]
struct WorkspaceBody {
    name: Option<String>,
    description: Option<String>,
    item_prefix: Option<String>,
}

#[derive(serde::Deserialize)]
struct SayBody {
    text: String,
    files: Option<Vec<String>>,
    mentions: Option<Value>,
    item: Option<String>,
    reply_to: Option<String>,
    client_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct TextBody {
    text: String,
}

#[derive(serde::Deserialize)]
struct ReadBody {
    at: Option<i64>,
}

#[derive(serde::Deserialize)]
struct RerouteBody {
    to: String,
}

#[derive(serde::Deserialize)]
struct OfferBody {
    take: bool,
    #[serde(default)]
    bot: Option<String>,
}

#[derive(serde::Deserialize)]
struct A2uiBody {
    block: String,
    #[serde(rename = "surfaceId")]
    surface_id: String,
    #[serde(rename = "sourceComponentId")]
    source_component_id: String,
    name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    timestamp: String,
    context: Option<Value>,
    data: Option<Value>,
}

#[derive(serde::Deserialize)]
struct FollowBody {
    follow: bool,
}

#[derive(serde::Deserialize)]
struct ResolveBody {
    revision: Option<i64>,
    choice: String,
    #[serde(default)]
    text: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum TagCat {
    #[serde(rename = "项目")]
    Project,
    #[serde(rename = "类型")]
    Kind,
    #[serde(rename = "领域")]
    Domain,
    #[serde(rename = "客户")]
    Client,
    #[serde(rename = "主题")]
    Topic,
}

#[derive(serde::Deserialize)]
struct TagBody {
    name: Option<String>,
    cat: Option<TagCat>,
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct MergeBody {
    into: String,
}

#[derive(serde::Deserialize)]
struct ConfigTargetIn {
    kind: String,
    id: Option<String>,
    field: Option<String>,
    base: Option<String>,
}

#[derive(serde::Deserialize)]
struct ConfigDraftBody {
    target: ConfigTargetIn,
    text: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum DeviceStatus {
    #[serde(rename = "正常")]
    Normal,
    #[serde(rename = "暂停接活")]
    Paused,
}

#[derive(serde::Deserialize)]
struct DevicePatchBody {
    name: Option<String>,
    status: Option<DeviceStatus>,
    max_sessions: Option<i64>,
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) use crate::js::num_value as js_num;

pub async fn me(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    let version = st.cfg.version.clone();
    let web_version = crate::web::web_version(&st.cfg);
    st.read(move |r| {
        let ws = super::workspaces_out(r.workspaces_of_user(&c.me)?);
        let person = r.member_identity(&c.ws, &c.me)?;
        let mut user = c.user;
        if let Some(ref p) = person {
            user.insert("login".into(), json!(p.login));
            user.insert("name".into(), json!(p.name));
            if let Some(ref pid) = p.party_id {
                user.insert("party_id".into(), json!(pid));
            }
            user.insert(
                "avatar".into(),
                serde_json::to_value(&p.avatar).unwrap_or(Value::Null),
            );
            if let Some(ref sub) = p.subtitle {
                user.insert("subtitle".into(), json!(sub));
            }
        }
        ok(json!({ "user": user, "person": person, "version": version, "web_version": web_version, "workspaces": objs(ws), "workspace": c.ws, "client": c.client }))
    })
    .await
}

/// Content-derived validators keep polling cheap for unchanged resources.
fn conditional_json(headers: &HeaderMap, value: &impl serde::Serialize) -> ApiResult {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let body = serde_json::to_vec(value).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut hash = DefaultHasher::new();
    body.hash(&mut hash);
    let etag = format!("\"{:016x}\"", hash.finish());
    let cache = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, "private, no-cache".into()),
    ];
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return Ok((StatusCode::NOT_MODIFIED, cache).into_response());
    }
    Ok((cache, [(header::CONTENT_TYPE, "application/json")], body).into_response())
}

fn update(st: &AppState, owner: bool) -> Option<UpdateNotice> {
    crate::upgrades::available(&st.cfg).map(|version| UpdateNotice { version, owner })
}

#[derive(serde::Serialize)]
struct UpdateNotice {
    version: String,
    owner: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Shell {
    version: String,
    #[serde(rename = "web_version")]
    web_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateNotice>,
    owner: bool,
    me: Option<steve_types::MemberIdentity>,
    members: Vec<steve_types::MemberIdentity>,
    workspace: Option<Obj>,
    workspaces: Vec<Obj>,
    bots: Vec<Obj>,
    all_bots: Vec<Value>,
    projects: Vec<Obj>,
    tags: Vec<Obj>,
    settings: Obj,
    harness_names: std::collections::BTreeMap<String, String>,
    modules: steve_types::integrations::OptionalModules,
    notify_seen: i64,
    dismissed_alerts: Vec<String>,
}

/// Workspace configuration and the shell's navigation catalogue.
pub async fn shell(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    headers: HeaderMap,
) -> ApiResult {
    let owner = c.user.get("role").and_then(Value::as_str) == Some("owner");
    let update = update(&st, owner);
    let version = st.cfg.version.clone();
    let web_version = crate::web::web_version(&st.cfg);
    let modules = st.optional_modules;
    let harness_names = st
        .cfg
        .harness_ids()
        .into_iter()
        .map(|id| {
            let spec = crate::harnesses::adapter_spec(&id, st.cfg.declared_adapters.get(&id));
            (id, spec.name)
        })
        .collect();
    st.read(move |r| {
        let ws = &c.ws;
        let mut workspace = r.workspace_get(ws)?;
        if let Some(w) = workspace.as_mut() {
            steve_store::seeded::show(w, steve_store::seeded::WORKSPACE);
        }
        let my_workspaces = r.workspaces_of_user(&c.me)?;
        let allowed_ws: std::collections::BTreeSet<String> = my_workspaces
            .iter()
            .map(|w| str_of(w, "id"))
            .filter(|id| !id.is_empty())
            .collect();
        let (bots, all_bots) = super::bots_out(r, ws, &c.me, &allowed_ws)?;
        let mut projects = r.projects_list(ws)?;
        for p in &mut projects {
            steve_store::seeded::show(p, steve_store::seeded::PROJECT);
        }
        let mut settings: Obj = steve_types::settings::defaults()
            .map(|(k, v)| (k.into(), json!(v)))
            .collect();
        settings.extend(r.settings_all(ws)?);
        let members = r.member_identities(ws)?;
        let me = members
            .iter()
            .find(|m| m.user_id.as_str() == c.me)
            .cloned()
            .or(r.member_identity(ws, &c.me)?);
        conditional_json(
            &headers,
            &Shell {
                version,
                web_version,
                update,
                owner,
                me,
                members,
                workspace,
                workspaces: super::workspaces_out(my_workspaces),
                bots,
                all_bots,
                projects,
                tags: r.tags_list(ws)?,
                settings,
                harness_names,
                modules,
                notify_seen: crate::notify::seen(r, ws)?,
                dismissed_alerts: crate::notify::dismissed(r, ws)?,
            },
        )
    })
    .await
}

#[derive(serde::Serialize)]
struct Summary {
    ws: String,
    workspace_name: String,
    version: String,
    web_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateNotice>,
    thinking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<steve_types::ThreadLiveSnapshot>,
    hold: Value,
    local_node_updating: Option<steve_types::devices::LocalNodeUpdating>,
    sessions_running: usize,
    decisions_open: usize,
    devices_online: usize,
    devices_total: usize,
}

/// Current operational counts and the starting values for live conversation signals.
pub async fn summary(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    headers: HeaderMap,
) -> ApiResult {
    let owner = c.user.get("role").and_then(Value::as_str) == Some("owner");
    let ws = c.ws;
    let snap = async {
        if let Some(projection) = st.projection.as_ref() {
            projection.snapshot(&ws).await.map(|s| s.views)
        } else {
            None
        }
    };
    let ws_for_read = ws.clone();
    let facts = st.read(move |r| Ok(r.summary_facts(&ws_for_read)?));
    let (snap, hold, facts) = tokio::join!(snap, st.domain.hold_status(), facts);
    let facts = facts?;
    let live = st.domain.thread_live(&ws);
    if live.is_some() {
        metrics::counter!("steve_thread_live_hydrations_total").increment(1);
    }
    let thinking = facts.pending || live.is_some() || st.domain.thread_active(&ws);
    let workspace_name = steve_store::seeded::shown(
        &facts.workspace_name,
        &[steve_store::seeded::WORKSPACE_NAME],
    )
    .unwrap_or(facts.workspace_name);
    let views = snap.unwrap_or_default();
    let sessions_running = views
        .session
        .unwrap_or_default()
        .iter()
        .filter(|s| s.busy || matches!(s.status.as_str(), "running" | "starting"))
        .count();
    let decisions_open = views
        .decision
        .unwrap_or_default()
        .iter()
        .filter(|d| d.status == steve_types::DecisionStatus::Open)
        .count();
    conditional_json(
        &headers,
        &Summary {
            ws,
            workspace_name,
            version: st.cfg.version.clone(),
            web_version: crate::web::web_version(&st.cfg),
            update: update(&st, owner),
            thinking,
            live,
            hold: hold?,
            local_node_updating: st.nodes.as_ref().and_then(|s| s.local_status.get()),
            sessions_running,
            decisions_open,
            devices_online: facts.devices_online,
            devices_total: facts.devices_total,
        },
    )
}

pub async fn notifications(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    headers: HeaderMap,
) -> ApiResult {
    st.read(move |r| conditional_json(&headers, &crate::notify::list(r, &c.ws, 100)?))
        .await
}

pub async fn runtimes(
    State(st): State<AppState>,
    Query(q): Query<Fresh>,
    headers: HeaderMap,
) -> ApiResult {
    conditional_json(
        &headers,
        &st.runtimes.get(q.fresh.as_deref() == Some("1")).await,
    )
}

#[derive(serde::Deserialize)]
pub struct TagQuery {
    uses: Option<String>,
}

pub async fn tags(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<TagQuery>,
    headers: HeaderMap,
) -> ApiResult {
    st.read(move |r| {
        let uses = (q.uses.as_deref() == Some("1"))
            .then(|| r.tag_uses(&c.ws))
            .transpose()?;
        conditional_json(
            &headers,
            &json!({ "tags": r.tags_list(&c.ws)?, "uses": uses }),
        )
    })
    .await
}

// ── workspaces ──

/// `{name, description?, item_prefix?}`; `name` required unless `partial`.
fn workspace_in(v: &Value, partial: bool) -> Result<Obj, ApiError> {
    let req: WorkspaceBody = from_body(v.clone())?;
    let mut bad = Vec::new();
    let mut p = Obj::new();
    match req.name {
        Some(n) if check_len(&n, 1, 40) => {
            p.insert("name".into(), json!(n));
        }
        Some(_) => bad.push("name".into()),
        None if !partial => bad.push("name".into()),
        None => {}
    }
    if let Some(d) = req.description {
        if check_len(&d, 0, 400) {
            p.insert("description".into(), json!(d));
        } else {
            bad.push("description".into());
        }
    }
    if let Some(x) = req.item_prefix {
        if valid_ws_prefix(&x) {
            p.insert("item_prefix".into(), json!(x));
        } else {
            bad.push("item_prefix".into());
        }
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    steve_store::seeded::store(&mut p, steve_store::seeded::WORKSPACE);
    Ok(p)
}

pub async fn workspace_create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let p = workspace_in(&body(&b)?, false)?;
    st.write(move |r| {
        let w = r.workspace_create(
            &str_of(&p, "name"),
            p.get("description").and_then(Value::as_str),
            p.get("item_prefix").and_then(Value::as_str),
            &c.me,
        )?;
        let id = str_of(&w, "id");
        let mut steve = r.bot_workspaces("steve")?;
        steve.push(id.clone());
        r.bot_set_workspaces("steve", &steve)?;
        r.audit(&c.me, "workspace.create", &id, &Value::Object(p), &id)?;
        ok(Value::Object(w))
    })
    .await
}

pub async fn workspace_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (me, i) = (c.me.clone(), id.clone());
    if !st
        .read(move |r| Ok(r.workspace_is_member(&i, &me)?))
        .await?
    {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.not_found.workspace"
        )));
    }
    let p = workspace_in(&body(&b)?, true)?;
    st.write(move |r| {
        let mut w = r.workspace_patch(&id, &p)?;
        steve_store::seeded::show(&mut w, steve_store::seeded::WORKSPACE);
        ok(Value::Object(w))
    })
    .await
}

pub async fn workspace_switch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult {
    let i = id.clone();
    if !st
        .read(move |r| Ok(r.workspace_is_member(&i, &c.me)?))
        .await?
    {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.not_found.workspace"
        )));
    }
    let cookie = set_cookie(
        "steve_ws",
        &id,
        "/",
        365 * 24 * 3600,
        crate::access::secure_request(&st.cfg, &headers),
    );
    let mut res = ok_true()?;
    if let Ok(v) = cookie.parse() {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    Ok(res.into_response())
}

// ── conversation ──

pub async fn thread_say(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: SayBody = json_body(&b)?;
    let mut bad = Vec::new();
    if !check_len(&req.text, 1, 20000) {
        bad.push("text".into());
    }
    if req.item.as_deref().is_some_and(|s| !check_len(s, 1, 40)) {
        bad.push("item".into());
    }
    if req
        .reply_to
        .as_deref()
        .is_some_and(|s| !check_len(s, 1, 80))
    {
        bad.push("reply_to".into());
    }
    if req
        .client_id
        .as_deref()
        .is_some_and(|s| !check_len(s, 1, 80))
    {
        bad.push("client_id".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    check_file_ids(&req.files)?;
    let SayBody {
        text,
        files,
        mentions,
        item,
        reply_to,
        client_id,
    } = req;
    // `@` mentions (contacts P3): checked like the module does before anything happens
    let mut checked = Vec::new();
    if let Some(ms) = mentions.filter(|v| !v.is_null()) {
        let ms: Vec<steve_modules::contacts::item_people::Mention> =
            match serde_json::from_value(ms) {
                Ok(ms) => ms,
                Err(_) => return Err(ApiError::Invalid(vec!["mentions".into()])),
            };
        if let Err(e) = steve_modules::contacts::item_people::check_mentions(&ms) {
            return Ok(e.into_response());
        }
        checked = ms;
    }
    let refs = super::files::refs(&st, &c.ws, files).await?;
    st.domain
        .say_to(
            &c.ws, &c.me, &text, refs, checked, item, reply_to, client_id,
        )
        .await?;
    ok(json!({ "ok": true }))
}

pub async fn thread_stop(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    ok(json!({ "ok": st.domain.stop(&c.ws).await? }))
}

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    limit: Option<i64>,
}

/// `GET /api/thread/search?q=…`: messages of the conversation that mention `q` or belong to item `q`, newest first.
pub async fn thread_search(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<SearchQuery>,
) -> ApiResult {
    let text = q.q.unwrap_or_default().trim().to_string();
    if text.is_empty() || text.chars().count() > 200 {
        return Err(ApiError::Invalid(vec!["q".into()]));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    st.read(move |r| {
        let list = crate::notify::thread_search(r, &c.ws, &text, limit)?;
        ok(json!({ "messages": objs(list) }))
    })
    .await
}

/// `PATCH /api/thread/queued/:id {text}`: rewrite a message still waiting for the turn before it.
pub async fn thread_queued_edit(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: TextBody = json_body(&b)?;
    if !check_len(&req.text, 1, 20000) {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    if !st.domain.thread_queued_edit(&c.ws, &id, &text).await? {
        return Err(ApiError::Json(409, steve_i18n::t!("hub.queued.gone")));
    }
    ok_true()
}

/// `DELETE /api/thread/queued/:id` → `{text, files}`: withdraw a message still waiting for the turn
/// before it (the composer takes it back).
pub async fn thread_queued_withdraw(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    match st.domain.thread_queued_withdraw(&c.ws, &id).await? {
        Some(m) => ok(Value::Object(m)),
        None => Err(ApiError::Json(409, steve_i18n::t!("hub.queued.gone"))),
    }
}

/// Notifications: everything up to `at` (default now) counts as read.
pub async fn notifications_read(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: ReadBody = json_body(&b)?;
    if req.at.is_some_and(|a| a <= 0) {
        return Err(ApiError::Invalid(vec!["at".into()]));
    }
    let now = now_ms();
    let at = req.at.map(|a| a.min(now)).unwrap_or(now);
    let bus = st.bus.clone();
    st.write(move |r| ok(json!({ "seen": crate::notify::mark_seen(r, &bus, &c.ws, at)? })))
        .await
}

/// Notifications: the user closes one (its id, or its line in the conversation).
pub async fn notifications_dismiss(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        crate::notify::dismiss(r, &c.ws, &id)?;
        ok_true()
    })
    .await
}

pub async fn thread_promote(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    ok(Value::Object(st.domain.promote(&c.ws, &id, &c.me).await?))
}

/// A thread action's answer: the result, or `409 已经处理过了` with the result when it was handled before.
fn thread_result((o, already): (Obj, bool)) -> ApiResult {
    if already {
        let mut v = o;
        v.insert(
            "error".into(),
            json!(steve_i18n::t!("hub.thread.already_handled")),
        );
        return Ok(crate::error::json_response(409, &Value::Object(v)));
    }
    ok(Value::Object(o))
}

/// `POST /api/thread/:id/reroute {to: "inline" | "item" | "item:<ST-n>"}`.
pub async fn thread_reroute(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: RerouteBody = json_body(&b)?;
    let to = req.to;
    if !check_len(&to, 1, 40)
        || !(to == "inline"
            || to == "item"
            || to.strip_prefix("item:").is_some_and(|i| !i.is_empty()))
    {
        return Err(ApiError::Invalid(vec!["to".into()]));
    }
    thread_result(st.domain.reroute(&c.ws, &id, &to, &c.me).await?)
}

/// Maximum length of an explicit Bot identifier accepted by `POST /api/thread/:id/offer`.
const MAX_OFFER_BOT_ID_LEN: usize = 128;

/// `POST /api/thread/:id/offer {take: bool, bot?: string}`.
pub async fn thread_offer(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: OfferBody = json_body(&b)?;
    let bot = req.bot.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if bot.is_some_and(|b| !check_len(b, 1, MAX_OFFER_BOT_ID_LEN)) {
        return Err(ApiError::Invalid(vec!["bot".into()]));
    }
    thread_result(
        st.domain
            .offer(
                &c.ws,
                &id,
                req.take,
                req.take.then_some(bot).flatten(),
                &c.me,
            )
            .await?,
    )
}

/// `POST /api/thread/:id/a2ui` (ST-46): a button of an A2UI interface in Steve's reply. Answers the record
/// with `already: true` when that surface had been submitted, so a second click is harmless.
pub async fn thread_a2ui(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: A2uiBody = json_body(&b)?;
    let mut bad = Vec::new();
    if !check_len(&req.block, 1, 16) {
        bad.push("block".into());
    }
    if !check_len(&req.surface_id, 1, 120) {
        bad.push("surfaceId".into());
    }
    if !check_len(&req.source_component_id, 1, 120) {
        bad.push("sourceComponentId".into());
    }
    if !check_len(&req.name, 1, 64) {
        bad.push("name".into());
    }
    if !check_len(&req.timestamp, 0, 40) {
        bad.push("timestamp".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let action = steve_core::thread::A2uiAction {
        block: req.block,
        surface: req.surface_id,
        component: req.source_component_id,
        name: req.name,
        label: req.label,
        context: req.context.unwrap_or_else(|| json!({})),
        data: req.data.unwrap_or(Value::Null),
        timestamp: req.timestamp,
    };
    ok(Value::Object(
        st.domain.a2ui_action(&c.ws, &c.me, &id, action).await?,
    ))
}

/// `POST /api/thread/runs/:id/stop` and `DELETE /api/thread/runs/:id`: stop a research run or take
/// a queued one off the queue.
pub async fn thread_run_stop(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    ok(json!({ "ok": st.domain.stop_run(&c.ws, &id).await? }))
}

/// `PATCH /api/items/:id/thread {follow: bool}`.
pub async fn item_thread_follow(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: FollowBody = json_body(&b)?;
    if !st.domain.thread_follow(&c.ws, &id, req.follow).await? {
        return Err(ApiError::not_found(Noun::Item.missing()));
    }
    ok_true()
}

// ── decisions ──

fn decision_revision(body: &Value) -> Result<Option<i64>, ApiError> {
    body.get("revision")
        .map(|revision| {
            revision
                .as_i64()
                .filter(|revision| *revision > 0)
                .ok_or_else(|| ApiError::Invalid(vec!["revision".into()]))
        })
        .transpose()
}

async fn settle_decision(
    st: &AppState,
    c: &Ctx,
    id: String,
    revision: Option<i64>,
    command: steve_core::DecisionCommand,
) -> Result<Obj, ApiError> {
    let caller = steve_modules::Caller {
        ws: c.ws.clone(),
        me: c.me.clone(),
    };
    Ok(st
        .domain
        .settle_decision(caller, id, revision, command)
        .await?)
}

pub async fn decision_resolve(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: ResolveBody = json_body(&b)?;
    if req.revision.is_some_and(|r| r <= 0) {
        return Err(ApiError::Invalid(vec!["revision".into()]));
    }
    let command = steve_core::DecisionCommand::Resolve {
        choice: req.choice,
        text: req.text,
    };
    let done = settle_decision(&st, &c, id.clone(), req.revision, command).await?;
    if let Some(revision) = done.get("revision").and_then(Value::as_i64) {
        crate::access::note_via(&st, &id, revision, &c);
    }
    ok(Value::Object(done))
}

pub async fn decision_dismiss(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let revision = if b.is_empty() {
        None
    } else {
        decision_revision(&body(&b)?)?
    };
    let d = settle_decision(&st, &c, id, revision, steve_core::DecisionCommand::Dismiss).await?;
    ok(Value::Object(d))
}

pub async fn decision_undo(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let revision = if b.is_empty() {
        None
    } else {
        decision_revision(&body(&b)?)?
    };
    let d = settle_decision(&st, &c, id, revision, steve_core::DecisionCommand::Undo).await?;
    ok(Value::Object(d))
}

// ── tags ──

/// `TagIn` (or `TagIn.partial()`), as the parsed object with only the present keys.
fn tag_in(v: &Value, partial: bool) -> Result<Obj, ApiError> {
    let req: TagBody = from_body(v.clone())?;
    let mut bad = Vec::new();
    let mut p = Obj::new();
    match req.name {
        Some(n) if check_len(&n, 1, 20) => {
            p.insert("name".into(), json!(n));
        }
        Some(_) => bad.push("name".into()),
        None if !partial => bad.push("name".into()),
        None => {}
    }
    match req.cat {
        Some(cat) => {
            p.insert("cat".into(), json!(cat));
        }
        None if !partial => bad.push("cat".into()),
        None => {}
    }
    match req.description {
        Some(desc) if check_len(&desc, 1, 400) => {
            p.insert("description".into(), json!(desc));
        }
        Some(_) => bad.push("description".into()),
        None if !partial => bad.push("description".into()),
        None => {}
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    Ok(p)
}

pub async fn tag_create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let p = tag_in(&body(&b)?, false)?;
    st.write(move |r| {
        let name = str_of(&p, "name");
        if r.tag_by_name(&c.ws, &name)?.is_some() {
            return Err(ApiError::bad_request(steve_i18n::t!("hub.tag.duplicate")));
        }
        ok(Value::Object(r.tag_create(
            &c.ws,
            &name,
            &str_of(&p, "cat"),
            &str_of(&p, "description"),
            "你",
        )?))
    })
    .await
}

pub async fn tag_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let i = id.clone();
    st.read(move |r| Ok(own(r.tag_get(&i)?, &c.ws, Noun::Tag)?))
        .await?;
    let p = tag_in(&body(&b)?, true)?;
    st.write(move |r| ok(opt_obj(r.tag_patch(&id, &p)?))).await
}

pub async fn tag_remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        own(r.tag_get(&id)?, &c.ws, Noun::Tag)?;
        r.tag_remove(&id)?;
        ok_true()
    })
    .await
}

pub async fn tag_merge(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.tag_get(&i)?, &ws, Noun::Tag)?))
        .await?;
    let req: MergeBody = json_body(&b)?;
    st.write(move |r| {
        own(r.tag_get(&req.into)?, &c.ws, Noun::Tag)?;
        r.tag_merge(&id, &req.into)?;
        ok_true()
    })
    .await
}

// ── settings and conversational configuration ──

pub async fn settings_schema() -> ApiResult {
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        steve_types::settings::schema_json(),
    )
        .into_response())
}

pub async fn settings_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let Value::Object(p) = body(&b)? else {
        return Err(ApiError::Invalid(vec![String::new()]));
    };
    let mut values = Obj::new();
    for (key, val) in &p {
        let s = crate::js::string(val);
        if steve_types::settings::allowed(key, &s) {
            values.insert(key.clone(), json!(s));
        } else {
            return Err(ApiError::bad_request(steve_i18n::t!(
                "hub.settings.bad_key",
                key = key
            )));
        }
    }
    if values.contains_key(steve_store::bot_dispatch::NO_EXECUTOR_SETTING)
        && let Some(store) = super::bot_dispatch::store(&st).cloned()
    {
        let ws = c.ws.clone();
        let me = c.me.clone();
        let owner = tokio::task::spawn_blocking(move || {
            store.read(|r| {
                Ok::<_, steve_store::StoreError>(
                    steve_store::Members::new(r).role(&ws, &me)?.as_deref() == Some("owner"),
                )
            })
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::internal(e.to_string()))?;
        if !owner {
            return Err(ApiError::Json(
                403,
                steve_i18n::t!("hub.dispatch.owner_only"),
            ));
        }
    }
    st.write(move |r| {
        r.settings_set(&c.ws, &values, &c.me)?;
        ok_true()
    })
    .await
}

/// 设置 · 模型: which models each of Steve's own decisions uses, and the agent defaults.
pub async fn models_get(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    ok(st.domain.models_view(&c.ws).await?)
}

/// `{"purposes": {"route": {"models": [...], "effort": "high"} | null}, "agents": {"codex": "id" | null}}`;
/// answers the new view. Takes effect on the next call.
pub async fn models_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let harnesses = st.domain.harnesses(false).await?;
    let values = steve_core::models::validate(&v, &harnesses).map_err(ApiError::bad_request)?;
    st.domain.models_write(&c.ws, &values, &c.me).await?;
    ok(st.domain.models_view(&c.ws).await?)
}

/// Save a default without needing a working model. Steve is shared across workspaces, so only
/// the instance owner can make this change; purpose overrides reset only in the current workspace.
pub async fn models_select(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.onboard.owner_only"))?;
    let choice: steve_core::models::SteveSelection = serde_json::from_value(body(&b)?)
        .map_err(|_| ApiError::bad_request(steve_i18n::t!("hub.models.invalid_selection")))?;
    ok(st.domain.models_select(&c.ws, &choice, &c.me).await?)
}

/// `POST /api/onboard/start`: Steve opens the first meeting in the main conversation (the tour's
/// last step, and "meet again" in the settings). Answers `{message}`, the welcome; one that is on
/// already is left alone. The instance's owner only: the meeting acts on this machine.
pub async fn onboard_start(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.onboard.owner_only"))?;
    let message = st.domain.onboard_start(&c.ws, &c.me).await?;
    ok(json!({"ok": true, "message": message}))
}

/// `POST /api/onboard/skip`: no first meeting, or the end of the one that is on.
pub async fn onboard_skip(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.onboard.owner_only"))?;
    st.domain.onboard_end(&c.ws, &c.me).await?;
    ok_true()
}

#[derive(serde::Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

/// `GET /api/audit?before=<id>&limit=<n>`: what was changed in this workspace and by whom, newest
/// first: `{entries: [{id, ts, actor, action, target, detail}], names: {id: name}, more}`. `names`
/// says what the ids in the page are called now; `before` is the id of the oldest entry already
/// shown. The instance's owner only: the trail names every member, client and token.
pub async fn audit_list(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.audit.owner_only"))?;
    let limit = q.limit.unwrap_or(50);
    st.read(move |r| ok(r.audit_page(&c.ws, &c.me, q.before, limit)?))
        .await
}

pub async fn config_draft(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: ConfigDraftBody = json_body(&b)?;
    let mut kinds = vec!["bot", "project", "settings", "models", "tags", "global"];
    if let Some(m) = &st.modules {
        kinds.extend(m.registry.target_kinds());
    }
    let mut bad = Vec::new();
    if !kinds.contains(&req.target.kind.as_str()) {
        bad.push("target.kind".into());
    }
    if req
        .target
        .field
        .as_deref()
        .is_some_and(|f| !matches!(f, "rules" | "prompt"))
    {
        bad.push("target.field".into());
    }
    if req
        .target
        .base
        .as_deref()
        .is_some_and(|base| !check_len(base, 0, 40000))
    {
        bad.push("target.base".into());
    }
    if !check_len(&req.text, 1, 4000) {
        bad.push("text".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let mut target = Obj::new();
    let kind = req.target.kind;
    target.insert("kind".into(), json!(kind));
    if let Some(id) = req.target.id {
        target.insert("id".into(), json!(id));
    }
    if let Some(f) = req.target.field {
        target.insert("field".into(), json!(f));
    }
    if let Some(base) = req.target.base {
        target.insert("base".into(), json!(base));
    }
    let text = req.text;
    if let Some(m) = st
        .modules
        .as_ref()
        .filter(|m| m.registry.target_kinds().contains(&kind.as_str()))
    {
        let id = target.get("id").and_then(Value::as_str).map(str::to_string);
        let caller = steve_modules::Caller {
            ws: c.ws.clone(),
            me: c.me.clone(),
        };
        return Ok(
            match steve_modules::changes::draft(
                m,
                &caller,
                &kind,
                id.as_deref(),
                &text,
                Default::default(),
            )
            .await
            {
                Ok(p) => super::ok(
                    serde_json::to_value(p).map_err(|e| ApiError::internal(e.to_string()))?,
                )?,
                Err(e) => e.into_response(),
            },
        );
    }
    ok(Value::Object(
        st.domain.config_draft(&c.ws, &target, &text).await?,
    ))
}

/// What to do with a stored module changeset.
#[derive(Clone, Copy)]
enum Act {
    Apply,
    Undo,
    Drop,
}

/// Executes apply, undo or drop against the single persisted `changesets` engine and updates
/// the card in the conversation when it succeeds.
async fn module_change(st: &AppState, c: &Ctx, id: &str, act: Act) -> ApiResult {
    let m = st
        .modules
        .clone()
        .ok_or_else(|| ApiError::internal("modules not configured"))?;
    let (caller, cid) = (
        steve_modules::Caller {
            ws: c.ws.clone(),
            me: c.me.clone(),
        },
        id.to_string(),
    );
    let r = tokio::task::spawn_blocking(steve_i18n::carry(move || match act {
        Act::Apply => steve_modules::changes::apply(&m, &caller, &cid),
        Act::Undo => steve_modules::changes::undo(&m, &caller, &cid),
        Act::Drop => steve_modules::changes::drop_draft(&m, &caller, &cid),
    }))
    .await
    .map_err(|e| ApiError::internal(format!("config change task: {e}")))?;
    Ok(match r {
        Ok(v) => {
            let state = match act {
                Act::Apply => Some("applied"),
                Act::Undo => None,
                Act::Drop => Some("dropped"),
            };
            if let Err(e) = st.domain.config_mark(&c.ws, id, state).await {
                tracing::warn!("config card {id}: {e}");
            }
            crate::error::json_response(200, &v)
        }
        Err(e) => e.into_response(),
    })
}

pub async fn config_apply(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    module_change(&st, &c, &id, Act::Apply).await
}

pub async fn config_undo(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    module_change(&st, &c, &id, Act::Undo).await
}

pub async fn config_drop(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    module_change(&st, &c, &id, Act::Drop).await
}

// ── models a harness offers ──

#[derive(serde::Deserialize)]
pub struct ModelsQuery {
    harness: Option<String>,
    fresh: Option<String>,
}

/// `GET /api/system/models?harness=<adapter>[&fresh=1]`: the models the harness adapter offers
/// (what it reported, else its declared list), grouped by family, with what each ran on this
/// week. Never reads keys.
pub async fn system_models(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<ModelsQuery>,
) -> ApiResult {
    let h = q
        .harness
        .unwrap_or_else(|| steve_harness_spec::DEFAULT_ADAPTER.0.into());
    let harnesses = st.domain.harnesses(q.fresh.as_deref() == Some("1")).await?;
    let list = harnesses
        .iter()
        .find(|x| x.id == h)
        .map(|x| st.catalog.list(x))
        .ok_or_else(|| {
            ApiError::bad_request(steve_i18n::t!("hub.models.no_harness", harness = h))
        })?;
    let used = st
        .domain
        .models_used(&c.ws, &h)
        .await
        .unwrap_or_else(|_| json!({}));
    ok(crate::catalog::with_usage(list, &used))
}

/// `POST /api/system/harnesses` `{id, name?, command, args?, properties?}`: declares a custom
/// harness adapter in the Hub's manifest (the instance's owner only); in force once the Hub
/// restarts, when its Node asks the adapter what it supports. Built-in harness ids and `env`
/// injection are refused.
pub async fn system_harness_add(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    instance_owner(&c)?;
    let h = crate::harnesses::parse_new_harness(body(&b)?)
        .map_err(crate::harnesses::HarnessError::into_api_error)?;
    let path = st.cfg.harness_manifest.clone();
    tokio::task::spawn_blocking(move || crate::harnesses::declare(&path, h))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(crate::harnesses::HarnessError::into_api_error)?;
    ok(json!({"saved": true, "restart": true}))
}

/// Owner-only setup of a registry id on the local Node. No package, command or destination is
/// accepted from the client. The Node journals and executes it asynchronously.
pub async fn system_harness_install(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    instance_owner(&c)?;
    if !b.is_empty() && body(&b)? != json!({}) {
        return Err(ApiError::bad_request(
            "installation takes no package, command or destination parameters",
        ));
    }
    if steve_harness_spec::components(&id).is_empty() || !st.cfg.knows_harness(&id) {
        return Err(ApiError::bad_request(
            "only registered built-in harness components can be installed",
        ));
    }
    st.runtimes.install(id).await.map_err(|e| {
        use steve_node_api::Code;
        let code = match e.code {
            Code::Forbidden => 403,
            Code::Busy | Code::Conflict => 409,
            Code::Unavailable | Code::OutcomeUnknown => 503,
            _ => 400,
        };
        ApiError::Json(code, e.safe_detail)
    })?;
    ok(json!({"accepted": true, "restart": false}))
}

/// `DELETE /api/system/harnesses/:id`: removes a custom adapter from the Hub's manifest (the
/// instance's owner only); in force once the Hub restarts. Built-in harness ids are refused.
pub async fn system_harness_remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    instance_owner(&c)?;
    let path = st.cfg.harness_manifest.clone();
    let removed = tokio::task::spawn_blocking(move || crate::harnesses::undeclare(&path, &id))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(crate::harnesses::HarnessError::into_api_error)?;
    ok(json!({"removed": removed, "restart": true}))
}

// ── task workspaces ──

/// Paths and sizes on this machine are the instance manager's.
fn instance_owner(c: &Ctx) -> Result<(), ApiError> {
    owner_or(c, || steve_i18n::t!("hub.workspaces.owner_only"))
}

fn owner_or(c: &Ctx, why: impl FnOnce() -> String) -> Result<(), ApiError> {
    if c.user.get("role").and_then(Value::as_str) == Some("owner") {
        Ok(())
    } else {
        Err(ApiError::Json(403, why()))
    }
}

/// `GET /api/system/workspaces`: every item's worktree with its size and caches, the disk's free space and
/// thresholds, folders no item points at. Walks the worktrees: seconds for a hundred GB.
pub async fn system_workspaces(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
) -> ApiResult {
    instance_owner(&c)?;
    ok(st.domain.disk_report().await?)
}

/// `POST /api/system/workspaces/reclaim`: one reclaim pass now (what the ten-minute watcher does).
pub async fn system_workspaces_reclaim(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
) -> ApiResult {
    instance_owner(&c)?;
    ok(st.domain.reclaim_workspaces().await?)
}

#[derive(serde::Serialize)]
struct BackupStatus {
    settings: crate::backup::DailySettings,
    available: bool,
    database: std::path::PathBuf,
    dir: std::path::PathBuf,
    #[serde(flatten)]
    inventory: crate::backup::Inventory,
}

/// `GET /api/system/backup`: where the database and its copies are on the Hub's machine, whether the
/// daily copy is on, and the newest one.
pub async fn system_backup(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    instance_owner(&c)?;
    let cfg = st.cfg.clone();
    let store = st
        .modules
        .as_ref()
        .ok_or_else(|| ApiError::internal("storage unavailable"))?
        .store
        .clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<BackupStatus> {
        let settings = crate::backup::daily_settings(&store)?;
        Ok(BackupStatus {
            settings,
            available: cfg.backup && !cfg.diagnostic(),
            database: cfg.db_path(),
            dir: crate::backup::root(&cfg.home),
            inventory: crate::backup::inventory(&cfg.home),
        })
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))?)
}

/// Installation owner settings; stored atomically through the existing Settings repository.
pub async fn system_backup_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    instance_owner(&c)?;
    let settings: crate::backup::DailySettings =
        serde_json::from_value(body(&b)?).map_err(|e| ApiError::bad_request(e.to_string()))?;
    settings
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let store = st
        .modules
        .as_ref()
        .ok_or_else(|| ApiError::internal("storage unavailable"))?
        .store
        .clone();
    tokio::task::spawn_blocking(move || {
        crate::backup::save_daily_settings(&store, settings, &c.me)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(serde_json::to_value(settings).map_err(|e| ApiError::internal(e.to_string()))?)
}

/// An owner-requested manual copy, using the same plan as daily/admin backups.
pub async fn system_backup_now(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
) -> ApiResult {
    instance_owner(&c)?;
    let cfg = st.cfg.clone();
    let manifest = tokio::task::spawn_blocking(move || {
        crate::backup::create(&cfg.home, &cfg.db_path(), crate::backup::Purpose::Manual)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(serde_json::to_value(manifest).map_err(|e| ApiError::internal(e.to_string()))?)
}

/// The outbox's store: only a Hub with its modules wired has one.
fn outbox_store(st: &AppState) -> Result<steve_store::Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("hub.outbox.none")))
}

/// `GET /api/system/outbox`: what the outbox still owes, and the dead deliveries grouped by consumer.
pub async fn system_outbox(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.outbox.owner_only"))?;
    let store = outbox_store(&st)?;
    let (stats, dead) = tokio::task::spawn_blocking(move || {
        Ok::<_, steve_store::StoreError>((
            store.read(|r| steve_store::outbox::stats(r))?,
            store.read(|r| steve_store::outbox::dead(r))?,
        ))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(
        json!({ "pending": stats.pending, "dead": stats.dead, "oldest": stats.oldest, "groups": dead }),
    )
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct HealthQuery {
    window: Option<String>,
}

/// `GET /api/system/health`: the Hub's health over the selected window (`15m`, `1h`, `24h`), each
/// figure with its limit, warnings, and golden-signal time series (`crate::metrics::health`).
/// The instance owner's.
pub(crate) async fn system_health(
    Extension(c): Extension<Ctx>,
    Query(query): Query<HealthQuery>,
) -> Result<axum::Json<crate::metrics::health::Health>, ApiError> {
    owner_or(&c, || steve_i18n::t!("hub.health.owner_only"))?;
    let window = match query.window.as_deref().map(str::trim) {
        None | Some("") => crate::metrics::health::HealthWindow::M15,
        Some(raw) => crate::metrics::health::HealthWindow::parse(raw)
            .ok_or_else(|| ApiError::Invalid(vec!["window".to_string()]))?,
    };
    crate::metrics::health(window)
        .map(axum::Json)
        .ok_or_else(|| ApiError::internal("metrics are not collected"))
}

/// The most samples of each kind one web report carries.
const WEB_SAMPLES: usize = 500;
const WEB_NAV_KEYS_MAX: usize = 64;
const WEB_NAV_COUNT_MAX: u32 = 10_000;

#[derive(Debug, serde::Deserialize)]
struct WebPerfReport {
    page: Option<String>,
    long_tasks: Option<Vec<f64>>,
    interactions: Option<Vec<f64>>,
    patch_apply: Option<Vec<f64>>,
    snapshots: Option<Vec<f64>>,
    transcript_open: Option<Vec<f64>>,
    dom_nodes: Option<u64>,
    nav_opens: Option<std::collections::BTreeMap<String, u32>>,
    errors: Option<std::collections::BTreeMap<String, u32>>,
    sse_reconnects: Option<std::collections::BTreeMap<String, u32>>,
}

/// Validates a web telemetry report and records it into the in-memory metrics pipeline.
/// Returns the bounded menu open counts accepted for `steve_web_nav_opens_total`.
pub(crate) fn record_web_perf(b: &Bytes) -> Result<Vec<(&'static str, u32)>, ApiError> {
    let report: WebPerfReport =
        serde_json::from_slice(b).map_err(|_| ApiError::Invalid(vec!["body".to_string()]))?;
    let seconds = |k: &str, list: Option<Vec<f64>>| -> Result<Vec<f64>, ApiError> {
        let bad = || ApiError::Invalid(vec![k.to_string()]);
        let Some(list) = list else {
            return Ok(vec![]);
        };
        if list.len() > WEB_SAMPLES {
            return Err(bad());
        }
        list.into_iter()
            .map(|ms| {
                (0.0..=60_000.0)
                    .contains(&ms)
                    .then_some(ms / 1000.0)
                    .ok_or_else(bad)
            })
            .collect()
    };
    let nav_opens = report.nav_opens.unwrap_or_default();
    if nav_opens.len() > WEB_NAV_KEYS_MAX
        || nav_opens.values().any(|&count| count > WEB_NAV_COUNT_MAX)
    {
        return Err(ApiError::Invalid(vec!["nav_opens".to_string()]));
    }
    let errors = report.errors.unwrap_or_default();
    if errors.len() > WEB_NAV_KEYS_MAX || errors.values().any(|&count| count > WEB_NAV_COUNT_MAX) {
        return Err(ApiError::Invalid(vec!["errors".to_string()]));
    }
    let sse_reconnects = report.sse_reconnects.unwrap_or_default();
    if sse_reconnects.len() > WEB_NAV_KEYS_MAX
        || sse_reconnects
            .values()
            .any(|&count| count > WEB_NAV_COUNT_MAX)
    {
        return Err(ApiError::Invalid(vec!["sse_reconnects".to_string()]));
    }
    let page = report.page.as_deref().unwrap_or("other");
    Ok(crate::metrics::web_report(
        page,
        &seconds("long_tasks", report.long_tasks)?,
        &seconds("interactions", report.interactions)?,
        &seconds("patch_apply", report.patch_apply)?,
        &seconds("snapshots", report.snapshots)?,
        &seconds("transcript_open", report.transcript_open)?,
        report.dom_nodes.filter(|n| *n <= 1_000_000),
        &nav_opens,
        &errors,
        &sse_reconnects,
    ))
}

/// `POST /api/system/web-perf` `{page?, long_tasks: [ms], interactions: [ms], patch_apply: [ms], transcript_open: [ms], nav_opens?: {item: count}}`:
/// what a signed-in web client measured since its last report (`steve_web_*`), on which page (one
/// of [`crate::metrics::WEB_PAGES`], else `other`). Each list holds at most [`WEB_SAMPLES`]
/// durations of 0 to 60 000 ms; bounded `nav_opens` are also persisted daily in `telemetry_daily`.
pub async fn system_web_perf(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let nav_opens = record_web_perf(&b)?;
    if !nav_opens.is_empty()
        && let Some(store) = st.modules.as_ref().map(|m| m.store.clone())
    {
        let ws = c.ws;
        let at = steve_store::now();
        super::blocking(move || {
            store
                .write(|w| {
                    let tz = steve_stats::refresh::timezone(w, &ws)?;
                    let day = steve_stats::day::day(at, tz);
                    steve_store::statistics::TelemetryDaily(w).record_counts(
                        &ws,
                        &day,
                        steve_store::statistics::TelemetryMetric::WebNavOpen,
                        nav_opens
                            .iter()
                            .map(|&(item, count)| (item, i64::from(count))),
                    )
                })
                .map_err(|e| ApiError::internal(e.to_string()))
        })
        .await?;
    }
    ok_true()
}

/// `POST /api/system/outbox/revive` `{consumer?}`: the dead deliveries (of one consumer, or all) are
/// tried again, with a fresh day before they can be given up on again.
pub async fn system_outbox_revive(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.outbox.owner_only"))?;
    let store = outbox_store(&st)?;
    let v = if b.is_empty() { json!({}) } else { body(&b)? };
    let consumer = match v.get("consumer") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(_) => return Err(ApiError::Invalid(vec!["consumer".into()])),
    };
    let n = tokio::task::spawn_blocking(move || {
        steve_store::outbox::revive(&store, consumer.as_deref())
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(json!({ "revived": n }))
}

// ── devices ──

#[derive(serde::Deserialize)]
pub struct Fresh {
    fresh: Option<String>,
}

fn device_connected(st: &AppState) -> bool {
    st.node.as_ref().is_some_and(|node| {
        node.connection_ready(
            st.nodes
                .as_ref()
                .is_some_and(|s| s.registry.current("local").is_some()),
        )
    })
}

/// The stable `hub` inventory identity is registered by system::register_hub, never inferred
/// from a role/name. Remote inventory identities have no established binding contract yet.
fn device_facts(mut devices: Vec<Obj>, owner: bool, local: bool, connected: bool) -> Vec<Obj> {
    for d in &mut devices {
        let bound = str_of(d, "id") == crate::wire::workspaces::HUB_DEVICE && local;
        d.insert(
            "node_id".into(),
            if bound { json!("local") } else { Value::Null },
        );
        d.insert(
            "connected".into(),
            if bound { json!(connected) } else { Value::Null },
        );
        d.insert("can_configure".into(), json!(owner && bound));
        d.insert(
            "config_reason".into(),
            json!(if !owner {
                "owner_required"
            } else if !bound {
                "unmapped"
            } else {
                ""
            }),
        );
        d.insert(
            "eligibility".into(),
            json!({"state": "unknown", "reason": "workspace_checks_required"}),
        );
        d.insert("occupancy".into(), Value::Null);
    }
    devices
}

pub async fn devices(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    headers: HeaderMap,
) -> ApiResult {
    let local = st.node.is_some();
    let connected = device_connected(&st);
    let owner = c.user.get("role").and_then(Value::as_str) == Some("owner");
    st.read(move |r| {
        conditional_json(
            &headers,
            &json!({ "devices": objs(device_facts(r.devices_list()?, owner, local, connected)) }),
        )
    })
    .await
}

pub async fn device_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    owner_or(&c, || "device_owner_required".into())?;
    if id != crate::wire::workspaces::HUB_DEVICE || st.node.is_none() {
        return Err(ApiError::Json(
            409,
            "device_configuration_unsupported".into(),
        ));
    }
    let connected = device_connected(&st);
    if !connected {
        return Err(ApiError::Json(409, "device_node_disconnected".into()));
    }
    let v = body(&b)?;
    let revision = v
        .get("revision")
        .and_then(Value::as_i64)
        .filter(|r| *r >= 0)
        .ok_or_else(|| ApiError::Json(400, "device_revision_required_refresh_or_upgrade".into()))?;
    let req: DevicePatchBody = from_body(v)?;
    let mut bad = Vec::new();
    let mut p = Obj::new();
    p.insert("revision".into(), json!(revision));
    if let Some(n) = req.name {
        if check_len(&n, 1, 60) {
            p.insert("name".into(), json!(n));
        } else {
            bad.push("name".into());
        }
    }
    if let Some(s) = req.status {
        p.insert("status".into(), json!(s));
    }
    if let Some(m) = req.max_sessions {
        if (1..=steve_store::runtime::SESSIONS_MAX as i64).contains(&m) {
            if id != steve_store::runtime::HUB_DEVICE {
                return Err(ApiError::bad_request(steve_i18n::t!(
                    "hub.devices.limit_node_api"
                )));
            }
            owner_or(&c, || steve_i18n::t!("hub.devices.limit_owner_only"))?;
            p.insert("max_sessions".into(), json!(m));
        } else {
            bad.push("max_sessions".into());
        }
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let store = st
        .modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "device_configuration_unavailable".into()))?;
    super::blocking(move || {
        store.write(|w| {
            // Check current ownership after taking the writer lock.
            let owner = steve_store::UserRows(w).role(&c.me)?.as_deref() == Some("owner");
            if !owner {
                return Err(ApiError::Json(403, "device_owner_required".into()));
            }
            let patch = serde_json::from_value::<steve_store::DevicePatch>(json!(p))
                .map_err(steve_store::StoreError::from)?;
            store.devices().patch(&id, &patch).map_err(|e| match e {
                steve_store::StoreError::Conflict(message) => ApiError::Json(409, message),
                e => e.into(),
            })?;
            // Read the receipt on this transaction's connection, before releasing the
            // lock. Both its revision and permission come from the approved write.
            let d = store
                .devices()
                .list()?
                .into_iter()
                .find(|d| d.id == id)
                .ok_or_else(|| ApiError::not_found("device_not_found"))?;
            let d = serde_json::to_value(d).map_err(steve_store::StoreError::from)?;
            let mut d = d
                .as_object()
                .cloned()
                .ok_or_else(|| ApiError::internal("invalid device"))?;
            let room = steve_store::node_write::session_room(w, steve_store::runtime::HUB_NODE)?;
            d.insert("running".into(), json!(room.running));
            d.insert("waiting".into(), json!(room.waiting));
            ok(json!({"device": device_facts(vec![d], owner, true, connected).remove(0)}))
        })
    })
    .await
}

/// gateway tiers by name, for the route modules.
pub(crate) const SMART: Tier = Tier::Smart;
pub(crate) const FAST: Tier = Tier::Fast;

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/me", get(misc::me))
        .route("/shell", get(misc::shell))
        .route("/summary", get(misc::summary))
        .route("/notifications", get(misc::notifications))
        .route("/runtimes", get(misc::runtimes))
        .route("/workspaces", post(misc::workspace_create))
        .route("/workspaces/{id}", patch(misc::workspace_patch))
        .route("/workspaces/{id}/switch", post(misc::workspace_switch))
        .route("/thread", post(misc::thread_say))
        .route("/thread/stop", post(misc::thread_stop))
        .route("/thread/search", get(misc::thread_search))
        .route(
            "/thread/queued/{id}",
            patch(misc::thread_queued_edit).delete(misc::thread_queued_withdraw),
        )
        .route("/notifications/read", post(misc::notifications_read))
        .route(
            "/notifications/{id}/dismiss",
            post(misc::notifications_dismiss),
        )
        .route("/thread/{id}/promote", post(misc::thread_promote))
        .route("/thread/{id}/reroute", post(misc::thread_reroute))
        .route("/thread/{id}/offer", post(misc::thread_offer))
        .route("/thread/{id}/a2ui", post(misc::thread_a2ui))
        .route("/thread/runs/{id}/stop", post(misc::thread_run_stop))
        .route("/thread/runs/{id}", delete(misc::thread_run_stop))
        .route("/items/{id}/thread", patch(misc::item_thread_follow))
        .route("/decisions/{id}", post(misc::decision_resolve))
        .route("/decisions/{id}/dismiss", post(misc::decision_dismiss))
        .route("/decisions/{id}/undo", post(misc::decision_undo))
        .route("/tags", get(misc::tags).post(misc::tag_create))
        .route(
            "/tags/{id}",
            patch(misc::tag_patch).delete(misc::tag_remove),
        )
        .route("/tags/{id}/merge", post(misc::tag_merge))
        .route("/access", get(crate::access::overview))
        .route(
            "/access/oidc",
            get(crate::access::oidc_get)
                .put(crate::access::oidc_put)
                .delete(crate::access::oidc_delete),
        )
        .route("/access/tokens", post(crate::access::token_create))
        .route("/capabilities/steve", get(crate::caps_api::steve_h))
        .route("/access/tokens/{id}", delete(crate::access::token_revoke))
        .route("/access/clients/{id}", delete(crate::access::client_revoke))
        .route("/access/pairings", post(crate::access::pair_create))
        .route("/access/pairings/{id}", get(crate::access::pair_status))
        .route("/settings/schema", get(misc::settings_schema))
        .route(
            "/settings/models",
            get(misc::models_get).patch(misc::models_patch),
        )
        .route("/settings/models/select", post(misc::models_select))
        .route("/settings", patch(misc::settings_patch))
        .route("/onboard/start", post(misc::onboard_start))
        .route("/onboard/skip", post(misc::onboard_skip))
        .route("/audit", get(misc::audit_list))
        .route("/config/draft", post(misc::config_draft))
        .route("/config/{id}/apply", post(misc::config_apply))
        .route("/config/{id}/undo", post(misc::config_undo))
        .route("/config/{id}/drop", post(misc::config_drop))
        .route("/devices", get(misc::devices))
        .route("/vitals", get(crate::nodes::vitals::get))
        .route("/devices/{id}", patch(misc::device_patch))
        .route("/system/models", get(misc::system_models))
        .route("/system/harnesses", post(misc::system_harness_add))
        .route(
            "/system/harnesses/{id}/install",
            post(misc::system_harness_install),
        )
        .route(
            "/system/harnesses/{id}",
            delete(misc::system_harness_remove),
        )
        .route("/system/workspaces", get(misc::system_workspaces))
        .route(
            "/system/workspaces/reclaim",
            post(misc::system_workspaces_reclaim),
        )
        .route(
            "/system/backup",
            get(misc::system_backup)
                .post(misc::system_backup_now)
                .patch(misc::system_backup_patch),
        )
        .route("/system/outbox", get(misc::system_outbox))
        .route("/system/outbox/revive", post(misc::system_outbox_revive))
        .route("/system/health", get(misc::system_health))
        .route("/system/web-perf", post(misc::system_web_perf))
}
