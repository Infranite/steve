//! The flow graph: its shape, validation, and the edge rules the engine
//! walks by. The JSON is the canvas one-to-one; nothing here touches the database.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::nodes::NodeRegistry;

/// Nodes per graph.
pub const MAX_NODES: usize = 60;

/// One node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default = "empty_object")]
    pub config: Value,
    /// `stop` (default) | `continue` | `branch:<label>`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

impl Node {
    /// The name shown in messages: the title, else the id.
    pub fn name(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.id
        } else {
            self.title.trim()
        }
    }

    pub fn on_fail(&self) -> OnFail {
        match self.on_fail.as_deref().unwrap_or("stop") {
            "continue" => OnFail::Continue,
            s => match s.strip_prefix("branch:") {
                Some(l) => OnFail::Branch(l.to_string()),
                None => OnFail::Stop,
            },
        }
    }
}

/// What a failing node does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnFail {
    Stop,
    Continue,
    Branch(String),
}

/// One edge; `label` selects it when a node names a branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

fn id_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{0,15}$").expect("id regex"))
}

impl Graph {
    pub fn from_value(v: &Value) -> Result<Self, String> {
        serde_json::from_value(v.clone()).map_err(|_| steve_i18n::t!("flow.graph.bad_format"))
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"nodes": [], "edges": []}))
    }

    /// A new flow's graph: only a trigger.
    pub fn starter(source: &str) -> Self {
        Self {
            nodes: vec![Node {
                id: "t".into(),
                kind: "trigger".into(),
                title: "触发".into(),
                note: String::new(),
                config: json!({ "source": source }),
                on_fail: None,
            }],
            edges: Vec::new(),
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn trigger(&self) -> Option<&Node> {
        self.nodes.iter().find(|n| n.kind == "trigger")
    }

    /// Outgoing edges in their order.
    pub fn outs<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Edge> + 'a {
        self.edges.iter().filter(move |e| e.from == id)
    }

    /// The edge a node continues on. A named branch takes the edge with that label (none = the path
    /// ends here). No branch takes the unlabelled edge, else the first one.
    pub fn next<'a>(&'a self, id: &'a str, branch: Option<&'a str>) -> Option<&'a Edge> {
        match branch {
            Some(b) => self.outs(id).find(|e| e.label == b),
            None => self
                .outs(id)
                .find(|e| e.label.is_empty())
                .or_else(|| self.outs(id).next()),
        }
    }

    /// The edge of the first outgoing edge whose target is in `prefer` (dry runs of `approve` and
    /// `bot`), else the first edge.
    pub fn preferred<'a>(&'a self, id: &'a str, prefer: &'a [String]) -> Option<&'a Edge> {
        self.outs(id)
            .find(|e| prefer.contains(&e.to))
            .or_else(|| self.outs(id).next())
    }

    /// Does the graph use a model (`bot` nodes)?
    pub fn uses_ai(&self) -> bool {
        self.nodes.iter().any(|n| n.kind == "bot")
    }
}

/// Validation; the error is the `400` text.
pub fn validate(v: &Value, reg: &NodeRegistry) -> Result<Graph, String> {
    let g = Graph::from_value(v)?;
    if g.nodes.is_empty() {
        return Err(steve_i18n::t!("flow.graph.one_trigger"));
    }
    if g.nodes.len() > MAX_NODES {
        return Err(steve_i18n::t!("flow.graph.too_many_nodes", max = MAX_NODES));
    }
    let mut ids = HashSet::new();
    for n in &g.nodes {
        if !id_re().is_match(&n.id) {
            return Err(steve_i18n::t!("flow.graph.bad_id", id = n.id));
        }
        if !ids.insert(n.id.as_str()) {
            return Err(steve_i18n::t!("flow.graph.duplicate_id", id = n.id));
        }
        if n.title.chars().count() > 60 || n.note.chars().count() > 200 {
            return Err(steve_i18n::t!("flow.graph.text_too_long", node = n.name()));
        }
    }
    let triggers: Vec<&Node> = g.nodes.iter().filter(|n| n.kind == "trigger").collect();
    if triggers.len() != 1 {
        return Err(steve_i18n::t!("flow.graph.one_trigger"));
    }
    let trigger = triggers[0].id.clone();
    let mut seen_edges = HashSet::new();
    for e in &g.edges {
        for end in [&e.from, &e.to] {
            if !ids.contains(end.as_str()) {
                return Err(steve_i18n::t!("flow.graph.edge_to_nowhere", node = end));
            }
        }
        if e.to == trigger {
            return Err(steve_i18n::t!("flow.graph.edge_into_trigger"));
        }
        if e.from == e.to {
            return Err(steve_i18n::t!(
                "flow.graph.cycle",
                node = name_of(&g, &e.from)
            ));
        }
        if e.label.chars().count() > 40 {
            return Err(steve_i18n::t!("flow.graph.edge_label_too_long"));
        }
        if !seen_edges.insert((e.from.as_str(), e.to.as_str(), e.label.as_str())) {
            return Err(steve_i18n::t!(
                "flow.graph.duplicate_edge",
                node = name_of(&g, &e.from)
            ));
        }
    }
    for n in &g.nodes {
        let Some(k) = reg.get(&n.kind) else {
            return Err(steve_i18n::t!("flow.graph.unknown_kind", kind = n.kind));
        };
        if !n.config.is_object() {
            return Err(steve_i18n::t!("flow.graph.bad_config", node = n.name()));
        }
        k.validate(&n.config)
            .map_err(|e| steve_i18n::t!("flow.graph.node_error", node = n.name(), error = e))?;
        if let Some(f) = &n.on_fail {
            let ok = f == "stop"
                || f == "continue"
                || f.strip_prefix("branch:")
                    .is_some_and(|l| !l.is_empty() && g.outs(&n.id).any(|e| e.label == l));
            if !ok {
                return Err(steve_i18n::t!("flow.graph.bad_on_fail", node = n.name()));
            }
        }
    }
    if let Some(at) = cycle(&g) {
        return Err(steve_i18n::t!("flow.graph.cycle", node = name_of(&g, &at)));
    }
    let reach = reachable(&g, &trigger);
    let lost = g.nodes.len() - reach.len();
    if lost > 0 {
        return Err(steve_i18n::t!("flow.graph.unreachable", count = lost));
    }
    Ok(g)
}

fn name_of(g: &Graph, id: &str) -> String {
    g.node(id)
        .map_or_else(|| id.to_string(), |n| n.name().to_string())
}

fn reachable(g: &Graph, from: &str) -> HashSet<String> {
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

/// A node on a cycle, if there is one (iterative three-colour DFS).
fn cycle(g: &Graph) -> Option<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum C {
        White,
        Grey,
        Black,
    }
    let mut colour: HashMap<&str, C> = g.nodes.iter().map(|n| (n.id.as_str(), C::White)).collect();
    for start in &g.nodes {
        if colour[start.id.as_str()] != C::White {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start.id.as_str(), 0)];
        colour.insert(start.id.as_str(), C::Grey);
        while let Some((n, i)) = stack.pop() {
            let outs: Vec<&str> = g.outs(n).map(|e| e.to.as_str()).collect();
            if i < outs.len() {
                stack.push((n, i + 1));
                let m = outs[i];
                match colour.get(m).copied() {
                    Some(C::Grey) => return Some(m.to_string()),
                    Some(C::White) => {
                        colour.insert(m, C::Grey);
                        stack.push((m, 0));
                    }
                    _ => {}
                }
            } else {
                colour.insert(n, C::Black);
            }
        }
    }
    None
}
