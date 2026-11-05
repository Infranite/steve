//! Conversational configuration for the module targets: drafts stored in `changesets` as an
//! ordered list of operations `{"op":"<module>.<verb>", …}`, applied in one transaction, undoable for 24
//! hours in reverse order. Modules register a [`DraftTarget`] per `target.kind` and a [`ChangeOp`] per
//! operation; this file owns the flow around them.
//!
//! Operations can refer to what an earlier one in the same changeset created: an op with `"as": "org"`
//! whose apply calls `cx.bind("org", id)` lets a later op write `"$org"` for that id (`org.create` then
//! `job.open`). Values are substituted before `apply`, and the resolved op is what `undo` sees.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::base::changesets::{self, Changeset, NewChangeset};
use steve_store::base::{self, Change, Viewer};
use steve_store::{Tx, now};

use crate::{ApiError, ApiResult, Caller, Modules};

/// Model timeout for drafts (smart tier, 90 s like changes.ts).
pub const DRAFT_TIMEOUT: Duration = Duration::from_secs(90);

/// The TS messages, verbatim.
pub const STALE: &str = "这份草稿已经失效，请重新说一次";
pub const NOT_UNDOABLE: &str = "这次改动已经不能撤销了";

/// [`STALE`] in the request's language, for an API answer.
pub fn stale() -> String {
    steve_i18n::t!("modules.changes.stale")
}

/// [`NOT_UNDOABLE`] in the request's language, for an API answer.
pub fn not_undoable() -> String {
    steve_i18n::t!("modules.changes.not_undoable")
}

/// A field the model may change (`FieldSpec`): `type` is `text | enum | list | ref`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// Shown in Chinese, as the user sees it.
    pub current: Value,
}

impl FieldSpec {
    pub fn new(key: &str, label: &str, ty: &str, current: Value) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ty: ty.into(),
            values: None,
            current,
        }
    }
    pub fn values(mut self, v: &[&str]) -> Self {
        self.values = Some(v.iter().map(|s| s.to_string()).collect());
        self
    }
}

/// An operation the model may emit for this target: name and a one-line parameter description.
#[derive(Debug, Clone, PartialEq)]
pub struct OpSpec {
    pub op: String,
    pub params: String,
}

impl OpSpec {
    pub fn new(op: &str, params: &str) -> Self {
        Self {
            op: op.into(),
            params: params.into(),
        }
    }
}

/// What a target gives the draft flow, after its ownership check.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub target_id: Option<String>,
    /// The target's `revision` now; apply refuses (409) if it changed.
    pub revision: Option<i64>,
    /// Card title (“Sarah Chen 的资料”) and link (`#/contacts/ct_…`).
    pub title: String,
    pub href: String,
    /// How the prompt names the target (`the contact "Sarah Chen"`).
    pub subject: String,
    pub fields: Vec<FieldSpec>,
    pub ops: Vec<OpSpec>,
    /// The key of the free-text field unplaceable content is appended to (“（追加）”).
    pub free_text: Option<String>,
    /// Anything else the target wants back in `plan` (names → ids, …).
    pub context: Value,
}

/// The checked result of a draft: operations to store and the preview lines.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub ops: Vec<Value>,
    pub changes: Vec<Change>,
}

/// One `global` candidate for `pickTarget`: `code` like `contact:ct_…` or `team:new`, label after the dash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub code: String,
    pub label: String,
}

impl Candidate {
    /// The option line changes.ts shows the router: `<code> — <label>`.
    pub fn line(&self) -> String {
        format!("{} — {}", self.code, self.label)
    }
}

/// A `target.kind` of `POST /api/config/draft`, implemented by the module that owns the object.
pub trait DraftTarget: Send + Sync {
    /// Loads the target (`id` from the request; `None` for creating targets like `team:new`), checks
    /// ownership (404), and describes its fields and operations.
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared>;

    /// Turns the model's raw operations into checked ones with preview lines. Names resolve to ids only by
    /// exact name or alias; anything unrecognized becomes a “（没认出：X）” line, never a guess.
    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        prepared: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan>;

    /// The target's current revision (for the staleness check at apply); `None` if it is gone.
    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>>;

    /// `global` candidates for `pickTarget`.
    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let _ = (c, viewer);
        Ok(Vec::new())
    }
}

/// Context of one operation run.
pub struct OpCx<'a> {
    pub ws: &'a str,
    pub actor: &'a str,
    pub changeset_id: &'a str,
    vars: &'a RefCell<BTreeMap<String, Value>>,
}

impl OpCx<'_> {
    /// Makes `"$name"` in later operations of this changeset stand for `value`.
    pub fn bind(&self, name: &str, value: Value) {
        self.vars.borrow_mut().insert(name.into(), value);
    }
}

/// One operation kind (`party.patch`, `org.create`, …).
pub trait ChangeOp: Send + Sync {
    /// Checks the parameters against the data as it is now (references still exist and are visible).
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let _ = (c, cx, op);
        Ok(())
    }
    /// Applies it and returns what `undo` needs to put things back (`before`).
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value>;
    /// Puts back `before`. Runs in reverse order of apply, in one transaction.
    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()>;
}

/// The response of `POST /api/config/draft`, same shape as the in-memory drafts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub id: String,
    pub title: String,
    pub target: Value,
    pub href: String,
    pub changes: Vec<Change>,
}

impl Proposal {
    fn of(cs: &Changeset) -> Self {
        let mut target = Map::new();
        target.insert("kind".into(), json!(cs.target_type));
        if let Some(id) = &cs.target_id {
            target.insert("id".into(), json!(id));
        }
        Self {
            id: cs.id.clone(),
            title: cs.title.clone(),
            target: Value::Object(target),
            href: cs.href.clone(),
            changes: cs.changes.clone(),
        }
    }
}

/// Where a draft came from and why (stored on the changeset).
#[derive(Debug, Clone)]
pub struct Origin {
    /// `me` | `steve` | `plugin:<id>` | `signal`
    pub origin: String,
    pub basis: String,
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            origin: "me".into(),
            basis: String::new(),
        }
    }
}

/// The prompt for a target: fields, operations, the rules (exact names only, append free text).
pub fn prompt(p: &Prepared, request: &str) -> String {
    let mut lines = vec![
        format!(
            "You configure {} in Steve, a personal assistant app. Turn the user's request into operations. Change only what the request asks for.",
            p.subject
        ),
        "Fields (key · label · type · allowed values · current value):".to_string(),
    ];
    for f in &p.fields {
        let vals = f
            .values
            .as_ref()
            .map(|v| format!(" · {}", v.join("|")))
            .unwrap_or_default();
        lines.push(format!(
            "  {} · {} · {}{} · {}",
            f.key, f.label, f.ty, vals, f.current
        ));
    }
    lines.push("Operations (op · parameters):".into());
    lines.push(
        "  set · {\"key\": <field key>, \"value\": <new value; for list fields the full new list>}"
            .into(),
    );
    for o in &p.ops {
        lines.push(format!("  {} · {}", o.op, o.params));
    }
    lines.push("Refer to people, teams and organizations by the exact name the user used; never guess who is meant.".into());
    if let Some(k) = &p.free_text {
        lines.push(format!("Anything that fits no field or operation: append it to \"{k}\" (op set, the full new text)."));
    }
    lines.push("Never put passwords, tokens or other secret values into operations.".into());
    lines.push(format!(
        "Request: {}",
        serde_json::to_string(request).unwrap_or_default()
    ));
    lines.push("Reply with ONLY JSON: {\"ops\":[{\"op\":\"\", ...}]}".into());
    lines.join("\n")
}

/// The first `{…}` in a model reply (changes.ts `firstJson`), or `None`.
pub fn first_json(raw: &str) -> Option<Value> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    serde_json::from_str(raw.get(start..=end)?).ok()
}

/// `POST /api/config/draft` for a module target: prepare → model → plan → stored draft. A model reply
/// without usable JSON gives a draft with `changes: []`, like the old drafts.
pub async fn draft(
    m: &Modules,
    caller: &Caller,
    kind: &str,
    id: Option<&str>,
    request: &str,
    origin: Origin,
) -> ApiResult<Proposal> {
    if !m.registry.targets.contains_key(kind) {
        return Err(ApiError::invalid("target.kind"));
    }
    let (kind_s, id_s, caller_c) = (kind.to_string(), id.map(String::from), caller.clone());
    let (viewer, prepared) = m
        .blocking(move |m| {
            let viewer = m.viewer(&caller_c)?;
            let t = &m.registry.targets[&kind_s];
            let p = stored(|| m.store.reader(|c| t.prepare(c, &viewer, id_s.as_deref())))?;
            Ok((viewer, p))
        })
        .await?;
    let reply = crate::gateway::tagged(
        "draft",
        &caller.ws,
        m.gateway.smart(&prompt(&prepared, request), DRAFT_TIMEOUT),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("draft model call: {e}")))?;
    let raw = first_json(&reply)
        .and_then(|v| v.get("ops").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let (kind_s, request, caller_c) = (kind.to_string(), request.to_string(), caller.clone());
    m.blocking(move |m| {
        let t = &m.registry.targets[&kind_s];
        let plan = stored(|| {
            m.store
                .reader(|c| t.plan(c, &viewer, &prepared, raw.clone()))
        })?;
        for op in &plan.ops {
            let name = op.get("op").and_then(Value::as_str).unwrap_or("");
            if !m.registry.ops.contains_key(name) {
                return Err(ApiError::Internal(format!(
                    "target {kind_s} planned unknown op {name}"
                )));
            }
        }
        let cs = m.store.transaction(|tx| {
            changesets::create(
                tx,
                &caller_c.ws,
                &NewChangeset {
                    target_type: kind_s.clone(),
                    target_id: prepared.target_id.clone(),
                    target_revision: prepared.revision,
                    title: prepared.title.clone(),
                    href: prepared.href.clone(),
                    changes: plan.changes,
                    ops: plan.ops,
                    origin: origin.origin,
                    request,
                    basis: origin.basis,
                    created_by: caller_c.me.clone(),
                },
            )
        })?;
        Ok(Proposal::of(&cs))
    })
    .await
}

/// `f` (a read-only prepare or plan) in zh-CN: its title and preview lines are stored in `changesets` and read by
/// every member, so they don't follow the request's language (see steve-i18n, "What stays Chinese"). An error
/// is the request's own: `f` runs again in the request's language for it.
pub fn stored<R>(f: impl Fn() -> ApiResult<R>) -> ApiResult<R> {
    steve_i18n::stored(&f).or_else(|_| f())
}

/// Stores an already planned changeset (one-click corrections, signal suggestions) and returns it as a
/// draft; the caller may [`apply`] it right away. Build `prepared.title` and `plan.changes` under
/// [`steve_i18n::stored`]: they are stored for every member to read.
pub fn store_plan(
    m: &Modules,
    caller: &Caller,
    kind: &str,
    prepared: &Prepared,
    plan: Plan,
    origin: Origin,
) -> ApiResult<Proposal> {
    let cs = m.store.transaction(|tx| {
        changesets::create(
            tx,
            &caller.ws,
            &NewChangeset {
                target_type: kind.into(),
                target_id: prepared.target_id.clone(),
                target_revision: prepared.revision,
                title: prepared.title.clone(),
                href: prepared.href.clone(),
                changes: plan.changes,
                ops: plan.ops,
                origin: origin.origin,
                request: String::new(),
                basis: origin.basis,
                created_by: caller.me.clone(),
            },
        )
    })?;
    Ok(Proposal::of(&cs))
}

/// Replaces `"$name"` strings anywhere in `v` with bound values.
fn substitute(v: &Value, vars: &BTreeMap<String, Value>) -> Value {
    match v {
        Value::String(s) if s.starts_with('$') => {
            vars.get(&s[1..]).cloned().unwrap_or_else(|| v.clone())
        }
        Value::Array(xs) => Value::Array(xs.iter().map(|x| substitute(x, vars)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), substitute(x, vars)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn op_of<'a>(m: &'a Modules, op: &Value) -> ApiResult<&'a dyn ChangeOp> {
    let name = op.get("op").and_then(Value::as_str).unwrap_or("");
    m.registry
        .ops
        .get(name)
        .map(|b| b.as_ref())
        .ok_or_else(|| ApiError::Internal(format!("unknown change op {name}")))
}

enum Outcome {
    Applied,
    Stale,
}

/// `POST /api/config/:id/apply` for stored drafts. `None` = no such changeset (try the in-memory drafts).
/// Not a draft any more, another workspace, or the target's revision moved since drafting →
/// `409 这份草稿已经失效，请重新说一次` (a moved target also marks the draft `stale`). Otherwise every
/// operation is validated and applied in order in one transaction; `{ok:true, undo:id}`.
pub fn apply(m: &Modules, caller: &Caller, id: &str) -> Option<ApiResult<Value>> {
    let cs = m.store.reader(|c| changesets::get(c, id)).ok()??;
    Some((|| {
        if cs.workspace_id != caller.ws || cs.status != "draft" {
            return Err(ApiError::conflict(stale()));
        }
        let outcome = m.store.transaction(|tx| {
            let cs = changesets::get(tx, id)?.ok_or_else(|| ApiError::conflict(stale()))?;
            if cs.status != "draft" {
                return Err(ApiError::conflict(stale()));
            }
            if let (Some(tid), Some(rev)) = (&cs.target_id, cs.target_revision) {
                let t = m
                    .registry
                    .targets
                    .get(&cs.target_type)
                    .ok_or_else(|| ApiError::conflict(stale()))?;
                if t.revision(tx, &cs.workspace_id, tid)? != Some(rev) {
                    changesets::close_draft(tx, &cs, "stale")?;
                    return Ok(Outcome::Stale);
                }
            }
            let vars = RefCell::new(BTreeMap::new());
            let cx = OpCx {
                ws: &cs.workspace_id,
                actor: &caller.me,
                changeset_id: &cs.id,
                vars: &vars,
            };
            let mut before = Vec::with_capacity(cs.ops.len());
            for raw in &cs.ops {
                let op = substitute(raw, &vars.borrow());
                let h = op_of(m, &op)?;
                h.validate(tx, &cx, &op)?;
                let b = h.apply(tx, &cx, &op)?;
                before.push(json!({ "op": op, "before": b }));
            }
            if !changesets::mark_applied(tx, &cs, &before)? {
                return Err(ApiError::conflict(stale()));
            }
            base::audit(
                tx,
                &cs.workspace_id,
                &caller.me,
                "config.apply",
                &cs.id,
                &json!({ "target": { "kind": cs.target_type, "id": cs.target_id }, "ops": cs.ops.len() }),
            )?;
            Ok(Outcome::Applied)
        })?;
        match outcome {
            Outcome::Applied => Ok(json!({ "ok": true, "undo": id })),
            Outcome::Stale => Err(ApiError::conflict(stale())),
        }
    })())
}

/// `POST /api/config/:id/undo` for stored changesets. `None` = no such changeset. Not applied, another
/// workspace, or applied more than 24 hours ago → `409 这次改动已经不能撤销了`. Undoes in reverse order
/// in one transaction; `{ok:true}`.
pub fn undo(m: &Modules, caller: &Caller, id: &str) -> Option<ApiResult<Value>> {
    undo_at(m, caller, id, now())
}

/// [`undo`] with an explicit clock (tests of the 24-hour window).
pub fn undo_at(m: &Modules, caller: &Caller, id: &str, at: i64) -> Option<ApiResult<Value>> {
    let cs = m.store.reader(|c| changesets::get(c, id)).ok()??;
    Some((|| {
        if cs.workspace_id != caller.ws || !cs.undoable(at) {
            return Err(ApiError::conflict(not_undoable()));
        }
        m.store.transaction(|tx| {
            let cs = changesets::get(tx, id)?
                .filter(|c| c.undoable(at))
                .ok_or_else(|| ApiError::conflict(not_undoable()))?;
            let vars = RefCell::new(BTreeMap::new());
            let cx = OpCx {
                ws: &cs.workspace_id,
                actor: &caller.me,
                changeset_id: &cs.id,
                vars: &vars,
            };
            for entry in cs.before.iter().flatten().rev() {
                let op = entry.get("op").cloned().unwrap_or(Value::Null);
                let before = entry.get("before").cloned().unwrap_or(Value::Null);
                op_of(m, &op)?.undo(tx, &cx, &op, &before)?;
            }
            if !changesets::mark_undone(tx, &cs)? {
                return Err(ApiError::conflict(not_undoable()));
            }
            base::audit(
                tx,
                &cs.workspace_id,
                &caller.me,
                "config.undo",
                &cs.id,
                &json!({}),
            )?;
            Ok(json!({ "ok": true }))
        })
    })())
}

/// Throws a draft away (`dropped`); `None` = no such changeset, `409` if it is no longer a draft.
pub fn drop_draft(m: &Modules, caller: &Caller, id: &str) -> Option<ApiResult<Value>> {
    let cs = m.store.reader(|c| changesets::get(c, id)).ok()??;
    Some((|| {
        if cs.workspace_id != caller.ws {
            return Err(ApiError::conflict(stale()));
        }
        m.store.transaction(|tx| {
            if !changesets::close_draft(tx, &cs, "dropped")? {
                return Err(ApiError::conflict(stale()));
            }
            Ok(json!({ "ok": true }))
        })
    })())
}

/// A module target as it is now (fields with their current values), after the ownership check; no
/// model call and nothing stored. For a request that only asks how something is set.
pub async fn prepared(
    m: &Modules,
    caller: &Caller,
    kind: &str,
    id: Option<&str>,
) -> ApiResult<Prepared> {
    if !m.registry.targets.contains_key(kind) {
        return Err(ApiError::invalid("target.kind"));
    }
    let (kind, id, caller) = (kind.to_string(), id.map(String::from), caller.clone());
    m.blocking(move |m| {
        let viewer = m.viewer(&caller)?;
        let t = &m.registry.targets[&kind];
        m.store.reader(|c| t.prepare(c, &viewer, id.as_deref()))
    })
    .await
}

/// Every registered target's `global` candidates, for steve-core's `pickTarget` option list.
pub fn candidates(m: &Modules, caller: &Caller) -> ApiResult<Vec<Candidate>> {
    let viewer = m.viewer(caller)?;
    let mut out = Vec::new();
    for t in m.registry.targets.values() {
        out.extend(m.store.reader(|c| t.candidates(c, &viewer))?);
    }
    Ok(out)
}
