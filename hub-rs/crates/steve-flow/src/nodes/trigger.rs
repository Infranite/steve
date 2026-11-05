//! `trigger`: `{source, types[], connections[], scopes[], spaces[], cron?, tz?, missed?}`. It outputs
//! what started the run: the manual or sub-flow input, else the event, else `{at}`. Saving a flow
//! turns it into `flow_triggers` rows ([`crate::flows::rebuild_triggers`]).

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{NodeKind, NodeOutcome, RunCx, opt_str, str_list};
use crate::cron::{CronExpr, parse_tz};

pub struct Trigger;

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

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let source = opt_str(cfg, "source")?.unwrap_or("").trim();
        if source.is_empty() {
            return Err(steve_i18n::t!("flow.node.trigger_source"));
        }
        if source.chars().count() > 40 {
            return Err(steve_i18n::t!("flow.node.trigger_source_long"));
        }
        for k in ["types", "connections", "scopes", "spaces"] {
            let xs = str_list(cfg, k)?;
            if xs.len() > 50 {
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
            && m != "catch_up_once"
            && m != "skip"
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
