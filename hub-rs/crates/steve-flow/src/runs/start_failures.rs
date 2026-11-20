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

/// Persisted identity, independent of any worker/request locale. Parameters are empty for
/// this rejection today; keeping an explicit object makes the stored contract unambiguous.
pub const START_REJECTED_CODE: &str = "startup_rejected";
fn error_identity() -> Value {
    json!({"code":START_REJECTED_CODE,"params":{}})
}
fn valid_error(context: &Value) -> bool {
    context
        .get("start_error")
        .is_some_and(|value| *value == error_identity())
}
pub(super) fn error_text(context: &Value) -> Option<String> {
    context.get("start_error").map(|_| {
        if valid_error(context) {
            crate::scheduler::start_rejected()
        } else {
            steve_i18n::t!("flow.startup.metadata_invalid")
        }
    })
}

fn metadata(context: &Value) -> Result<Metadata, String> {
    let meta: Metadata =
        serde_json::from_value(context["startup"].clone()).map_err(|e| e.to_string())?;
    if meta.format != 1
        || meta.limit < 1
        || meta.attempts < 0
        || meta.attempts > meta.limit
        || (meta.state == StartupStatus::Started && meta.attempts != 0)
        || (meta.state == StartupStatus::Retrying && meta.attempts >= meta.limit)
        || (meta.state == StartupStatus::Dead && meta.attempts != meta.limit)
    {
        return Err("Invalid startup budget".into());
    }
    if meta.state != StartupStatus::Started
        && (!valid_error(context)
            || context["start_failures"]
                .as_i64()
                .is_none_or(|n| n < 1 || n > meta.attempts)
            || context["first_at"].as_i64().is_none())
    {
        return Err("Invalid startup failure history".into());
    }
    Ok(meta)
}

pub(super) fn summary(context: &Value) -> Option<StartupSummary> {
    let meta = metadata(context).ok()?;
    Some(StartupSummary {
        source: context.get("source")?.as_str()?.into(),
        state: meta.state,
        attempts: meta.attempts,
        limit: meta.limit,
    })
}

/// A source cannot safely spend or reset an unreadable budget. This read-only diagnostic
/// lets its owner locate that record without exposing database/decoder internals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartupDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub flow_id: String,
    pub run_id: String,
}
fn diagnostic(
    row: &StartupRecord,
    flow: &str,
    source: &str,
    revision: Option<&Value>,
) -> StartupDiagnostic {
    let (code, message) = if row.context["source"].as_str() != Some(source) {
        (
            "startup_metadata_unattributed",
            steve_i18n::t!("flow.startup.metadata_unattributed"),
        )
    } else if revision.is_some_and(|current| {
        row.context["startup"]
            .get("revision")
            .is_some_and(|old| old != current)
    }) {
        (
            "startup_metadata_recovered",
            steve_i18n::t!("flow.startup.metadata_recovered"),
        )
    } else {
        (
            "startup_metadata_invalid",
            steve_i18n::t!("flow.startup.metadata_invalid"),
        )
    };
    StartupDiagnostic {
        code,
        message,
        flow_id: flow.into(),
        run_id: row.id.clone(),
    }
}

/// The latest damaged record remains diagnosable even after a new configuration starts.
pub fn startup_diagnostic(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
) -> FlowResult<Option<StartupDiagnostic>> {
    let revision = match crate::scheduler::startup_revision(c, ws, flow)? {
        Some(revision) => Some(revision),
        None => inspect_records(c, ws, flow, source, false, Some)?
            .filter(|row| !row.unreadable && metadata(&row.context).is_ok())
            .map(|row| row.context["startup"]["revision"].clone()),
    };
    inspect_records(c, ws, flow, source, true, |row| {
        (row.unreadable || metadata(&row.context).is_err())
            .then(|| diagnostic(&row, flow, source, revision.as_ref()))
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum StartupCheck {
    State(Option<StartupState>),
    Invalid(StartupDiagnostic),
}

/// A rejected startup; revision is the caller's immutable configuration snapshot.
pub struct FailedStart<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub source: &'a str,
    /// Internal rejection identity/details for the low-level recorder, never localized for storage.
    pub error: &'a str,
    pub now: i64,
    pub revision: &'a Value,
    pub limit: i64,
}

struct StartupRecord {
    id: String,
    context: Value,
    unreadable: bool,
}

// Only the identifiers and metadata are needed. Decode each column independently so unrelated
// damaged columns (or a malformed context) cannot make the owner's diagnostic endpoint fail.
fn inspect_records<T>(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
    include_unattributed: bool,
    mut select: impl FnMut(StartupRecord) -> Option<T>,
) -> FlowResult<Option<T>> {
    let mut stmt = c.prepare_cached(
        "SELECT rowid,id,context FROM flow_runs WHERE workspace_id=?1 AND flow_id=?2
         AND CASE WHEN json_valid(CAST(context AS TEXT)) AND json_type(CAST(context AS TEXT))='object' THEN
           json_extract(CAST(context AS TEXT),'$.source')=?3 AND json_type(CAST(context AS TEXT),'$.startup') IS NOT NULL
         ELSE ?4 END ORDER BY rowid DESC",
    )?;
    let rows = stmt.query_map(params![ws, flow, source, include_unattributed], |r| {
        let rowid: i64 = r.get(0)?;
        let id = r.get::<_, String>(1).ok();
        let raw = r.get_ref(2)?;
        let context = match raw {
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                serde_json::from_slice(bytes).ok()
            }
            _ => None,
        };
        let context_type_ok = matches!(raw, rusqlite::types::ValueRef::Text(_));
        Ok(StartupRecord {
            unreadable: id.is_none() || context.is_none() || !context_type_ok,
            id: id.unwrap_or_else(|| format!("rowid:{rowid}")),
            context: context.unwrap_or(Value::Null),
        })
    })?;
    for row in rows {
        if let Some(value) = select(row?) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn latest(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
    revision: &Value,
) -> FlowResult<Option<StartupRecord>> {
    // Filter the configuration identity BEFORE decoding the budget. Keep the damaged old row
    // unchanged; a deliberate save has a new identity and must be able to start at zero.
    Ok(inspect_records(c, ws, flow, source, false, Some)?
        .filter(|row| row.context["startup"]["revision"] == *revision))
}

pub fn startup_check(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
    revision: &Value,
) -> FlowResult<StartupCheck> {
    let Some(row) = latest(c, ws, flow, source, revision)? else {
        return Ok(StartupCheck::State(None));
    };
    match metadata(&row.context) {
        Ok(meta) if !row.unreadable => Ok(StartupCheck::State(Some(StartupState {
            run_id: row.id,
            state: meta.state,
            attempts: meta.attempts,
            limit: meta.limit,
        }))),
        _ => Ok(StartupCheck::Invalid(diagnostic(
            &row,
            flow,
            source,
            Some(revision),
        ))),
    }
}

/// Only source-owned records in this configuration advance the budget.
pub fn startup_state(
    c: &Connection,
    ws: &str,
    flow: &str,
    source: &str,
    revision: &Value,
) -> FlowResult<Option<StartupState>> {
    match startup_check(c, ws, flow, source, revision)? {
        StartupCheck::State(state) => Ok(state),
        StartupCheck::Invalid(d) => Err(crate::FlowError::Internal(format!(
            "Invalid startup state for run {}",
            d.run_id
        ))),
    }
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
    record_start_rejection(tx, f, f.error)
}

/// Store only a stable code/parameter object and a fingerprint of the internal rejection.
/// Distinct errors still split history even when their safe user-facing explanation is identical.
pub(crate) fn record_start_rejection(
    tx: &Tx,
    f: &FailedStart<'_>,
    detail: &str,
) -> FlowResult<String> {
    let fingerprint = steve_store::sha(detail);
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
    let previous_id: Option<(Option<String>, Value)> = tx.query_row(
        "SELECT id,context FROM flow_runs WHERE workspace_id=?1 AND flow_id=?2 ORDER BY rowid DESC LIMIT 1",
        params![f.ws,f.flow_id], |r| {
            let context = r.get::<_,String>(1).ok().and_then(|s|serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
            Ok((r.get(0)?,context))
        }).optional()?;
    let row = match previous_id.filter(|(_, ctx)| ctx["startup"]["revision"] == *f.revision) {
        Some((Some(id), _)) => get(tx, &id)?,
        _ => None,
    };
    let previous = row.filter(|r| {
        r.status == "failed"
            && r.error.as_deref() == Some(START_REJECTED_CODE)
            && valid_error(&r.context)
            && r.context["error_fingerprint"] == fingerprint
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
        "start_failures":failures,"startup":meta,"error_fingerprint":fingerprint,"start_error":error_identity()});
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
                outcome_label: START_REJECTED_CODE,
                error: Some(START_REJECTED_CODE),
                cursor: None,
                context: &context,
                now: f.now,
            },
        )?,
    };
    if reused {
        tx.execute(
            "UPDATE flow_runs SET error=?1,outcome_label=?1,context=?2,started=?3,ended=?3 WHERE id=?4",
            params![START_REJECTED_CODE, context.to_string(), f.now, run],
        )?;
        publish(tx, f.ws, f.flow_id, &run);
    }
    Ok(run)
}

// One causal state anchor per live flow/source, including a successful reset. Do not resurrect
// an older dead record by pruning its later reset. Superseded anchors use ordinary retention.
pub(super) const ANCHORS: &str = "SELECT max(r.rowid) FROM flow_runs r JOIN flows f ON f.id=r.flow_id
    WHERE f.deleted=0 AND CASE WHEN json_valid(r.context) THEN json_type(r.context,'$.startup') IS NOT NULL ELSE 0 END
    GROUP BY r.workspace_id,r.flow_id,CASE WHEN json_valid(r.context) THEN json_extract(r.context,'$.source') END";
