//! `/api/bots*` and `/api/memories*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use steve_store::seeded;

use super::misc::{SMART, js_num};
use super::{
    ApiResult, AppState, Ctx, Noun, body, bot_out, objs, ok, ok_true, own, own_bot, str_of,
};
use crate::backend::Obj;
use crate::error::ApiError;
use crate::js;
use crate::validate::{Check, Num, Str};

const ACCEPT: &[&str] = &["inherit", "manual", "auto"];
const PERM: &[&str] = &[
    "default",
    "acceptEdits",
    "auto",
    "plan",
    "bypassPermissions",
];

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

fn check_bot_harness_caps(
    p: &Obj,
    target_id: &str,
    harnesses: &[steve_core::harnesses::Harness],
) -> Result<(), ApiError> {
    let Some(target) = harnesses.iter().find(|h| h.id == target_id) else {
        return Ok(());
    };
    let mut bad = Vec::new();
    if let Some(eff) = p.get("reasoning_effort").and_then(Value::as_str)
        && !target.takes_effort(eff)
    {
        bad.push("reasoning_effort".into());
    }
    if let Some(pm) = p.get("perm_mode").and_then(Value::as_str)
        && perm_mode_of_key(pm).is_some_and(|m| !target.offers_mode(m))
    {
        bad.push("perm_mode".into());
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Invalid(bad))
    }
}

/// `BotPatch` (or the create variant: `name` required), as the parsed object with only the present
/// keys, in schema order. `harnesses`: the adapter ids the Hub knows ([`crate::config::Config::harness_ids`]).
fn bot_in(v: &Value, create: bool, harnesses: &[String]) -> Result<Obj, ApiError> {
    let mut c = Check::object(v);
    let mut p = Obj::new();
    let mut put = |k: &str, x: Option<Value>| {
        if let Some(x) = x {
            p.insert(k.into(), x);
        }
    };
    let name = if create {
        Some(c.str("name", Str::len(1, 40)))
    } else {
        c.opt_str("name", Str::len(1, 40))
    };
    put("name", name.map(Value::from));
    put("role", c.opt_str("role", Str::max(200)).map(Value::from));
    let allowed: Vec<&str> = harnesses.iter().map(String::as_str).collect();
    let harness = c.opt_enum("harness", &allowed);
    put("harness", harness.map(Value::from));
    put(
        "model",
        c.opt_nullable_str("model", Str::ANY).map(|m| json!(m)),
    );
    put(
        "reasoning_effort",
        c.opt_nullable_str("reasoning_effort", Str::pattern("^[A-Za-z0-9_.:-]{1,64}$"))
            .map(|m| json!(m)),
    );
    put("rules", c.opt_str("rules", Str::max(8000)).map(Value::from));
    put(
        "persona",
        c.opt_str("persona", Str::max(400)).map(Value::from),
    );
    put(
        "proactive",
        c.opt_num(
            "proactive",
            Num {
                int: true,
                min: Some(0.0),
                max: Some(2.0),
                gt: None,
            },
        )
        .map(js_num),
    );
    put(
        "accept_mode",
        c.opt_enum("accept_mode", ACCEPT).map(Value::from),
    );
    put("perm_mode", c.opt_enum("perm_mode", PERM).map(Value::from));
    if let Some(mut a) = c.sub("avatar", true) {
        let mut av = Obj::new();
        if let Some(x) = a.opt_str("color", Str::pattern(r"^#[0-9a-fA-F]{6}$")) {
            av.insert("color".into(), json!(x));
        }
        if let Some(x) = a.opt_nullable_str("skin", Str::pattern(r"^\d{3,4}$")) {
            av.insert("skin".into(), json!(x));
        }
        if let Some(x) = a.opt_num(
            "round",
            Num {
                int: false,
                min: Some(0.0),
                max: Some(100.0),
                gt: None,
            },
        ) {
            av.insert("round".into(), js_num(x));
        }
        c.absorb(a);
        put("avatar", Some(Value::Object(av)));
    }
    put(
        "default_project",
        c.opt_nullable_str("default_project", Str::ANY)
            .map(|x| json!(x)),
    );
    put(
        "prompt",
        c.opt_nullable_str("prompt", Str::max(20000))
            .map(|x| json!(x)),
    );
    put(
        "device",
        c.opt_nullable_str("device", Str::ANY).map(|x| json!(x)),
    );
    put("enabled", c.opt_bool("enabled").map(Value::from));
    c.finish()?;
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
    r: &dyn crate::backend::Repo,
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
    let harnesses = st.domain.harnesses(false).await?;
    let allowed = known_harness_ids(&st.cfg, &harnesses);
    let mut p = bot_in(&body(&b)?, true, &allowed)?;
    if let Some(h) = p.get("harness").and_then(Value::as_str) {
        check_bot_harness_caps(&p, h, &harnesses)?;
    }
    let (ws, me) = (c.ws.clone(), c.me.clone());
    let (out, id) = st
        .write(move |r| {
            fill_new_bot_defaults(r, &c.ws, &harnesses, &mut p);
            let target_id = p.get("harness").and_then(Value::as_str).unwrap_or_default();
            check_bot_harness_caps(&p, target_id, &harnesses)?;
            let b = r.bot_create(&c.ws, &p, &c.me)?;
            let out = bot_out(r, b, &c.ws)?;
            let id = out.get("id").and_then(Value::as_str).map(str::to_string);
            Ok((Value::Object(out), id))
        })
        .await?;
    // the user who creates a Bot is its 负责人 (best effort: the Bot exists either way)
    if let (Some(m), Some(id)) = (st.modules.clone(), id) {
        let done = tokio::task::spawn_blocking(move || {
            m.store.transaction(|tx| {
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
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let desc = k.str("desc", Str::len(2, 2000));
    k.finish()?;
    let harnesses = st.domain.harnesses(false).await?;
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
        Some(m) if !m.is_empty() && offered.contains(&m) => json!(m),
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
    st.write(move |w| {
        let b = w.bot_create(&c.ws, &f, &c.me)?;
        ok(Value::Object(bot_out(w, b, &c.ws)?))
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
    let harnesses = st.domain.harnesses(false).await?;
    let allowed = known_harness_ids(&st.cfg, &harnesses);
    let mut p = bot_in(&body(&b)?, false, &allowed)?;
    st.write(move |r| {
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
        check_bot_harness_caps(&p, target_id, &harnesses)?;
        if let Some(target_id) = &switched_to
            && let Some(target) = harnesses.iter().find(|h| h.id == *target_id)
        {
            if !p.contains_key("perm_mode") {
                let cur_pm = str_of(&cur, "perm_mode");
                if perm_mode_of_key(&cur_pm).is_some_and(|m| !target.offers_mode(m)) {
                    p.insert("perm_mode".into(), json!(target.default_perm_mode()));
                }
            }
            if !p.contains_key("reasoning_effort")
                && let Some(eff) = cur.get("reasoning_effort").and_then(Value::as_str)
                && !target.takes_effort(eff)
            {
                p.insert("reasoning_effort".into(), Value::Null);
            }
        }
        if let Some(Value::String(dp)) = p.get("default_project")
            && !dp.is_empty()
        {
            own(r.project_get(dp)?, &c.ws, Noun::Project)?;
        }
        let b = r.bot_patch(&id, &p, &c.me)?;
        ok(Value::Object(bot_out(r, b, &c.ws)?))
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
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let ids = k.str_array("ids", Some(1), None);
    k.finish()?;
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
    st.write(move |r| {
        own_bot(r, &id, &c.ws)?;
        let Some(mut b) = r.bot_get(&id)? else {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        };
        let name = format!("{} 副本", str_of(&b, "name"));
        b.insert("name".into(), json!(name));
        b.insert("enabled".into(), json!(false));
        b.insert("main".into(), json!(false));
        let n = r.bot_create(&c.ws, &b, &c.me)?;
        let nid = str_of(&n, "id");
        let mut off = Obj::new();
        off.insert("enabled".into(), json!(false));
        r.bot_patch(&nid, &off, &c.me)?;
        let got = r.bot_get(&nid)?.unwrap_or_default();
        ok(Value::Object(bot_out(r, got, &c.ws)?))
    })
    .await
}

pub async fn delete_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let to = k.str("to", Str::ANY);
    let memory = k
        .opt_enum("memory", &["delete", "export"])
        .unwrap_or_else(|| "export".into());
    k.finish()?;
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
    let mut k = Check::object(v);
    let text = k.str("text", Str::len(1, 2000));
    k.finish()?;
    Ok(text)
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
