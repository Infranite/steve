//! Durable one-shot ownership. The same report reducer as ordinary turns feeds a replaceable
//! call projection; Node apply nests these writes in its inbox transaction. No waiter is an owner.
use super::stats::{Stats, remove_usage_in, usage_in};
use crate::{ModelUsage, NewUsage, Result, Tokens, now, stats_changed};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_node_api::{UsageMeter, UsageReport, UsageScope, accounting_usage, accumulate_usage};

#[derive(Debug, Clone, Default)]
pub struct CallBegin {
    pub id: String,
    pub workspace: String,
    pub item: Option<String>,
    pub bot: Option<String>,
    pub project: Option<String>,
    pub purpose: String,
    pub started: i64,
    pub day: String,
}

#[derive(Debug, Clone, Default)]
pub struct CallFinish {
    pub ok: bool,
    pub model: String,
    pub harness: String,
    pub attempts: u32,
    pub duration_ms: u64,
    pub failures: Vec<String>,
    pub usage: Vec<ModelUsage>,
}

impl Stats<'_> {
    pub fn gateway_begin(&self, b: &CallBegin) -> Result<()> {
        self.0.write(|tx| {
            if let Some(item)=&b.item {
                let foreign:bool=tx.conn.query_row("SELECT EXISTS(SELECT 1 FROM items WHERE id=? AND workspace_id<>? AND ?<>'')",params![item,b.workspace,b.workspace],|r|r.get(0))?;
                if foreign {return Err(crate::StoreError::Hook("billing item belongs to another workspace".into()));}
            }
            let (bot, project): (Option<String>, Option<String>) = match &b.item {
                Some(item) => tx.conn.query_row("SELECT bot_id,project_id FROM items WHERE id=? AND workspace_id=?",params![item,b.workspace],|r| Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or_default(),
                None => (None,None),
            };
            tx.conn.execute("INSERT INTO gateway_calls(call_id,workspace_id,item_id,bot_id,project_id,purpose,started,day) VALUES(?,?,?,?,?,?,?,?)",
                params![b.id,b.workspace,b.item,bot.or(b.bot.clone()),project.or(b.project.clone()),b.purpose,b.started,b.day])?;
            Ok(())
        })
    }

    /// Minted before invoking a model, including same-model retries. The parent and ordinal survive
    /// cancellation; a job's dispatch transaction links its separate execution identity to this key.
    pub fn gateway_attempt(
        &self,
        call: &str,
        attempt: &str,
        harness: &str,
        model: &str,
    ) -> Result<()> {
        self.0.write(|tx| {
            let previous: Option<String> = tx.conn.query_row("SELECT attempt_id FROM gateway_attempts WHERE call_id=? ORDER BY ordinal DESC LIMIT 1",[call],|r|r.get(0)).optional()?;
            tx.conn.execute("INSERT INTO gateway_attempts(attempt_id,call_id,ordinal,previous_attempt,harness,model,started) SELECT ?,?,COUNT(*),?,?,?,? FROM gateway_attempts WHERE call_id=?",
                params![attempt,call,previous,harness,model,now(),call])?;
            tx.conn.execute("UPDATE gateway_calls SET attempts=attempts+1,model=?,harness=? WHERE call_id=?", params![model,harness,call])?;
            project(self,call)
        })
    }

    pub fn gateway_link_job(
        &self,
        job_attempt: &str,
        attempt: &str,
        standalone: bool,
    ) -> Result<()> {
        self.0.write(|tx| {
            let mismatch:bool=tx.conn.query_row("SELECT EXISTS(SELECT 1 FROM gateway_attempts a JOIN gateway_calls c USING(call_id) JOIN runtime_job_attempts r ON r.attempt_id=? JOIN runtime_jobs j USING(job_id) WHERE a.attempt_id=? AND c.workspace_id<>'' AND c.workspace_id<>j.workspace_id)",params![job_attempt,attempt],|r|r.get(0))?;
            if mismatch {return Err(crate::StoreError::Hook("billing and Job workspaces differ".into()));}
            let linked=tx.conn.execute(
                "INSERT INTO gateway_job_accounts(job_attempt_id,attempt_id,finish_on_end,node_id,job_id) SELECT ?,?,?,node_id,job_id FROM runtime_job_attempts WHERE attempt_id=?",
                params![job_attempt, attempt, standalone,job_attempt],
            )?;
            if linked!=1 {return Err(crate::StoreError::Hook("Job ownership is missing before dispatch".into()));}
            Ok(())
        })
    }

    pub fn gateway_job_owner(&self, job_attempt: &str) -> Result<Option<(String, String)>> {
        self.0.sqlite_read(|c|Ok(c.query_row("SELECT node_id,job_id FROM gateway_job_accounts WHERE job_attempt_id=? AND node_id IS NOT NULL AND job_id IS NOT NULL",[job_attempt],|r|Ok((r.get(0)?,r.get(1)?))).optional()?))
    }

    pub fn gateway_job_report(
        &self,
        job_attempt: &str,
        report: &UsageReport,
        source_at: i64,
    ) -> Result<bool> {
        self.0.write(|tx| {
            let attempt: Option<String> = tx
                .conn
                .query_row(
                    "SELECT attempt_id FROM gateway_job_accounts WHERE job_attempt_id=?",
                    [job_attempt],
                    |r| r.get(0),
                )
                .optional()?;
            match attempt {
                Some(a) => self.gateway_report(&a, report, source_at),
                None => Ok(false),
            }
        })
    }

    pub fn gateway_report(
        &self,
        attempt: &str,
        report: &UsageReport,
        source_at: i64,
    ) -> Result<bool> {
        if report.scope == UsageScope::Background {
            return Ok(false);
        }
        self.0.write(|tx| {
            let call: Option<String> = tx.conn.query_row("SELECT call_id FROM gateway_attempts WHERE attempt_id=?",[attempt],|r|r.get(0)).optional()?;
            let Some(call) = call else { return Ok(false); };
            if tx.conn.execute("INSERT OR IGNORE INTO gateway_usage_reports(attempt_id,report_id,source_at,payload) VALUES(?,?,?,?)",params![attempt,report.report_id,source_at,serde_json::to_string(report)?])?==0 { return Ok(false); }
            let payloads=tx.conn.prepare_cached("SELECT payload FROM gateway_usage_meters WHERE attempt_id=? ORDER BY rowid")?.query_map([attempt],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut heads=payloads.iter().map(|p|serde_json::from_str::<UsageMeter>(p)).collect::<serde_json::Result<Vec<_>>>()?;
            accumulate_usage(&mut heads,report.clone());
            tx.conn.execute("DELETE FROM gateway_usage_meters WHERE attempt_id=?",[attempt])?;
            for h in &heads { let r=&h.report;
                tx.conn.execute("INSERT INTO gateway_usage_meters(attempt_id,meter_epoch,model,scope,payload) VALUES(?,?,?,?,?)",params![attempt,r.meter_epoch.as_str(),r.model.as_deref().unwrap_or(""),serde_json::to_string(&r.scope)?,serde_json::to_string(h)?])?;
            }
            project(self,&call)?;
            Ok(true)
        })
    }

    /// Capture is acknowledged independently of projection. Applying the identity, meter,
    /// bill/cube and inbox applied flag remains one transaction; failed applications stay queued.
    pub fn gateway_local_receive(
        &self,
        attempt: &str,
        report: &UsageReport,
        observed_at: i64,
    ) -> Result<bool> {
        self.0.write(|tx| {
            let received=tx.conn.execute("INSERT OR IGNORE INTO gateway_local_inbox(attempt_id,report_id,observed_at,payload) VALUES(?,?,?,?)",params![attempt,report.report_id,observed_at,serde_json::to_string(report)?])?!=0;
            tx.conn.execute("INSERT OR IGNORE INTO gateway_local_ready(attempt_id) SELECT ? WHERE EXISTS(SELECT 1 FROM gateway_local_inbox WHERE attempt_id=? AND applied=0)",params![attempt,attempt])?;
            Ok(received)
        })
    }
    /// Bounded round-robin by attempt. A failed cumulative report must not be overtaken by that
    /// attempt's later delta, and a broken/backlogged attempt must not starve unrelated calls.
    pub fn gateway_apply_local(&self, limit: usize) -> Result<usize> {
        let ready = self.0.sqlite_read::<_, crate::StoreError>(|c| {
            Ok(c.prepare_cached(
                "SELECT attempt_id FROM gateway_local_ready ORDER BY tried,rowid LIMIT ?",
            )?
            .query_map([limit as i64], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        let mut applied = 0;
        let mut budget = limit;
        let mut failure = None;
        for attempt in ready {
            if budget == 0 {
                break;
            }
            let pending=self.0.sqlite_read::<_,crate::StoreError>(|c|Ok(c.prepare_cached("SELECT report_id,observed_at,payload FROM gateway_local_inbox WHERE attempt_id=? AND applied=0 ORDER BY rowid LIMIT ?")?.query_map(params![attempt,budget as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?))?;
            for (id, at, payload) in pending {
                budget -= 1;
                let result:Result<()>=self.0.write(|tx| {
                    self.gateway_report(&attempt,&serde_json::from_str::<UsageReport>(&payload)?,at)?;
                    tx.conn.execute("UPDATE gateway_local_inbox SET applied=1 WHERE attempt_id=? AND report_id=?",params![attempt,id])?;
                    Ok(())
                });
                match result {
                    Ok(()) => applied += 1,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            self.0.write::<_,crate::StoreError>(|tx| {
                tx.conn.execute("UPDATE gateway_local_ready SET tried=? WHERE attempt_id=?",params![now(),attempt])?;
                tx.conn.execute("DELETE FROM gateway_local_ready WHERE attempt_id=? AND NOT EXISTS(SELECT 1 FROM gateway_local_inbox WHERE attempt_id=? AND applied=0)",params![attempt,attempt])?;
                Ok(())
            })?;
        }
        // Claim and rotate a bounded FIFO prefix in one committed transaction, BEFORE trying
        // projections. A failed projection must not roll its scheduling progress back. New work
        // joins the tail; AUTOINCREMENT tickets do not depend on wall-clock resolution or uptime.
        // Concurrent workers serialize this step, then may harmlessly overlap idempotent projects
        // if there are fewer queued calls than their combined budgets. No crash-sensitive lease.
        let dirty = self.0.write::<_, crate::StoreError>(|tx| {
            let calls = tx
                .conn
                .prepare_cached("SELECT call_id FROM gateway_result_ready ORDER BY ticket LIMIT ?")?
                .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for call in &calls {
                tx.conn
                    .execute("DELETE FROM gateway_result_ready WHERE call_id=?", [call])?;
                tx.conn.execute(
                    "INSERT INTO gateway_result_ready(call_id) VALUES(?)",
                    [call],
                )?;
            }
            Ok(calls)
        })?;
        for call in dirty {
            match project(self, &call) {
                Ok(()) => {}
                Err(e) => failure = Some(e),
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(applied),
        }
    }

    /// The first caller result stays final. Usage can still change afterwards. Never allow a
    /// delayed successful Node completion to erase a caller's timeout/cancelled result.
    pub fn gateway_finish(&self, call: &str, f: &CallFinish) -> Result<()> {
        self.0.write::<_,crate::StoreError>(|tx| {
            let failures=if f.failures.is_empty(){attempt_failures(tx,call)?}else{f.failures.clone()};
            tx.conn.execute("UPDATE gateway_calls SET ended=?,ok=?,model=?,harness=?,attempts=MAX(attempts,?),duration_ms=?,failures=?,fallback_usage=?,projection_dirty=1 WHERE call_id=? AND ended IS NULL",
                params![now(),f.ok,f.model,f.harness,f.attempts,i64::try_from(f.duration_ms).unwrap_or(i64::MAX),serde_json::to_string(&failures)?,serde_json::to_string(&f.usage)?,call])?;
            Ok(())
        })?;
        project(self, call)
    }

    pub fn gateway_attempt_end(
        &self,
        attempt: &str,
        ok: bool,
        error: Option<&str>,
        source_at: i64,
    ) -> Result<()> {
        self.0.write(|tx| {
            tx.conn.execute("UPDATE gateway_attempts SET ended=?,ok=?,error=? WHERE attempt_id=? AND ended IS NULL",params![source_at,ok,error,attempt])?;
            let call: Option<String>=tx.conn.query_row("SELECT call_id FROM gateway_attempts WHERE attempt_id=?",[attempt],|r|r.get(0)).optional()?;
            if let Some(call)=call {
                let failures=attempt_failures(tx,&call)?;
                tx.conn.execute("UPDATE gateway_calls SET failures=? WHERE call_id=? AND ended IS NULL",params![serde_json::to_string(&failures)?,call])?;
                project(self,&call)?;
            }
            Ok(())
        })
    }

    pub fn gateway_cancel(&self, call: &str, reason: &str) -> Result<()> {
        self.0.write::<_, crate::StoreError>(|tx| {
            cancel_in(tx, call, reason)?;
            Ok(())
        })?;
        project(self, call)
    }
    pub fn gateway_job_cancel(&self, job_attempt: &str, reason: &str) -> Result<()> {
        self.0.write(|tx| {
            let call:Option<String>=tx.conn.query_row("SELECT a.call_id FROM gateway_job_accounts j JOIN gateway_attempts a USING(attempt_id) WHERE j.job_attempt_id=?",[job_attempt],|r|r.get(0)).optional()?;
            if let Some(call)=call { self.gateway_cancel(&call,reason)?; }
            Ok(())
        })
    }
    /// Call recovery runs once before a live Hub admits work; snapshots never call it.
    /// Projection outages must not prevent startup (and hence prevent the periodic retry worker).
    /// Cancelling orphan callers is committed independently of their replaceable projections.
    pub fn gateway_recover(&self) -> Result<usize> {
        loop {
            match self.gateway_apply_local(128) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("recover gateway projection; periodic worker will retry: {e}");
                    break;
                }
            }
        }
        let cancelled = self.0.write::<_, crate::StoreError>(|tx| {
            let calls = tx
                .conn
                .prepare_cached("SELECT call_id FROM gateway_calls WHERE ended IS NULL")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for call in &calls {
                cancel_in(tx, call, "Hub restart: caller unavailable")?;
            }
            Ok(calls.len())
        })?;
        if let Err(e) = self.gateway_apply_local(128) {
            tracing::warn!("recover gateway result; periodic worker will retry: {e}");
        }
        Ok(cancelled)
    }
    pub fn gateway_job_detach(&self, job_attempt: &str) -> Result<()> {
        self.0.write(|tx| {
            let own:bool=tx.conn.query_row("SELECT EXISTS(SELECT 1 FROM gateway_job_accounts WHERE job_attempt_id=? AND finish_on_end=1)",[job_attempt],|r|r.get(0))?;
            if own {self.gateway_job_cancel(job_attempt,"caller dropped")?;}
            Ok(())
        })
    }
    pub fn gateway_job_end(
        &self,
        job_attempt: &str,
        ok: bool,
        error: Option<&str>,
        source_at: i64,
    ) -> Result<()> {
        self.0.write(|tx| {
            let a:Option<(String,bool)>=tx.conn.query_row("SELECT attempt_id,finish_on_end FROM gateway_job_accounts WHERE job_attempt_id=?",[job_attempt],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((a,own))=a {
                self.gateway_attempt_end(&a,ok,error,source_at)?;
                if own {
                    let call:String=tx.conn.query_row("SELECT call_id FROM gateway_attempts WHERE attempt_id=?",[&a],|r|r.get(0))?;
                    let failures=error.map(|e|vec![e]).unwrap_or_default();
                    tx.conn.execute("UPDATE gateway_calls SET ended=?,ok=?,duration_ms=MAX(0,?-started),failures=? WHERE call_id=? AND ended IS NULL",params![source_at,ok,source_at,serde_json::to_string(&failures)?,call])?;
                    project(self,&call)?;
                }
            }
            Ok(())
        })
    }
}

// Keep recovery and ordinary cancellation on the same durable-result/queue transition.
fn cancel_in(tx: &mut crate::W<'_>, call: &str, reason: &str) -> Result<()> {
    let mut failures = attempt_failures(tx, call)?;
    failures.push(reason.to_string());
    tx.conn.execute("UPDATE gateway_calls SET ended=?,ok=0,duration_ms=MAX(0,?-started),failures=?,projection_dirty=1 WHERE call_id=? AND ended IS NULL",params![now(),now(),serde_json::to_string(&failures)?,call])?;
    Ok(())
}

fn attempt_failures(tx: &mut crate::W<'_>, call: &str) -> Result<Vec<String>> {
    Ok(tx.conn.prepare_cached("SELECT model,error FROM gateway_attempts WHERE call_id=? AND ok=0 AND error IS NOT NULL ORDER BY ordinal")?.query_map([call],|r|Ok(format!("{}: {}",r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn project(stats: &Stats<'_>, call_id: &str) -> Result<()> {
    stats.0.write(|tx| {
        let (ws,item,bot_id,project_id,purpose,ts,day,ended,mut a): (String,Option<String>,Option<String>,Option<String>,String,i64,String,Option<i64>,CallFinish)=tx.conn.query_row(
            "SELECT workspace_id,item_id,bot_id,project_id,purpose,started,day,ended,ok,model,harness,attempts,duration_ms,failures,fallback_usage FROM gateway_calls WHERE call_id=?",[call_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,CallFinish{ok:r.get(8)?,model:r.get(9)?,harness:r.get(10)?,attempts:r.get(11)?,duration_ms:r.get::<_,i64>(12)?.max(0) as u64,failures:vec![],usage:vec![]})))?;
        let (failures,fallback):(String,String)=tx.conn.query_row("SELECT failures,fallback_usage FROM gateway_calls WHERE call_id=?",[call_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        a.failures=serde_json::from_str(&failures)?;
        let attempts=tx.conn.prepare_cached("SELECT attempt_id,model FROM gateway_attempts WHERE call_id=? ORDER BY ordinal")?.query_map([call_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut has_reports=false;
        for (id,model) in attempts {
            let payloads=tx.conn.prepare_cached("SELECT payload FROM gateway_usage_meters WHERE attempt_id=? ORDER BY rowid")?.query_map([&id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            has_reports |= !payloads.is_empty();
            let heads=payloads.iter().map(|p|serde_json::from_str::<UsageMeter>(p)).collect::<serde_json::Result<Vec<_>>>()?;
            let reports=accounting_usage(&heads,&model);
            let mut rows=vec![];
            merge_usage(&mut rows,reports.iter().cloned().map(model_usage).collect());
            for u in &mut rows {
                if u.tokens==Tokens::default() && reports.iter().filter(|r|r.model.as_deref().unwrap_or_default()==u.model).all(|r|!steve_node_api::usage::accounting_tokens(r,&reports)) {
                    u.detail["accounting_tokens"]=json!(false);
                }
            }
            merge_usage(&mut a.usage,rows);
        }
        if !has_reports { a.usage=serde_json::from_str(&fallback)?; }
        // Pending calls have a concrete unknown row, so coverage is pending, never fabricated zero.
        let tag=CallProjection { purpose:&purpose, item_id:&item };
        write_projection(tx,call_id,&ws,ts,&day,bot_id,project_id,&tag,&a,ended.is_none())?;
        tx.conn.execute("UPDATE gateway_calls SET projection_dirty=0 WHERE call_id=?",[call_id])?;
        tx.emit(stats_changed(&ws,Some(&day)));
        Ok(())
    })
}
struct CallProjection<'a> {
    purpose: &'a str,
    item_id: &'a Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn write_projection(
    tx: &mut crate::W<'_>,
    call_id: &str,
    ws: &str,
    ts: i64,
    day: &str,
    bot_id: Option<String>,
    project_id: Option<String>,
    tag: &CallProjection<'_>,
    a: &CallFinish,
    pending: bool,
) -> Result<()> {
    let ok = a.ok || pending;
    let mut usages = a.usage.clone();
    fold_companion_costs(&mut usages);
    let ProjectionRows { rows, unspent } = projection_rows(a, usages);
    remove_usage_in(tx.conn, "live_call_id", call_id)?;
    {
        for (i, mut usage) in rows.into_iter().enumerate() {
            let first = i == 0;
            if first && (!a.failures.is_empty() || !unspent.is_empty()) {
                if !usage.detail.is_object() {
                    usage.detail = json!({"reported_detail": usage.detail});
                }
                if let Value::Object(d) = &mut usage.detail {
                    if !a.failures.is_empty() {
                        d.insert("failures".into(), json!(a.failures));
                    }
                    if !unspent.is_empty() {
                        d.insert("unspent_usage".into(), json!(unspent));
                        if a.attempts <= 1 {
                            d.insert("unspent_accounting".into(), json!(true));
                        }
                    }
                }
            }
            if a.attempts <= 1
                && usage.cost_reported.is_none()
                && unspent
                    .iter()
                    .any(|u| u["cost_reported"].as_f64() == Some(0.0))
            {
                usage.cost_reported = Some(0.0);
            }
            if !usage.detail.is_object() {
                usage.detail = json!({"reported_detail": usage.detail});
            }
            usage.detail["call_id"] = json!(call_id);
            usage.detail["pending"] = json!(pending);
            if [
                usage.tokens.input,
                usage.tokens.output,
                usage.tokens.cache_read,
                usage.tokens.cache_write,
            ]
            .iter()
            .any(Option::is_some)
                || usage.detail.get("accounting_tokens").is_none()
            {
                usage.detail["accounting_tokens"] = json!(true);
            }
            let id = usage_in(
                tx.conn,
                &NewUsage {
                    workspace_id: ws.to_string(),
                    ts,
                    day: day.to_string(),
                    source: "gateway".into(),
                    purpose: tag.purpose.into(),
                    harness: a.harness.clone(),
                    turn_id: None,
                    session_id: None,
                    item_id: tag.item_id.clone(),
                    bot_id: bot_id.clone(),
                    project_id: project_id.clone(),
                    usage,
                    /* the call's time and dispatch attempts once, on its first row */
                    duration_ms: first.then(|| i64::try_from(a.duration_ms).unwrap_or(i64::MAX)),
                    ok,
                    attempts: if first {
                        i64::from(if pending {
                            a.attempts.max(1)
                        } else {
                            a.attempts
                        })
                    } else {
                        1
                    },
                },
            )?;
            tx.conn.execute(
                "UPDATE usage_records SET live_call_id=? WHERE id=?",
                params![call_id, id],
            )?;
        }
        Ok(())
    }
}

/// Attach model-less context fees to the first token-bearing usage, retaining their evidence.
fn fold_companion_costs(usages: &mut Vec<ModelUsage>) {
    let has_tokens = |u: &ModelUsage| {
        [
            u.tokens.input,
            u.tokens.output,
            u.tokens.cache_read,
            u.tokens.cache_write,
        ]
        .iter()
        .any(Option::is_some)
    };
    if usages.iter().any(has_tokens) {
        // Model-less/context fee companions carry evidence, not another model request row.
        // The reported quote stays in the total, with its original attribution in detail.
        let mut companions = vec![];
        usages.retain(|u| {
            if !has_tokens(u) && u.detail["accounting_tokens"] == false {
                companions.push(u.clone());
                false
            } else {
                true
            }
        });
        if !companions.is_empty() {
            let first = usages
                .iter_mut()
                .find(|u| has_tokens(u))
                .expect("a token row");
            for companion in &companions {
                first.cost_reported = match (first.cost_reported, companion.cost_reported) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                };
            }
            if !first.detail.is_object() {
                first.detail = json!({"reported_detail":first.detail});
            }
            first.detail["cost_companions"] = json!(companions.iter().map(|u|json!({"model":u.model,"cost_reported":u.cost_reported,"detail":u.detail})).collect::<Vec<_>>());
        }
    } else {
        // A cost-only call with no token peer is genuinely unavailable, even when its source
        // was a context counter. Unlike a durable turn it has no separate missing-attempt row.
        for u in usages {
            if u.detail["accounting_tokens"] == false {
                u.detail["accounting_tokens"] = json!(true);
            }
        }
    }
}

struct ProjectionRows {
    rows: Vec<ModelUsage>,
    unspent: Vec<Value>,
}

/// Select request rows while preserving answering-model zero precedence and other unspent evidence.
fn projection_rows(a: &CallFinish, usages: Vec<ModelUsage>) -> ProjectionRows {
    // A retry that reports no consumption is not an extra gateway request row. Keep its
    // actual zeros (and missing fields) as evidence instead of replacing them with unknowns.
    let reported = |u: &ModelUsage| {
        let t = &u.tokens;
        [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
            .iter()
            .any(Option::is_some)
            || u.cost_reported.is_some()
    };
    let (mut rows, mut unspent): (Vec<_>, Vec<_>) = usages.into_iter().partition(|u| {
        let t = &u.tokens;
        [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
            .iter()
            .any(|x| x.is_some_and(|x| x != 0))
            || u.cost_reported.is_some_and(|c| c != 0.0)
    });
    if rows.is_empty() {
        let zero = unspent
            .iter()
            .rposition(|u| u.model == a.model && reported(u))
            // With one try, the adapter may report a canonical model name instead of the
            // requested alias. With retries, another model's zero says nothing about the
            // answering model's missing usage.
            .or_else(|| {
                (a.attempts <= 1)
                    .then(|| unspent.iter().position(reported))
                    .flatten()
            });
        rows.push(match zero {
            Some(i) => unspent.remove(i),
            // No usage reported (a timeout, a crash): one unknown row still counts the call.
            None => ModelUsage {
                model: a.model.clone(),
                detail: json!({"scope": "none"}),
                ..ModelUsage::default()
            },
        });
    }
    let unspent: Vec<_> = unspent
        .into_iter()
        .filter(reported)
        .map(|u| {
            json!({
                "model": u.model,
                "tokens": {
                    "input": u.tokens.input, "output": u.tokens.output,
                    "cache_read": u.tokens.cache_read, "cache_write": u.tokens.cache_write,
                    "reasoning": u.tokens.reasoning,
                },
                "cost_reported": u.cost_reported,
                "detail": u.detail,
            })
        })
        .collect();
    ProjectionRows { rows, unspent }
}

pub fn model_usage(r: UsageReport) -> ModelUsage {
    use steve_node_api::UsageScope;
    let n = |x: Option<u64>| x.and_then(steve_node_api::usage::token_count);
    let scope = match r.scope {
        UsageScope::All => "all",
        UsageScope::Main => "main",
        UsageScope::Background => "background",
        UsageScope::Subtask => "subtask",
    };
    ModelUsage {
        model: r.model.unwrap_or_default(),
        tokens: Tokens {
            input: n(r.tokens.input),
            output: n(r.tokens.output),
            cache_read: n(r.tokens.cache_read),
            cache_write: n(r.tokens.cache_write),
            reasoning: n(r.tokens.reasoning),
        },
        cost_reported: r
            .cost
            .as_ref()
            .filter(|m| m.currency == "USD")
            .and_then(steve_node_api::usage::amount),
        detail: json!({"scope": scope}),
    }
}

pub fn merge_usage(into: &mut Vec<ModelUsage>, more: Vec<ModelUsage>) {
    fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        }
    }
    let accounting_scope = |d: &Value| {
        d.as_object().is_some_and(|o| o.len() == 1)
            && matches!(d["scope"].as_str(), Some("main" | "all"))
    };
    for m in more {
        match into.iter_mut().find(|r| {
            r.model == m.model
                && (r.detail == m.detail
                    || (accounting_scope(&r.detail) && accounting_scope(&m.detail)))
        }) {
            Some(r) => {
                if r.detail != m.detail {
                    r.detail = json!({"scope": "all"});
                }
                let (a, b) = (&mut r.tokens, m.tokens);
                a.input = add(a.input, b.input);
                a.output = add(a.output, b.output);
                a.cache_read = add(a.cache_read, b.cache_read);
                a.cache_write = add(a.cache_write, b.cache_write);
                a.reasoning = add(a.reasoning, b.reasoning);
                r.cost_reported = add(r.cost_reported, m.cost_reported);
            }
            None => into.push(m),
        }
    }
}
