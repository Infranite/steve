//! The flow graph: its shape, validation, and the edge rules the engine
//! walks by. The JSON is the canvas one-to-one; nothing here touches the database.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::nodes::{NodeRegistry, Rule};

/// Nodes per graph.
pub const MAX_NODES: usize = 60;
/// Characters of a node's title, a node's note and an edge's label.
pub const TITLE_MAX: usize = 60;
pub const NOTE_MAX: usize = 200;
pub const EDGE_LABEL_MAX: usize = 40;
/// The node id pattern.
pub const ID_PATTERN: &str = r"^[a-z][a-z0-9_]{0,15}$";

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
    RE.get_or_init(|| Regex::new(ID_PATTERN).expect("id regex"))
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

/// A mismatch between a node's branches and its edges. Not an error: the graph still saves and
/// runs, but an event can end where you did not expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchWarning {
    pub node: String,
    /// The edge's or branch's label.
    pub label: String,
    /// `edge`: an edge whose label is none of the node's branches (never taken: strong);
    /// `branch`: a branch with no edge (the run ends there: weak).
    pub kind: &'static str,
    pub strong: bool,
    /// In the request's language.
    pub text: String,
}

fn str_items(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The branches a node can name, when they are set in its config (`None`: it names none, or takes
/// them from its edges).
pub fn branches_of(n: &Node) -> Option<Vec<String>> {
    let cfg = &n.config;
    let mut out = match n.kind.as_str() {
        "filter" => {
            let mut v: Vec<String> = cfg["rules"]
                .as_array()
                .map(|rs| {
                    rs.iter()
                        .filter_map(|r| r["label"].as_str())
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(e) = cfg["else_label"]
                .as_str()
                .map(str::trim)
                .filter(|e| !e.is_empty())
            {
                v.push(e.to_string());
            }
            v
        }
        "approve" => str_items(&cfg["options"]),
        "bot" => str_items(&cfg["branches"]),
        _ => return None,
    };
    if out.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    out.retain(|b| seen.insert(b.clone()));
    Some(out)
}

/// The branch/edge mismatches of `g` (strong ones first).
pub fn branch_warnings(g: &Graph) -> Vec<BranchWarning> {
    let mut strong = Vec::new();
    let mut weak = Vec::new();
    for n in &g.nodes {
        let fail_label = match n.on_fail() {
            OnFail::Branch(l) => Some(l),
            _ => None,
        };
        if let Some(bs) = branches_of(n) {
            let mut seen = HashSet::new();
            for e in g.outs(&n.id) {
                if e.label.is_empty()
                    || bs.contains(&e.label)
                    || fail_label.as_deref() == Some(e.label.as_str())
                    || !seen.insert(e.label.clone())
                {
                    continue;
                }
                strong.push(BranchWarning {
                    node: n.id.clone(),
                    label: e.label.clone(),
                    kind: "edge",
                    strong: true,
                    text: steve_i18n::t!(
                        "flow.graph.edge_no_branch",
                        node = n.name(),
                        label = e.label,
                        branches = bs.join("、")
                    ),
                });
            }
            for b in &bs {
                if !g.outs(&n.id).any(|e| &e.label == b) {
                    weak.push(BranchWarning {
                        node: n.id.clone(),
                        label: b.clone(),
                        kind: "branch",
                        strong: false,
                        text: steve_i18n::t!(
                            "flow.graph.branch_no_edge",
                            node = n.name(),
                            label = b
                        ),
                    });
                }
            }
        }
        if n.kind == "wait"
            && let Some(t) = n.config["timeout_label"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            && n.config.get("timeout").is_some_and(|v| !v.is_null())
            && !g.outs(&n.id).any(|e| e.label == t)
        {
            weak.push(BranchWarning {
                node: n.id.clone(),
                label: t.to_string(),
                kind: "branch",
                strong: false,
                text: steve_i18n::t!("flow.graph.branch_no_edge", node = n.name(), label = t),
            });
        }
    }
    strong.extend(weak);
    strong
}

/// Is this a valid node id?
pub fn valid_id(id: &str) -> bool {
    id_re().is_match(id)
}

/// What [`validate`] enforces of the graph as a whole, for the draft prompt.
pub fn rules() -> Vec<Rule> {
    vec![
        Rule::new(
            &["flow.graph.bad_format", "flow.graph.bad_config"],
            "图的格式：{\"nodes\":[{\"id\",\"kind\",\"title\",\"note\",\"config\":{…},\"on_fail\"?}],\"edges\":[{\"from\",\"to\",\"label\"}]}，config 是对象",
        ),
        Rule::new(
            &["flow.graph.one_trigger", "flow.graph.edge_into_trigger"],
            "恰好一个 trigger 节点，且没有连进它的线",
        ),
        Rule::new(
            &["flow.graph.bad_id", "flow.graph.duplicate_id"],
            format!(
                "id 唯一，匹配 {ID_PATTERN}（小写字母开头，只有小写字母、数字、下划线，最多 16 个字符）"
            ),
        ),
        Rule::new(
            &["flow.graph.text_too_long"],
            format!("title 最多 {TITLE_MAX} 字，note 最多 {NOTE_MAX} 字"),
        ),
        Rule::new(
            &[
                "flow.graph.edge_to_nowhere",
                "flow.graph.duplicate_edge",
                "flow.graph.edge_label_too_long",
            ],
            format!("边两端的节点都要存在，同样的边不要重复，label 最多 {EDGE_LABEL_MAX} 字"),
        ),
        Rule::new(&["flow.graph.cycle"], "不能有环，也不能连到自己"),
        Rule::new(&["flow.graph.unreachable"], "每个节点都要从 trigger 连到"),
        Rule::new(
            &["flow.graph.too_many_nodes"],
            format!("最多 {MAX_NODES} 个节点"),
        ),
        Rule::new(
            &["flow.graph.unknown_kind", "flow.graph.node_error"],
            "kind 只能是下面列出的节点种类，config 要符合该种类的规则",
        ),
        Rule::new(
            &["flow.graph.edge_no_branch", "flow.graph.branch_no_edge"],
            "filter、table、bot、approve 的出边 label 要和它们的分支名一致，每个分支都要有同名的出边（wait 的 timeout_label 也是）",
        ),
        Rule::new(
            &["flow.graph.bad_on_fail"],
            "on_fail 只能是 stop、continue 或 branch:<label>，branch 的 label 要是这个节点某条出边的 label",
        ),
        Rule::new(
            &["flow.node.text_list_only", "flow.node.text_only"],
            "config 里的文字字段只能是字符串，列表字段只能是字符串列表",
        ),
        Rule::new(
            &[
                "flow.expr.bad_template",
                "flow.expr.unclosed_quote",
                "flow.expr.bad_number",
                "flow.expr.bad_word",
                "flow.expr.bad_symbol",
                "flow.expr.bad_regex",
                "flow.expr.unclosed_paren",
                "flow.expr.unclosed_list",
                "flow.expr.incomplete",
                "flow.expr.empty",
                "flow.expr.too_long",
                "flow.expr.trailing",
            ],
            "模板写 {{event.<路径>}}、{{steps.<id>.<路径>}}、{{sender.name}}，路径只有字母、数字、_、-；条件表达式的引号、括号、[] 要配对",
        ),
    ]
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
        if n.title.chars().count() > TITLE_MAX || n.note.chars().count() > NOTE_MAX {
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
        if e.label.chars().count() > EDGE_LABEL_MAX {
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
