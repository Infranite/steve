//! Steve drafts and edits flows, relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `POST /flows/draft` | `{source, text: 2..2000}` → `Flow`: the smart tier (90 s) drafts a graph from the node registry; it is validated and stored disabled (`origin=steve`, “Steve 起草，待你确认”) |
//! | `POST /flows/:id/edit-draft` | `{text: 1..2000, graph}` → `{graph, note}`: “让 Steve 修改这条流程” on the canvas's working copy, **not saved** |
//!
//! The prompt states every constraint the validator enforces ([`crate::graph::rules`] and each
//! kind's [`crate::nodes::NodeKind::rules`], built from the validators' own constants). A drafted
//! graph is first repaired where the fix is unambiguous ([`crate::draft_repair`]; the repairs that
//! change what the flow does are noted on their nodes and returned as `draft_notes`). One that still
//! does not validate is sent back to the model once with the error and, node by node, the config
//! that is wrong and the rules of its kind; a second failure is
//! `400 Steve 起草的流程没有通过检查：<error>`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Path;
use axum::routing::post;
use axum::{Extension, Json, Router};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::changes::first_json;
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules, Tier};

use crate::FlowCx;
use crate::flows::{self, DESCRIPTION_MAX, NAME_MAX, NewFlow};
use crate::flows_api::check_graph;
use crate::graph::{self, Graph};
use crate::nodes::{NodeRegistry, Rule};

/// The smart tier's limit for one draft.
pub const TIMEOUT: Duration = Duration::from_secs(90);
/// The start of the draft and edit prompts (tests script the fake gateway by them).
pub const DRAFT_START: &str = "你在为 Steve 起草一条自动化流程";
pub const EDIT_START: &str = "你在修改 Steve 的一条自动化流程";
/// Event sources a draft can start from (plus those of registered adapters).
pub const SOURCES: [&str; 8] = [
    "mail", "cal", "im", "github", "webhook", "time", "item", "manual",
];

const NOTE_DEFAULT: &str = "按你说的改了";

type Cx = Extension<Arc<FlowCx>>;

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/flows/draft", post(draft))
        .route("/flows/{id}/edit-draft", post(edit_draft))
}

/// What the model is told about a node kind; registered kinds without an entry are listed by name.
fn guide(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "trigger" => {
            "trigger {source, types:[事件类型], connections:[], scopes:[], spaces:[], cron?, tz?, missed?}：唯一的起点，没有连进来的线"
        }
        "filter" => {
            "filter {rules:[{label, when:\"表达式，如 event.payload.from matches \\\"(?i)noreply\\\"\"}], else_label}：第一个成立的 rule 走同名的出边，都不成立走 else_label"
        }
        "table" => {
            "table {hit:\"first\"|\"collect\", inputs:[路径], outputs:[键], rows:[[单元格]]}：单元格空 = 任意，\"<= 500\"、\"\\\"交通\\\", \\\"餐饮\\\"\"、\"\\\"auto\\\"\"；出边 label 等于第一个输出值"
        }
        "script" => {
            "script {code}：沙箱 JS，export const handler = async (input) => …，返回值是输出"
        }
        "cmd" => "cmd {command, project_id, timeout_s ≤600}：在项目目录执行命令（每次要审批）",
        "http" => {
            "http {method, url, headers{}, body, effect:bool}：effect=false 只能 GET；有副作用的请求 effect=true（要审批）"
        }
        "flow" => "flow {flow_id, input}：调用另一条流程",
        "bot" => {
            "bot {bot_id?, instruction, branches:[label], fields?:[name]}：让 Bot 判断走哪个分支（出边 label 与 branches 一致），可提取字段，之后用 {{steps.<id>.<字段>}}"
        }
        "wait" => {
            "wait {mode:\"delay\"|\"until\"|\"event\", delay:\"30m\", until:\"next_workday 09:00\", match:{source,type,when}, timeout:\"3d\", timeout_label}：等待"
        }
        "approve" => "approve {question, draft, options:[label]}：问你，你选的 label 就是分支",
        "action" => {
            "action {action, …}：item.create {mode:\"start\"|\"propose\", bot?, project?, tags?, title?, goal?, accept?:\"auto\"|\"inherit\"}、item.link {by:\"branch\"|\"ref\"|\"thread\", fallback:\"create\"|\"none\"}、notify {text}、digest {text}、ignore {reason}；渠道模块另有 reply_as、send_to_space"
        }
        _ => return None,
    })
}

fn rule_texts(rules: Vec<Rule>) -> String {
    rules
        .into_iter()
        .map(|r| r.text)
        .collect::<Vec<_>>()
        .join("；")
}

fn kinds_text(reg: &NodeRegistry) -> String {
    reg.kinds()
        .into_iter()
        .map(|k| {
            let mut line = format!("- {}", guide(k).map_or_else(|| k.to_string(), String::from));
            let rules = reg.get(k).map(|n| n.rules()).unwrap_or_default();
            if !rules.is_empty() {
                line.push_str(&format!("\n  规则：{}", rule_texts(rules)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The workspace's enabled bots, for `bot_id` / `bot` references.
fn bots_text(c: &Connection, ws: &str) -> ApiResult<String> {
    let mut st = c.prepare(
        "SELECT b.id, b.name, b.role, b.main FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ? WHERE b.deleted = 0 AND b.enabled = 1 ORDER BY b.main DESC, b.name",
    )?;
    let rows = st
        .query_map([ws], |r| {
            Ok(format!(
                "- {}（{}{}）{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                if r.get::<_, i64>(3)? != 0 {
                    "，主 Bot"
                } else {
                    ""
                },
                r.get::<_, String>(2)?
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.join("\n"))
}

/// Every constraint the validator enforces, for the draft and edit prompts: the graph's, then each
/// node kind's with its guide.
fn constraints(reg: &NodeRegistry) -> String {
    let graph_rules: Vec<String> = graph::rules()
        .into_iter()
        .map(|r| format!("- {}", r.text))
        .collect();
    format!(
        "图的规则（都会检查，不符合就整个草稿作废）：\n{}\n- 标题和说明用中文\n\n可用的节点种类和各自的规则：\n{}",
        graph_rules.join("\n"),
        kinds_text(reg)
    )
}

/// The ids of the workspace's Bots (as [`check_graph`] accepts them).
fn bot_ids(c: &Connection, ws: &str) -> ApiResult<HashSet<String>> {
    let mut st = c.prepare(
        "SELECT bw.bot_id FROM bot_workspaces bw JOIN bots b ON b.id = bw.bot_id WHERE bw.workspace_id = ? AND b.deleted = 0",
    )?;
    let ids = st
        .query_map([ws], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(ids)
}

/// What is wrong with a graph that did not validate, node by node: the error, the config as it is
/// and the rules of its kind (model-facing, so called in zh-CN).
fn problems(g: &Value, reg: &NodeRegistry) -> Vec<String> {
    let Some(nodes) = g["nodes"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in nodes {
        let id = n["id"].as_str().unwrap_or("?");
        let kind = n["kind"].as_str().unwrap_or("");
        let title = n["title"].as_str().unwrap_or("");
        let Some(k) = reg.get(kind) else {
            out.push(format!(
                "- 节点 id「{id}」：没有「{kind}」这种节点，kind 只能是 {}",
                reg.kinds().join("、")
            ));
            continue;
        };
        let cfg = &n["config"];
        if !cfg.is_object() {
            out.push(format!("- 节点 id「{id}」（{kind}）：config 要是对象"));
            continue;
        }
        if let Err(e) = k.validate(cfg) {
            out.push(format!(
                "- 节点 id「{id}」（{kind}，「{title}」）：{e}\n  它现在的 config：{}\n  {kind} 的规则：{}",
                serde_json::to_string(cfg).unwrap_or_default(),
                rule_texts(k.rules_for(cfg))
            ));
        }
    }
    out
}

/// Asks the model, takes the first JSON object of its answer.
async fn ask(cx: &FlowCx, prompt: &str) -> ApiResult<Value> {
    let gw = cx.modules.gateway.clone();
    let out = steve_modules::gateway::purpose("draft", gw.run(Tier::Smart, prompt, TIMEOUT))
        .await
        .map_err(|e| {
            tracing::warn!("flow draft: gateway failed: {e}");
            ApiError::rule(steve_i18n::t!("flow.flow_draft.no_model"))
        })?;
    first_json(&out.text)
        .ok_or_else(|| ApiError::rule(steve_i18n::t!("flow.flow_draft.unreadable")))
}

/// Puts the requested source on the drafted trigger when the model left it out or changed it.
fn force_source(g: &mut Value, source: &str) {
    if let Some(nodes) = g["nodes"].as_array_mut() {
        for n in nodes {
            if n["kind"] == "trigger" {
                if !n["config"].is_object() {
                    n["config"] = json!({});
                }
                n["config"]["source"] = json!(source);
            }
        }
    }
}

/// Repairs and validates a drafted graph; on failure asks the model once more with what is wrong.
/// Returns the graph, the answer and the notes of the repairs (in the request's language).
async fn settle_graph(
    cx: &FlowCx,
    ws: &str,
    prompt: &str,
    mut answer: Value,
    fix: impl Fn(&mut Value),
) -> ApiResult<(Graph, Value, Vec<String>)> {
    let bots = cx.store().reader(|c| bot_ids(c, ws))?;
    for attempt in 0..2 {
        let mut g = answer["graph"].clone();
        fix(&mut g);
        let notes = crate::draft_repair::repair(&mut g, &bots);
        // the model reads the reason, so it is checked in zh-CN
        let checked = steve_i18n::scope(steve_i18n::ZH_CN, async {
            cx.store().reader(|c| check_graph(c, cx, ws, &g))
        })
        .await;
        match checked {
            Ok(graph) => return Ok((graph, answer, notes)),
            Err(ApiError::Rule(e)) if attempt == 0 => {
                let lines =
                    steve_i18n::scope(steve_i18n::ZH_CN, async { problems(&g, &cx.nodes) }).await;
                let detail = if lines.is_empty() {
                    String::new()
                } else {
                    format!("要改的地方：\n{}\n", lines.join("\n"))
                };
                let retry = format!(
                    "{prompt}\n\n你上次的回答：{}\n没有通过检查：{e}\n{detail}请按上面的规则改正（其余保持原样）后重新回复完整的 JSON。",
                    serde_json::to_string(&answer).unwrap_or_default()
                );
                answer = ask(cx, &retry).await?;
            }
            Err(ApiError::Rule(zh)) => {
                // the same check once more for the person, in the request's language
                let e = match cx.store().reader(|c| check_graph(c, cx, ws, &g)) {
                    Err(ApiError::Rule(e)) => e,
                    _ => zh,
                };
                return Err(ApiError::rule(steve_i18n::t!(
                    "flow.flow_draft.failed_check",
                    reason = e
                )));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns on its second attempt")
}

fn cut(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

#[derive(Debug, Deserialize)]
struct DraftBody {
    source: String,
    text: String,
}

async fn draft(Extension(cx): Cx, who: Caller, Body(b): Body<DraftBody>) -> ApiResult<Json<Value>> {
    let source = b.source.trim().to_string();
    let text = b.text.trim().to_string();
    let mut c = Check::new();
    c.len("text", &text, 2, 2000);
    c.that(
        "source",
        SOURCES.contains(&source.as_str()) || cx.events.all().iter().any(|a| a.source() == source),
    );
    c.done()?;
    let bots = cx.store().reader(|c| bots_text(c, &who.ws))?;
    let prompt = format!(
        "{DRAFT_START}。用户说的是：\n<<<\n{text}\n>>>\n触发来源：{source}\n\n{}\n\n工作空间里的 Bot（bot_id 或 bot 只能用这些 id，不填就交给默认的 Bot）：\n{bots}\n\n\
         只回复 JSON，不要别的文字：{{\"name\":\"<流程名，最多 {NAME_MAX} 字>\",\"description\":\"<一句话说明>\",\"graph\":{{…}}}}",
        constraints(&cx.nodes),
    );
    let answer = ask(&cx, &prompt).await?;
    let (graph, answer, notes) =
        settle_graph(&cx, &who.ws, &prompt, answer, |g| force_source(g, &source)).await?;
    let mut name = cut(answer["name"].as_str().unwrap_or(""), NAME_MAX);
    if name.is_empty() {
        name = cut(&text, 20);
    }
    let description = cut(
        answer["description"].as_str().unwrap_or(""),
        DESCRIPTION_MAX,
    );
    let now = cx.now();
    let id = cx.store().transaction(|tx| {
        flows::create(
            tx,
            &who.ws,
            &who.me,
            &NewFlow {
                kind: "flow",
                name: &name,
                description: &description,
                graph: &graph,
                origin: "steve",
                origin_label: flows::origin_label("steve"),
                enabled: false,
            },
            now,
        )
    })?;
    let mut v = cx.store().reader(|c| -> ApiResult<_> {
        let f = flows::own(c, &who.ws, &id)?;
        flows::view(c, &f, now)
    })?;
    v["draft_notes"] = json!(notes);
    Ok(Json(v))
}

#[derive(Debug, Deserialize)]
struct EditBody {
    text: String,
    graph: Value,
}

async fn edit_draft(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<EditBody>,
) -> ApiResult<Json<Value>> {
    let text = b.text.trim().to_string();
    let mut c = Check::new();
    c.len("text", &text, 1, 2000);
    c.that("graph", b.graph.is_object());
    c.done()?;
    // the working copy may be half-edited: it only has to be a graph
    let working = Graph::from_value(&b.graph).map_err(ApiError::rule)?;
    let (f, bots) = cx.store().reader(|c| -> ApiResult<_> {
        let f = flows::own(c, &who.ws, &id)?;
        Ok((f, bots_text(c, &who.ws)?))
    })?;
    if f.kind == "schedule" {
        return Err(ApiError::rule(steve_i18n::t!(
            "flow.draft.schedule_elsewhere"
        )));
    }
    let prompt = format!(
        "{EDIT_START}「{}」。现在的图：\n{}\n\n用户要的改动：\n<<<\n{text}\n>>>\n\n{}\n\n工作空间里的 Bot（bot_id 或 bot 只能用这些 id）：\n{bots}\n\n\
         只改用户要求的部分，其余节点的 id、标题和配置保持原样。只回复 JSON，不要别的文字：{{\"graph\":{{…完整的图…}},\"note\":\"<一句话说明改了什么>\"}}",
        f.name,
        serde_json::to_string(&working.to_value()).unwrap_or_default(),
        constraints(&cx.nodes),
    );
    let answer = ask(&cx, &prompt).await?;
    let (graph, answer, notes) = settle_graph(&cx, &who.ws, &prompt, answer, |_| {}).await?;
    let mut note = cut(answer["note"].as_str().unwrap_or(""), 200);
    if note.is_empty() {
        note = NOTE_DEFAULT.into();
    }
    Ok(Json(json!({
        "graph": graph.to_value(),
        "note": note,
        "draft_notes": notes,
        "warnings": graph::branch_warnings(&graph),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::action::Actions;

    /// The sources of everything `check_graph` runs.
    const VALIDATORS: [(&str, &str); 15] = [
        ("graph.rs", include_str!("graph.rs")),
        ("flows_api.rs", include_str!("flows_api.rs")),
        ("expr.rs", include_str!("expr.rs")),
        ("nodes/mod.rs", include_str!("nodes/mod.rs")),
        ("nodes/action.rs", include_str!("nodes/action.rs")),
        ("nodes/approve.rs", include_str!("nodes/approve.rs")),
        ("nodes/bot.rs", include_str!("nodes/bot.rs")),
        ("nodes/cmd.rs", include_str!("nodes/cmd.rs")),
        ("nodes/filter.rs", include_str!("nodes/filter.rs")),
        ("nodes/flow.rs", include_str!("nodes/flow.rs")),
        ("nodes/http.rs", include_str!("nodes/http.rs")),
        ("nodes/script.rs", include_str!("nodes/script.rs")),
        ("nodes/table.rs", include_str!("nodes/table.rs")),
        ("nodes/trigger.rs", include_str!("nodes/trigger.rs")),
        ("nodes/wait.rs", include_str!("nodes/wait.rs")),
    ];

    fn all_rules(reg: &NodeRegistry) -> Vec<Rule> {
        let mut rules = graph::rules();
        for k in reg.kinds() {
            rules.extend(reg.get(k).map(|n| n.rules()).unwrap_or_default());
        }
        rules
    }

    #[test]
    fn the_prompt_states_every_constraint_the_validator_enforces() {
        let reg = NodeRegistry::builtin(Actions::default());
        let prompt = constraints(&reg);
        let rules = all_rules(&reg);
        let covered: HashSet<&str> = rules.iter().flat_map(|r| r.keys.iter().copied()).collect();
        // every message a validator can give is covered by a rule of the prompt
        let re = regex::Regex::new(r#""(flow\.(?:graph|node|expr)\.[a-z_]+)""#).unwrap();
        let (mut missing, mut found) = (Vec::new(), 0);
        for (file, src) in VALIDATORS {
            for c in re.captures_iter(src) {
                found += 1;
                let key = c.get(1).unwrap().as_str();
                if !covered.contains(key) {
                    missing.push(format!("{file}: {key}"));
                }
            }
        }
        assert!(found > 80, "the scan found only {found} messages");
        assert!(
            missing.is_empty(),
            "validator messages no draft rule states: {missing:#?}"
        );
        // no rule names a message that does not exist
        let zh = include_str!("../locales/zh-CN.json");
        for k in &covered {
            assert!(zh.contains(&format!("\"{k}\"")), "no message {k}");
        }
        // every rule is in the prompt, kind rules under their kind
        for r in &rules {
            assert!(
                prompt.contains(&r.text),
                "missing in the prompt: {}",
                r.text
            );
        }
        // the two the reporter hit, with their allowed values from the validators' constants
        assert!(
            prompt.contains("item.create 的 accept 只能是 auto、inherit"),
            "{prompt}"
        );
        assert!(prompt.contains("fields 不能叫 branch、reason"), "{prompt}");
        assert!(prompt.contains("item.create 的 mode 只能是 start、propose"));
        assert!(prompt.contains("item.link 的 by 只能是 branch、ref、thread"));
        assert!(prompt.contains("method 只能是 GET、POST、PUT、PATCH、DELETE、HEAD"));
    }

    #[test]
    fn problems_name_the_node_its_config_and_the_rules() {
        let reg = NodeRegistry::builtin(Actions::default());
        let g = json!({"nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "github"}},
            {"id": "w", "kind": "wait", "title": "等一下", "config": {"mode": "sleep"}},
            {"id": "x", "kind": "fax", "config": {}}
        ]});
        let p = problems(&g, &reg);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(
            p[0].starts_with(
                "- 节点 id「w」（wait，「等一下」）：mode 只能是 delay、until 或 event"
            )
        );
        assert!(p[0].contains("它现在的 config：{\"mode\":\"sleep\"}"));
        assert!(p[0].contains("wait 的规则：mode 只能是 delay、until、event"));
        assert!(p[1].contains("没有「fax」这种节点"));
    }
}
