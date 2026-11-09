//! The seams to the flow module: signals into `steve_flow::ingest`, the `gh_pr`
//! lookup of `item.link`, and the “PR 检查失败” template installed when the first repo is linked.
//!
//! S2's `item.link` only accepts its built-in methods, so this module registers an `item.link`
//! that accepts `by:"gh_pr"` too and hands every other method to S2's own action unchanged
//! ([`GhItemLink`]). For `gh_pr` the supplement is the spec's fixed text, so the agent knows to use
//! `steve_github_ci_log` and to push to the same branch.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_flow::flows::{self, NewFlow};
use steve_flow::nodes::action::ItemLink;
use steve_flow::nodes::{NodeOutcome, Rule, RunCx};
use steve_flow::{Action, ActorRef, FlowCx, FlowResult, ItemFinder, RawEvent};
use steve_modules::ApiError;
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};

use crate::GitHub;
use crate::events::{EventSink, SignalEvent};

/// The `item.link` method of this module: the item a PR event's `payload.item_id` names.
pub const BY: &str = "gh_pr";
/// `flows.origin` of the template.
pub const TEMPLATE_ORIGIN: &str = "template:github";
pub const TEMPLATE_NAME: &str = "PR 检查失败";
/// D2: a new issue becomes a suggestion (a proposed item the user starts).
pub const ISSUE_TEMPLATE_ORIGIN: &str = "template:github-issues";
pub const ISSUE_TEMPLATE_NAME: &str = "新 issue";

/// [`EventSink`] over S1: `RawEvent { source: "github", .. }` through the repo's connection.
pub struct FlowSink(pub Arc<dyn steve_flow::EventSink>);

#[async_trait]
impl EventSink for FlowSink {
    async fn ingest(&self, ws: &str, connection_id: &str, ev: SignalEvent) -> Result<(), String> {
        let mut raw = RawEvent::new(
            "github",
            ev.type_key,
            ev.external_id,
            ev.occurred,
            ev.summary,
        );
        raw.thread_key = ev.thread_key;
        raw.group_key = ev.group_key;
        raw.scope_ext_id = ev.scope_ext_id;
        raw.actor = ev.actor.map(|a| ActorRef::Handle {
            system: "github".into(),
            handle: a.login,
            display_name: None,
            signature_org: None,
        });
        raw.payload = ev.payload;
        self.0
            .ingest(ws, Some(connection_id), raw)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// `item.link {by:"gh_pr"}`: `payload.item_id` when that item still exists in the workspace.
pub struct GhPrFinder;

impl ItemFinder for GhPrFinder {
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        let Some(id) = event["payload"]["item_id"].as_str() else {
            return Ok(None);
        };
        Ok(crate::repo::item_brief(c, ws, id)?.map(|_| id.to_string()))
    }
}

/// S2's `item.link` plus `by:"gh_pr"` (see the module docs).
pub struct GhItemLink;

const GH_PR_FALLBACK: &str = "by 为 gh_pr 时 fallback 只能是 none";

/// The supplement for a PR whose checks failed.
pub fn check_failed_text(payload: &Value) -> String {
    let names: Vec<&str> = payload["failed"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["name"].as_str()).collect())
        .unwrap_or_default();
    format!(
        "PR #{} 的检查失败：{}。用 steve_github_ci_log 读日志后修复，修完推送到同一分支。",
        payload["number"],
        names.join("、")
    )
}

#[async_trait]
impl Action for GhItemLink {
    fn name(&self) -> &'static str {
        "item.link"
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        if cfg["by"] != BY {
            return ItemLink.validate(cfg);
        }
        match cfg["fallback"].as_str().unwrap_or("none") {
            "none" => Ok(()),
            _ => Err(GH_PR_FALLBACK.into()),
        }
    }
    fn rules(&self) -> Vec<Rule> {
        let mut v = ItemLink.rules();
        v.push(Rule::new(
            &[],
            format!("by 也可以是 {BY}（按 PR 找事项），这时 {GH_PR_FALLBACK}"),
        ));
        v
    }
    fn outcome(&self) -> Option<&'static str> {
        Some("linked")
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if cfg["by"] != BY {
            return ItemLink.run(cx, cfg, input).await;
        }
        let ev = cx.event().clone();
        let ws = cx.ws().to_string();
        let found = cx
            .flow
            .store()
            .reader(|c| cx.flow.finders.find(c, &ws, BY, &ev));
        let item = match found {
            Ok(Some(i)) => i,
            Ok(None) => {
                cx.effects.label = Some("没有找到对应的事项".into());
                return NodeOutcome::next(json!({ "found": false }));
            }
            Err(e) => return NodeOutcome::Fail(e.to_string()),
        };
        let text = if ev["type"] == "github.check_failed" {
            check_failed_text(&ev["payload"])
        } else {
            format!("GitHub：{}", ev["summary"].as_str().unwrap_or(""))
        };
        if let Err(e) = cx.flow.core.supplement(&ws, &item, &text, "auto").await {
            return NodeOutcome::Fail(format!("补充到 {item} 失败：{e}"));
        }
        if let Some(e) = ev["id"].as_str() {
            let r = cx.flow.store().transaction(|tx| {
                links::add(
                    tx,
                    &ws,
                    &NewLink::new(
                        ObjRef::new("item", &item),
                        "from",
                        ObjRef::new("event", e),
                        "sig",
                    ),
                )
            });
            if let Err(err) = r {
                tracing::warn!(item = %item, "item.link link: {err}");
            }
        }
        cx.effects.outcome = Some("linked".into());
        cx.effects.label = Some(format!("关联到 {item}"));
        cx.effects.item_id = Some(item.clone());
        NodeOutcome::next(json!({ "found": true, "item_id": item }))
    }
}

/// The template graph (原型 r5): a failed check on a PR linked to an item whose repo says
/// `ci_fail=supplement` → supplement the item; a failure on the default branch → a suggestion (a
/// proposed item, D2); a linked PR with `ci_fail=signal` → nothing beyond the signal.
pub fn template_graph() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "检查失败",
             "config": {"source": "github", "types": ["github.check_failed"]}},
            {"id": "f", "kind": "filter", "title": "有没有关联事项",
             "config": {"rules": [
                 {"label": "交给事项", "when": "event.payload.item_id != null and event.payload.ci_fail == 'supplement'"},
                 {"label": "主干", "when": "event.payload.item_id == null"}
             ], "else_label": "只记录"}},
            {"id": "l", "kind": "action", "title": "送补充给事项",
             "config": {"action": "item.link", "by": BY, "fallback": "none"}},
            {"id": "n", "kind": "action", "title": "建议去修",
             "config": {"action": "item.create", "mode": "propose", "title": "主干检查失败：{{event.summary}}"}}
        ],
        "edges": [
            {"from": "t", "to": "f"},
            {"from": "f", "to": "l", "label": "交给事项"},
            {"from": "f", "to": "n", "label": "主干"}
        ]
    })
}

/// D2: a new issue on a linked repository → a suggestion card (a proposed item, its text quoted as
/// untrusted). The user starts it; set the node's mode to `start` to have Steve start such items
/// itself. YOLO never accepts a GitHub-sourced proposal on its own.
pub fn issue_template_graph() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "新 issue",
             "config": {"source": "github", "types": ["github.issue_opened"]}},
            {"id": "c", "kind": "action", "title": "建议处理",
             "config": {"action": "item.create", "mode": "propose", "title": "{{event.summary}}"}}
        ],
        "edges": [{"from": "t", "to": "c"}]
    })
}

impl GitHub {
    /// The flow module (template install). Unset: no template.
    pub fn set_flow(&self, f: Arc<FlowCx>) {
        *self.0.flow.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
        // workspaces with repositories from before a template existed get it once (D2)
        let wss: Vec<String> = self
            .store()
            .reader(|c| {
                let mut st = c.prepare("SELECT DISTINCT workspace_id FROM gh_repos")?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .unwrap_or_default();
        for ws in wss {
            match self.install_template(&ws, "steve") {
                Ok(Some(f)) => {
                    tracing::info!(flow = %f, ws = %ws, "github flow template installed")
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, ws = %ws, "github flow template not installed")
                }
            }
        }
    }

    pub(crate) fn flow(&self) -> Option<Arc<FlowCx>> {
        self.0
            .flow
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Installs “PR 检查失败” (enabled, `origin=template:github`) unless the workspace ever had it
    /// (a deleted template stays deleted). Returns the new flow's id.
    pub fn install_template(&self, ws: &str, actor: &str) -> Result<Option<String>, ApiError> {
        let issues = self.install_one(
            ws,
            actor,
            ISSUE_TEMPLATE_ORIGIN,
            ISSUE_TEMPLATE_NAME,
            "关联仓库里有新 issue 时，提一个建议事项，由你决定要不要开始。",
            issue_template_graph(),
        )?;
        let checks = self.install_one(
            ws,
            actor,
            TEMPLATE_ORIGIN,
            TEMPLATE_NAME,
            "关联事项的 PR 检查失败时，把失败的检查送给事项去修；主干失败时提一个建议事项。",
            template_graph(),
        )?;
        Ok(checks.or(issues))
    }

    /// One template, unless the workspace ever had it (a deleted template stays deleted).
    fn install_one(
        &self,
        ws: &str,
        actor: &str,
        origin: &'static str,
        name: &'static str,
        description: &'static str,
        graph: Value,
    ) -> Result<Option<String>, ApiError> {
        let Some(flow) = self.flow() else {
            return Ok(None);
        };
        let graph = steve_flow::graph::validate(&graph, &flow.nodes).map_err(ApiError::rule)?;
        let now = self.now();
        self.store().transaction(|tx| {
            let had: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM flows WHERE workspace_id = ? AND origin = ?)",
                rusqlite::params![ws, origin],
                |r| r.get(0),
            )?;
            if had {
                return Ok(None);
            }
            let id = flows::create(
                tx,
                ws,
                actor,
                &NewFlow {
                    kind: "flow",
                    name,
                    description,
                    graph: &graph,
                    origin,
                    origin_label: flows::origin_label(origin),
                    enabled: true,
                },
                now,
            )?;
            Ok(Some(id))
        })
    }
}
