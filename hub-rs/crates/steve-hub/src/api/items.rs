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
            create_now(&st2, &c2, goal, title, bot, project, mode).await
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
) -> Result<Value, ApiError> {
    // whatever the user left open (bot, project, title, tags) Steve decides, the same way as in the conversation
    let needs_route = bot.as_deref().is_none_or(str::is_empty)
        || project.is_none()
        || title.as_deref().is_none_or(str::is_empty);
    let r = if needs_route {
        Some(st.domain.route(&c.ws, &goal).await?)
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
    let bot = bot.or_else(|| r.as_ref().map(|r| r.bot.clone()));
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

pub async fn get_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let lifecycle = st.domain.lifecycle_view(&id).await?;
    let workflow = st.domain.workflow_get(&id).await?;
    let dependency_wait = st.domain.dependency_get(&id).await?;
    st.read(move |r| {
        let i = own(r.item_get(&id)?, &c.ws, Noun::Item)?;
        let mut out = summary(r, i)?;
        if let Some(l) = lifecycle {
            out.insert("lifecycle".into(), l);
        }
        out.insert("workflow".into(), workflow);
        out.insert("dependency_wait".into(), dependency_wait);
        out.insert("cycles".into(), objs(r.item_cycles(&id)?));
        out.insert("goalsList".into(), objs(r.item_goals(&id)?));
        out.insert("planRounds".into(), objs(r.item_plan_rounds(&id)?));
        out.insert("timeline".into(), objs(r.timeline_list(&id)?));
        out.insert("artifacts".into(), objs(r.artifacts_list(&id)?));
        out.insert(
            "artifactRoles".into(),
            Value::Object(r.artifact_roles(&id)?),
        );
        out.insert("annotations".into(), objs(r.annotations_list(&id)?));
        out.insert("decisions".into(), objs(r.decisions_of_item(&id)?));
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
    ok(st.domain.dependency_get(&id).await?)
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

pub async fn workflow_get(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    ok(st.domain.workflow_get(&id).await?)
}

pub async fn workflow_change(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let args = body(&b)?;
    // Authenticated user boundary. single_session is deliberately unavailable to Bot/MCP calls.
    ok(st.domain.workflow_change(&id, args, &c.me).await?)
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
        &[
            "pause",
            "resume",
            "finish",
            "unfinish",
            "drop",
            "reopen",
            "archive",
            "unarchive",
        ],
    );
    k.finish()?;
    ok(Value::Object(
        st.domain.item_action(&id, &action, &c.me).await?,
    ))
}

pub async fn bulk(State(st): State<AppState>, Extension(c): Extension<Ctx>, b: Bytes) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let ids = k.str_array("ids", Some(1), Some(200));
    let op = k.enum_("op", &["pause", "resume", "drop", "archive"]);
    k.finish()?;
    let mut n = 0;
    for id in ids {
        let i = id.clone();
        let Some(item) = st.read(move |r| Ok(r.item_get(&i)?)).await? else {
            continue;
        };
        if item.get("workspace_id").and_then(Value::as_str) != Some(c.ws.as_str()) {
            continue;
        }
        let status = str_of(&item, "status");
        let skip = match op.as_str() {
            "pause" => status != "run",
            "resume" => status != "pause",
            "drop" => status == "done" || status == "drop",
            _ => false,
        };
        if skip {
            continue;
        }
        st.domain.item_action(&id, &op, &c.me).await?;
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
    st.domain.revise_goal(&id, &text).await?;
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
                r.timeline_add(&id, me, "me", &text, None)?;
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
                r.timeline_add(&id, me, "me", &format!("移除标签：{}", rm.join("、")), None)?;
            }
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
    st.domain.supplement(&id, &text, &mode, refs).await?;
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
    st.domain.add_dep(&id, &dep).await?;
    ok_true()
}

pub async fn rm_dep(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, dep)): Path<(String, String)>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    st.domain.rm_dep(&id, &dep).await?;
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
        .blocking(move |m| {
            m.store
                .reader(|conn| steve_modules::panels::panels(m, conn, &c.ws, &c.me, &id))
        })
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
    let command = k.enum_("command", &["accept", "cancel", "fail", "new_cycle"]);
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
