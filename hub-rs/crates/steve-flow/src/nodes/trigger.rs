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
            return Err("要选触发来源".into());
        }
        if source.chars().count() > 40 {
            return Err("触发来源太长了".into());
        }
        for k in ["types", "connections", "scopes", "spaces"] {
            let xs = str_list(cfg, k)?;
            if xs.len() > 50 {
                return Err(format!("{k} 最多 50 个"));
            }
        }
        if source == "time" {
            let cron = opt_str(cfg, "cron")?.unwrap_or("");
            CronExpr::parse(cron).map_err(|_| "定时触发要有正确的 cron".to_string())?;
            if let Some(tz) = opt_str(cfg, "tz")? {
                parse_tz(tz).map_err(|_| format!("不认识的时区：{tz}"))?;
            }
        }
        if let Some(m) = opt_str(cfg, "missed")?
            && m != "catch_up_once"
            && m != "skip"
        {
            return Err("missed 只能是 catch_up_once 或 skip".into());
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
