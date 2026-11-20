//! Temporary R4c/d/e developer-install cleanup (T49). No legacy loading or conversion.
use super::*;

pub const UNPUBLISHED_START_BATCH: usize = 128;

// Exact unpublished shapes: {at,source,trigger}, optionally a positive integer count and then
// an integer first_at. Unknown keys, duplicate keys and current/future startup formats stay put.
const OLD_START: &str = "trigger_kind='manual' AND status='failed' AND typeof(error)='text'
    AND outcome_label=error AND work_item_id IS NULL AND cursor IS NULL AND event_id IS NULL
    AND parent_run IS NULL AND dry_run=0 AND depth=0 AND attempts=0 AND cost=0
    AND retry_at IS NULL AND outcome IS NULL AND ended=started
    AND CASE WHEN json_valid(context) THEN json_type(context)='object'
      AND json_extract(context,'$.source')='digest.bot'
      AND json_extract(context,'$.trigger')='manual'
      AND json_type(context,'$.at')='integer' AND json_extract(context,'$.at')=started
      AND NOT EXISTS (SELECT 1 FROM json_each(context)
        WHERE key NOT IN ('at','source','trigger','start_failures','first_at'))
      AND (SELECT count(*) FROM json_each(context))=3
        + (json_type(context,'$.start_failures') IS NOT NULL)
        + (json_type(context,'$.first_at') IS NOT NULL)
      AND (json_type(context,'$.start_failures') IS NULL OR
        (json_type(context,'$.start_failures')='integer' AND json_extract(context,'$.start_failures')>0))
      AND (json_type(context,'$.first_at') IS NULL OR
        (json_type(context,'$.start_failures')='integer' AND json_type(context,'$.first_at')='integer'))
    ELSE 0 END";

// R4f stored the translated explanation in error/outcome_label. Those rows cannot be
// interpreted as the new code+params contract: delete the exact old shape, never convert it.
const TEXT_START: &str = "trigger_kind='manual' AND status='failed' AND typeof(error)='text'
    AND outcome_label=error AND work_item_id IS NULL AND cursor IS NULL AND event_id IS NULL
    AND parent_run IS NULL AND dry_run=0 AND depth=0 AND attempts=0 AND cost=0
    AND retry_at IS NULL AND outcome IS NULL AND ended=started
    AND CASE WHEN json_valid(context) THEN json_type(context)='object'
      AND json_extract(context,'$.source')='digest.bot' AND json_extract(context,'$.trigger')='manual'
      AND json_type(context,'$.at')='integer' AND json_extract(context,'$.at')=started
      AND (SELECT count(*) FROM json_each(context))=6 + (json_type(context,'$.error_fingerprint') IS NOT NULL)
      AND NOT EXISTS (SELECT 1 FROM json_each(context)
        WHERE key NOT IN ('at','source','trigger','start_failures','first_at','error_fingerprint','startup'))
      AND json_type(context,'$.start_failures')='integer' AND json_extract(context,'$.start_failures')>0
      AND json_type(context,'$.first_at')='integer'
      AND (json_type(context,'$.error_fingerprint') IS NULL OR
        (json_type(context,'$.error_fingerprint')='text' AND length(json_extract(context,'$.error_fingerprint'))=64
          AND json_extract(context,'$.error_fingerprint') NOT GLOB '*[^0-9a-f]*'))
      AND json_type(context,'$.startup')='object'
      AND (SELECT count(*) FROM json_each(context,'$.startup'))=5
      AND NOT EXISTS (SELECT 1 FROM json_each(context,'$.startup')
        WHERE key NOT IN ('format','revision','state','attempts','limit'))
      AND json_type(context,'$.startup.format')='integer' AND json_extract(context,'$.startup.format')=1
      AND json_type(context,'$.startup.revision')='object'
      AND json_type(context,'$.startup.limit')='integer' AND json_extract(context,'$.startup.limit')>0
      AND json_type(context,'$.startup.attempts')='integer'
      AND json_extract(context,'$.start_failures')<=json_extract(context,'$.startup.attempts')
      AND ((json_extract(context,'$.startup.state')='retrying' AND json_extract(context,'$.startup.attempts')<json_extract(context,'$.startup.limit'))
        OR (json_extract(context,'$.startup.state')='dead' AND json_extract(context,'$.startup.attempts')=json_extract(context,'$.startup.limit')))
    ELSE 0 END";

/// Remove one bounded batch of provably unpublished digest startup markers and their dependents.
/// Call in a transaction until a short batch is returned. The named digest job owns the retry.
/// Independent items/entities and child runs survive, with relations to deleted runs removed.
pub fn cleanup_unpublished_digest_starts(tx: &Tx) -> FlowResult<usize> {
    let rows = tx
        .prepare(&format!(
            "SELECT id,workspace_id,flow_id FROM flow_runs WHERE ({OLD_START}) OR ({TEXT_START}) ORDER BY rowid LIMIT ?"
        ))?
        .query_map([UNPUBLISHED_START_BATCH as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, ws, flow) in &rows {
        tx.execute("DELETE FROM flow_run_steps WHERE run_id=?", [id])?;
        tx.execute("DELETE FROM flow_waits WHERE run_id=?", [id])?;
        tx.execute("DELETE FROM links WHERE workspace_id=?1 AND ((subject_type='flow_run' AND subject_id=?2) OR (object_type='flow_run' AND object_id=?2))", params![ws,id])?;
        tx.execute(
            "UPDATE flow_runs SET parent_run=NULL WHERE parent_run=?",
            [id],
        )?;
        tx.execute("DELETE FROM flow_runs WHERE id=?", [id])?;
        publish(tx, ws, flow, id);
    }
    Ok(rows.len())
}
