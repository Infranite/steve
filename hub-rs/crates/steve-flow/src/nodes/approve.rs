//! `approve`: `{question, draft, options:[label]}`. Opens a decision `{kind:"approve",
//! subject_type:"flow_step", subject_id:"<run>:<node>"}` and parks the run; the label you choose is
//! the branch ([`crate::decisions::FlowStepDecisions`]). Choosing anything lets the effectful nodes
//! after it on this run's path go ahead: the approval is yours.

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_store::NewDecision;

use super::{NodeKind, NodeOutcome, Rule, RunCx, WaitSpec, req_str, str_list};

pub struct Approve;

const QUESTION_MAX: usize = 500;
const OPTIONS_MAX: usize = 6;
const OPTION_MAX: usize = 20;

/// `subject_type` of decisions about one step of a run.
pub const SUBJECT: &str = "flow_step";

/// The options: the configured ones, else the labels of the node's edges.
pub fn options(cx: &RunCx<'_>, cfg: &Value) -> Vec<String> {
    let mut opts = str_list(cfg, "options").unwrap_or_default();
    opts.retain(|o| !o.trim().is_empty());
    if opts.is_empty() {
        for e in cx.graph.outs(&cx.node.id) {
            if !e.label.is_empty() && !opts.contains(&e.label) {
                opts.push(e.label.clone());
            }
        }
    }
    if opts.is_empty() {
        opts = vec!["同意".into(), "不同意".into()];
    }
    opts
}

/// `outcome_label` while the run waits for you.
pub fn waiting_label(question: &str) -> String {
    let q: String = question.chars().take(60).collect();
    format!("等你审批：{q}")
}

#[async_trait]
impl NodeKind for Approve {
    fn kind(&self) -> &'static str {
        "approve"
    }

    fn rules(&self) -> Vec<Rule> {
        vec![
            Rule::new(
                &["flow.node.missing_question", "flow.node.too_long_question"],
                format!("question 必填，最多 {QUESTION_MAX} 字"),
            ),
            Rule::new(&["flow.node.approve_draft"], "draft 只能是文字"),
            Rule::new(
                &["flow.node.approve_options"],
                format!("options 最多 {OPTIONS_MAX} 个，每个 1–{OPTION_MAX} 字"),
            ),
        ]
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        req_str(cfg, "question", super::Required::Question, QUESTION_MAX)?;
        if let Some(d) = cfg.get("draft")
            && !d.is_null()
            && !d.is_string()
        {
            return Err(steve_i18n::t!("flow.node.approve_draft"));
        }
        let opts = str_list(cfg, "options")?;
        if opts.len() > OPTIONS_MAX
            || opts
                .iter()
                .any(|o| o.trim().is_empty() || o.chars().count() > OPTION_MAX)
        {
            return Err(steve_i18n::t!("flow.node.approve_options"));
        }
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let question = cx.render(cfg["question"].as_str().unwrap_or(""), input);
        let draft = cfg["draft"]
            .as_str()
            .map(|d| cx.render(d, input))
            .filter(|d| !d.is_empty());
        let opts = options(cx, cfg);
        cx.effects.outcome = Some("need".into());
        cx.effects.label = Some(waiting_label(&question));
        NodeOutcome::Wait(Box::new(WaitSpec {
            mode: "decision",
            until: None,
            matcher: json!({ "kind": "approve" }),
            decision: Some(NewDecision {
                workspace_id: cx.ws().to_string(),
                kind: "approve".into(),
                subject_type: SUBJECT.into(),
                subject_id: String::new(),
                item_id: cx.run.work_item_id.clone(),
                session_id: None,
                question,
                draft,
                options: Some(opts.clone()),
                requested_by: "steve".into(),
            }),
            output: json!({ "waiting": "decision", "options": opts }),
        }))
    }
}
