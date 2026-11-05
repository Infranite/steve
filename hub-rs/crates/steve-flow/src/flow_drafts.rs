//! Steve drafts and edits flows, relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `POST /flows/draft` | `{source, text: 2..2000}` → `Flow`: the smart tier (90 s) drafts a graph from the node registry; it is validated and stored disabled (`origin=steve`, “Steve 起草，待你确认”) |
//! | `POST /flows/:id/edit-draft` | `{text: 1..2000, graph}` → `{graph, note}`: “让 Steve 修改这条流程” on the canvas's working copy, **not saved** |
//!
//! A drafted graph that does not validate is sent back to the model once with the error; a second
//! failure is `400 Steve 起草的流程没有通过检查：<error>`.

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
use crate::graph::{Graph, MAX_NODES};

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
            "action {action, …}：item.create {mode:\"start\"|\"propose\", bot?, project?, tags?, title?, goal?, accept?}、item.link {by:\"branch\"|\"ref\"|\"thread\", fallback:\"create\"|\"none\"}、notify {text}、digest {text}、ignore {reason}；渠道模块另有 reply_as、send_to_space"
        }
        _ => return None,
    })
}

fn kinds_text(cx: &FlowCx) -> String {
    cx.nodes
        .kinds()
        .into_iter()
        .map(|k| format!("- {}", guide(k).map_or_else(|| k.to_string(), String::from)))
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

fn rules_text() -> String {
    format!(
        "图的格式：{{\"nodes\":[{{\"id\",\"kind\",\"title\",\"note\",\"config\":{{…}},\"on_fail\"?}}],\"edges\":[{{\"from\",\"to\",\"label\"}}]}}\n\
         规则：恰好一个 trigger 节点且没有入边；id 唯一，匹配 ^[a-z][a-z0-9_]{{0,15}}$；边两端都存在；不能有环；每个节点都要从 trigger 连到；最多 {MAX_NODES} 个节点；\
         filter、table、bot、approve 的出边 label 要和它们的分支名一致；on_fail 可以是 stop、continue 或 branch:<label>；\
         模板写 {{{{event.<路径>}}}}、{{{{steps.<id>.<路径>}}}}、{{{{sender.name}}}}；标题和说明用中文。\n\
         可用的节点种类："
    )
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

/// Validates a drafted graph; on failure asks the model once more with the error.
async fn settle_graph(
    cx: &FlowCx,
    ws: &str,
    prompt: &str,
    mut answer: Value,
    fix: impl Fn(&mut Value),
) -> ApiResult<(Graph, Value)> {
    for attempt in 0..2 {
        let mut g = answer["graph"].clone();
        fix(&mut g);
        // the model reads the reason, so it is checked in zh-CN
        let checked = steve_i18n::scope(steve_i18n::ZH_CN, async {
            cx.store().reader(|c| check_graph(c, cx, ws, &g))
        })
        .await;
        match checked {
            Ok(graph) => return Ok((graph, answer)),
            Err(ApiError::Rule(e)) if attempt == 0 => {
                let retry = format!(
                    "{prompt}\n\n你上次的回答：{}\n没有通过检查：{e}\n请改正后重新回复完整的 JSON。",
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
        "{DRAFT_START}。用户说的是：\n<<<\n{text}\n>>>\n触发来源：{source}\n\n{}\n{}\n\n工作空间里的 Bot（bot_id 或 bot 只能用这些 id，不填就交给默认的 Bot）：\n{bots}\n\n\
         只回复 JSON，不要别的文字：{{\"name\":\"<流程名，最多 {NAME_MAX} 字>\",\"description\":\"<一句话说明>\",\"graph\":{{…}}}}",
        rules_text(),
        kinds_text(&cx),
    );
    let answer = ask(&cx, &prompt).await?;
    let (graph, answer) =
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
    cx.store().reader(|c| -> ApiResult<_> {
        let f = flows::own(c, &who.ws, &id)?;
        Ok(Json(flows::view(c, &f, now)?))
    })
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
        "{EDIT_START}「{}」。现在的图：\n{}\n\n用户要的改动：\n<<<\n{text}\n>>>\n\n{}\n{}\n\n工作空间里的 Bot（bot_id 或 bot 只能用这些 id）：\n{bots}\n\n\
         只改用户要求的部分，其余节点的 id、标题和配置保持原样。只回复 JSON，不要别的文字：{{\"graph\":{{…完整的图…}},\"note\":\"<一句话说明改了什么>\"}}",
        f.name,
        serde_json::to_string(&working.to_value()).unwrap_or_default(),
        rules_text(),
        kinds_text(&cx),
    );
    let answer = ask(&cx, &prompt).await?;
    let (graph, answer) = settle_graph(&cx, &who.ws, &prompt, answer, |_| {}).await?;
    let mut note = cut(answer["note"].as_str().unwrap_or(""), 200);
    if note.is_empty() {
        note = NOTE_DEFAULT.into();
    }
    Ok(Json(json!({ "graph": graph.to_value(), "note": note })))
}
