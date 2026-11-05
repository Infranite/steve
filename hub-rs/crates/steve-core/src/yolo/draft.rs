//! Conversational YOLO configuration: the draft target `yolo`
//! with the operations `yolo.set {on}` and `yolo.scope {type, id, mode}`. “开 YOLO 到明早 8 点，前端项目
//! 除外” becomes two operations the user confirms; applying them is the same as the switches (audit,
//! revisions, windows) and the changeset undoes them for 24 hours.
//!
//! The user dropped expiry: an `until` the model writes is not kept, and the preview line says so.
//! Like the switch, applying refuses to turn YOLO on while it is forced off or paused.

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::changes::{
    Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared,
};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::{Change, Viewer, audit};
use steve_store::yolo::{self as repo, ITEM, PROJECT, ScopeChange, WORKSPACE};
use steve_store::{Tx, now};

use super::scope::Forced;

/// `target.kind`.
pub const KIND: &str = "yolo";
pub const OP_SET: &str = "yolo.set";
pub const OP_SCOPE: &str = "yolo.scope";

/// Registers the target and its operations; `forced` as the Hub computed it (`STEVE_YOLO=off`,
/// `STEVE_NO_RESUME=1`).
pub fn register(r: &mut Registry, forced: Option<Forced>) {
    r.target(KIND, Box::new(Target))
        .op(OP_SET, Box::new(SetOp { forced }))
        .op(OP_SCOPE, Box::new(ScopeOp));
}

fn on_label(on: bool) -> &'static str {
    if on { "开" } else { "关" }
}

/// [`on_label`] as the card shows it, in the request's language.
fn on_shown(on: bool) -> String {
    if on {
        steve_i18n::t!("core.draft.value.on")
    } else {
        steve_i18n::t!("core.draft.value.off")
    }
}

/// A scope mode as the card shows it, in the request's language.
fn mode_shown(object_type: &str, mode: &str) -> String {
    match (object_type, mode) {
        (_, "full") => steve_i18n::t!("core.yolo.mode.full"),
        (PROJECT, "on") => steve_i18n::t!("core.yolo.mode.project_on"),
        (PROJECT, "off") => steve_i18n::t!("core.yolo.mode.project_off"),
        (_, "on") => steve_i18n::t!("core.yolo.mode.on"),
        (_, "off") => steve_i18n::t!("core.yolo.mode.off"),
        _ => steve_i18n::t!("core.yolo.mode.inherit"),
    }
}

fn ws_on(c: &Connection, ws: &str) -> ApiResult<bool> {
    Ok(repo::scope_get(c, ws, WORKSPACE, ws)?.is_some_and(|r| r.mode == "on" || r.mode == "full"))
}

fn scope_mode(c: &Connection, ws: &str, ty: &str, id: &str) -> ApiResult<Option<String>> {
    Ok(repo::scope_get(c, ws, ty, id)?.map(|r| r.mode))
}

/// `true`/`"on"`/`"开"`… → on.
fn parse_on(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "on" | "true" | "开" | "开启" | "打开" => Some(true),
            "off" | "false" | "关" | "关闭" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn parse_mode(v: &Value) -> Option<&'static str> {
    match v.as_str()?.trim() {
        "on" | "总是" | "放手做" | "开" => Some("on"),
        "off" | "从不" | "我来盯" | "关" | "除外" => Some("off"),
        "inherit" | "跟随" => Some("inherit"),
        _ => None,
    }
}

struct Target;

impl DraftTarget for Target {
    fn prepare(&self, c: &Connection, viewer: &Viewer, _id: Option<&str>) -> ApiResult<Prepared> {
        let ws = viewer.ws.as_str();
        let on = ws_on(c, ws)?;
        let projects: Vec<Value> = {
            let mut st = c.prepare_cached(
                "SELECT id, name FROM projects WHERE workspace_id = ? AND archived = 0 ORDER BY name",
            )?;
            let rows = st.query_map([ws], |r| {
                Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)? }))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let names: Vec<&str> = projects.iter().filter_map(|p| p["name"].as_str()).collect();
        Ok(Prepared {
            target_id: Some(ws.to_string()),
            revision: Some(repo::scope_get(c, ws, WORKSPACE, ws)?.map(|r| r.revision).unwrap_or(0)),
            title: steve_i18n::t!("core.yolo.draft.title"),
            href: "#/yolo".into(),
            subject: "YOLO mode (Steve decides for the user while they are away; there is no end time — it stays on until switched off)".into(),
            fields: vec![FieldSpec::new("on", "YOLO", "enum", json!(on_label(on))).values(&["开", "关"])],
            ops: vec![
                OpSpec::new(OP_SET, "{\"on\": true|false} — switch YOLO for the whole workspace (no end time)"),
                OpSpec::new(
                    OP_SCOPE,
                    &format!(
                        "{{\"type\": \"project\"|\"item\", \"name\": <project name ({}) or item id>, \"mode\": \"on\"|\"off\"|\"inherit\"}} — a project or item that YOLO always / never handles, or follows the workspace",
                        names.join("、")
                    ),
                ),
            ],
            free_text: None,
            context: json!({ "projects": projects }),
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        p: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let ws = viewer.ws.as_str();
        let mut plan = Plan::default();
        let mut on_now = ws_on(c, ws)?;
        for op in raw {
            let name = op["op"].as_str().unwrap_or("");
            match name {
                "set" | OP_SET => {
                    let v = if name == "set" {
                        &op["value"]
                    } else {
                        &op["on"]
                    };
                    let Some(on) = parse_on(v) else {
                        plan.changes.push(Change::new(
                            "YOLO",
                            "—",
                            steve_i18n::t!("core.yolo.draft.unrecognized_value", value = v),
                        ));
                        continue;
                    };
                    let to = if on && (op.get("until").is_some_and(|u| !u.is_null())) {
                        steve_i18n::t!("core.yolo.draft.on_no_end")
                    } else {
                        on_shown(on)
                    };
                    plan.changes.push(Change::new("YOLO", on_shown(on_now), to));
                    plan.ops.push(json!({ "op": OP_SET, "on": on }));
                    if on && !on_now {
                        let open: i64 = c
                            .prepare_cached("SELECT count(*) FROM decisions WHERE workspace_id = ? AND status = 'open'")?
                            .query_row([ws], |r| r.get(0))?;
                        plan.changes.push(Change::new(
                            steve_i18n::t!("core.yolo.draft.open_cards"),
                            steve_i18n::t!("core.yolo.draft.cards", n = open),
                            steve_i18n::t!("core.yolo.draft.open_cards_stay"),
                        ));
                    }
                    on_now = on;
                }
                OP_SCOPE => {
                    let ty = match op["type"].as_str() {
                        Some(PROJECT) | Some("项目") => PROJECT,
                        Some(ITEM) | Some("事项") => ITEM,
                        _ => {
                            plan.changes.push(Change::new(
                                steve_i18n::t!("core.yolo.draft.unrecognized"),
                                "—",
                                op.to_string(),
                            ));
                            continue;
                        }
                    };
                    let said = op["name"]
                        .as_str()
                        .or(op["id"].as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let Some(mode) = parse_mode(&op["mode"]) else {
                        plan.changes.push(Change::new(
                            steve_i18n::t!("core.yolo.draft.unrecognized"),
                            "—",
                            op.to_string(),
                        ));
                        continue;
                    };
                    let found = if ty == PROJECT {
                        p.context["projects"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|x| {
                                x["name"].as_str() == Some(said.as_str())
                                    || x["id"].as_str() == Some(said.as_str())
                            })
                            .map(|x| {
                                (
                                    x["id"].as_str().unwrap_or("").to_string(),
                                    x["name"].as_str().unwrap_or("").to_string(),
                                )
                            })
                    } else {
                        c.prepare_cached(
                            "SELECT id, title FROM items WHERE workspace_id = ? AND id = ?",
                        )?
                        .query_row([ws, said.as_str()], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })
                        .ok()
                    };
                    let Some((id, label)) = found else {
                        plan.changes.push(Change::new(
                            if ty == PROJECT {
                                steve_i18n::t!("core.yolo.draft.project", name = said)
                            } else {
                                steve_i18n::t!("core.yolo.draft.item", name = said)
                            },
                            "—",
                            steve_i18n::t!("core.yolo.draft.unrecognized_value", value = said),
                        ));
                        continue;
                    };
                    let before = scope_mode(c, ws, ty, &id)?.unwrap_or_else(|| "inherit".into());
                    plan.changes.push(Change::new(
                        if ty == PROJECT {
                            steve_i18n::t!("core.yolo.draft.project_yolo", name = label)
                        } else {
                            steve_i18n::t!("core.yolo.draft.item_yolo", name = label)
                        },
                        mode_shown(ty, &before),
                        mode_shown(ty, mode),
                    ));
                    plan.ops
                        .push(json!({ "op": OP_SCOPE, "type": ty, "id": id, "mode": mode }));
                }
                _ => plan.changes.push(Change::new(
                    steve_i18n::t!("core.yolo.draft.unrecognized"),
                    "—",
                    op.to_string(),
                )),
            }
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, _id: &str) -> ApiResult<Option<i64>> {
        Ok(Some(
            repo::scope_get(c, ws, WORKSPACE, ws)?
                .map(|r| r.revision)
                .unwrap_or(0),
        ))
    }

    fn candidates(&self, _c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(vec![Candidate {
            code: format!("{KIND}:{}", viewer.ws),
            label: "YOLO 模式（用户不在时 Steve 替他做决定；开关、哪些项目或事项除外）".into(),
        }])
    }
}

/// Switches the workspace in `tx` like `PUT /yolo`; returns whether it was on before.
fn switch(tx: &Tx, ws: &str, actor: &str, on: bool) -> ApiResult<bool> {
    let row = repo::scope_get(tx, ws, WORKSPACE, ws)?;
    // a draft never turns full access on; `on` while it is on leaves it as it is
    let was = row
        .as_ref()
        .is_some_and(|r| r.mode == "on" || r.mode == "full");
    if was == on {
        return Ok(was);
    }
    let t = now();
    repo::scope_put(
        tx,
        ws,
        WORKSPACE,
        ws,
        &ScopeChange {
            mode: Some(if on { "on" } else { "off" }),
            ..Default::default()
        },
        actor,
        t,
    )?;
    let mut window = None;
    if on {
        window = Some(repo::window_open(tx, ws, false, t)?.id);
    } else if let Some(w) = repo::window_current(tx, ws)? {
        repo::cancel_scheduled(tx, ws, &w.id, t)?;
        repo::window_close(tx, &w.id, "off", t)?;
        window = Some(w.id);
    }
    audit(
        tx,
        ws,
        actor,
        if on { "yolo.on" } else { "yolo.off" },
        &format!("workspace:{ws}"),
        &json!({ "object_type": WORKSPACE, "object_id": ws, "mode": if on { "on" } else { "off" }, "window": window }),
    )?;
    Ok(was)
}

struct SetOp {
    forced: Option<Forced>,
}

impl ChangeOp for SetOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        if op["on"] == true {
            if let Some(f) = self.forced {
                return Err(ApiError::conflict(f.message()));
            }
            if repo::scope_get(c, cx.ws, WORKSPACE, cx.ws)?.is_some_and(|r| r.paused.is_some()) {
                return Err(ApiError::conflict(steve_i18n::t!("core.yolo.paused")));
            }
        }
        Ok(())
    }
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let on = op["on"].as_bool().ok_or_else(|| ApiError::invalid("on"))?;
        let was = switch(tx, cx.ws, cx.actor, on)?;
        Ok(json!({ "on": was }))
    }
    fn undo(&self, tx: &Tx, cx: &OpCx, _op: &Value, before: &Value) -> ApiResult<()> {
        let on = before["on"].as_bool().unwrap_or(false);
        if on && self.forced.is_some() {
            return Ok(());
        }
        switch(tx, cx.ws, cx.actor, on).map(|_| ())
    }
}

struct ScopeOp;

fn put_scope(tx: &Tx, ws: &str, actor: &str, ty: &str, id: &str, mode: &str) -> ApiResult<()> {
    let (before, after) = repo::scope_put(
        tx,
        ws,
        ty,
        id,
        &ScopeChange {
            mode: Some(mode),
            reason: Some(""),
            ..Default::default()
        },
        actor,
        now(),
    )?;
    if before.as_ref() != Some(&after) {
        audit(
            tx,
            ws,
            actor,
            "yolo.scope",
            &format!("{ty}:{id}"),
            &json!({ "object_type": ty, "object_id": id, "mode": mode }),
        )?;
    }
    Ok(())
}

impl ChangeOp for ScopeOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let id = op["id"].as_str().unwrap_or("");
        let table = match op["type"].as_str() {
            Some(PROJECT) => "projects",
            Some(ITEM) => "items",
            _ => return Err(ApiError::invalid("type")),
        };
        let n: i64 = c
            .prepare_cached(&format!(
                "SELECT count(*) FROM {table} WHERE id = ? AND workspace_id = ?"
            ))?
            .query_row([id, cx.ws], |r| r.get(0))?;
        if n == 0 {
            return Err(ApiError::not_found(if table == "projects" {
                "项目"
            } else {
                "事项"
            }));
        }
        Ok(())
    }
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let (ty, id) = (
            op["type"].as_str().unwrap_or(""),
            op["id"].as_str().unwrap_or(""),
        );
        let mode = op["mode"]
            .as_str()
            .filter(|m| matches!(*m, "on" | "off" | "inherit"))
            .ok_or_else(|| ApiError::invalid("mode"))?;
        let before = scope_mode(tx, cx.ws, ty, id)?;
        put_scope(tx, cx.ws, cx.actor, ty, id, mode)?;
        Ok(json!({ "mode": before }))
    }
    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let (ty, id) = (
            op["type"].as_str().unwrap_or(""),
            op["id"].as_str().unwrap_or(""),
        );
        match before["mode"].as_str() {
            Some(mode) => put_scope(tx, cx.ws, cx.actor, ty, id, mode),
            None => {
                if repo::scope_clear(tx, cx.ws, ty, id, cx.actor)? {
                    audit(
                        tx,
                        cx.ws,
                        cx.actor,
                        "yolo.scope",
                        &format!("{ty}:{id}"),
                        &json!({ "object_type": ty, "object_id": id, "mode": null }),
                    )?;
                }
                Ok(())
            }
        }
    }
}
