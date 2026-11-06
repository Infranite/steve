//! The `flow` draft target: `flow:<id>`, what is said in the main
//! conversation (“GitHub 通知以后都忽略”). The model gets the current graph and answers with graph
//! edits (`node.add`, `node.update`, `node.remove`, `edge.add`, `edge.remove`); the server applies
//! them to the graph, validates the result like `PUT /flows/:id`, and stores one `flow.save {graph}`
//! (a new version) with a line per node change. `name`, `description` and `enabled` are fields
//! (`flow.patch`). The flow page's own input box uses `edit-draft` (S4), not this.

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Map, Value, json};
use steve_modules::changes::{
    Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared,
};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::Tx;
use steve_store::base::{Change, Viewer, show};

use super::{parse_bool, text_of, unrecognized, unrecognized_value};
use crate::flows::{self, DESCRIPTION_MAX, FlowRow, NAME_MAX};
use crate::flows_api::check_graph;
use crate::graph::{Edge, Graph, Node};
use crate::{FlowCx, LateFlow};

pub const KIND: &str = "flow";

/// Note of the version a confirmed draft saves, and of the one its undo saves.
pub const NOTE: &str = "按你说的修改";

pub fn register(r: &mut Registry, late: Arc<LateFlow>) {
    r.target(KIND, Box::new(FlowTarget(late.clone())))
        .op("flow.save", Box::new(SaveOp(late.clone())))
        .op("flow.patch", Box::new(PatchOp(late)));
}

/// A flow this target edits: of the workspace, not deleted, not a schedule (that is `schedule`).
fn own_flow(c: &Connection, ws: &str, id: &str) -> ApiResult<FlowRow> {
    let f = flows::own(c, ws, id)?;
    if f.kind == "schedule" {
        return Err(ApiError::rule(steve_i18n::t!(
            "flow.draft.schedule_elsewhere"
        )));
    }
    Ok(f)
}

/// For the model (field values it reads and writes back).
fn yes_no(b: bool) -> &'static str {
    if b { "是" } else { "否" }
}

/// For the card, in the request's language.
fn yes_no_shown(b: bool) -> String {
    if b {
        steve_i18n::t!("flow.draft.yes")
    } else {
        steve_i18n::t!("flow.draft.no")
    }
}

fn quoted(g: &Graph, id: &str) -> String {
    steve_i18n::t!(
        "flow.draft.quoted",
        name = g.node(id).map_or(id, Node::name)
    )
}

fn edge_text(g: &Graph, e: &Edge) -> String {
    let (from, to) = (quoted(g, &e.from), quoted(g, &e.to));
    if e.label.is_empty() {
        steve_i18n::t!("flow.draft.edge", from = from, to = to)
    } else {
        steve_i18n::t!(
            "flow.draft.edge_labelled",
            from = from,
            to = to,
            label = e.label
        )
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Null => steve_i18n::t!("flow.draft.empty"),
        Value::String(s) => show(s),
        v => show(&v.to_string()),
    }
}

/// A node id for a new node of `kind`: the kind made id-safe, numbered from 2 when taken.
fn fresh_id(g: &Graph, wanted: Option<&str>, kind: &str) -> String {
    let ok = |id: &str| {
        let mut cs = id.chars();
        cs.next().is_some_and(|c| c.is_ascii_lowercase())
            && id.len() <= 16
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if let Some(w) = wanted.filter(|w| ok(w) && g.node(w).is_none()) {
        return w.to_string();
    }
    let mut base: String = kind
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .take(12)
        .collect();
    if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base = format!("n{base}");
        base.truncate(12);
    }
    if g.node(&base).is_none() {
        return base;
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|id| g.node(id).is_none())
        .unwrap_or(base)
}

/// Every node reachable from `from` (itself included).
fn below(g: &Graph, from: &str) -> HashSet<String> {
    let mut seen = HashSet::from([from.to_string()]);
    let mut stack = vec![from.to_string()];
    while let Some(n) = stack.pop() {
        for e in g.outs(&n) {
            if seen.insert(e.to.clone()) {
                stack.push(e.to.clone());
            }
        }
    }
    seen
}

/// Applies the model's graph edits to a working copy, one by one. An edit that names something
/// that is not there becomes a “（没认出…）” line and is skipped; nothing is guessed.
pub struct GraphEdit<'k> {
    pub graph: Graph,
    pub changes: Vec<Change>,
    /// Did any edit change the graph?
    pub changed: bool,
    kinds: &'k [&'k str],
    /// Nodes below a removed one: dropped at the end if nothing connects them any more.
    orphans: HashSet<String>,
}

impl<'k> GraphEdit<'k> {
    pub fn new(graph: Graph, kinds: &'k [&'k str]) -> Self {
        Self {
            graph,
            changes: Vec::new(),
            changed: false,
            kinds,
            orphans: HashSet::new(),
        }
    }

    fn unknown(&mut self, label: String, said: &str) {
        self.changes
            .push(Change::new(label, "—", unrecognized_value(said)));
    }

    /// One edit; `false` if `op` is not a graph edit at all.
    pub fn apply(&mut self, op: &Value) -> bool {
        let s = |k: &str| op[k].as_str().map(str::trim).unwrap_or("");
        match op["op"].as_str().unwrap_or("") {
            "node.add" => self.add(
                s("after"),
                s("kind"),
                s("title"),
                op.get("config"),
                s("edge_label"),
                op["id"].as_str(),
            ),
            "node.update" => self.update(s("id"), &op["set"]),
            "node.remove" => self.remove(s("id")),
            "edge.add" => self.edge_add(s("from"), s("to"), s("label")),
            "edge.remove" => self.edge_remove(s("from"), s("to"), op["label"].as_str()),
            _ => return false,
        }
        true
    }

    /// `node.add {after, kind, title, config, edge_label}`: the new node goes on `after`'s edge
    /// labelled `edge_label`; whatever was on that edge now follows the new node.
    fn add(
        &mut self,
        after: &str,
        kind: &str,
        title: &str,
        config: Option<&Value>,
        label: &str,
        id: Option<&str>,
    ) {
        if self.graph.node(after).is_none() {
            return self.unknown(
                steve_i18n::t!("flow.draft.node_add"),
                &steve_i18n::t!("flow.draft.after_node", node = after),
            );
        }
        if kind == "trigger" || !self.kinds.contains(&kind) {
            return self.unknown(
                steve_i18n::t!("flow.draft.node_add"),
                &steve_i18n::t!("flow.draft.node_kind", kind = kind),
            );
        }
        let id = fresh_id(&self.graph, id, kind);
        let config = config
            .filter(|c| c.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        for e in self.graph.edges.iter_mut() {
            if e.from == after && e.label == label {
                e.from = id.clone();
                e.label = String::new();
            }
        }
        self.graph.nodes.push(Node {
            id: id.clone(),
            kind: kind.into(),
            title: title.into(),
            note: String::new(),
            config,
            on_fail: None,
        });
        self.graph.edges.push(Edge {
            from: after.into(),
            to: id.clone(),
            label: label.into(),
        });
        let shown = if title.is_empty() { &id } else { title };
        let after = quoted(&self.graph, after);
        let line = if label.is_empty() {
            steve_i18n::t!(
                "flow.draft.node_added",
                title = shown,
                kind = kind,
                after = after
            )
        } else {
            steve_i18n::t!(
                "flow.draft.node_added_on",
                title = shown,
                kind = kind,
                after = after,
                label = label
            )
        };
        self.changes.push(Change::new(
            steve_i18n::t!("flow.draft.node_add"),
            "—",
            line,
        ));
        self.changed = true;
    }

    /// `node.update {id, set:{title?, note?, config?, on_fail?}}`; `config` is merged key by key
    /// (`null` removes a key). A line per changed key.
    fn update(&mut self, id: &str, set: &Value) {
        let Some(i) = self.graph.nodes.iter().position(|n| n.id == id) else {
            return self.unknown(steve_i18n::t!("flow.draft.node_update"), id);
        };
        let Some(set) = set.as_object() else {
            return self.unknown(steve_i18n::t!("flow.draft.node_update"), &set.to_string());
        };
        let name = quoted(&self.graph, id);
        let mut lines = Vec::new();
        let node = &mut self.graph.nodes[i];
        for (k, v) in set {
            match k.as_str() {
                "title" | "note" => {
                    let new = text_of(v).unwrap_or_default();
                    let (field, label) = if k == "title" {
                        (
                            &mut node.title,
                            steve_i18n::t!("flow.draft.node_title", node = name),
                        )
                    } else {
                        (
                            &mut node.note,
                            steve_i18n::t!("flow.draft.node_note", node = name),
                        )
                    };
                    if *field != new {
                        lines.push(Change::new(label, show(field), show(&new)));
                        *field = new;
                    }
                }
                "on_fail" => {
                    let new = v.as_str().map(String::from);
                    if node.on_fail != new {
                        lines.push(Change::new(
                            steve_i18n::t!("flow.draft.node_on_fail", node = name),
                            node.on_fail.as_deref().unwrap_or("stop"),
                            new.as_deref().unwrap_or("stop"),
                        ));
                        node.on_fail = new;
                    }
                }
                "config" if v.is_object() => {
                    if !node.config.is_object() {
                        node.config = json!({});
                    }
                    for (ck, cv) in v.as_object().into_iter().flatten() {
                        let old = node.config.get(ck).cloned().unwrap_or(Value::Null);
                        if old == *cv {
                            continue;
                        }
                        lines.push(Change::new(
                            steve_i18n::t!("flow.draft.node_config", node = name, key = ck),
                            value_text(&old),
                            value_text(cv),
                        ));
                        if let Some(o) = node.config.as_object_mut() {
                            if cv.is_null() {
                                o.remove(ck);
                            } else {
                                o.insert(ck.clone(), cv.clone());
                            }
                        }
                    }
                }
                _ => lines.push(Change::new(
                    unrecognized(),
                    "—",
                    show(&format!("{id}.{k}: {v}")),
                )),
            }
        }
        let unrecognized = unrecognized();
        self.changed |= lines.iter().any(|l| l.label != unrecognized);
        self.changes.extend(lines);
    }

    /// `node.remove {id}`: a node with one way out is cut out and its predecessors joined to its
    /// successor; otherwise what hung only below it goes too (listed at [`GraphEdit::finish`]).
    fn remove(&mut self, id: &str) {
        let Some(node) = self.graph.node(id).cloned() else {
            return self.unknown(steve_i18n::t!("flow.draft.node_remove"), id);
        };
        if node.kind == "trigger" {
            return self.unknown(
                steve_i18n::t!("flow.draft.node_remove"),
                &steve_i18n::t!("flow.draft.trigger_kept", node = id),
            );
        }
        let outs: Vec<Edge> = self.graph.outs(id).cloned().collect();
        let ins: Vec<Edge> = self
            .graph
            .edges
            .iter()
            .filter(|e| e.to == id)
            .cloned()
            .collect();
        let line = Change::new(
            steve_i18n::t!("flow.draft.node_remove"),
            quoted(&self.graph, id),
            "—",
        );
        if outs.len() == 1 {
            for e in &ins {
                let joined = Edge {
                    from: e.from.clone(),
                    to: outs[0].to.clone(),
                    label: e.label.clone(),
                };
                if !self.graph.edges.contains(&joined) {
                    self.graph.edges.push(joined);
                }
            }
        } else {
            self.orphans.extend(below(&self.graph, id));
        }
        self.graph.edges.retain(|e| e.from != id && e.to != id);
        self.graph.nodes.retain(|n| n.id != id);
        self.orphans.remove(id);
        self.changes.push(line);
        self.changed = true;
    }

    fn edge_add(&mut self, from: &str, to: &str, label: &str) {
        if self.graph.node(from).is_none() || self.graph.node(to).is_none() {
            return self.unknown(
                steve_i18n::t!("flow.draft.edge_add"),
                &format!("{from} → {to}"),
            );
        }
        let e = Edge {
            from: from.into(),
            to: to.into(),
            label: label.into(),
        };
        if self.graph.edges.contains(&e) {
            return;
        }
        self.changes.push(Change::new(
            steve_i18n::t!("flow.draft.edge_add"),
            "—",
            edge_text(&self.graph, &e),
        ));
        self.graph.edges.push(e);
        self.changed = true;
    }

    fn edge_remove(&mut self, from: &str, to: &str, label: Option<&str>) {
        let Some(i) = self.graph.edges.iter().position(|e| {
            e.from == from && e.to == to && label.is_none_or(|l| e.label == l.trim())
        }) else {
            return self.unknown(
                steve_i18n::t!("flow.draft.edge_remove"),
                &format!("{from} → {to}"),
            );
        };
        let e = self.graph.edges.remove(i);
        self.changes.push(Change::new(
            steve_i18n::t!("flow.draft.edge_remove"),
            edge_text(&self.graph, &e),
            "—",
        ));
        self.changed = true;
    }

    /// Drops the nodes below removed ones that nothing reaches any more, with a line each.
    pub fn finish(&mut self) {
        let Some(t) = self.graph.trigger().map(|t| t.id.clone()) else {
            return;
        };
        let reach = below(&self.graph, &t);
        let gone: Vec<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| self.orphans.contains(&n.id) && !reach.contains(&n.id))
            .map(|n| n.id.clone())
            .collect();
        for id in &gone {
            self.changes.push(Change::new(
                steve_i18n::t!("flow.draft.node_remove"),
                quoted(&self.graph, id),
                "—",
            ));
        }
        self.graph.nodes.retain(|n| !gone.contains(&n.id));
        self.graph
            .edges
            .retain(|e| !gone.contains(&e.from) && !gone.contains(&e.to));
    }
}

struct FlowTarget(Arc<LateFlow>);

fn node_ops(cx: &FlowCx) -> Vec<OpSpec> {
    let kinds: Vec<&str> = cx
        .nodes
        .kinds()
        .into_iter()
        .filter(|k| *k != "trigger")
        .collect();
    vec![
        OpSpec::new(
            "node.add",
            &format!(
                "{{\"after\": <node id>, \"kind\": {}, \"title\": <short Chinese title>, \"config\": <node config, e.g. {{\"action\": \"ignore\", \"reason\": \"…\"}}>, \"edge_label\": <label of the edge from \"after\" it goes on, \"\" for the plain one>}} (what was on that edge follows the new node)",
                kinds.join("|")
            ),
        ),
        OpSpec::new(
            "node.update",
            "{\"id\": <node id>, \"set\": {\"title\"?, \"note\"?, \"config\"?: <only the config keys to change, null removes>, \"on_fail\"?}}",
        ),
        OpSpec::new(
            "node.remove",
            "{\"id\": <node id>} (a node with one way out is cut out; otherwise what hangs only below it goes too)",
        ),
        OpSpec::new(
            "edge.add",
            "{\"from\": <node id>, \"to\": <node id>, \"label\": \"\"}",
        ),
        OpSpec::new(
            "edge.remove",
            "{\"from\": <node id>, \"to\": <node id>, \"label\"?}",
        ),
    ]
}

impl DraftTarget for FlowTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let cx = self.0.get()?;
        let f = own_flow(
            c,
            &viewer.ws,
            id.ok_or_else(|| ApiError::not_found("流程"))?,
        )?;
        Ok(Prepared {
            target_id: Some(f.id.clone()),
            revision: Some(f.updated),
            title: steve_i18n::t!("flow.draft.flow_title", name = f.name),
            href: format!("#/auto/rule/{}", f.id),
            subject: format!(
                "the automation flow \"{}\". Its current graph (node ids are what the graph operations refer to): {}",
                f.name, f.graph
            ),
            fields: vec![
                FieldSpec::new("name", "名字", "text", json!(f.name)),
                FieldSpec::new("description", "说明", "text", json!(f.description)),
                FieldSpec::new("enabled", "启用", "enum", json!(yes_no(f.enabled)))
                    .values(&["是", "否"]),
            ],
            ops: node_ops(&cx),
            free_text: Some("description".into()),
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
        let cx = self.0.get()?;
        let f = own_flow(c, &viewer.ws, p.target_id.as_deref().unwrap_or(""))?;
        let kinds = cx.nodes.kinds();
        let mut edit = GraphEdit::new(Graph::from_value(&f.graph).unwrap_or_default(), &kinds);
        let mut set = Map::new();
        let mut meta = Vec::new();
        for op in raw {
            if edit.apply(&op) {
                continue;
            }
            if op["op"] != "set" {
                meta.push(Change::new(unrecognized(), "—", show(&op.to_string())));
                continue;
            }
            let key = op["key"].as_str().unwrap_or("");
            let said = text_of(&op["value"]).unwrap_or_default();
            let bad = |label: String| Change::new(label, "—", unrecognized_value(&said));
            let (name_l, desc_l, enabled_l) = (
                steve_i18n::t!("flow.draft.flow_name"),
                steve_i18n::t!("flow.draft.flow_description"),
                steve_i18n::t!("flow.draft.enabled"),
            );
            match key {
                "name" if (1..=NAME_MAX).contains(&said.chars().count()) => {
                    meta.push(Change::new(name_l, show(&f.name), show(&said)));
                    set.insert("name".into(), json!(said));
                }
                "name" => meta.push(bad(name_l)),
                "description" if said.chars().count() <= DESCRIPTION_MAX => {
                    meta.push(Change::new(desc_l, show(&f.description), show(&said)));
                    set.insert("description".into(), json!(said));
                }
                "description" => meta.push(bad(desc_l)),
                "enabled" => match parse_bool(&said) {
                    Some(v) => {
                        meta.push(Change::new(
                            enabled_l,
                            yes_no_shown(f.enabled),
                            yes_no_shown(v),
                        ));
                        set.insert("enabled".into(), json!(v));
                    }
                    None => meta.push(bad(enabled_l)),
                },
                _ => meta.push(Change::new(
                    unrecognized(),
                    "—",
                    show(&format!("{key}: {said}")),
                )),
            }
        }
        edit.finish();
        let mut plan = Plan::default();
        if edit.changed {
            match check_graph(c, &cx, &viewer.ws, &edit.graph.to_value()) {
                Ok(g) => {
                    plan.changes.extend(edit.changes);
                    // branch/edge mismatches this edit brings in: said, not refused
                    let before = crate::graph::branch_warnings(
                        &Graph::from_value(&f.graph).unwrap_or_default(),
                    );
                    for w in crate::graph::branch_warnings(&g) {
                        if !before
                            .iter()
                            .any(|b| (&b.node, &b.label, b.kind) == (&w.node, &w.label, w.kind))
                        {
                            plan.changes.push(Change::new(
                                steve_i18n::t!("flow.draft.warning"),
                                "—",
                                w.text,
                            ));
                        }
                    }
                    plan.ops.push(json!({
                        "op": "flow.save", "id": f.id, "graph": g.to_value(), "note": NOTE,
                    }));
                }
                Err(ApiError::Internal(e)) => return Err(ApiError::Internal(e)),
                // nothing half-done goes into the draft: the reason is the only line
                Err(e) => plan.changes.push(Change::new(
                    steve_i18n::t!("flow.draft.flow"),
                    "—",
                    steve_i18n::t!("flow.draft.cannot_change", reason = e),
                )),
            }
        } else {
            plan.changes.extend(edit.changes);
        }
        plan.changes.extend(meta);
        if !set.is_empty() {
            plan.ops
                .push(json!({ "op": "flow.patch", "id": f.id, "set": set }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(flows::get(c, id)?
            .filter(|f| f.workspace_id == ws && !f.deleted)
            .map(|f| f.updated))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(flows::list(c, &viewer.ws, None)?
            .into_iter()
            .filter(|f| f.kind != "schedule")
            .map(|f| Candidate {
                code: format!("flow:{}", f.id),
                label: format!("流程 {}", f.name),
            })
            .collect())
    }
}

/// `flow.save {id, graph, note?, author?}`: the graph as a new version (validated again at
/// apply). Author `steve` unless given (Steve drafted it, you confirmed). Undo saves the graph
/// from before as one more version: versions only ever grow.
pub struct SaveOp(pub Arc<LateFlow>);

impl ChangeOp for SaveOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let flow = self.0.get()?;
        own_flow(c, cx.ws, op["id"].as_str().unwrap_or(""))?;
        check_graph(c, &flow, cx.ws, &op["graph"]).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let flow = self.0.get()?;
        let f = own_flow(tx, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let g = check_graph(tx, &flow, cx.ws, &op["graph"])?;
        let author = op["author"].as_str().unwrap_or("steve");
        let note = op["note"].as_str().unwrap_or(NOTE);
        flows::save_version(tx, &f, &g, author, note, flow.now())?;
        flow.wake();
        Ok(json!({ "version": f.version, "graph": f.graph }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let flow = self.0.get()?;
        let Some(f) = flows::get(tx, op["id"].as_str().unwrap_or(""))?
            .filter(|f| f.workspace_id == cx.ws && !f.deleted)
        else {
            // deleted since: nothing to put back
            return Ok(());
        };
        let g = Graph::from_value(&before["graph"]).map_err(ApiError::rule)?;
        let note = format!(
            "撤销对话里的改动（回到 v{}）",
            before["version"].as_i64().unwrap_or(0)
        );
        flows::save_version(tx, &f, &g, cx.actor, &note, flow.now())?;
        flow.wake();
        Ok(())
    }
}

/// `flow.patch {id, set:{name?, description?, enabled?}}`: like `PATCH /flows/:id` (no new
/// version); switching on needs a graph that validates.
struct PatchOp(Arc<LateFlow>);

fn patch_values(f: &FlowRow, set: &Value) -> (String, String, bool) {
    (
        set["name"].as_str().unwrap_or(&f.name).trim().to_string(),
        set["description"]
            .as_str()
            .unwrap_or(&f.description)
            .trim()
            .to_string(),
        set["enabled"].as_bool().unwrap_or(f.enabled),
    )
}

impl ChangeOp for PatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let flow = self.0.get()?;
        let f = own_flow(c, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let (name, description, enabled) = patch_values(&f, &op["set"]);
        if !(1..=NAME_MAX).contains(&name.chars().count()) {
            return Err(ApiError::invalid("name"));
        }
        if description.chars().count() > DESCRIPTION_MAX {
            return Err(ApiError::invalid("description"));
        }
        if enabled && !f.enabled {
            check_graph(c, &flow, cx.ws, &f.graph)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let flow = self.0.get()?;
        let f = own_flow(tx, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let (name, description, enabled) = patch_values(&f, &op["set"]);
        flows::patch(
            tx,
            &f,
            cx.actor,
            Some(&name),
            Some(&description),
            Some(enabled),
            flow.now(),
        )?;
        flow.wake();
        Ok(json!({ "name": f.name, "description": f.description, "enabled": f.enabled }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let flow = self.0.get()?;
        let Some(f) = flows::get(tx, op["id"].as_str().unwrap_or(""))?
            .filter(|f| f.workspace_id == cx.ws && !f.deleted)
        else {
            return Ok(());
        };
        let (name, description, enabled) = patch_values(&f, before);
        flows::patch(
            tx,
            &f,
            cx.actor,
            Some(&name),
            Some(&description),
            Some(enabled),
            flow.now(),
        )?;
        flow.wake();
        Ok(())
    }
}
