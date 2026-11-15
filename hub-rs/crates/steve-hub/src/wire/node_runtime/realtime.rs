//! Persistent realtime collection at the ordered Node apply boundary. A watermark, its transcript
//! projection, and these samples commit or roll back together. Final usage never becomes output.

use super::*;

impl NodeRuntime {
    pub(super) fn realtime_in(&self, tx: &Tx, ev: &NodeEvent) -> Result<(), StoreError> {
        let Subject::Execution(ExecutionRef::Turn {
            session, attempt, ..
        }) = &ev.subject
        else {
            return Ok(()); // imported history, background work and jobs are not session output
        };
        let received: i64 = tx.query_row(
            "SELECT received FROM node_event_inbox WHERE node_id=? AND stream=? AND epoch=? AND seq=?",
            rusqlite::params![self.node_id.as_str(),ev.cursor.stream.as_str(),ev.cursor.epoch.as_str(),ev.cursor.seq as i64],
            |r| r.get(0),
        )?;
        // Even without a start baseline (an upgrade mid-attempt), persist Node stop evidence.
        // Seeing more events or a Hub-side settle must never manufacture a complete baseline.
        let started_here = matches!(ev.body, EventBody::AttemptStarted);
        let start_at = if ev.at.0 > 0 && ev.at.0 <= received + 1_000 {
            ev.at.0.min(received)
        } else {
            received
        };
        tx.execute(
            "INSERT OR IGNORE INTO realtime_output_attempts
                    (attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started,baseline)
                 SELECT a.attempt_id,s.id,s.workspace_id,i.project_id,s.bot_id,a.node_id,
                        CASE WHEN ?5 THEN ?4 ELSE MIN(a.created,?6) END,?5
                   FROM runtime_attempts a JOIN runtime_turns t USING(turn_id)
                   JOIN sessions s ON s.id=t.session_id
                   LEFT JOIN items i ON i.id=s.item_id AND i.workspace_id=s.workspace_id
                  WHERE a.attempt_id=?1 AND s.id=?2 AND a.node_id=?3 AND s.readonly=0",
            rusqlite::params![
                attempt.as_str(),
                session.as_str(),
                self.node_id.as_str(),
                start_at,
                started_here,
                received
            ],
        )?;
        let baseline: Option<(i64,Option<i64>)> = tx.query_row(
            "SELECT started,ended FROM realtime_output_attempts WHERE attempt_id=? AND session_id=? AND node_id=?",
            [attempt.as_str(),session.as_str(),self.node_id.as_str()], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((started, ended)) = baseline else {
            return Ok(());
        };
        if ended.is_some() {
            return Ok(()); // a new position after a confirmed end is not a new output interval
        }
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM realtime_output_samples WHERE node_id=? AND (event_id=? OR (stream=? AND epoch=? AND seq=?)))",
            rusqlite::params![self.node_id.as_str(),ev.id.as_str(),ev.cursor.stream.as_str(),ev.cursor.epoch.as_str(),ev.cursor.seq as i64],
            |r| r.get(0),
        )?;
        if seen {
            return Ok(());
        }
        let last: Option<i64> = tx.query_row(
            "SELECT MAX(at) FROM realtime_output_samples WHERE attempt_id=?",
            [attempt.as_str()],
            |r| r.get(0),
        )?;
        // A bad clock cannot put output into the future or reverse a block's time. Retain the
        // source timestamp for audit, but only an unknown marker (zero units) uses arrival time.
        let bad_clock = ev.at.0 < started
            || ev.at.0 <= 0
            || ev.at.0 > received + 1_000
            || last.is_some_and(|last| ev.at.0 < last);
        // In particular, a corrupt AttemptEnded cannot cut off already collected output or its
        // own gap. A valid late stop keeps source time; an invalid one uses this receipt boundary.
        let effective_at = if bad_clock {
            received.max(started).max(last.unwrap_or(started))
        } else {
            ev.at.0
        };
        let mut kind = if bad_clock { "gap" } else { "activity" };
        let mut units = 0;
        match &ev.body {
            EventBody::MessageStarted {
                message,
                block,
                role,
            } => {
                let role = match role {
                    Role::Assistant => "assistant",
                    Role::Thought => "thought",
                    Role::User => "user",
                };
                tx.execute(
                    "INSERT OR IGNORE INTO realtime_output_blocks(attempt_id,message,block,role) VALUES (?,?,?,?)",
                    [attempt.as_str(), message, block, role],
                )?;
            }
            EventBody::TextDelta {
                message,
                block,
                utf8_offset,
                text,
            } => {
                let checkpoint: Option<(String,i64)> = tx.query_row(
                    "SELECT role,utf8_end FROM realtime_output_blocks WHERE attempt_id=? AND message=? AND block=?",
                    [attempt.as_str(),message,block], |r| Ok((r.get(0)?,r.get(1)?)),
                ).optional()?;
                if let Some((role, end)) = checkpoint {
                    let offset = i64::try_from(*utf8_offset).ok();
                    let next = offset.and_then(|o| o.checked_add(text.len() as i64));
                    if let (Some(offset), Some(next)) = (offset, next) {
                        if offset > end {
                            kind = "gap"; // missing prefix: never pretend this is a complete rate
                        } else if next > end && (role == "assistant" || role == "thought") {
                            if let Some(suffix) = text.get((end - offset) as usize..) {
                                if !bad_clock {
                                    units = steve_stats::realtime::text_units(suffix);
                                    kind = "text";
                                }
                            } else {
                                kind = "gap";
                            } // overlapping UTF-8 code point
                        }
                        tx.execute(
                            "UPDATE realtime_output_blocks SET utf8_end=MAX(utf8_end,?) WHERE attempt_id=? AND message=? AND block=?",
                            rusqlite::params![next,attempt.as_str(),message,block],
                        )?;
                    } else {
                        kind = "gap";
                    }
                } else {
                    kind = "gap"; // no message/role baseline, including an upgrade mid-message
                }
            }
            EventBody::AttemptEnded { .. } => {
                if !bad_clock {
                    kind = "end";
                }
                tx.execute(
                    "UPDATE realtime_output_attempts SET ended=COALESCE(ended,?) WHERE attempt_id=?",
                    rusqlite::params![effective_at,attempt.as_str()],
                )?;
            }
            EventBody::Diagnostic { code, .. }
                if matches!(
                    code.as_str(),
                    "event_too_large" | "adapter_exited" | "reader_failed"
                ) =>
            {
                kind = "gap";
            }
            EventBody::Usage { report }
                if report.measurement == Measurement::Final
                    && report.scope != UsageScope::Background
                    && report.tokens.output.is_some_and(|n| n > 0) =>
            {
                let observed: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM realtime_output_samples WHERE attempt_id=? AND units>0)",
                    [attempt.as_str()], |r| r.get(0),
                )?;
                if !observed {
                    kind = "gap";
                } // final accounting proves output existed, but no text was collected
            }
            // In particular Usage::Final is merely activity with zero units. A context meter is
            // not an output meter, even when the protocol names its measurement cumulative.
            _ => {}
        }
        tx.execute(
            "INSERT INTO realtime_output_samples(node_id,stream,epoch,seq,event_id,attempt_id,at,source_at,received_at,kind,units)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![self.node_id.as_str(),ev.cursor.stream.as_str(),ev.cursor.epoch.as_str(),ev.cursor.seq as i64,
                ev.id.as_str(),attempt.as_str(),effective_at,ev.at.0,received,kind,units],
        )?;
        Ok(())
    }
}
