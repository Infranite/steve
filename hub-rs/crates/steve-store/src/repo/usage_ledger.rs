//! Live accounting is part of the event-application transaction, not a post-commit callback.
//! Report identities survive inbox pruning and restarts; meter heads survive cancellation. Only
//! the projection is replaced. The daily cube and item dirty flag move with it in one transaction.
use rusqlite::{OptionalExtension, params};
use steve_node_api::{UsageMeter, UsageReport, UsageScope, accounting_usage, accumulate_usage};

use super::stats::{Stats, remove_turn_usage_in, usage_in};
use crate::{ModelUsage, NewUsage, Result, Tokens, TurnBegin, stats_changed};

impl Stats<'_> {
    /// Beginning and linking an attempt are atomic and repeatable, including after Hub restart.
    pub fn node_turn_begin(&self, b: &TurnBegin, attempt: &str) -> Result<String> {
        self.0.transaction(|tx| {
            if let Some(id) = tx
                .query_row(
                    "SELECT id FROM agent_turns WHERE node_attempt_id=?",
                    [attempt],
                    |r| r.get(0),
                )
                .optional()?
            {
                return Ok(id);
            }
            let id = self.turn_begin(b)?;
            tx.execute(
                "UPDATE agent_turns SET node_attempt_id=? WHERE id=?",
                params![attempt, id],
            )?;
            tx.publish(stats_changed(&b.workspace_id, None));
            Ok(id)
        })
    }

    /// The day is the attempt's start day, supplied in the workspace's statistics timezone.
    /// Returns false on replay or when the turn does not exist. A late final may repair a settled
    /// turn; a late delta cannot overwrite a final. No network or collector runs here.
    pub fn usage_report(
        &self,
        turn: &str,
        report: &UsageReport,
        fallback_model: &str,
        day: &str,
    ) -> Result<bool> {
        if report.scope == UsageScope::Background {
            return Ok(false);
        }
        self.0.transaction(|tx| {
            let owner = tx.query_row(
                "SELECT workspace_id, session_id, item_id, bot_id, project_id, harness, started FROM agent_turns WHERE id=?",
                [turn], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<String>>(2)?, r.get::<_,Option<String>>(3)?, r.get::<_,Option<String>>(4)?, r.get::<_,String>(5)?, r.get::<_,i64>(6)?))
            ).optional()?;
            let Some((ws,session,item,bot,project,harness,started)) = owner else { return Ok(false); };
            if tx.execute("INSERT OR IGNORE INTO usage_reports(turn_id,report_id,payload) VALUES(?,?,?)", params![turn,report.report_id,serde_json::to_string(report)?])? == 0 {
                return Ok(false);
            }
            let payloads = tx.prepare_cached("SELECT payload FROM usage_meters WHERE turn_id=? ORDER BY rowid")?
                .query_map([turn], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut heads = payloads.iter().map(|v| serde_json::from_str::<UsageMeter>(v)).collect::<serde_json::Result<Vec<_>>>();
            if let Ok(heads) = &mut heads {
                accumulate_usage(heads, report.clone());
            } else {
                // Pre-shared-reducer heads lacked independent token/cost state. Rebuild them
                // once from durable source reports, preserving original anonymous model identity.
                let reports = tx.prepare_cached("SELECT payload FROM usage_reports WHERE turn_id=? ORDER BY rowid")?
                    .query_map([turn], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                let mut replay = vec![];
                for p in reports { accumulate_usage(&mut replay, serde_json::from_str(&p)?); }
                heads = Ok(replay);
            }
            let heads = heads?;
            tx.execute("DELETE FROM usage_meters WHERE turn_id=?", [turn])?;
            for h in &heads {
                let r = &h.report;
                tx.execute("INSERT INTO usage_meters(turn_id,meter_epoch,model,scope,payload) VALUES(?,?,?,?,?)",
                    params![turn,r.meter_epoch.as_str(),r.model.as_deref().unwrap_or(""),serde_json::to_string(&r.scope)?,serde_json::to_string(h)?])?;
            }
            remove_turn_usage_in(tx, turn)?;
            let snapshot = accounting_usage(&heads, fallback_model);
            for r in &snapshot {
                let n = |v:Option<u64>| v.and_then(steve_node_api::usage::token_count);
                let id = usage_in(tx, &NewUsage {
                    workspace_id:ws.clone(), ts:started, day:day.into(), source:"agent".into(), purpose:"turn".into(), harness:harness.clone(),
                    turn_id:Some(turn.into()), session_id:Some(session.clone()), item_id:item.clone(), bot_id:bot.clone(), project_id:project.clone(),
                    usage:ModelUsage {
                        model:r.model.clone().unwrap_or_default(),
                        tokens:Tokens {input:n(r.tokens.input), output:n(r.tokens.output), cache_read:n(r.tokens.cache_read), cache_write:n(r.tokens.cache_write), reasoning:n(r.tokens.reasoning)},
                        cost_reported:r.cost.as_ref().filter(|m|m.currency=="USD").and_then(steve_node_api::usage::amount),
                        detail:serde_json::json!({"scope":r.scope,"meter_epoch":r.meter_epoch,"measurement":r.measurement,"accounting_tokens":steve_node_api::usage::accounting_tokens(r,&snapshot),"call_id":turn}),
                    }, duration_ms:None, ok:true, attempts:1,
                })?;
                tx.execute("UPDATE usage_records SET live_turn_id=? WHERE id=?", params![turn,id])?;
            }
            tx.publish(stats_changed(&ws, Some(day)));
            Ok(true)
        })
    }
}
