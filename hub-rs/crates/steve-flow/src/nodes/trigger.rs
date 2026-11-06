//! `trigger`: `{source, types[], connections[], scopes[], spaces[], cron?, tz?, missed?}`. It outputs
//! what started the run: the manual or sub-flow input, else the event, else `{at}`. Saving a flow
//! turns it into `flow_triggers` rows ([`crate::flows::rebuild_triggers`]).

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{NodeKind, NodeOutcome, Rule, RunCx, one_of, opt_str, str_list};
use crate::cron::{CronExpr, parse_tz};

pub struct Trigger;

/// `missed` of time triggers.
pub const MISSED: [&str; 2] = ["catch_up_once", "skip"];
/// The list fields and their length limit.
const LISTS: [&str; 4] = ["types", "connections", "scopes", "spaces"];
const LIST_MAX: usize = 50;
const SOURCE_MAX: usize = 40;

/// `flow_triggers.kind` of a trigger config.
pub fn index_kind(cfg: &Value) -> &'static str {
    match cfg["source"].as_str().unwrap_or("") {
        "time" => "time",
        "manual" => "manual",
        "item" => "item",
        "im" => "message",
        _ => "event",
    }
}

#[async_trait]
impl NodeKind for Trigger {
    fn kind(&self) -> &'static str {
        "trigger"
    }

    fn rules(&self) -> Vec<Rule> {
        vec![
            Rule::new(
                &["flow.node.trigger_source", "flow.node.trigger_source_long"],
                format!("source 必填，最多 {SOURCE_MAX} 字"),
            ),
            Rule::new(
                &["flow.node.trigger_list_max"],
                format!("{} 是文字列表，每个最多 {LIST_MAX} 项", LISTS.join("、")),
            ),
            Rule::new(
                &["flow.node.trigger_cron", "flow.node.trigger_tz"],
                "source 为 time 时要有 5 段的 cron（如 \"0 9 * * 1-5\"），tz 是 IANA 时区名（如 Asia/Shanghai）",
            ),
            Rule::new(
                &["flow.node.trigger_missed"],
                format!("missed 只能是 {}", one_of(&MISSED)),
            ),
        ]
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let source = opt_str(cfg, "source")?.unwrap_or("").trim();
        if source.is_empty() {
            return Err(steve_i18n::t!("flow.node.trigger_source"));
        }
        if source.chars().count() > SOURCE_MAX {
            return Err(steve_i18n::t!("flow.node.trigger_source_long"));
        }
        for k in LISTS {
            let xs = str_list(cfg, k)?;
            if xs.len() > LIST_MAX {
                return Err(steve_i18n::t!("flow.node.trigger_list_max", key = k));
            }
        }
        if source == "time" {
            let cron = opt_str(cfg, "cron")?.unwrap_or("");
            CronExpr::parse(cron).map_err(|_| steve_i18n::t!("flow.node.trigger_cron"))?;
            if let Some(tz) = opt_str(cfg, "tz")? {
                parse_tz(tz).map_err(|_| steve_i18n::t!("flow.node.trigger_tz", tz = tz))?;
            }
        }
        if let Some(m) = opt_str(cfg, "missed")?
            && !MISSED.contains(&m)
        {
            return Err(steve_i18n::t!("flow.node.trigger_missed"));
        }
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, _cfg: &Value, _input: &Value) -> NodeOutcome {
        let ctx = &cx.run.context;
        let out = match (ctx.get("input"), ctx.get("event")) {
            (Some(i), _) if !i.is_null() => i.clone(),
            (_, Some(e)) if !e.is_null() => e.clone(),
            _ => json!({ "at": cx.run.at() }),
        };
        NodeOutcome::next(out)
    }
}
