//! `filter`: `{rules:[{label, when}], else_label}`. The first rule whose condition holds names the
//! branch; none → `else_label`. The output is the input, passed through, so the next node sees the
//! same thing.

use async_trait::async_trait;
use serde_json::Value;

use super::{NodeKind, NodeOutcome, Rule, RunCx, opt_str};
use crate::expr;

pub struct Filter;

pub const MAX_RULES: usize = 20;

#[async_trait]
impl NodeKind for Filter {
    fn kind(&self) -> &'static str {
        "filter"
    }

    fn rules(&self) -> Vec<Rule> {
        vec![
            Rule::new(
                &[
                    "flow.node.filter_rules",
                    "flow.node.filter_max",
                    "flow.node.filter_label",
                ],
                format!("rules 是列表，最多 {MAX_RULES} 条，每条都要有非空的 label"),
            ),
            Rule::new(
                &["flow.node.filter_rule"],
                "when 是条件表达式：路径（event.payload.x、steps.<id>.x）配 == != < <= > >= contains matches in，用 and or not 组合，文字加引号",
            ),
        ]
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let rules = match cfg.get("rules") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(a)) => a.as_slice(),
            Some(_) => return Err(steve_i18n::t!("flow.node.filter_rules")),
        };
        if rules.len() > MAX_RULES {
            return Err(steve_i18n::t!("flow.node.filter_max", max = MAX_RULES));
        }
        for (i, r) in rules.iter().enumerate() {
            let label = r["label"].as_str().unwrap_or("").trim();
            if label.is_empty() {
                return Err(steve_i18n::t!("flow.node.filter_label", n = i + 1));
            }
            let when = r["when"].as_str().unwrap_or("");
            expr::parse(when)
                .map_err(|e| steve_i18n::t!("flow.node.filter_rule", n = i + 1, error = e))?;
        }
        opt_str(cfg, "else_label")?;
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let scope = cx.scope(input);
        let hit = cfg["rules"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| expr::eval(r["when"].as_str().unwrap_or(""), &scope))
            .and_then(|r| r["label"].as_str());
        let branch = hit
            .or_else(|| cfg["else_label"].as_str())
            .unwrap_or("")
            .to_string();
        NodeOutcome::branch(input.clone(), branch)
    }
}
