//! Repairs of a graph Steve drafted, before it is validated: the mistakes a model makes whose fix is
//! unambiguous are corrected instead of failing the whole draft. Graphs you save are not repaired;
//! the validator stays as strict as it is.
//!
//! | mistake | repair | noted |
//! | --- | --- | --- |
//! | node id that does not match the pattern | a valid id; edges and `steps.<id>` updated | no |
//! | title / note too long | cut | no |
//! | the same edge twice | once | no |
//! | `item.create` (or `item.link` creating) with an `accept` other than auto / inherit | `inherit` | yes |
//! | `bot` field named `branch` / `reason` | `branch_name` / `reason_text`, `steps.<id>.<field>` updated | yes |
//! | the same `bot` branch or field twice | once | no |
//! | `bot_id` / `bot` that is not a Bot of the workspace | removed (the default Bot) | yes |
//! | `http` method in lower case, `effect` as text | upper case, a boolean | no |
//! | `http` request other than GET without `effect` | `effect: true` (asks before it runs) | yes |
//! | `cmd` `timeout_s` out of range | clamped | yes |
//! | an `on_fail` that is not valid | removed (stop) | yes |
//!
//! Each noted repair goes into the node's note (stored, so in zh-CN) and into the returned list (in the
//! request's language).

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::graph::{self, NOTE_MAX, TITLE_MAX};
use crate::nodes::action::ITEM_ACCEPTS;
use crate::nodes::bot::RESERVED_FIELDS;
use crate::nodes::cmd::MAX_TIMEOUT_S;

/// A repair that changes what the flow does.
#[derive(Debug, Clone, PartialEq)]
enum Fix {
    Accept(String),
    Field(String, String),
    Bot(String),
    OnFail(String),
    HttpEffect(String),
    CmdTimeout(u64),
}

impl Fix {
    /// In the current language.
    fn text(&self) -> String {
        use steve_i18n::t;
        match self {
            Fix::Accept(v) => t!("flow.flow_draft.fix_accept", value = v),
            Fix::Field(from, to) => t!("flow.flow_draft.fix_field", from = from, to = to),
            Fix::Bot(b) => t!("flow.flow_draft.fix_bot", bot = b),
            Fix::OnFail(v) => t!("flow.flow_draft.fix_on_fail", value = v),
            Fix::HttpEffect(m) => t!("flow.flow_draft.fix_http_effect", method = m),
            Fix::CmdTimeout(v) => t!("flow.flow_draft.fix_cmd_timeout", value = v),
        }
    }
}

/// Repairs `g` in place; returns the notes of the repairs that change what the flow does.
/// `bots`: the ids of the workspace's Bots.
pub fn repair(g: &mut Value, bots: &HashSet<String>) -> Vec<String> {
    let Some(nodes) = g["nodes"].as_array().cloned() else {
        return Vec::new();
    };
    if nodes.iter().any(|n| !n.is_object()) {
        return Vec::new();
    }
    fix_ids(g);
    let mut noted: Vec<(usize, Fix)> = Vec::new();
    let labels = edge_labels(g);
    let n = g["nodes"].as_array().map_or(0, Vec::len);
    for i in 0..n {
        let node = &mut g["nodes"][i];
        cut_text(node, "title", TITLE_MAX);
        cut_text(node, "note", NOTE_MAX);
        let id = node["id"].as_str().unwrap_or("").to_string();
        if !node["config"].is_object() {
            continue;
        }
        let kind = node["kind"].as_str().unwrap_or("").to_string();
        let cfg = &mut node["config"];
        let mut fixes = Vec::new();
        let mut field_renames = Vec::new();
        match kind.as_str() {
            "action" => {
                fix_accept(cfg, &mut fixes);
                fix_bot_ref(cfg, "bot", bots, &mut fixes);
            }
            "bot" => {
                dedupe_list(cfg, "branches");
                dedupe_list(cfg, "fields");
                fix_bot_ref(cfg, "bot_id", bots, &mut fixes);
                field_renames = fix_fields(cfg, &mut fixes);
            }
            "http" => fix_http(cfg, &mut fixes),
            "cmd" => fix_cmd(cfg, &mut fixes),
            _ => {}
        }
        let node = &mut g["nodes"][i];
        if let Some(f) = node["on_fail"].as_str().map(String::from) {
            let ok = f == "stop"
                || f == "continue"
                || f.strip_prefix("branch:").is_some_and(|l| {
                    !l.is_empty() && labels.contains(&(id.clone(), l.to_string()))
                });
            if !ok {
                node.as_object_mut().map(|o| o.remove("on_fail"));
                fixes.push(Fix::OnFail(f));
            }
        }
        for (from, to) in field_renames {
            for (a, b) in [
                (format!("steps.{id}.{from}"), format!("steps.{id}.{to}")),
                (
                    format!("steps.{id}.fields.{from}"),
                    format!("steps.{id}.fields.{to}"),
                ),
            ] {
                rewrite_configs(g, &a, &b);
            }
        }
        noted.extend(fixes.into_iter().map(|f| (i, f)));
    }
    dedupe_edges(g);
    note_nodes(g, &noted)
}

/// Writes the notes into their nodes' notes (first, so a cut keeps them) and lists them.
fn note_nodes(g: &mut Value, noted: &[(usize, Fix)]) -> Vec<String> {
    let mut out = Vec::new();
    let n = g["nodes"].as_array().map_or(0, Vec::len);
    for i in 0..n {
        let fixes: Vec<&Fix> = noted
            .iter()
            .filter(|(j, _)| *j == i)
            .map(|(_, f)| f)
            .collect();
        if fixes.is_empty() {
            continue;
        }
        let node = &mut g["nodes"][i];
        let name = match node["title"].as_str().map(str::trim) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => node["id"].as_str().unwrap_or("").to_string(),
        };
        for f in &fixes {
            out.push(steve_i18n::t!(
                "flow.flow_draft.fix_at",
                node = name,
                fix = f.text()
            ));
        }
        // the graph is stored: its note is in zh-CN like every stored text
        let mut note = steve_i18n::stored(|| {
            let texts: Vec<String> = fixes.iter().map(|f| f.text()).collect();
            steve_i18n::t!("flow.flow_draft.fix_note", fixes = texts.join("；"))
        });
        if let Some(old) = node["note"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            note = format!("{note} {old}");
        }
        node["note"] = json!(note.chars().take(NOTE_MAX).collect::<String>());
    }
    out
}

fn cut_text(node: &mut Value, key: &str, max: usize) {
    if let Some(s) = node[key].as_str()
        && s.chars().count() > max
    {
        node[key] = json!(s.chars().take(max).collect::<String>());
    }
}

/// `(from, label)` of every edge.
fn edge_labels(g: &Value) -> HashSet<(String, String)> {
    g["edges"]
        .as_array()
        .map(|es| {
            es.iter()
                .map(|e| {
                    (
                        e["from"].as_str().unwrap_or("").to_string(),
                        e["label"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A valid id for `id`, not in `taken`.
fn valid_id_for(id: &str, index: usize, taken: &HashSet<String>) -> String {
    let mut s: String = id
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !s.chars().any(|c| c.is_ascii_alphanumeric()) {
        s = format!("n{index}");
    }
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s = format!("n{s}");
    }
    let base: String = s.chars().take(16).collect();
    let mut candidate = base.clone();
    let mut k = 2;
    while taken.contains(&candidate) || !graph::valid_id(&candidate) {
        let suffix = format!("_{k}");
        candidate = format!(
            "{}{suffix}",
            base.chars().take(16 - suffix.len()).collect::<String>()
        );
        k += 1;
    }
    candidate
}

/// Gives nodes with an invalid id a valid one, updating the edges and the `steps.<id>` references.
fn fix_ids(g: &mut Value) {
    let nodes = g["nodes"].as_array().cloned().unwrap_or_default();
    let mut taken: HashSet<String> = nodes
        .iter()
        .filter_map(|n| n["id"].as_str())
        .filter(|id| graph::valid_id(id))
        .map(String::from)
        .collect();
    let mut renames = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let Some(id) = n["id"].as_str() else {
            continue;
        };
        if graph::valid_id(id) {
            continue;
        }
        let new = valid_id_for(id, i + 1, &taken);
        taken.insert(new.clone());
        g["nodes"][i]["id"] = json!(new);
        renames.push((id.to_string(), new));
    }
    if renames.is_empty() {
        return;
    }
    if let Some(es) = g["edges"].as_array_mut() {
        for e in es {
            for end in ["from", "to"] {
                if let Some((_, new)) = renames.iter().find(|(old, _)| e[end] == old.as_str()) {
                    e[end] = json!(new);
                }
            }
        }
    }
    // longest first, so `steps.ab` is not taken for a part of `steps.ab-c`
    renames.sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));
    for (old, new) in renames {
        rewrite_configs(g, &format!("steps.{old}"), &format!("steps.{new}"));
    }
}

/// Characters of a path segment ([`crate::expr::valid_path`]).
fn path_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '$'
}

/// `s` with the path `from` replaced by `to` where it stands as a whole (not part of a longer path
/// segment, not after a dot).
fn rewrite_path(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().last().or_else(|| out.chars().last());
        let after = rest[i + from.len()..].chars().next();
        out.push_str(&rest[..i]);
        if before.is_some_and(|c| path_char(c) || c == '.') || after.is_some_and(path_char) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

fn rewrite_value(v: &mut Value, from: &str, to: &str) {
    match v {
        Value::String(s) if s.contains(from) => *s = rewrite_path(s, from, to),
        Value::Array(a) => a.iter_mut().for_each(|x| rewrite_value(x, from, to)),
        Value::Object(m) => m.values_mut().for_each(|x| rewrite_value(x, from, to)),
        _ => {}
    }
}

/// Rewrites a path in every node's config.
fn rewrite_configs(g: &mut Value, from: &str, to: &str) {
    if let Some(ns) = g["nodes"].as_array_mut() {
        for n in ns {
            rewrite_value(&mut n["config"], from, to);
        }
    }
}

/// Does this action create an item (`item.create`, `item.link` that creates when nothing is found)?
fn creates_item(cfg: &Value) -> bool {
    cfg["action"] == "item.create" || (cfg["action"] == "item.link" && cfg["fallback"] == "create")
}

fn fix_accept(cfg: &mut Value, fixes: &mut Vec<Fix>) {
    if !creates_item(cfg) {
        return;
    }
    let ok = match &cfg["accept"] {
        Value::Null => true,
        Value::String(s) => ITEM_ACCEPTS.contains(&s.as_str()),
        _ => false,
    };
    if !ok {
        let value = match &cfg["accept"] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        cfg["accept"] = json!("inherit");
        fixes.push(Fix::Accept(value));
    }
}

/// Removes a Bot reference that is not a Bot of the workspace (templates are left alone).
fn fix_bot_ref(cfg: &mut Value, key: &str, bots: &HashSet<String>, fixes: &mut Vec<Fix>) {
    let Some(b) = cfg[key].as_str().map(str::trim) else {
        return;
    };
    if b.is_empty() || b.contains("{{") || bots.contains(b) {
        return;
    }
    let b = b.to_string();
    cfg.as_object_mut().map(|o| o.remove(key));
    fixes.push(Fix::Bot(b));
}

fn dedupe_list(cfg: &mut Value, key: &str) {
    if let Some(a) = cfg[key].as_array_mut() {
        let mut seen = Vec::new();
        a.retain(|x| {
            if seen.contains(x) {
                false
            } else {
                seen.push(x.clone());
                true
            }
        });
    }
}

/// Renames the reserved field names of a `bot` node; returns `(from, to)` of each.
fn fix_fields(cfg: &mut Value, fixes: &mut Vec<Fix>) -> Vec<(String, String)> {
    let Some(fields) = cfg["fields"].as_array_mut() else {
        return Vec::new();
    };
    let mut taken: HashSet<String> = fields
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect();
    let mut renames = Vec::new();
    for f in fields.iter_mut() {
        let Some(name) = f.as_str().filter(|s| RESERVED_FIELDS.contains(s)) else {
            continue;
        };
        let base = if name == "branch" {
            "branch_name".to_string()
        } else {
            format!("{name}_text")
        };
        let mut to = base.clone();
        let mut k = 2;
        while taken.contains(&to) {
            to = format!("{base}_{k}");
            k += 1;
        }
        taken.insert(to.clone());
        fixes.push(Fix::Field(name.to_string(), to.clone()));
        renames.push((name.to_string(), to.clone()));
        *f = json!(to);
    }
    renames
}

fn fix_http(cfg: &mut Value, fixes: &mut Vec<Fix>) {
    if let Some(m) = cfg["method"].as_str() {
        let up = m.trim().to_ascii_uppercase();
        if up != m {
            cfg["method"] = json!(up);
        }
    }
    if let Some(e) = cfg["effect"].as_str() {
        match e.trim() {
            "true" => cfg["effect"] = json!(true),
            "false" => cfg["effect"] = json!(false),
            _ => {}
        }
    }
    let method = cfg["method"].as_str().unwrap_or("GET").to_string();
    let effect_ok = matches!(cfg.get("effect"), None | Some(Value::Null | Value::Bool(_)));
    if method != "GET"
        && crate::nodes::http::METHODS.contains(&method.as_str())
        && effect_ok
        && cfg["effect"] != json!(true)
    {
        cfg["effect"] = json!(true);
        fixes.push(Fix::HttpEffect(method));
    }
}

fn fix_cmd(cfg: &mut Value, fixes: &mut Vec<Fix>) {
    let t = match &cfg["timeout_s"] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let Some(t) = t else {
        return;
    };
    let clamped = t.clamp(1.0, MAX_TIMEOUT_S as f64).ceil() as u64;
    if (clamped as f64 - t).abs() > f64::EPSILON || cfg["timeout_s"].is_string() {
        cfg["timeout_s"] = json!(clamped);
        if (clamped as f64 - t).abs() > f64::EPSILON {
            fixes.push(Fix::CmdTimeout(clamped));
        }
    }
}

fn dedupe_edges(g: &mut Value) {
    if let Some(es) = g["edges"].as_array_mut() {
        let mut seen = HashSet::new();
        es.retain(|e| {
            seen.insert((
                e["from"].to_string(),
                e["to"].to_string(),
                e["label"].as_str().unwrap_or("").to_string(),
            ))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::NodeRegistry;
    use crate::nodes::action::Actions;

    fn bots() -> HashSet<String> {
        HashSet::from(["ops".to_string(), "fe".to_string()])
    }

    fn valid(g: &Value) -> Result<crate::graph::Graph, String> {
        graph::validate(g, &NodeRegistry::builtin(Actions::default()))
    }

    #[test]
    fn a_valid_graph_is_left_alone() {
        let mut g = json!({
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "触发", "config": {"source": "github"}},
                {"id": "b", "kind": "bot", "title": "判断", "config": {"bot_id": "ops", "instruction": "看看", "branches": ["是", "否"], "fields": ["summary"]}},
                {"id": "a", "kind": "action", "title": "建事项", "config": {"action": "item.create", "accept": "auto", "bot": "fe"}},
                {"id": "h", "kind": "http", "title": "查", "config": {"url": "https://example.com"}}
            ],
            "edges": [{"from": "t", "to": "b"}, {"from": "b", "to": "a", "label": "是"}, {"from": "b", "to": "h", "label": "否"}]
        });
        let before = g.clone();
        assert!(repair(&mut g, &bots()).is_empty());
        assert_eq!(g, before);
        assert!(valid(&g).is_ok());
    }

    #[test]
    fn accept_becomes_inherit_with_a_note() {
        let mut g = json!({
            "nodes": [
                {"id": "t", "kind": "trigger", "config": {"source": "github"}},
                {"id": "a", "kind": "action", "title": "建事项交给前端 Bot", "note": "原来的说明", "config": {"action": "item.create", "mode": "start", "accept": "manual"}},
                {"id": "l", "kind": "action", "title": "关联", "config": {"action": "item.link", "by": "branch", "fallback": "create", "accept": "review"}}
            ],
            "edges": [{"from": "t", "to": "a"}, {"from": "a", "to": "l"}]
        });
        assert!(valid(&g).is_err());
        let notes = repair(&mut g, &bots());
        assert_eq!(g["nodes"][1]["config"]["accept"], "inherit");
        assert_eq!(g["nodes"][2]["config"]["accept"], "inherit");
        assert_eq!(notes.len(), 2);
        assert_eq!(
            notes[0],
            "「建事项交给前端 Bot」：验收方式「manual」不支持，改成了 inherit（沿用 Bot 或项目的验收方式）"
        );
        let note = g["nodes"][1]["note"].as_str().unwrap();
        assert!(
            note.starts_with("Steve 起草时改过：验收方式「manual」"),
            "{note}"
        );
        assert!(note.ends_with("原来的说明"), "{note}");
        valid(&g).unwrap();
    }

    #[tokio::test]
    async fn the_list_is_in_the_request_language_the_note_in_zh_cn() {
        let mut g = json!({"nodes": [{"id": "a", "kind": "action", "title": "建事项", "config": {"action": "item.create", "accept": "manual"}}]});
        let notes = steve_i18n::scope(steve_i18n::EN, async { repair(&mut g, &bots()) }).await;
        assert!(
            notes[0].contains("acceptance “manual” isn't supported"),
            "{notes:?}"
        );
        assert!(
            g["nodes"][0]["note"]
                .as_str()
                .unwrap()
                .starts_with("Steve 起草时改过：")
        );
    }

    #[test]
    fn reserved_fields_are_renamed_everywhere() {
        let mut g = json!({
            "nodes": [
                {"id": "t", "kind": "trigger", "config": {"source": "github"}},
                {"id": "judge", "kind": "bot", "title": "判断测试是否偶发", "config": {"instruction": "是不是偶发", "branches": ["偶发", "不是偶发"], "fields": ["branch", "reason", "reason_text"]}},
                {"id": "f", "kind": "filter", "config": {"rules": [{"label": "主干", "when": "steps.judge.branch == \"main\""}], "else_label": "其他"}},
                {"id": "n", "kind": "action", "config": {"action": "notify", "text": "{{steps.judge.branch}}：{{ steps.judge.fields.reason }}，{{steps.judge.branches}}"}}
            ],
            "edges": [{"from": "t", "to": "judge"}, {"from": "judge", "to": "f", "label": "不是偶发"}, {"from": "judge", "to": "n", "label": "偶发"}]
        });
        let notes = repair(&mut g, &bots());
        assert_eq!(
            g["nodes"][1]["config"]["fields"],
            json!(["branch_name", "reason_text_2", "reason_text"])
        );
        assert_eq!(
            g["nodes"][2]["config"]["rules"][0]["when"],
            "steps.judge.branch_name == \"main\""
        );
        assert_eq!(
            g["nodes"][3]["config"]["text"],
            "{{steps.judge.branch_name}}：{{ steps.judge.fields.reason_text_2 }}，{{steps.judge.branches}}"
        );
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("字段名 branch 是保留的，改成了 branch_name"));
        valid(&g).unwrap();
    }

    #[test]
    fn bad_ids_are_made_valid_with_their_references() {
        let mut g = json!({
            "nodes": [
                {"id": "Trigger", "kind": "trigger", "config": {"source": "github"}},
                {"id": "check-lint", "kind": "bot", "config": {"instruction": "看看", "branches": ["是", "否", "是"]}},
                {"id": "check", "kind": "action", "config": {"action": "notify", "text": "{{steps.check-lint.reason}} {{steps.check.x}}"}},
                {"id": "判断", "kind": "action", "config": {"action": "ignore"}},
                {"id": "a_very_long_node_identifier", "kind": "action", "config": {"action": "ignore"}}
            ],
            "edges": [
                {"from": "Trigger", "to": "check-lint"},
                {"from": "check-lint", "to": "check", "label": "是"},
                {"from": "check-lint", "to": "check", "label": "是"},
                {"from": "check-lint", "to": "判断", "label": "否"},
                {"from": "判断", "to": "a_very_long_node_identifier"}
            ]
        });
        assert!(repair(&mut g, &bots()).is_empty());
        let ids: Vec<&str> = g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["trigger", "check_lint", "check", "n4", "a_very_long_node"]
        );
        assert_eq!(
            g["nodes"][2]["config"]["text"],
            "{{steps.check_lint.reason}} {{steps.check.x}}"
        );
        assert_eq!(g["nodes"][1]["config"]["branches"], json!(["是", "否"]));
        assert_eq!(g["edges"].as_array().unwrap().len(), 4);
        valid(&g).unwrap();
    }

    #[test]
    fn other_unambiguous_mistakes() {
        let mut g = json!({
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "触".repeat(70), "config": {"source": "github"}},
                {"id": "b", "kind": "bot", "title": "判断", "config": {"bot_id": "frontend", "instruction": "看看", "branches": ["是"]}, "on_fail": "branch:出错"},
                {"id": "a", "kind": "action", "title": "建事项", "config": {"action": "item.create", "bot": "frontend-bot"}},
                {"id": "h", "kind": "http", "title": "重跑", "config": {"method": "post", "url": "https://api.github.com/x", "effect": "false"}},
                {"id": "c", "kind": "cmd", "title": "跑测试", "config": {"command": "npm test", "project_id": "p", "timeout_s": 3600}}
            ],
            "edges": [{"from": "t", "to": "b"}, {"from": "b", "to": "a", "label": "是"}, {"from": "a", "to": "h"}, {"from": "h", "to": "c"}]
        });
        let notes = repair(&mut g, &bots());
        assert_eq!(g["nodes"][0]["title"].as_str().unwrap().chars().count(), 60);
        assert!(g["nodes"][1]["config"].get("bot_id").is_none());
        assert!(g["nodes"][1].get("on_fail").is_none());
        assert!(g["nodes"][2]["config"].get("bot").is_none());
        assert_eq!(g["nodes"][3]["config"]["method"], "POST");
        assert_eq!(g["nodes"][3]["config"]["effect"], true);
        assert_eq!(g["nodes"][4]["config"]["timeout_s"], MAX_TIMEOUT_S);
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("没有 Bot「frontend」")));
        assert!(
            notes
                .iter()
                .any(|n| n.contains("出错处理「branch:出错」不对"))
        );
        assert!(notes.iter().any(|n| n.contains("POST 请求会改动外部系统")));
        valid(&g).unwrap();
    }

    #[test]
    fn paths_are_rewritten_only_as_a_whole() {
        assert_eq!(
            rewrite_path(
                "steps.a.branch steps.a.branches xsteps.a.branch {{steps.a.branch}}",
                "steps.a.branch",
                "steps.a.branch_name"
            ),
            "steps.a.branch_name steps.a.branches xsteps.a.branch {{steps.a.branch_name}}"
        );
        assert_eq!(
            rewrite_path("event.steps.a.b", "steps.a", "steps.z"),
            "event.steps.a.b"
        );
    }
}
