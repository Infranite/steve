//! `/api/bots*` and `/api/memories*`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use super::misc::{SMART, js_num};
use super::{ApiResult, AppState, Ctx, body, bot_out, objs, ok, ok_true, own, own_bot, str_of};
use crate::backend::Obj;
use crate::error::ApiError;
use crate::js;
use crate::validate::{Check, Num, Str};

const HARNESSES: &[&str] = &["claude-code", "codex"];
const ACCEPT: &[&str] = &["inherit", "manual", "auto"];
const PERM: &[&str] = &["default", "acceptEdits", "auto", "plan"];

/// `BotPatch` (or the create variant: `name` required, `harness` defaulting to `claude-code`), as the
/// parsed object with only the present keys, in schema order.
fn bot_in(v: &Value, create: bool) -> Result<Obj, ApiError> {
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
    let harness = c.opt_enum("harness", HARNESSES);
    put(
        "harness",
        if create {
            Some(json!(harness.unwrap_or_else(|| "claude-code".into())))
        } else {
            harness.map(Value::from)
        },
    );
    put(
        "model",
        c.opt_nullable_str("model", Str::ANY).map(|m| json!(m)),
    );
    put(
        "reasoning_effort",
        c.opt_nullable_str(
            "reasoning_effort",
            Str::pattern("^(none|minimal|low|medium|high|xhigh|max)$"),
        )
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
            return Err(ApiError::not_found("没有这个 Bot"));
        };
        let custom = b.get("prompt").cloned().unwrap_or(Value::Null);
        let mut g = b;
        g.insert("prompt".into(), Value::Null);
        ok(json!({ "generated": d.bot_prompt(&g, &c.ws)?, "custom": custom }))
    })
    .await
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let p = bot_in(&body(&b)?, true)?;
    let (ws, me) = (c.ws.clone(), c.me.clone());
    let (out, id) = st
        .write(move |r| {
            let b = r.bot_create(&c.ws, &p, &c.me)?;
            let out = bot_out(r, b, &c.ws)?;
            let id = out.get("id").and_then(Value::as_str).map(str::to_string);
            Ok((Value::Object(out), id))
        })
        .await?;
    // ownership.md §3: the user who creates a Bot is its 负责人 (best effort: the Bot exists either way)
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
    let prompt = format!(
        "Draft a bot for Steve (a personal assistant app) from this description. Reply in Chinese, ONLY JSON: {{\"name\":\"<= 6 chars\",\"role\":\"one line\",\"persona\":\"how it talks\",\"rules\":\"working rules, a few lines\",\"model\":\"opus|sonnet|haiku\"}}\nDescription: {}",
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
        "新 Bot".into()
    };
    let model = match get("model").map(js::string).as_deref() {
        Some("opus") => json!("claude-opus-5-5"),
        Some("sonnet") => json!("claude-sonnet-5-5"),
        Some("haiku") => json!("claude-haiku-5-5"),
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
    f.insert("harness".into(), json!("claude-code"));
    f.insert("perm_mode".into(), json!("default"));
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
    let p = bot_in(&body(&b)?, false)?;
    st.write(move |r| {
        if let Some(Value::String(dp)) = p.get("default_project")
            && !dp.is_empty()
        {
            own(r.project_get(dp)?, &c.ws, "项目")?;
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
            return Err(ApiError::not_found("没有这个工作空间"));
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
            return Err(ApiError::not_found("没有这个 Bot"));
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
        own(r.memory_get(&id)?, &c.ws, "记忆")?;
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
        own(r.memory_get(&id)?, &c.ws, "记忆")?;
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

    #[test]
    fn bot_patch_validation() {
        assert!(bot_in(&json!({"reasoning_effort": "high"}), false).is_ok());
        assert!(bot_in(&json!({"reasoning_effort": null}), false).is_ok());
        assert_eq!(
            bot_in(&json!({"reasoning_effort": "turbo"}), false).unwrap_err(),
            ApiError::Invalid(vec!["reasoning_effort".into()])
        );
        let e = bot_in(&json!({"name": "", "proactive": 3, "avatar": {"color": "red", "skin": "12", "round": 101}}), false).unwrap_err();
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
        )
        .expect("ok");
        assert_eq!(
            serde_json::to_string(&p).unwrap_or_default(),
            r#"{"name":"A","harness":"claude-code","model":null,"avatar":{"round":70}}"#
        );
        assert_eq!(
            bot_in(&json!({}), true).unwrap_err(),
            ApiError::Invalid(vec!["name".into()])
        );
    }
}
