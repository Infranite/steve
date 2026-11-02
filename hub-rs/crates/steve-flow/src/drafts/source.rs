//! The `source` draft target (signals-automation.md §8): `source:<adapter>`, fields `paused`,
//! `triage_bot` (ref), each event type's `on` (labelled with the type's Chinese name) and each
//! scope's `mode`; one operation `source.patch`, which is `PATCH /sources/:adapter` inside the
//! changeset's transaction and undoes to the settings each connection had.

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use steve_modules::changes::{Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, Plan, Prepared};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::{Change, Viewer, show};
use steve_store::{ChangeEvent, Tx};

use super::{UNRECOGNIZED, bot_name, find_bot, parse_bool, text_of};
use crate::LateFlow;
use crate::sources::{
    self, ScopeMode, ScopeSetting, SourcePatch, TypeSetting, check_patch, connections_of,
    own_adapter, patch_tx,
};

pub const KIND: &str = "source";

/// What the user calls the main bot (`triage_bot = NULL`).
const MAIN_BOT: &str = "主 Bot";
const ON: &str = "开";
const OFF: &str = "关";

pub fn register(r: &mut Registry, late: Arc<LateFlow>) {
    r.target(KIND, Box::new(SourceTarget(late.clone())))
        .op("source.patch", Box::new(PatchOp(late)));
}

fn mode_label(m: ScopeMode) -> &'static str {
    match m {
        ScopeMode::Receive => "接收",
        ScopeMode::Record => "只记录",
        ScopeMode::Ignore => "忽略",
    }
}

fn parse_scope_mode(s: &str) -> Option<ScopeMode> {
    match s {
        "receive" | "接收" => Some(ScopeMode::Receive),
        "record" | "只记录" | "记录" => Some(ScopeMode::Record),
        "ignore" | "忽略" => Some(ScopeMode::Ignore),
        _ => None,
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "是" } else { "否" }
}

fn on_off(b: bool) -> &'static str {
    if b { ON } else { OFF }
}

fn triage_name(c: &Connection, bot: Option<&str>) -> String {
    bot.map_or_else(|| MAIN_BOT.to_string(), |b| bot_name(c, b))
}

/// The state of the adapter's connections `me` sees, as the source page shows it (the first
/// connection's types and triage bot; paused when all are).
struct Current {
    name: String,
    paused: bool,
    triage_bot: Option<String>,
    /// `(key, label, on)` per event type of the adapter.
    types: Vec<(String, String, bool)>,
    /// `(field key, label, connection_id, ext_id, mode)` per scope.
    scopes: Vec<(String, String, String, String, ScopeMode)>,
}

fn current(
    late: &LateFlow,
    c: &Connection,
    ws: &str,
    me: &str,
    adapter: &str,
) -> ApiResult<Current> {
    let cx = late.get()?;
    let a = own_adapter(&cx, adapter)?.clone();
    let conns = connections_of(c, ws, me, adapter)?;
    let sets = conns
        .iter()
        .map(|x| sources::settings(c, ws, &x.id))
        .collect::<ApiResult<Vec<_>>>()?;
    let first = sets.first();
    let types = a
        .event_types()
        .iter()
        .map(|t| {
            let on = first.map_or(t.default_on, |s| s.type_on(t.key, t.default_on));
            (t.key.to_string(), t.label.to_string(), on)
        })
        .collect();
    let many = conns.len() > 1;
    let mut scopes = Vec::new();
    for (conn, s) in conns.iter().zip(&sets) {
        for x in &s.scopes {
            let name = if x.name.is_empty() {
                &x.ext_id
            } else {
                &x.name
            };
            let label = if many {
                format!("范围「{name}」（{}）", conn.name)
            } else {
                format!("范围「{name}」")
            };
            scopes.push((
                format!("scope:{}:{}", conn.id, x.ext_id),
                label,
                conn.id.clone(),
                x.ext_id.clone(),
                x.mode,
            ));
        }
    }
    Ok(Current {
        name: a.name().to_string(),
        paused: !sets.is_empty() && sets.iter().all(|s| s.paused),
        triage_bot: first.and_then(|s| s.triage_bot.clone()),
        types,
        scopes,
    })
}

/// The staleness revision of a source: the revisions of all its connections' settings together.
fn revision_of(c: &Connection, ws: &str, adapter: &str) -> ApiResult<i64> {
    Ok(c.query_row(
        "SELECT coalesce(sum(s.revision), 0) FROM source_settings s JOIN connections c ON c.id = s.connection_id WHERE c.workspace_id = ? AND c.adapter = ?",
        params![ws, adapter],
        |r| r.get(0),
    )?)
}

struct SourceTarget(Arc<LateFlow>);

impl DraftTarget for SourceTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let adapter = id.ok_or_else(|| ApiError::not_found("事件源"))?;
        let cur = current(&self.0, c, &viewer.ws, &viewer.user, adapter)?;
        let mut fields = vec![
            FieldSpec::new("paused", "暂停接收", "enum", json!(yes_no(cur.paused)))
                .values(&["是", "否"]),
            FieldSpec::new(
                "triage_bot",
                "先交给谁判断（Bot 的名字，主 Bot 写“主 Bot”）",
                "ref",
                json!(triage_name(c, cur.triage_bot.as_deref())),
            ),
        ];
        for (key, label, on) in &cur.types {
            fields.push(
                FieldSpec::new(
                    &format!("type:{key}"),
                    &format!("接收「{label}」"),
                    "enum",
                    json!(on_off(*on)),
                )
                .values(&[ON, OFF]),
            );
        }
        for (key, label, _, _, mode) in &cur.scopes {
            fields.push(
                FieldSpec::new(key, label, "enum", json!(mode_label(*mode))).values(&[
                    "接收",
                    "只记录",
                    "忽略",
                ]),
            );
        }
        Ok(Prepared {
            target_id: Some(adapter.to_string()),
            revision: Some(revision_of(c, &viewer.ws, adapter)?),
            title: format!("事件源「{}」", cur.name),
            href: format!("#/auto/source/{adapter}"),
            subject: format!("the event source \"{}\"", cur.name),
            fields,
            ops: vec![],
            free_text: None,
            context: Value::Null,
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        p: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let adapter = p.target_id.as_deref().unwrap_or("");
        let cur = current(&self.0, c, &viewer.ws, &viewer.user, adapter)?;
        let mut plan = Plan::default();
        let mut patch = Map::new();
        let mut types = Map::new();
        let mut scopes = Vec::new();
        for op in raw {
            if op["op"] != "set" {
                plan.changes
                    .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                continue;
            }
            let key = op["key"].as_str().unwrap_or("");
            let said = text_of(&op["value"]).unwrap_or_default();
            let bad = |label: &str| Change::new(label, "—", format!("（没认出：{}）", show(&said)));
            if key == "paused" {
                match parse_bool(&said) {
                    Some(v) => {
                        patch.insert("paused".into(), json!(v));
                        plan.changes
                            .push(Change::new("暂停接收", yes_no(cur.paused), yes_no(v)));
                    }
                    None => plan.changes.push(bad("暂停接收")),
                }
            } else if key == "triage_bot" {
                let to = if op["value"].is_null() || said.is_empty() || said == MAIN_BOT {
                    Some(None)
                } else {
                    find_bot(c, &viewer.ws, &said)?.map(Some)
                };
                match to {
                    Some(b) => {
                        plan.changes.push(Change::new(
                            "先交给谁判断",
                            triage_name(c, cur.triage_bot.as_deref()),
                            triage_name(c, b.as_deref()),
                        ));
                        patch.insert("triage_bot".into(), json!(b));
                    }
                    None => plan.changes.push(bad("先交给谁判断")),
                }
            } else if let Some((tk, label, on)) = key
                .strip_prefix("type:")
                .and_then(|k| cur.types.iter().find(|t| t.0 == k))
            {
                let label = format!("接收「{label}」");
                match parse_bool(&said) {
                    Some(v) => {
                        types.insert(tk.clone(), json!({ "on": v }));
                        plan.changes
                            .push(Change::new(label, on_off(*on), on_off(v)));
                    }
                    None => plan.changes.push(bad(&label)),
                }
            } else if let Some((_, label, conn, ext, mode)) = cur.scopes.iter().find(|s| s.0 == key)
            {
                match parse_scope_mode(&said) {
                    Some(m) => {
                        scopes.push(json!({ "connection_id": conn, "ext_id": ext, "mode": m }));
                        plan.changes
                            .push(Change::new(label, mode_label(*mode), mode_label(m)));
                    }
                    None => plan.changes.push(bad(label)),
                }
            } else {
                plan.changes.push(Change::new(
                    UNRECOGNIZED,
                    "—",
                    show(&format!("{key}: {said}")),
                ));
            }
        }
        if !types.is_empty() {
            patch.insert("types".into(), Value::Object(types));
        }
        if !scopes.is_empty() {
            patch.insert("scopes".into(), Value::Array(scopes));
        }
        if !patch.is_empty() {
            plan.ops
                .push(json!({ "op": "source.patch", "adapter": adapter, "patch": patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(Some(revision_of(c, ws, id)?))
    }

    fn candidates(&self, _: &Connection, _: &Viewer) -> ApiResult<Vec<Candidate>> {
        let cx = self.0.get()?;
        Ok(cx
            .events
            .all()
            .iter()
            .filter(|a| a.caps().contains(&"events"))
            .map(|a| Candidate {
                code: format!("source:{}", a.id()),
                label: format!("事件源 {}", a.name()),
            })
            .collect())
    }
}

struct PatchOp(Arc<LateFlow>);

fn parse_patch(op: &Value) -> ApiResult<SourcePatch> {
    serde_json::from_value(op["patch"].clone()).map_err(|_| ApiError::invalid("patch"))
}

impl ChangeOp for PatchOp {
    fn validate(&self, _: &Connection, _: &OpCx, op: &Value) -> ApiResult<()> {
        let cx = self.0.get()?;
        let a = own_adapter(&cx, op["adapter"].as_str().unwrap_or(""))?;
        check_patch(a.as_ref(), &parse_patch(op)?)
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let flow = self.0.get()?;
        let adapter = op["adapter"].as_str().unwrap_or("");
        let p = parse_patch(op)?;
        let before = connections_of(tx, cx.ws, cx.actor, adapter)?
            .iter()
            .map(|conn| -> ApiResult<Value> {
                let s = sources::settings(tx, cx.ws, &conn.id)?;
                Ok(json!({
                    "connection_id": s.connection_id,
                    "paused": s.paused,
                    "triage_bot": s.triage_bot,
                    "types": s.types,
                    "scopes": s.scopes,
                }))
            })
            .collect::<ApiResult<Vec<_>>>()?;
        patch_tx(tx, cx.ws, cx.actor, adapter, &p, flow.now())?;
        Ok(json!({ "settings": before }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let flow = self.0.get()?;
        let now = flow.now();
        for b in before["settings"].as_array().into_iter().flatten() {
            let id = b["connection_id"].as_str().unwrap_or("");
            // the cursor moves on its own; only what the user set goes back
            let mut s = sources::settings(tx, cx.ws, id)?;
            s.paused = b["paused"].as_bool().unwrap_or(false);
            s.triage_bot = b["triage_bot"].as_str().map(String::from);
            s.types = serde_json::from_value::<BTreeMap<String, TypeSetting>>(b["types"].clone())
                .unwrap_or_default();
            s.scopes = serde_json::from_value::<Vec<ScopeSetting>>(b["scopes"].clone())
                .unwrap_or_default();
            sources::save(tx, &s, now)?;
            tx.publish(ChangeEvent::Connection {
                id: id.into(),
                ws: cx.ws.into(),
            });
        }
        steve_store::base::audit(
            tx,
            cx.ws,
            cx.actor,
            "source.patch",
            op["adapter"].as_str().unwrap_or(""),
            &json!({ "undo": true }),
        )?;
        Ok(())
    }
}
