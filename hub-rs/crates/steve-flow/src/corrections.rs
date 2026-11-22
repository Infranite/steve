//! Corrections of a signal's handling: `POST /signals/:id/correct
//! {action, scope}`. One click is consent, so the change is applied at once and comes back as an
//! undoable changeset (`POST /config/:changeset_id/undo`).
//!
//! - `remind` / `ignore`: an exception is put right after the trigger of the flow that handled the
//!   event (or of a new `origin=correction` flow when nothing did): a `filter` “例外：<描述>” whose hit
//!   branch goes to `action notify` / `action ignore` and whose other branch goes where the trigger
//!   went before. Plus a rule memory “<来源>里「<摘要>」这类以后<提醒你|忽略>”.
//! - `to_item`: an item from the event (the same path as “转成事项” on a `notify` decision).
//!
//! Every correction is a `signal_corrections` row. The changeset's operations are this module's
//! own, `correction.flow` and `correction.rule`, so the graph change and the memory undo together
//! and nothing collides with the `flow.save` of the conversational drafts.

use std::sync::Arc;

use axum::extract::Path;
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::changes::{self, ChangeOp, OpCx, Origin, Plan, Prepared};
use steve_modules::{ApiError, ApiResult, Body, Caller, Clock, Modules, Registry};
use steve_store::base::mems::NewMem;
use steve_store::base::{Change, ObjRef, Viewer, Visibility};
use steve_store::knowledge::Memories;
use steve_store::{NewCorrection, SignalRows};

use crate::graph::{Edge, Graph, Node};
use crate::signals::source_label;
use crate::{FlowCx, FlowResult, flows};

/// Operation names in the changeset.
pub const OP_FLOW: &str = "correction.flow";
pub const OP_RULE: &str = "correction.rule";
/// Changeset `origin`.
pub const ORIGIN: &str = "signal";
/// Version note and `origin_label` of what a correction saves.
pub const NOTE: &str = "按你的纠正";
pub const UNDO_NOTE: &str = "撤销纠正";
/// Branch labels of the inserted filter.
pub const HIT: &str = "例外";
pub const ELSE: &str = "其他";

/// What the correction does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Remind,
    Ignore,
    ToItem,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "remind" => Some(Self::Remind),
            "ignore" => Some(Self::Ignore),
            "to_item" => Some(Self::ToItem),
            _ => None,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Remind => "remind",
            Self::Ignore => "ignore",
            Self::ToItem => "to_item",
        }
    }
    /// For the stored rule text and node titles (zh-CN).
    fn verb(self) -> &'static str {
        match self {
            Self::Remind => "提醒你",
            _ => "忽略",
        }
    }
    /// For the card, in the request's language.
    fn verb_shown(self) -> String {
        match self {
            Self::Remind => steve_i18n::t!("flow.correction.remind"),
            _ => steve_i18n::t!("flow.correction.ignore"),
        }
    }
}

/// Which later events the exception catches: the same group (default) or the same thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    One,
    Group,
}

impl Scope {
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s {
            None | Some("group") => Some(Self::Group),
            Some("one") => Some(Self::One),
            _ => None,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Group => "group",
        }
    }
}

/// Body of `POST /signals/:id/correct`.
#[derive(Debug, Clone, Deserialize)]
pub struct CorrectBody {
    pub action: String,
    #[serde(default)]
    pub scope: Option<String>,
}

/// The response: `{ok:true, changeset_id?, item_id?}` (+ `correction_id`, `flow_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct Corrected {
    pub id: String,
    pub changeset_id: Option<String>,
    pub item_id: Option<String>,
    pub flow_id: Option<String>,
}

impl Corrected {
    pub fn to_json(&self) -> Value {
        let mut v = json!({ "ok": true, "correction_id": self.id });
        if let Some(c) = &self.changeset_id {
            v["changeset_id"] = json!(c);
        }
        if let Some(i) = &self.item_id {
            v["item_id"] = json!(i);
        }
        if let Some(f) = &self.flow_id {
            v["flow_id"] = json!(f);
        }
        v
    }
}

/// Registers `correction.flow` and `correction.rule` (call from `FlowSetup::register`).
pub fn register(r: &mut Registry, clock: Arc<dyn Clock>) {
    r.op(OP_FLOW, Box::new(FlowOp { clock }))
        .op(OP_RULE, Box::new(RuleOp));
}

/// `POST /signals/:id/correct`.
pub fn routes() -> Router<Modules> {
    Router::new().route("/signals/{id}/correct", post(correct_route))
}

async fn correct_route(
    Extension(cx): Extension<Arc<FlowCx>>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<CorrectBody>,
) -> ApiResult<Json<Value>> {
    let action = Action::parse(&b.action).ok_or_else(|| ApiError::invalid("action"))?;
    let scope = Scope::parse(b.scope.as_deref()).ok_or_else(|| ApiError::invalid("scope"))?;
    Ok(Json(
        correct(&cx, &who, &id, action, scope).await?.to_json(),
    ))
}

/// The event fields a correction reads.
#[derive(Debug, Clone)]
struct Ev {
    id: String,
    source: String,
    type_key: String,
    connection_id: Option<String>,
    thread_key: Option<String>,
    group_key: Option<String>,
    actor_account_id: Option<String>,
    actor_name: Option<String>,
    summary: String,
    outcome: String,
    flow_id: Option<String>,
}

fn load_event(
    c: &mut impl steve_store::Read,
    viewer: &Viewer,
    id: &str,
) -> steve_store::Result<Ev> {
    let s = crate::signals::own(c, viewer, id)?;
    let actor_name = match s.party {
        Some(party) => steve_store::Contacts::new(c)
            .party_names([party.as_str()])?
            .remove(&party),
        None => None,
    };
    Ok(Ev {
        id: s.id,
        source: s.source,
        type_key: s.kind,
        connection_id: s.connection,
        thread_key: s.thread.filter(|k| !k.is_empty()),
        group_key: s.group.filter(|k| !k.is_empty()),
        actor_account_id: s.actor_account.filter(|k| !k.is_empty()),
        actor_name: actor_name.filter(|n| !n.is_empty()),
        summary: s.summary,
        outcome: s.outcome,
        flow_id: s.flow,
    })
}

/// A string literal of the condition language.
fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The exception's condition and its description (“同组（acme）”), zh-CN for the node title and in the
/// request's language for the card.
fn condition(ev: &Ev, scope: Scope) -> ApiResult<(String, String, String)> {
    match scope {
        Scope::Group => {
            if let Some(g) = &ev.group_key {
                return Ok((
                    format!("event.group_key == {}", lit(g)),
                    format!("同组（{g}）"),
                    steve_i18n::t!("flow.correction.same_group", group = g),
                ));
            }
            // no group: the same sender
            let a = ev
                .actor_account_id
                .as_ref()
                .ok_or_else(|| ApiError::rule(steve_i18n::t!("flow.correction.no_key")))?;
            let shown = match &ev.actor_name {
                Some(n) => steve_i18n::t!("flow.correction.from", who = n),
                None => steve_i18n::t!("flow.correction.from_sender"),
            };
            let who = ev.actor_name.clone().unwrap_or_else(|| "这个发件人".into());
            Ok((
                format!("event.actor_account_id == {}", lit(a)),
                format!("来自 {who}"),
                shown,
            ))
        }
        Scope::One => {
            let t = ev
                .thread_key
                .as_ref()
                .ok_or_else(|| ApiError::rule(steve_i18n::t!("flow.correction.no_key")))?;
            Ok((
                format!("event.thread_key == {}", lit(t)),
                "同一往来".into(),
                steve_i18n::t!("flow.correction.same_thread"),
            ))
        }
    }
}

fn cut(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// An unused node id with `prefix` (`cor`, `cor2`, …).
fn fresh_id(g: &Graph, prefix: &str) -> String {
    (1..)
        .map(|n| {
            if n == 1 {
                prefix.to_string()
            } else {
                format!("{prefix}{n}")
            }
        })
        .find(|id| g.node(id).is_none() && g.node(&format!("{id}_a")).is_none())
        .unwrap_or_else(|| prefix.into())
}

/// Inserts the exception after the trigger: `t → filter`, `filter -例外→ action`, `filter -其他→`
/// where the trigger went on (the edge the engine follows from the trigger). Other edges stay.
pub fn insert_exception(
    g: &Graph,
    action: Action,
    when: &str,
    describe: &str,
) -> Result<Graph, String> {
    let t = g.trigger().ok_or("流程要有且只有一个触发节点")?.id.clone();
    let mut g = g.clone();
    let fid = fresh_id(&g, "cor");
    let aid = format!("{fid}_a");
    let title = cut(&format!("例外：{describe}"), 60);
    g.nodes.push(Node {
        id: fid.clone(),
        kind: "filter".into(),
        title,
        note: String::new(),
        config: json!({ "rules": [{ "label": HIT, "when": when }], "else_label": ELSE }),
        on_fail: None,
    });
    let (atitle, acfg) = match action {
        Action::Remind => (
            "提醒你",
            json!({ "action": "notify", "text": "{{event.summary}}" }),
        ),
        _ => ("忽略", json!({ "action": "ignore", "reason": NOTE })),
    };
    g.nodes.push(Node {
        id: aid.clone(),
        kind: "action".into(),
        title: atitle.into(),
        note: String::new(),
        config: acfg,
        on_fail: None,
    });
    let followed = g.next(&t, None).map(|e| (e.to.clone(), e.label.clone()));
    let new_edge = Edge {
        from: t.clone(),
        to: fid.clone(),
        label: String::new(),
    };
    match followed {
        Some((to, label)) => {
            let at = g
                .edges
                .iter()
                .position(|e| e.from == t && e.to == to && e.label == label)
                .unwrap_or(0);
            g.edges[at] = new_edge;
            g.edges.push(Edge {
                from: fid.clone(),
                to,
                label: ELSE.into(),
            });
        }
        None => g.edges.push(new_edge),
    }
    g.edges.push(Edge {
        from: fid,
        to: aid,
        label: HIT.into(),
    });
    Ok(g)
}

/// The graph of a new correction flow: a trigger for this event's source, type and connection.
fn starter(ev: &Ev) -> Graph {
    let mut g = Graph::starter(&ev.source);
    if let Some(t) = g.nodes.first_mut() {
        t.title = format!("{} · {}", source_label(&ev.source), ev.type_key);
        t.config = json!({
            "source": ev.source,
            "types": [ev.type_key],
            "connections": ev.connection_id.iter().collect::<Vec<_>>(),
        });
    }
    g
}

/// Applies a correction. `remind`/`ignore` → applied changeset; `to_item` → item.
pub async fn correct(
    cx: &Arc<FlowCx>,
    who: &Caller,
    event_id: &str,
    action: Action,
    scope: Scope,
) -> FlowResult<Corrected> {
    let viewer = cx.modules.viewer(who)?;
    let ev = cx.store().read(|c| load_event(c, &viewer, event_id))?;
    if action == Action::ToItem {
        if ev.outcome == "item" {
            return Err(ApiError::conflict(steve_i18n::t!(
                "flow.correction.has_item"
            )));
        }
        let item = crate::decisions::to_item(cx, &who.ws, &ev.id).await?;
        let id = record(cx, who, &ev, action, scope, None, None, None, Some(&item))?;
        return Ok(Corrected {
            id,
            changeset_id: None,
            item_id: Some(item),
            flow_id: None,
        });
    }
    // the card (title, preview lines) is stored with the changeset and read by every member: zh-CN
    let (when, describe, describe_shown) = changes::stored(|| condition(&ev, scope))?;
    let target = cx.read(|c| -> FlowResult<Option<flows::FlowRow>> {
        Ok(match &ev.flow_id {
            Some(f) => flows::get(c, f)?
                .filter(|f| f.workspace_id == who.ws && !f.deleted && f.kind != "schedule"),
            None => None,
        })
    })?;
    let CorrectionPlan {
        prepared,
        plan,
        flow_id,
    } = prepare_correction(
        cx,
        &ev,
        action,
        target.as_ref(),
        Exception {
            when,
            describe,
            describe_shown,
        },
    )?;
    let origin = Origin {
        origin: ORIGIN.into(),
        basis: format!("signal:{}", ev.id),
    };
    let m = &cx.modules;
    let proposal = changes::store_plan(m, who, "flow", &prepared, plan, origin)?;
    changes::apply(m, who, &proposal.id)
        .ok_or_else(|| ApiError::Internal("correction changeset vanished".into()))??;
    let mem_id = cx.store().read(|c| {
        let cs = steve_store::base::changesets::ChangeSets(c).get(&proposal.id)?;
        Ok(cs.and_then(|cs| cs.before).and_then(|b| {
            b.iter()
                .find(|e| e["op"]["op"] == OP_RULE)
                .and_then(|e| e["before"]["mem_id"].as_str().map(String::from))
        }))
    })?;
    let id = record(
        cx,
        who,
        &ev,
        action,
        scope,
        Some(&flow_id),
        Some(&proposal.id),
        mem_id.as_deref(),
        None,
    )?;
    Ok(Corrected {
        id,
        changeset_id: Some(proposal.id),
        item_id: None,
        flow_id: Some(flow_id),
    })
}

struct Exception {
    when: String,
    describe: String,
    describe_shown: String,
}
struct CorrectionPlan {
    prepared: Prepared,
    plan: Plan,
    flow_id: String,
}
fn prepare_correction(
    cx: &FlowCx,
    ev: &Ev,
    action: Action,
    target: Option<&flows::FlowRow>,
    exception: Exception,
) -> FlowResult<CorrectionPlan> {
    let Exception {
        when,
        describe,
        describe_shown,
    } = exception;
    let label = source_label(&ev.source).to_string();
    let rule_text = format!(
        "{label}里「{}」这类以后{}",
        cut(&ev.summary, 40),
        action.verb()
    );
    let CorrectionTarget {
        base_graph,
        flow_op,
        prepared_title,
        flow_id,
    } = correction_target(ev, target, &label, &rule_text)?;
    let graph = insert_exception(&base_graph, action, &when, &describe).map_err(ApiError::rule)?;
    let graph = crate::graph::validate(&graph.to_value(), &cx.nodes).map_err(ApiError::rule)?;
    let mut flow_op = flow_op;
    flow_op["graph"] = graph.to_value();
    let rule_op = json!({
        "op": OP_RULE,
        "text": rule_text,
        "event_id": ev.id,
        "connection_id": ev.connection_id,
    });
    let exception = steve_i18n::stored(|| {
        steve_i18n::t!(
            "flow.correction.exception",
            describe = describe_shown,
            verb = action.verb_shown()
        )
    });
    let changes = vec![
        Change::new(
            prepared_title.clone(),
            if target.is_some() {
                String::new()
            } else {
                steve_i18n::stored(|| steve_i18n::t!("flow.correction.none"))
            },
            exception,
        ),
        Change::new(
            steve_i18n::stored(|| steve_i18n::t!("flow.correction.rule_memory")),
            "",
            rule_text.clone(),
        ),
    ];
    let prepared = Prepared {
        target_id: Some(flow_id.clone()),
        // applied right away: no staleness window
        revision: None,
        title: steve_i18n::stored(|| {
            steve_i18n::t!("flow.correction.title", summary = cut(&ev.summary, 30))
        }),
        href: format!("#/auto/rule/{flow_id}"),
        subject: prepared_title,
        fields: Vec::new(),
        ops: Vec::new(),
        free_text: None,
        context: Value::Null,
    };
    let plan = Plan {
        ops: vec![flow_op, rule_op],
        changes,
    };
    Ok(CorrectionPlan {
        prepared,
        plan,
        flow_id,
    })
}

struct CorrectionTarget {
    base_graph: Graph,
    flow_op: Value,
    prepared_title: String,
    flow_id: String,
}
fn correction_target(
    ev: &Ev,
    target: Option<&flows::FlowRow>,
    label: &str,
    rule_text: &str,
) -> FlowResult<CorrectionTarget> {
    let new_id = steve_store::uid("flw");
    let (base_graph, flow_op, prepared_title, flow_id) = match target {
        Some(f) => {
            let g = Graph::from_value(&f.graph).map_err(ApiError::rule)?;
            (
                g,
                json!({ "op": OP_FLOW, "flow_id": f.id, "version": f.version }),
                steve_i18n::stored(|| steve_i18n::t!("flow.draft.flow_title", name = f.name)),
                f.id.clone(),
            )
        }
        None => {
            let name = cut(&format!("纠正：{label} · {}", ev.type_key), 38);
            (
                starter(&ev),
                json!({ "op": OP_FLOW, "flow_id": new_id, "create": { "name": name, "description": rule_text } }),
                steve_i18n::stored(|| steve_i18n::t!("flow.correction.new_flow", name = name)),
                new_id.clone(),
            )
        }
    };
    Ok(CorrectionTarget {
        base_graph,
        flow_op,
        prepared_title,
        flow_id,
    })
}

/// Writes the `signal_corrections` row.
#[allow(clippy::too_many_arguments)]
fn record(
    cx: &FlowCx,
    who: &Caller,
    ev: &Ev,
    action: Action,
    scope: Scope,
    flow_id: Option<&str>,
    changeset_id: Option<&str>,
    mem_id: Option<&str>,
    item_id: Option<&str>,
) -> FlowResult<String> {
    let id = steve_store::uid("cor");
    let now = cx.now();
    cx.store().write(|w| -> FlowResult<()> {
        SignalRows(w).correct(&NewCorrection {
            id: &id,
            ws: &who.ws,
            event: &ev.id,
            scope: scope.code(),
            action: action.code(),
            flow: flow_id,
            change: changeset_id,
            memory: mem_id,
            item: item_id,
            actor: &who.me,
            at: now,
        })?;
        w.audit(
            &who.ws,
            &who.me,
            "signal.correct",
            &ev.id,
            &json!({ "action": action.code(), "scope": scope.code(), "flow": flow_id, "changeset": changeset_id, "item": item_id }),
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// `correction.flow {flow_id, graph, version}` saves a new version of an existing flow (author
/// steve, note 按你的纠正); `correction.flow {flow_id, graph, create:{name, description}}` creates
/// the enabled `origin=correction` flow. Undo puts the old graph back as a new version, or deletes
/// the created flow; either only while nobody saved the flow since.
struct FlowOp {
    clock: Arc<dyn Clock>,
}

fn op_str<'a>(op: &'a Value, k: &str) -> ApiResult<&'a str> {
    op[k]
        .as_str()
        .ok_or_else(|| ApiError::Internal(format!("{OP_FLOW}: no {k}")))
}

fn op_graph(op: &Value) -> ApiResult<Graph> {
    Graph::from_value(&op["graph"]).map_err(ApiError::rule)
}

impl ChangeOp for FlowOp {
    fn validate(&self, c: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let id = op_str(op, "flow_id")?;
        if op.get("create").is_some() {
            if flows::get(c, id)?.is_some() {
                return Err(ApiError::conflict(changes::STALE));
            }
        } else {
            let f = flows::own(c, cx.ws, id)?;
            if Some(f.version) != op["version"].as_i64() {
                return Err(ApiError::conflict(changes::STALE));
            }
        }
        op_graph(op).map(|_| ())
    }

    fn apply(&self, tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = op_str(op, "flow_id")?;
        let graph = op_graph(op)?;
        let now = self.clock.now();
        if let Some(create) = op.get("create") {
            let name = create["name"].as_str().unwrap_or("纠正");
            let description = cut(create["description"].as_str().unwrap_or(""), 400);
            flows::create_as(
                tx,
                id,
                cx.ws,
                cx.actor,
                &flows::NewFlow {
                    kind: "flow",
                    name,
                    description: &description,
                    graph: &graph,
                    origin: "correction",
                    origin_label: flows::origin_label("correction"),
                    enabled: true,
                },
                ("steve", NOTE),
                now,
            )?;
            return Ok(json!({ "created": true, "version": 1 }));
        }
        let f = flows::own(tx, cx.ws, id)?;
        let version = flows::save_version(tx, &f, &graph, "steve", NOTE, now)?;
        Ok(json!({ "created": false, "graph": f.graph, "version": version }))
    }

    fn undo(
        &self,
        tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let id = op_str(op, "flow_id")?;
        let f = flows::own(tx, cx.ws, id)?;
        if Some(f.version) != before["version"].as_i64() {
            return Err(ApiError::conflict(changes::NOT_UNDOABLE));
        }
        let now = self.clock.now();
        if before["created"] == json!(true) {
            flows::delete(tx, &f, cx.actor, now)?;
        } else {
            let g = Graph::from_value(&before["graph"]).map_err(ApiError::rule)?;
            flows::save_version(tx, &f, &g, "steve", UNDO_NOTE, now)?;
        }
        Ok(())
    }
}

/// `correction.rule {text, event_id, connection_id?}`: a private `rule` memory about the event's
/// connection. Undo deletes it.
struct RuleOp;

impl ChangeOp for RuleOp {
    fn apply(&self, w: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let text = op["text"].as_str().unwrap_or("").to_string();
        let about = op["connection_id"]
            .as_str()
            .map(|c| vec![ObjRef::new("connection", c)])
            .unwrap_or_default();
        let m = Memories(w).create(
            cx.ws,
            &NewMem {
                kind: "rule".into(),
                text,
                source_type: "event".into(),
                source_id: op["event_id"].as_str().map(String::from),
                source_label: NOTE.into(),
                visibility: Visibility::Private,
                writer: "steve".into(),
                created_by: cx.actor.into(),
                about,
                ..NewMem::default()
            },
        )?;
        Ok(json!({ "mem_id": m.id }))
    }

    fn undo(
        &self,
        w: &mut steve_store::W<'_>,
        _cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        if let Some(id) = before["mem_id"].as_str() {
            Memories(w).delete(id)?;
        }
        Ok(())
    }
}
