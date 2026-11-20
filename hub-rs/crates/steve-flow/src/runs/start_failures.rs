//! Persistence of source-driven schedule startup rejections; execution stays in the scheduler.
use super::*;

/// A startup attempt rejected before it created executable work.
pub struct FailedStart<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub source: &'a str,
    pub error: &'a str,
    pub now: i64,
}

/// Keep one history entry per uninterrupted sequence of Bot startup rejections. Other runs
/// (including manual/cron runs), a different error or a version change begin a new sequence.
pub fn record_start_failure(tx: &Tx, failure: &FailedStart<'_>) -> FlowResult<String> {
    let FailedStart {
        ws,
        flow_id: id,
        flow_version,
        source,
        error: reason,
        now,
    } = *failure;
    let previous = page(tx, ws, id, None, 1)?.pop().filter(|r| {
        r.status == "failed"
            && r.error.as_deref() == Some(reason)
            && r.flow_version == flow_version
            && r.context["source"] == source
            && r.work_item_id.is_none()
            && r.cursor.is_none()
            && !r.dry_run
    });
    // The first R4c marker already represents one failure even without an explicit count.
    // This is separate from attempts, which belongs to the flow engine's action retries.
    let failures = previous.as_ref().map_or(1, |r| {
        r.context["start_failures"]
            .as_i64()
            .unwrap_or(1)
            .max(1)
            .saturating_add(1)
    });
    let first_at = previous
        .as_ref()
        .map_or(now, |r| r.context["first_at"].as_i64().unwrap_or(r.started));
    let context = json!({"at":now,"first_at":first_at,"trigger":"manual","source":source,"start_failures":failures});
    let reused = previous.is_some();
    let run = match previous {
        Some(r) => r.id,
        None => insert(
            tx,
            &NewRun {
                ws,
                flow_id: id,
                flow_version,
                trigger_kind: "manual",
                status: "failed",
                outcome_label: reason,
                error: Some(reason),
                cursor: None,
                context: &context,
                now,
            },
        )?,
    };
    tx.execute(
        "UPDATE flow_runs SET error = ?1, outcome_label = ?1, context = ?2, started = ?3, ended = ?3 WHERE id = ?4",
        params![reason, context.to_string(), now, run],
    )?;
    if reused {
        publish(tx, ws, id, &run);
    }
    Ok(run)
}
