//! `/api/bots*` and `/api/memories*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use steve_store::seeded;

use super::misc::{SMART, js_num};
use super::{
    ApiResult, AppState, Ctx, Noun, Patch, body, bot_out, check_len, from_body, json_body, objs,
    ok, ok_true, own, own_bot, str_of, user_workspace_ids,
};
use crate::error::ApiError;
use crate::js;
use crate::wire::store::{Obj, StoreBackend};
use steve_types::ConfigReason;

#[derive(serde::Deserialize)]
struct AvatarIn {
    color: Option<String>,
    round: Option<f64>,
    #[serde(default)]
    accessory: Patch<String>,
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
    fallback_surfaces: Option<Value>,
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

fn valid_hex_color(s: &str) -> bool {
    s.len() == 7 && s.as_bytes()[0] == b'#' && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

const ACCEPT: &[&str] = &["inherit", "manual", "auto"];

use steve_core::harnesses::perm_mode_of_key;

fn record_workspace_bot_refusal(reason: &'static str) {
    metrics::counter!(
        "steve_security_refusals_total",
        "boundary" => "workspace_bot",
        "reason" => reason
    )
    .increment(1);
}

fn is_bot_owner(owner: Option<&str>, caller: &Ctx) -> bool {
    match owner.filter(|s| !s.is_empty()) {
        Some(owner) => owner == caller.me,
        None => caller.user.get("role").and_then(Value::as_str) == Some("owner"),
    }
}

fn check_default_project(r: &StoreBackend, ws: &str, project_id: &str) -> Result<(), ApiError> {
    let belongs = r
        .project_get(project_id)?
        .and_then(|p| {
            p.get("workspace_id")
                .and_then(Value::as_str)
                .map(|w| w == ws)
        })
        .unwrap_or(false);
    if !belongs {
        record_workspace_bot_refusal("foreign_default_project");
        return Err(ApiError::Contract(
            ConfigReason::bot_project_outside_workspace(
                "default_project does not belong to the current workspace",
            ),
        ));
    }
    Ok(())
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
    let explicit = bot.get("model").is_some_and(|v| !v.is_null())
        || bot
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .is_some_and(|v| v != steve_types::ReasoningEffort::Default.as_str());
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
        match m.as_deref() {
            None | Some("") => {
                p.insert(
                    "reasoning_effort".into(),
                    json!(steve_types::ReasoningEffort::Default),
                );
            }
            Some(s) => match steve_types::ReasoningEffort::parse(s) {
                Some(e) => {
                    p.insert("reasoning_effort".into(), json!(e));
                }
                None => bad.push("reasoning_effort".into()),
            },
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
        if perm_mode_of_key(&pm).is_some() {
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
        if let Some(x) = a.round {
            if (0.0..=100.0).contains(&x) {
                av.insert("round".into(), js_num(x));
            } else {
                bad.push("avatar.round".into());
            }
        }
        if let Some(x) = a.accessory.into_option() {
            match x.as_deref() {
                None | Some("") => {
                    av.insert("accessory".into(), Value::Null);
                }
                Some(raw) => match steve_types::BotAccessory::parse(raw) {
                    Some(acc) => {
                        av.insert("accessory".into(), json!(acc));
                    }
                    None => bad.push("avatar.accessory".into()),
                },
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
    if let Some(raw_fs) = req.fallback_surfaces {
        let Some(arr) = raw_fs.as_array() else {
            return Err(ApiError::Invalid(vec!["fallback_surfaces".into()]));
        };
        if arr.len() > 8 {
            return Err(ApiError::Invalid(vec!["fallback_surfaces".into()]));
        }
        let mut parsed: Vec<steve_store::ExecutionSurface> = Vec::with_capacity(arr.len());
        for (idx, item) in arr.iter().enumerate() {
            let Some(obj) = item.as_object() else {
                bad.push(format!("fallback_surfaces.{idx}"));
                continue;
            };
            let h = match obj.get("harness").and_then(Value::as_str) {
                Some(h) if harnesses.iter().any(|id| id == h) => h.to_string(),
                _ => {
                    bad.push(format!("fallback_surfaces.{idx}.harness"));
                    String::new()
                }
            };
            let m = match obj.get("model") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if check_len(s, 0, 200) => {
                    let trimmed = s.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                }
                _ => {
                    bad.push(format!("fallback_surfaces.{idx}.model"));
                    None
                }
            };
            let e = match obj.get("reasoning_effort") {
                None | Some(Value::Null) => steve_types::ReasoningEffort::Default,
                Some(Value::String(s)) if s.trim().is_empty() => {
                    steve_types::ReasoningEffort::Default
                }
                Some(Value::String(s)) => match steve_types::ReasoningEffort::parse(s.trim()) {
                    Some(effort) => effort,
                    None => {
                        bad.push(format!("fallback_surfaces.{idx}.reasoning_effort"));
                        steve_types::ReasoningEffort::Default
                    }
                },
                _ => {
                    bad.push(format!("fallback_surfaces.{idx}.reasoning_effort"));
                    steve_types::ReasoningEffort::Default
                }
            };
            if !h.is_empty() {
                let surface = steve_store::ExecutionSurface {
                    harness: h,
                    model: m,
                    reasoning_effort: e,
                };
                if !parsed.contains(&surface) {
                    parsed.push(surface);
                }
            }
        }
        p.insert("fallback_surfaces".into(), json!(parsed));
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

/// Validates `fields` against `harnesses`, creates the Bot (with steward adoption in the same
/// transaction via [`StoreBackend::bot_create`]), and returns its API representation.
fn create_bot_in(
    r: &StoreBackend,
    ws: &str,
    author: &str,
    fields: &Obj,
    harnesses: &[steve_core::harnesses::Harness],
) -> ApiResult {
    steve_core::models::validate_bot_perm_mode(fields, harnesses)
        .map_err(|f| ApiError::Invalid(vec![f.into()]))?;
    steve_core::models::validate_bot_patch(None, fields, harnesses)
        .map_err(ApiError::bad_request)?;
    if let Some(Value::String(dp)) = fields.get("default_project")
        && !dp.is_empty()
    {
        check_default_project(r, ws, dp)?;
    }
    let b = r.bot_create(ws, fields, author)?;
    let allowed_ws = user_workspace_ids(r, author)?;
    ok(Value::Object(selection_out(
        bot_out(r, b, ws, &allowed_ws)?,
        harnesses,
    )))
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let harnesses = st.domain.bot_harnesses(None).await?;
    let allowed = known_harness_ids(&st.cfg, &harnesses);
    let mut p = bot_in(&body(&b)?, true, &allowed)?;
    st.write(move |r| {
        fill_new_bot_defaults(r, &c.ws, &harnesses, &mut p);
        create_bot_in(r, &c.ws, &c.me, &p, &harnesses)
    })
    .await
}

fn draft_prompt(desc: &str, offered: &[String]) -> String {
    let model_hint = if offered.is_empty() {
        "null".to_string()
    } else {
        format!(
            "one of {} or null",
            serde_json::to_string(offered).unwrap_or_else(|_| "[]".into())
        )
    };
    let accessory_ids: Vec<&str> = steve_types::BotAccessory::ALL
        .iter()
        .map(|a| a.as_str())
        .collect();
    let accessory_cues: Vec<&str> = steve_types::BotAccessory::ALL
        .iter()
        .map(|a| a.prompt_cue())
        .collect();
    format!(
        "Draft a bot for Steve (a personal assistant app) from this description. Reply in Chinese, ONLY JSON: {{\"name\":\"<= 6 chars\",\"role\":\"one line\",\"persona\":\"how it talks\",\"rules\":\"working rules, a few lines\",\"model\":{model_hint},\"accessory\":one of {} or null}}\nAccessory catalog: {}\nPick the single best-matching accessory id for the bot's role (or null if none fits).\nDescription: {}",
        serde_json::to_string(&accessory_ids).unwrap_or_else(|_| "[]".into()),
        accessory_cues.join(", "),
        serde_json::to_string(desc).unwrap_or_default()
    )
}

fn draft_fields_from_reply(
    reply: &str,
    desc: &str,
    harness_id: &str,
    perm_mode: &str,
) -> Result<Obj, ApiError> {
    let r = js::first_json(reply)?.unwrap_or_else(|| json!({}));
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
    let accessory = get("accessory")
        .and_then(Value::as_str)
        .and_then(steve_types::BotAccessory::parse);
    let mut f = Obj::new();
    f.insert("name".into(), json!(js::slice(&name, 20)));
    f.insert(
        "role".into(),
        json!(
            get("role")
                .map(js::string)
                .unwrap_or_else(|| js::slice(desc, 40))
        ),
    );
    f.insert(
        "persona".into(),
        json!(get("persona").map(js::string).unwrap_or_default()),
    );
    f.insert(
        "rules".into(),
        json!(
            get("rules")
                .map(js::string)
                .unwrap_or_else(|| desc.to_string())
        ),
    );
    f.insert("model".into(), model);
    f.insert("harness".into(), json!(harness_id));
    f.insert("perm_mode".into(), json!(perm_mode));
    if let Some(acc) = accessory {
        f.insert("avatar".into(), json!({ "accessory": acc }));
    }
    Ok(f)
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
    let prompt = draft_prompt(&desc, &offered);
    let reply = st
        .domain
        .claude_print("draft", &c.ws, &prompt, SMART, 90_000)
        .await?;
    let f = draft_fields_from_reply(&reply, &desc, &harness_id, &perm_mode)?;
    let harnesses = st.domain.bot_harnesses(None).await?;
    st.write(move |w| create_bot_in(w, &c.ws, &c.me, &f, &harnesses))
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
        steve_core::models::reconcile_bot_perm_mode(Some(&cur), &mut p, &harnesses)
            .map_err(|f| ApiError::Invalid(vec![f.into()]))?;
        if let Some(Value::String(dp)) = p.get("default_project")
            && !dp.is_empty()
        {
            check_default_project(r, &c.ws, dp)?;
        }
        let b = r.bot_patch_checked(&id, &p, &c.me, &harnesses)?;
        let allowed_ws = user_workspace_ids(r, &c.me)?;
        ok(Value::Object(selection_out(
            bot_out(r, b, &c.ws, &allowed_ws)?,
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
    let req: WorkspacesBody = json_body(&b)?;
    if req.ids.is_empty() {
        return Err(ApiError::Invalid(vec!["ids".into()]));
    }
    let ids = req.ids;
    st.write(move |r| {
        let Some(bot) = r.bot_get(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        if !is_bot_owner(bot.get("owner").and_then(Value::as_str), &c) {
            record_workspace_bot_refusal("not_bot_owner");
            return Err(ApiError::Contract(ConfigReason::bot_owner_required(
                "only the bot owner can change workspace memberships",
            )));
        }
        let mine = user_workspace_ids(r, &c.me)?;
        if ids.iter().any(|x| !mine.contains(x)) {
            record_workspace_bot_refusal("foreign_workspace");
            return Err(ApiError::not_found(steve_i18n::t!(
                "hub.not_found.workspace"
            )));
        }
        // Keep memberships in workspaces the caller is not a member of untouched.
        let mut next: Vec<String> = r
            .bot_workspaces(&id)?
            .into_iter()
            .filter(|w| !mine.contains(w))
            .collect();
        for w in ids {
            if !next.contains(&w) {
                next.push(w);
            }
        }
        r.bot_set_workspaces(&id, &next)?;
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
        b.insert(
            "fallback_surfaces".into(),
            json!(r.bot_fallback_surfaces(&id)?),
        );
        if let Some(Value::String(dp)) = b.get("default_project")
            && !dp.is_empty()
            && check_default_project(r, &c.ws, dp).is_err()
        {
            b.insert("default_project".into(), Value::Null);
        }
        create_bot_in(r, &c.ws, &c.me, &b, &harnesses)
    })
    .await
}

/// Owner-only global deletion of a bot. Refuses non-owners and refuses if the bot is still a
/// member of any workspace the caller does not belong to or where the heir is absent.
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
    let (i, t, caller) = (id.clone(), to.clone(), c.clone());
    let (mem, bot_wss, mine_vec) = st
        .read(move |r| {
            let Some(bot) = r.bot_get(&i)? else {
                return Err(ApiError::not_found(Noun::Bot.missing()));
            };
            if !is_bot_owner(bot.get("owner").and_then(Value::as_str), &caller) {
                record_workspace_bot_refusal("not_bot_owner");
                return Err(ApiError::Contract(ConfigReason::bot_owner_required(
                    "only the bot owner can globally delete this bot",
                )));
            }
            let mine = user_workspace_ids(r, &caller.me)?;
            let bot_wss = r.bot_workspaces(&i)?;
            if bot_wss.iter().any(|w| !mine.contains(w)) {
                record_workspace_bot_refusal("cross_workspace_delete");
                return Err(ApiError::Contract(ConfigReason::bot_foreign_workspaces(
                    "bot belongs to workspaces you are not a member of",
                )));
            }
            if t == i || r.bot_get(&t)?.is_none() {
                return Err(ApiError::not_found(Noun::Bot.missing()));
            }
            for w in &bot_wss {
                if !r.bot_in_workspace(&t, w)? {
                    record_workspace_bot_refusal("heir_outside_workspace");
                    return Err(ApiError::Contract(
                        ConfigReason::bot_heir_outside_workspace(
                            "handover bot must belong to every affected workspace",
                        ),
                    ));
                }
            }
            let mut all = vec![];
            for w in &bot_wss {
                all.extend(r.memories_list(&i, w)?);
            }
            Ok((all, bot_wss, mine.into_iter().collect::<Vec<_>>()))
        })
        .await?;
    let mut out = st.domain.delete_bot(&id, &to, &c.me, mine_vec).await?;
    if memory == "delete" {
        let i = id.clone();
        st.write(move |r| {
            for w in &bot_wss {
                r.memories_delete_of_bot_in_workspace(&i, w)?;
            }
            Ok(())
        })
        .await?;
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

/// Workspace-scoped removal: detaches a bot from the caller's current workspace (`c.ws`),
/// hands over open items in `c.ws` to `to`, and exports/deletes memories only in `c.ws`.
pub async fn remove_from_workspace(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let req: DeleteBody = json_body(&b)?;
    let memory = req.memory.unwrap_or_else(|| "export".into());
    if !matches!(memory.as_str(), "delete" | "export" | "keep") {
        return Err(ApiError::Invalid(vec!["memory".into()]));
    }
    let to = req.to;
    let (i, t, ws) = (id.clone(), to.clone(), c.ws.clone());
    let mem = st
        .read(move |r| {
            own_bot(r, &i, &ws)?;
            if t == i || r.bot_get(&t)?.is_none() {
                return Err(ApiError::not_found(Noun::Bot.missing()));
            }
            if !r.bot_in_workspace(&t, &ws)? {
                record_workspace_bot_refusal("heir_outside_workspace");
                return Err(ApiError::Contract(
                    ConfigReason::bot_heir_outside_workspace(
                        "handover bot must belong to the current workspace",
                    ),
                ));
            }
            Ok(r.memories_list(&i, &ws)?)
        })
        .await?;
    let mut out = st
        .domain
        .remove_bot_from_workspace(&c.ws, &id, &to, &c.me)
        .await?;
    if memory == "delete" {
        let (i, ws) = (id.clone(), c.ws.clone());
        st.write(move |r| Ok(r.memories_delete_of_bot_in_workspace(&i, &ws)?))
            .await?;
    }
    let exported: Vec<Value> = if memory == "export" {
        mem.iter()
            .map(|m| json!({ "text": m.get("text"), "source": m.get("source"), "created": m.get("created") }))
            .collect()
    } else {
        vec![]
    };
    out.insert("memories".into(), Value::Array(exported));
    out.insert("removed_workspace".into(), json!(c.ws));
    ok(Value::Object(out))
}

pub async fn restore(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        let Some(owner) = r.bot_owner(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        if !is_bot_owner(owner.as_deref(), &c) {
            record_workspace_bot_refusal("not_bot_owner");
            return Err(ApiError::Contract(ConfigReason::bot_owner_required(
                "only the bot owner can restore this bot",
            )));
        }
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
        let p = bot_in(
            &json!({"fallback_surfaces": [
                {"harness": "codex", "model": "gpt-5.4", "reasoning_effort": "high"},
                {"harness": "codex", "model": "gpt-5.4", "reasoning_effort": "high"},
                {"harness": "mine", "model": null}
            ]}),
            false,
            &ids(),
        )
        .expect("fallback surfaces ok");
        assert_eq!(p["fallback_surfaces"].as_array().unwrap().len(), 2);
        assert_eq!(
            bot_in(
                &json!({"fallback_surfaces": [{"harness": "unknown"}]}),
                false,
                &ids(),
            )
            .unwrap_err(),
            ApiError::Invalid(vec!["fallback_surfaces.0.harness".into()])
        );
    }

    #[test]
    fn bot_patch_validation() {
        for ok in ["default", "low", "medium", "high", "max"] {
            assert!(
                bot_in(&json!({"reasoning_effort": ok}), false, &ids()).is_ok(),
                "{ok}"
            );
        }
        assert_eq!(
            bot_in(&json!({"reasoning_effort": null}), false, &ids()).unwrap()["reasoning_effort"],
            json!("default")
        );
        // Only Steve's five canonical levels are accepted; native/arbitrary names are rejected.
        for bad_eff in ["bad effort!", "turbo", "ultra", "xhigh", "minimal"] {
            assert_eq!(
                bot_in(&json!({"reasoning_effort": bad_eff}), false, &ids()).unwrap_err(),
                ApiError::Invalid(vec!["reasoning_effort".into()]),
                "{bad_eff}"
            );
        }
        let e = bot_in(&json!({"name": "", "proactive": 3, "avatar": {"color": "red", "round": 101, "accessory": "magic_wand"}}), false, &ids()).unwrap_err();
        assert_eq!(
            e,
            ApiError::Invalid(vec![
                "name".into(),
                "proactive".into(),
                "avatar.color".into(),
                "avatar.round".into(),
                "avatar.accessory".into(),
            ])
        );
        let p = bot_in(
            &json!({"name": "A", "extra": 1, "model": null, "avatar": {"round": 70.0, "accessory": "loupe", "x": 1}}),
            true,
            &ids(),
        )
        .expect("ok");
        assert_eq!(
            serde_json::to_string(&p).unwrap_or_default(),
            r#"{"name":"A","model":null,"avatar":{"round":70,"accessory":"loupe"}}"#
        );
        let cleared = bot_in(
            &json!({"avatar": {"color": "#8c7ae6", "round": 45, "accessory": null}}),
            false,
            &ids(),
        )
        .expect("clear accessory ok");
        assert_eq!(
            serde_json::to_string(&cleared).unwrap_or_default(),
            r##"{"avatar":{"color":"#8c7ae6","round":45,"accessory":null}}"##
        );

        let prompt = draft_prompt("负责前端视觉设计", &[]);
        for acc in steve_types::BotAccessory::ALL {
            assert!(
                prompt.contains(acc.as_str()),
                "draft prompt lists accessory {}",
                acc.as_str()
            );
        }
        let drafted_ok = draft_fields_from_reply(
            r#"{"name":"设计","role":"视觉走查","persona":"简洁","rules":"先看对比度","model":null,"accessory":"beret"}"#,
            "负责前端视觉设计",
            "claude-code",
            "ask",
        )
        .expect("valid draft");
        assert_eq!(drafted_ok["avatar"], json!({ "accessory": "beret" }));

        let drafted_bad = draft_fields_from_reply(
            r#"{"name":"开发","role":"写代码","persona":"直接","rules":"跑测试","model":null,"accessory":"spaceship"}"#,
            "负责写代码",
            "claude-code",
            "ask",
        )
        .expect("invalid accessory ignored cleanly without guessing");
        assert!(drafted_bad.get("avatar").is_none());

        let drafted_null = draft_fields_from_reply(
            r#"{"name":"通用","role":"协助","persona":"","rules":"","model":null,"accessory":null}"#,
            "通用协助",
            "claude-code",
            "ask",
        )
        .expect("null accessory ok");
        assert!(drafted_null.get("avatar").is_none());
        assert_eq!(
            bot_in(&json!({}), true, &ids()).unwrap_err(),
            ApiError::Invalid(vec!["name".into()])
        );
        for mode in ["plan", "ask", "accept_edits", "auto", "full_access"] {
            assert!(bot_in(&json!({"perm_mode": mode}), false, &ids()).is_ok());
        }
        for legacy in ["default", "acceptEdits", "bypassPermissions", "dontAsk"] {
            assert_eq!(
                bot_in(&json!({"perm_mode": legacy}), false, &ids()).unwrap_err(),
                ApiError::Invalid(vec!["perm_mode".into()])
            );
        }
    }

    #[test]
    fn bot_owner_and_refusal_metrics_enforce_contract() {
        let mut owner_user = Obj::new();
        owner_user.insert("role".into(), json!("owner"));
        let owner_ctx = Ctx {
            user: owner_user,
            me: "usr_alice".into(),
            ws: "ws_1".into(),
            client: None,
        };
        let mut member_user = Obj::new();
        member_user.insert("role".into(), json!("member"));
        let member_ctx = Ctx {
            user: member_user,
            me: "usr_bob".into(),
            ws: "ws_1".into(),
            client: None,
        };
        // Explicit owner wins over instance role.
        assert!(is_bot_owner(Some("usr_alice"), &owner_ctx));
        assert!(!is_bot_owner(Some("usr_bob"), &owner_ctx));
        assert!(is_bot_owner(Some("usr_bob"), &member_ctx));
        assert!(!is_bot_owner(Some("usr_alice"), &member_ctx));
        // Missing owner falls back to instance owner only.
        assert!(is_bot_owner(None, &owner_ctx));
        assert!(!is_bot_owner(None, &member_ctx));

        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            for reason in [
                "foreign_default_project",
                "not_bot_owner",
                "cross_workspace_delete",
                "heir_outside_workspace",
                "foreign_workspace",
            ] {
                record_workspace_bot_refusal(reason);
            }
        });
        let text = handle.render();
        for reason in [
            "foreign_default_project",
            "not_bot_owner",
            "cross_workspace_delete",
            "heir_outside_workspace",
            "foreign_workspace",
        ] {
            assert!(
                text.lines().any(|l| {
                    l.starts_with("steve_security_refusals_total{")
                        && l.contains("boundary=\"workspace_bot\"")
                        && l.contains(&format!("reason=\"{reason}\""))
                        && l.ends_with(" 1")
                }),
                "missing metric workspace_bot/{reason} in:\n{text}"
            );
        }
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
        .route("/bots/{id}/workspace", delete(bots::remove_from_workspace))
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
