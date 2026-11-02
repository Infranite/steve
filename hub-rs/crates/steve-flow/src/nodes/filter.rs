//! `filter`: `{rules:[{label, when}], else_label}`. The first rule whose condition holds names the
//! branch; none → `else_label`. The output is the input, passed through, so the next node sees the
//! same thing.

use async_trait::async_trait;
use serde_json::Value;

use super::{NodeKind, NodeOutcome, RunCx, opt_str};
use crate::expr;

pub struct Filter;

pub const MAX_RULES: usize = 20;

#[async_trait]
impl NodeKind for Filter {
    fn kind(&self) -> &'static str {
        "filter"
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let rules = match cfg.get("rules") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(a)) => a.as_slice(),
            Some(_) => return Err("rules 只能是列表".into()),
        };
        if rules.len() > MAX_RULES {
            return Err(format!("条件最多 {MAX_RULES} 条"));
        }
        for (i, r) in rules.iter().enumerate() {
            let label = r["label"].as_str().unwrap_or("").trim();
            if label.is_empty() {
                return Err(format!("第 {} 条条件要有分支名", i + 1));
            }
            let when = r["when"].as_str().unwrap_or("");
            expr::parse(when).map_err(|e| format!("第 {} 条条件：{e}", i + 1))?;
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
