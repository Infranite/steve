//! Durable state of a source-driven schedule start. Run retention must preserve its latest anchor.
use super::*;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupStatus {
    Started,
    Retrying,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    format: u8,
    revision: Value,
    state: StartupStatus,
    attempts: i64,
    limit: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartupState {
    pub run_id: String,
    pub state: StartupStatus,
    pub attempts: i64,
    pub limit: i64,
}

/// Public historical budget, without the source's internal configuration revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartupSummary {
    pub source: String,
    pub state: StartupStatus,
    pub attempts: i64,
    pub limit: i64,
}

pub(super) fn summary(context: &Value) -> Option<StartupSummary> {
    let meta: Metadata = serde_json::from_value(context.get("startup")?.clone()).ok()?;
    if meta.format != 1 || meta.limit < 1 || meta.attempts < 0 || meta.attempts > meta.limit {
        return None;
    }
    Some(StartupSummary {
        source: context.get("source")?.as_str()?.into(),
        state: meta.state,
        attempts: meta.attempts,
        limit: meta.limit,
    })
}

/// A rejected startup; revision is the caller's immutable configuration snapshot.
pub struct FailedStart<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub source: &'a str,
    pub error: &'a str,
    pub now: i64,
    pub revision: &'a Value,
    pub limit: i64,
}

fn latest(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
) -> FlowResult<Option<(RunRow, Metadata)>> {
    let row = c
        .query_row(
            "SELECT * FROM flow_runs WHERE workspace_id=?1 AND flow_id=?2
         AND CASE WHEN json_valid(context) THEN json_extract(context,'$.source')=?3
           AND json_extract(context,'$.startup.format')=1 ELSE 0 END
         ORDER BY rowid DESC LIMIT 1",
            params![ws, flow, source],
            RunRow::from_row,
        )
        .optional()?;
    row.map(|row| {
        let meta: Metadata = serde_json::from_value(row.context["startup"].clone())
            .map_err(|e| crate::FlowError::Internal(format!("Invalid startup state: {e}")))?;
        if meta.limit < 1 || meta.attempts < 0 || meta.attempts > meta.limit {
            return Err(crate::FlowError::Internal("Invalid startup budget".into()));
        }
        Ok((row, meta))
    })
    .transpose()
}

/// Only source-owned records advance the budget; unrelated manual/cron/skipped history does not.
pub fn startup_state(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
    revision: &Value,
) -> FlowResult<Option<StartupState>> {
    Ok(latest(c, ws, flow, source)?
        .filter(|(_, m)| &m.revision == revision)
        .map(|(r, m)| StartupState {
            run_id: r.id,
            state: m.state,
            attempts: m.attempts,
            limit: m.limit,
        }))
}

/// A successful start resets this source's budget in the same transaction as the new run.
pub fn record_started(
    tx: &Tx,
    run: &str,
    source: &str,
    revision: &Value,
    limit: i64,
) -> FlowResult<()> {
    if limit < 1 {
        return Err(crate::FlowError::invalid("startup limit"));
    }
    let row = get(tx, run)?.ok_or_else(|| crate::FlowError::not_found("运行"))?;
    let mut context = row.context;
    context["source"] = json!(source);
    context["startup"] = serde_json::to_value(Metadata {
        format: 1,
        revision: revision.clone(),
        state: StartupStatus::Started,
        attempts: 0,
        limit,
    })
    .expect("startup metadata");
    tx.execute(
        "UPDATE flow_runs SET context=?1 WHERE id=?2",
        params![context.to_string(), run],
    )?;
    // The run's insert already staged the ordinary Run/Flow notifications in this transaction.
    Ok(())
}

/// Coalesce only the latest matching error, but carry the budget across different error rows.
pub fn record_start_failure(tx: &Tx, f: &FailedStart<'_>) -> FlowResult<String> {
    let state = startup_state(tx, f.ws, f.flow_id, f.source, f.revision)?;
    if let Some(s) = &state
        && s.state == StartupStatus::Dead
    {
        return Ok(s.run_id.clone());
    }
    if f.limit < 1 {
        return Err(crate::FlowError::invalid("startup limit"));
    }
    let attempts = state
        .map_or(0, |s| s.attempts)
        .saturating_add(1)
        .min(f.limit);
    let row = tx.query_row("SELECT * FROM flow_runs WHERE workspace_id=?1 AND flow_id=?2 ORDER BY rowid DESC LIMIT 1",
        params![f.ws,f.flow_id],RunRow::from_row).optional()?;
    let previous = row.filter(|r| {
        r.status == "failed"
            && r.error.as_deref() == Some(f.error)
            && r.flow_version == f.flow_version
            && r.context["source"] == f.source
            && r.context["startup"]["revision"] == *f.revision
            && r.context["startup"]["format"] == 1
            && r.context["startup"]["state"] == "retrying"
            && r.work_item_id.is_none()
            && r.cursor.is_none()
            && !r.dry_run
    });
    let (failures, first_at) = match previous.as_ref() {
        Some(row) => (
            row.context["start_failures"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or_else(|| crate::FlowError::Internal("Invalid startup failure count".into()))?
                .saturating_add(1),
            row.context["first_at"].as_i64().ok_or_else(|| {
                crate::FlowError::Internal("Invalid startup first timestamp".into())
            })?,
        ),
        None => (1, f.now),
    };
    let meta = Metadata {
        format: 1,
        revision: f.revision.clone(),
        state: if attempts >= f.limit {
            StartupStatus::Dead
        } else {
            StartupStatus::Retrying
        },
        attempts,
        limit: f.limit,
    };
    let context = json!({"at":f.now,"first_at":first_at,"trigger":"manual","source":f.source,
        "start_failures":failures,"startup":meta});
    let reused = previous.is_some();
    let run = match previous {
        Some(r) => r.id,
        None => insert(
            tx,
            &NewRun {
                ws: f.ws,
                flow_id: f.flow_id,
                flow_version: f.flow_version,
                trigger_kind: "manual",
                status: "failed",
                outcome_label: f.error,
                error: Some(f.error),
                cursor: None,
                context: &context,
                now: f.now,
            },
        )?,
    };
    tx.execute(
        "UPDATE flow_runs SET error=?1,outcome_label=?1,context=?2,started=?3,ended=?3 WHERE id=?4",
        params![f.error, context.to_string(), f.now, run],
    )?;
    if reused {
        publish(tx, f.ws, f.flow_id, &run);
    }
    Ok(run)
}

// One causal state anchor per live flow/source, including a successful reset. Do not resurrect
// an older dead record by pruning its later reset. Superseded anchors use ordinary retention.
pub(super) const ANCHORS: &str = "SELECT max(r.rowid) FROM flow_runs r JOIN flows f ON f.id=r.flow_id
    WHERE f.deleted=0 AND CASE WHEN json_valid(r.context) THEN json_extract(r.context,'$.startup.format')=1 ELSE 0 END
    GROUP BY r.workspace_id,r.flow_id,CASE WHEN json_valid(r.context) THEN json_extract(r.context,'$.source') END";
