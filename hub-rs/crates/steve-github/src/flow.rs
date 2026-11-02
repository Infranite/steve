//! The seams to the flow module (github.md §5.2): signals into `steve_flow::ingest`, the `gh_pr`
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
use steve_flow::nodes::{NodeOutcome, RunCx};
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

/// The supplement for a PR whose checks failed (§5.2).
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
            _ => Err("by 为 gh_pr 时 fallback 只能是 none".into()),
        }
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
/// `ci_fail=supplement` → supplement the item; a failure on the default branch → notify; a linked
/// PR with `ci_fail=signal` → nothing beyond the signal.
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
            {"id": "n", "kind": "action", "title": "提醒你",
             "config": {"action": "notify", "text": "{{event.summary}}"}}
        ],
        "edges": [
            {"from": "t", "to": "f"},
            {"from": "f", "to": "l", "label": "交给事项"},
            {"from": "f", "to": "n", "label": "主干"}
        ]
    })
}

impl GitHub {
    /// The flow module (template install). Unset: no template.
    pub fn set_flow(&self, f: Arc<FlowCx>) {
        *self.0.flow.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
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
        let Some(flow) = self.flow() else {
            return Ok(None);
        };
        let graph =
            steve_flow::graph::validate(&template_graph(), &flow.nodes).map_err(ApiError::rule)?;
        let now = self.now();
        self.store().transaction(|tx| {
            let had: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM flows WHERE workspace_id = ? AND origin = ?)",
                rusqlite::params![ws, TEMPLATE_ORIGIN],
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
                    name: TEMPLATE_NAME,
                    description: "关联事项的 PR 检查失败时，把失败的检查送给事项去修；主干失败时提醒你。",
                    graph: &graph,
                    origin: TEMPLATE_ORIGIN,
                    origin_label: flows::origin_label(TEMPLATE_ORIGIN),
                    enabled: true,
                },
                now,
            )?;
            Ok(Some(id))
        })
    }
}
