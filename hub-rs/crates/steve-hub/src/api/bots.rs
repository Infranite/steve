//! `/api/bots*` and `/api/memories*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use steve_store::seeded;

use super::misc::{SMART, js_num};
use super::{
    ApiResult, AppState, Ctx, Noun, Patch, body, bot_out, check_len, from_body, json_body, objs,
    ok, ok_true, own, own_bot, str_of,
};
use crate::error::ApiError;
use crate::js;
use crate::wire::store::{Obj, StoreBackend};

#[derive(serde::Deserialize)]
struct AvatarIn {
    color: Option<String>,
    #[serde(default)]
    skin: Patch<String>,
    round: Option<f64>,
}

#[derive(serde::Deserialize)]
struct BotIn {
    name: Option<String>,
    role: Option<String>,
    specialties: Option<Vec<String>>,
    harness: Option<String>,
    #[serde(default)]
    model: Patch<String>,
    #[serde(default)]
    reasoning_effort: Patch<String>,
    rules: Option<String>,
    persona: Option<String>,
    proactive: Option<i64>,
    accept_mode: Option<String>,
    perm_mode: Option<String>,
    avatar: Option<AvatarIn>,
    #[serde(default)]
    default_project: Patch<String>,
    #[serde(default)]
    prompt: Patch<String>,
    #[serde(default)]
    device: Patch<String>,
    enabled: Option<bool>,
}

#[derive(serde::Deserialize)]
struct DraftBody {
    desc: String,
}

#[derive(serde::Deserialize)]
struct WorkspacesBody {
    ids: Vec<String>,
}

#[derive(serde::Deserialize)]
struct DeleteBody {
    to: String,
    memory: Option<String>,
}

#[derive(serde::Deserialize)]
struct TextBody {
    text: String,
}

fn valid_effort(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

fn valid_hex_color(s: &str) -> bool {
    s.len() == 7 && s.as_bytes()[0] == b'#' && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

fn valid_skin(s: &str) -> bool {
    (3..=4).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

const ACCEPT: &[&str] = &["inherit", "manual", "auto"];
const PERM: &[&str] = &["default", "plan", "bypassPermissions"];

fn perm_mode_of_key(k: &str) -> Option<steve_node_api::PermissionMode> {
    use steve_node_api::PermissionMode as P;
    Some(match k {
        "plan" => P::Plan,
        "default" => P::Ask,
        "acceptEdits" => P::AcceptEdits,
        "auto" => P::Auto,
        "bypassPermissions" => P::FullAccess,
        _ => return None,
    })
}

fn known_harness_ids(
    cfg: &crate::config::Config,
    harnesses: &[steve_core::harnesses::Harness],
) -> Vec<String> {
    let mut ids = cfg.harness_ids();
    for h in harnesses {
        if !ids.contains(&h.id) {
            ids.push(h.id.clone());
        }
    }
    ids
}

fn selection_out(mut bot: Obj, harnesses: &[steve_core::harnesses::Harness]) -> Obj {
    let explicit = ["model", "reasoning_effort"]
        .iter()
        .any(|k| bot.get(*k).is_some_and(|v| !v.is_null()));
    let known = harnesses
        .iter()
        .find(|h| Some(h.id.as_str()) == bot.get("harness").and_then(Value::as_str))
        .is_some_and(|h| h.ready == Some(true) && !h.selection_unknown);
    let state = if !explicit {
        "default"
    } else if !known {
        "unknown"
    } else if steve_core::models::validate_bot_patch(None, &bot, harnesses).is_ok() {
        "supported"
    } else {
        "unsupported"
    };
    bot.insert("selection_validation".into(), json!(state));
    bot
}

/// `BotPatch` (or the create variant: `name` required), as the parsed object with only the present
/// keys, in schema order. `harnesses`: the adapter ids the Hub knows ([`crate::config::Config::harness_ids`]).
#[cfg(test)]
#[path = "bot_selection_tests.rs"]
mod selection_tests;
fn bot_in(v: &Value, create: bool, harnesses: &[String]) -> Result<Obj, ApiError> {
    let req: BotIn = from_body(v.clone())?;
    let mut bad = Vec::new();
    let mut p = Obj::new();
    match req.name {
        Some(n) if check_len(&n, 1, 40) => {
            p.insert("name".into(), json!(n));
        }
        Some(_) => bad.push("name".into()),
        None if create => bad.push("name".into()),
        None => {}
    }
    if let Some(r) = req.role {
        if check_len(&r, 0, 200) {
            p.insert("role".into(), json!(r));
        } else {
            bad.push("role".into());
        }
    }
    if let Some(tags) = req.specialties {
        if tags.len() > 12 || tags.iter().any(|t| !check_len(t, 1, 48)) {
            bad.push("specialties".into());
        } else {
            p.insert(
                "specialties".into(),
                json!(steve_store::bot_dispatch::specialties(tags)),
            );
        }
    }
    if let Some(h) = req.harness {
        if harnesses.iter().any(|id| id == &h) {
            p.insert("harness".into(), json!(h));
        } else {
            bad.push("harness".into());
        }
    }
    if let Some(m) = req.model.into_option() {
        p.insert("model".into(), json!(m));
    }
    if let Some(m) = req.reasoning_effort.into_option() {
        if m.as_deref().is_some_and(|s| !valid_effort(s)) {
            bad.push("reasoning_effort".into());
        } else {
            p.insert("reasoning_effort".into(), json!(m));
        }
    }
    if let Some(r) = req.rules {
        if check_len(&r, 0, 8000) {
            p.insert("rules".into(), json!(r));
        } else {
            bad.push("rules".into());
        }
    }
    if let Some(x) = req.persona {
        if check_len(&x, 0, 400) {
            p.insert("persona".into(), json!(x));
        } else {
            bad.push("persona".into());
        }
    }
    if let Some(n) = req.proactive {
        if (0..=2).contains(&n) {
            p.insert("proactive".into(), json!(n));
        } else {
            bad.push("proactive".into());
        }
    }
    if let Some(a) = req.accept_mode {
        if ACCEPT.contains(&a.as_str()) {
            p.insert("accept_mode".into(), json!(a));
        } else {
            bad.push("accept_mode".into());
        }
    }
    if let Some(pm) = req.perm_mode {
        if PERM.contains(&pm.as_str()) {
            p.insert("perm_mode".into(), json!(pm));
        } else {
            bad.push("perm_mode".into());
        }
    }
    if let Some(a) = req.avatar {
        let mut av = Obj::new();
        if let Some(x) = a.color {
            if valid_hex_color(&x) {
                av.insert("color".into(), json!(x));
            } else {
                bad.push("avatar.color".into());
            }
        }
        if let Some(x) = a.skin.into_option() {
            if x.as_deref().is_some_and(|s| !valid_skin(s)) {
                bad.push("avatar.skin".into());
            } else {
                av.insert("skin".into(), json!(x));
            }
        }
        if let Some(x) = a.round {
            if (0.0..=100.0).contains(&x) {
                av.insert("round".into(), js_num(x));
            } else {
                bad.push("avatar.round".into());
            }
        }
        p.insert("avatar".into(), Value::Object(av));
    }
    if let Some(x) = req.default_project.into_option() {
        p.insert("default_project".into(), json!(x));
    }
    if let Some(x) = req.prompt.into_option() {
        if x.as_deref().is_some_and(|s| !check_len(s, 0, 20000)) {
            bad.push("prompt".into());
        } else {
            p.insert("prompt".into(), json!(x));
        }
    }
    if let Some(x) = req.device.into_option() {
        p.insert("device".into(), json!(x));
    }
    if let Some(e) = req.enabled {
        p.insert("enabled".into(), json!(e));
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    // a seeded text the page showed in its reader's language and sent back unchanged stays the seed
    seeded::store(&mut p, seeded::BOT);
    Ok(p)
}

async fn check_bot(st: &AppState, ws: &str, id: &str) -> Result<(), ApiError> {
    let (id, ws) = (id.to_string(), ws.to_string());
    st.read(move |r| Ok(own_bot(r, &id, &ws)?)).await
}

pub async fn prompt(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let d = st.domain.clone();
    st.read(move |r| {
        own_bot(r, &id, &c.ws)?;
        let Some(b) = r.bot_get(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        let custom = b.get("prompt").cloned().unwrap_or(Value::Null);
        let mut g = b;
        g.insert("prompt".into(), Value::Null);
        ok(json!({ "generated": d.bot_prompt(&g, &c.ws)?, "custom": custom }))
    })
    .await
}

/// Picks `(harness, perm_mode)` for a newly created bot when omitted: prefers `ws`'s main bot's
/// harness if ready/unknown, else the first ready harness, and picks a compatible `perm_mode`.
fn fill_new_bot_defaults(
    r: &StoreBackend,
    ws: &str,
    harnesses: &[steve_core::harnesses::Harness],
    p: &mut Obj,
) {
    let preferred = r
        .bots_list(Some(ws))
        .ok()
        .and_then(|bs| {
            bs.into_iter().find(|b| {
                b.get("main").and_then(Value::as_bool) == Some(true)
                    || b.get("main").and_then(Value::as_i64) == Some(1)
            })
        })
        .and_then(|b| b.get("harness").and_then(Value::as_str).map(str::to_string));
    let (def_harness, def_perm) =
        steve_core::harnesses::default_bot_choice(harnesses, preferred.as_deref());
    let h = p
        .get("harness")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            p.insert("harness".into(), json!(def_harness));
            def_harness
        });
    if !p.contains_key("perm_mode") {
        let pm = harnesses
            .iter()
            .find(|x| x.id == h)
            .map(|x| x.default_perm_mode().to_string())
            .unwrap_or(def_perm);
        p.insert("perm_mode".into(), json!(pm));
    }
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let harnesses = st.domain.bot_harnesses(None).await?;
    let allowed = known_harness_ids(&st.cfg, &harnesses);
    let mut p = bot_in(&body(&b)?, true, &allowed)?;
    let (ws, me) = (c.ws.clone(), c.me.clone());
    let (out, id) = st
        .write(move |r| {
            fill_new_bot_defaults(r, &c.ws, &harnesses, &mut p);
            let target_id = p.get("harness").and_then(Value::as_str).unwrap_or_default();
            if let Some(target) = harnesses.iter().find(|h| h.id == target_id)
                && let Some(pm) = p.get("perm_mode").and_then(Value::as_str)
                && perm_mode_of_key(pm).is_some_and(|m| !target.offers_bot_mode(m))
            {
                return Err(ApiError::Invalid(vec!["perm_mode".into()]));
            }
            steve_core::models::validate_bot_patch(None, &p, &harnesses)
                .map_err(ApiError::bad_request)?;
            let b = r.bot_create(&c.ws, &p, &c.me)?;
            let out = selection_out(bot_out(r, b, &c.ws)?, &harnesses);
            let id = out.get("id").and_then(Value::as_str).map(str::to_string);
            Ok((Value::Object(out), id))
        })
        .await?;
    // the user who creates a Bot is its 负责人 (best effort: the Bot exists either way)
    if let (Some(m), Some(id)) = (st.modules.clone(), id) {
        let done = tokio::task::spawn_blocking(move || {
            m.store.write(|tx| {
                steve_modules::contacts::stewards::adopt(tx, &ws, "bot", &id, Some(&me), "seed")
            })
        })
        .await;
        if let Ok(Err(e)) = done {
            tracing::warn!(error = %e, "bot create: no default 负责人");
        }
    }
    ok(out)
}

/// “描述给 Steve，让它起草”: the model fills the bot's fields from one sentence.
pub async fn draft(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: DraftBody = json_body(&b)?;
    if !check_len(&req.desc, 2, 2000) {
        return Err(ApiError::Invalid(vec!["desc".into()]));
    }
    let desc = req.desc;
    let harnesses = st.domain.bot_harnesses(None).await?;
    let ws_pref = c.ws.clone();
    let preferred = st
        .read(move |r| {
            Ok(r.bots_list(Some(&ws_pref))?
                .into_iter()
                .find(|b| {
                    b.get("main").and_then(Value::as_bool) == Some(true)
                        || b.get("main").and_then(Value::as_i64) == Some(1)
                })
                .and_then(|b| b.get("harness").and_then(Value::as_str).map(str::to_string)))
        })
        .await?;
    let (harness_id, perm_mode) =
        steve_core::harnesses::default_bot_choice(&harnesses, preferred.as_deref());
    let offered: Vec<String> = harnesses
        .iter()
        .find(|h| h.id == harness_id)
        .map(|h| h.model_ids())
        .unwrap_or_default();
    let model_hint = if offered.is_empty() {
        "null".to_string()
    } else {
        format!(
            "one of {} or null",
            serde_json::to_string(&offered).unwrap_or_else(|_| "[]".into())
        )
    };
    let prompt = format!(
        "Draft a bot for Steve (a personal assistant app) from this description. Reply in Chinese, ONLY JSON: {{\"name\":\"<= 6 chars\",\"role\":\"one line\",\"persona\":\"how it talks\",\"rules\":\"working rules, a few lines\",\"model\":{model_hint}}}\nDescription: {}",
        serde_json::to_string(&desc).unwrap_or_default()
    );
    let reply = st
        .domain
        .claude_print("draft", &c.ws, &prompt, SMART, 90_000)
        .await?;
    let r = js::first_json(&reply)?.unwrap_or_else(|| json!({}));
    let get = |k: &str| r.get(k).filter(|x| !x.is_null());
    let name = if js::truthy(r.get("name")) {
        js::string(&r["name"])
    } else {
        steve_i18n::t!("hub.bot.draft_name")
    };
    let model = match get("model").map(js::string) {
        Some(m) if !m.is_empty() => json!(m),
        _ => Value::Null,
    };
    let mut f = Obj::new();
    f.insert("name".into(), json!(js::slice(&name, 20)));
    f.insert(
        "role".into(),
        json!(
            get("role")
                .map(js::string)
                .unwrap_or_else(|| js::slice(&desc, 40))
        ),
    );
    f.insert(
        "persona".into(),
        json!(get("persona").map(js::string).unwrap_or_default()),
    );
    f.insert(
        "rules".into(),
        json!(get("rules").map(js::string).unwrap_or_else(|| desc.clone())),
    );
    f.insert("model".into(), model);
    f.insert("harness".into(), json!(harness_id));
    f.insert("perm_mode".into(), json!(perm_mode));
    let harnesses = st.domain.bot_harnesses(None).await?;
    st.write(move |w| {
        steve_core::models::validate_bot_patch(None, &f, &harnesses)
            .map_err(ApiError::bad_request)?;
        let b = w.bot_create(&c.ws, &f, &c.me)?;
        ok(Value::Object(selection_out(
            bot_out(w, b, &c.ws)?,
            &harnesses,
        )))
    })
    .await
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    check_bot(&st, &c.ws, &id).await?;
    let harnesses = st.domain.bot_harnesses(Some(&id)).await?;
    let allowed = known_harness_ids(&st.cfg, &harnesses);
    let mut p = bot_in(&body(&b)?, false, &allowed)?;
    st.write(move |r| {
        own_bot(r, &id, &c.ws)?;
        let Some(cur) = r.bot_get(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        let cur_h = str_of(&cur, "harness");
        let switched_to = p
            .get("harness")
            .and_then(Value::as_str)
            .filter(|h| !h.is_empty() && *h != cur_h.as_str())
            .map(str::to_string);
        let target_id = switched_to.as_deref().unwrap_or(&cur_h);
        if let Some(target) = harnesses.iter().find(|h| h.id == target_id)
            && let Some(pm) = p.get("perm_mode").and_then(Value::as_str)
            && perm_mode_of_key(pm).is_some_and(|m| !target.offers_bot_mode(m))
        {
            return Err(ApiError::Invalid(vec!["perm_mode".into()]));
        }
        if let Some(target_id) = &switched_to
            && let Some(target) = harnesses.iter().find(|h| h.id == *target_id)
            && !p.contains_key("perm_mode")
        {
            let cur_pm = str_of(&cur, "perm_mode");
            if perm_mode_of_key(&cur_pm).is_some_and(|m| !target.offers_bot_mode(m)) {
                p.insert("perm_mode".into(), json!(target.default_perm_mode()));
            }
        }
        if let Some(Value::String(dp)) = p.get("default_project")
            && !dp.is_empty()
        {
            own(r.project_get(dp)?, &c.ws, Noun::Project)?;
        }
        let b = r.bot_patch_checked(&id, &p, &c.me, &harnesses)?;
        ok(Value::Object(selection_out(
            bot_out(r, b, &c.ws)?,
            &harnesses,
        )))
    })
    .await
}

pub async fn set_workspaces(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    check_bot(&st, &c.ws, &id).await?;
    let req: WorkspacesBody = json_body(&b)?;
    if req.ids.is_empty() {
        return Err(ApiError::Invalid(vec!["ids".into()]));
    }
    let ids = req.ids;
    st.write(move |r| {
        let mine: Vec<String> = r
            .workspaces_of_user(&c.me)?
            .iter()
            .map(|w| str_of(w, "id"))
            .collect();
        if ids.iter().any(|x| !mine.contains(x)) {
            return Err(ApiError::not_found(steve_i18n::t!(
                "hub.not_found.workspace"
            )));
        }
        r.bot_set_workspaces(&id, &ids)?;
        ok_true()
    })
    .await
}

pub async fn duplicate(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    check_bot(&st, &c.ws, &id).await?;
    let harnesses = st.domain.bot_harnesses(Some(&id)).await?;
    st.write(move |r| {
        own_bot(r, &id, &c.ws)?;
        let Some(mut b) = r.bot_get(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        let name = format!("{} 副本", str_of(&b, "name"));
        b.insert("name".into(), json!(name));
        b.insert("enabled".into(), json!(false));
        b.insert("main".into(), json!(false));
        steve_core::models::validate_bot_patch(None, &b, &harnesses)
            .map_err(ApiError::bad_request)?;
        let n = r.bot_create(&c.ws, &b, &c.me)?;
        let nid = str_of(&n, "id");
        let mut off = Obj::new();
        off.insert("enabled".into(), json!(false));
        r.bot_patch(&nid, &off, &c.me)?;
        let got = r.bot_get(&nid)?.unwrap_or_default();
        ok(Value::Object(selection_out(
            bot_out(r, got, &c.ws)?,
            &harnesses,
        )))
    })
    .await
}

pub async fn delete_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: DeleteBody = json_body(&b)?;
    let memory = req.memory.unwrap_or_else(|| "export".into());
    if !matches!(memory.as_str(), "delete" | "export") {
        return Err(ApiError::Invalid(vec!["memory".into()]));
    }
    let to = req.to;
    let (i, t, ws) = (id.clone(), to.clone(), c.ws.clone());
    let mem = st
        .read(move |r| {
            own_bot(r, &i, &ws)?;
            own_bot(r, &t, &ws)?;
            let mut all = vec![];
            for w in r.bot_workspaces(&i)? {
                all.extend(r.memories_list(&i, &w)?);
            }
            Ok(all)
        })
        .await?;
    let mut out = st.domain.delete_bot(&id, &to, &c.me).await?;
    if memory == "delete" {
        let i = id.clone();
        st.write(move |r| Ok(r.memories_delete_of_bot(&i)?)).await?;
    }
    let exported: Vec<Value> = if memory == "export" {
        mem.iter()
            .map(|m| json!({ "text": m.get("text"), "source": m.get("source"), "created": m.get("created") }))
            .collect()
    } else {
        vec![]
    };
    out.insert("memories".into(), Value::Array(exported));
    ok(Value::Object(out))
}

pub async fn restore(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        own_bot(r, &id, &c.ws)?;
        r.bot_restore(&id)?;
        ok_true()
    })
    .await
}

pub async fn memories(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.read(move |r| {
        own_bot(r, &id, &c.ws)?;
        ok(objs(r.memories_list(&id, &c.ws)?))
    })
    .await
}

fn memory_text(v: &Value) -> Result<String, ApiError> {
    let req: TextBody = from_body(v.clone())?;
    if !check_len(&req.text, 1, 2000) {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    Ok(req.text)
}

pub async fn memory_add(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let text = memory_text(&body(&b)?)?;
    st.write(move |r| {
        own_bot(r, &id, &c.ws)?;
        ok(json!({ "id": r.memory_add(&id, &c.ws, &text, "你手动添加")? }))
    })
    .await
}

pub async fn memory_edit(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let text = memory_text(&body(&b)?)?;
    st.write(move |r| {
        own(r.memory_get(&id)?, &c.ws, Noun::Memory)?;
        r.memory_edit(&id, &text)?;
        ok_true()
    })
    .await
}

pub async fn memory_remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        own(r.memory_get(&id)?, &c.ws, Noun::Memory)?;
        r.memory_remove(&id)?;
        ok_true()
    })
    .await
}

pub async fn memories_clear(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    st.write(move |r| {
        r.memories_clear(&c.ws)?;
        ok_true()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Vec<String> {
        vec!["claude-code".into(), "codex".into(), "mine".into()]
    }

    #[test]
    fn bot_harness_is_any_known_adapter() {
        assert!(bot_in(&json!({"harness": "mine"}), false, &ids()).is_ok());
        assert_eq!(
            bot_in(&json!({"harness": "unknown"}), false, &ids()).unwrap_err(),
            ApiError::Invalid(vec!["harness".into()])
        );
    }

    #[test]
    fn bot_patch_validation() {
        assert!(bot_in(&json!({"reasoning_effort": "high"}), false, &ids()).is_ok());
        assert!(bot_in(&json!({"reasoning_effort": "turbo"}), false, &ids()).is_ok());
        assert!(bot_in(&json!({"reasoning_effort": null}), false, &ids()).is_ok());
        assert_eq!(
            bot_in(&json!({"reasoning_effort": "bad effort!"}), false, &ids()).unwrap_err(),
            ApiError::Invalid(vec!["reasoning_effort".into()])
        );
        // Membership is checked against the harness at write time, not a fixed enum here.
        assert!(bot_in(&json!({"reasoning_effort": "ultra"}), false, &ids()).is_ok());
        let e = bot_in(&json!({"name": "", "proactive": 3, "avatar": {"color": "red", "skin": "12", "round": 101}}), false, &ids()).unwrap_err();
        assert_eq!(
            e,
            ApiError::Invalid(vec![
                "name".into(),
                "proactive".into(),
                "avatar.color".into(),
                "avatar.skin".into(),
                "avatar.round".into()
            ])
        );
        let p = bot_in(
            &json!({"name": "A", "extra": 1, "model": null, "avatar": {"round": 70.0, "x": 1}}),
            true,
            &ids(),
        )
        .expect("ok");
        assert_eq!(
            serde_json::to_string(&p).unwrap_or_default(),
            r#"{"name":"A","model":null,"avatar":{"round":70}}"#
        );
        assert_eq!(
            bot_in(&json!({}), true, &ids()).unwrap_err(),
            ApiError::Invalid(vec!["name".into()])
        );
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/bots", post(bots::create))
        .route("/bots/draft", post(bots::draft))
        .route(
            "/bots/{id}",
            patch(bots::patch_one).delete(bots::delete_one),
        )
        .route("/bots/{id}/prompt", get(bots::prompt))
        .route("/bots/{id}/workspaces", put(bots::set_workspaces))
        .route("/bots/{id}/duplicate", post(bots::duplicate))
        .route("/bots/{id}/restore", post(bots::restore))
        .route(
            "/bots/{id}/memories",
            get(bots::memories).post(bots::memory_add),
        )
        .route("/memories", delete(bots::memories_clear))
        .route(
            "/memories/{id}",
            patch(bots::memory_edit).delete(bots::memory_remove),
        )
}
