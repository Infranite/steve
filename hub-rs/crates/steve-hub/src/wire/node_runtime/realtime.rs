//! Persistent realtime collection at the ordered Node apply boundary. A stream's position, its
//! transcript projection, and these samples commit or roll back together. Final usage never
//! becomes output.

use super::*;
use steve_store::realtime_output::{Block, Output, Position, Sample, SampleKind};

struct OutputSample {
    kind: SampleKind,
    units: i64,
    bad_clock: bool,
}

impl NodeRuntime {
    /// `received`: when the Hub received the event.
    pub(super) fn realtime_in(
        &self,
        w: &mut W<'_>,
        ev: &NodeEvent,
        received: i64,
    ) -> Result<(), StoreError> {
        let Subject::Execution(ExecutionRef::Turn {
            session, attempt, ..
        }) = &ev.subject
        else {
            return Ok(()); // imported history, background work and jobs are not session output
        };
        let position = self.realtime_position(ev);
        let started_at = matches!(ev.body, EventBody::AttemptStarted).then(|| {
            if ev.at.0 > 0 && ev.at.0 <= received + 1_000 {
                ev.at.0.min(received)
            } else {
                received
            }
        });
        let Some(started) = Output(w).admit(position, session, attempt, started_at, received)?
        else {
            return Ok(());
        };
        let last = Output(w).last_sample_at(attempt)?;
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
        let sample = self.realtime_body_in(w, ev, attempt, bad_clock, effective_at)?;
        Output(w).record(Sample {
            position,
            attempt,
            at: effective_at,
            source_at: ev.at.0,
            received_at: received,
            kind: sample.kind,
            units: sample.units,
        })
    }

    /// `ev` was received but never applied (its stream faulted on it): its attempt's output is
    /// unknown from then on.
    pub(super) fn realtime_gap_in(
        &self,
        w: &mut W<'_>,
        ev: &NodeEvent,
        received: i64,
    ) -> Result<(), StoreError> {
        let Subject::Execution(ExecutionRef::Turn { attempt, .. }) = &ev.subject else {
            return Ok(());
        };
        Output(w).record_gap(Sample {
            position: self.realtime_position(ev),
            attempt,
            at: received,
            source_at: ev.at.0,
            received_at: received,
            kind: SampleKind::Gap,
            units: 0,
        })
    }

    fn realtime_position<'a>(&'a self, ev: &'a NodeEvent) -> Position<'a> {
        Position {
            node: self.node_id.as_str(),
            stream: ev.cursor.stream.as_str(),
            epoch: ev.cursor.epoch.as_str(),
            sequence: ev.cursor.seq as i64,
            event: ev.id.as_str(),
        }
    }

    /// Classify the event's output evidence without treating final accounting as streamed text.
    fn realtime_body_in(
        &self,
        w: &mut W<'_>,
        ev: &NodeEvent,
        attempt: &AttemptId,
        bad_clock: bool,
        effective_at: i64,
    ) -> Result<OutputSample, StoreError> {
        let mut sample = OutputSample {
            kind: if bad_clock {
                SampleKind::Gap
            } else {
                SampleKind::Activity
            },
            units: 0,
            bad_clock,
        };
        match &ev.body {
            EventBody::MessageStarted {
                message,
                block,
                role,
            } => {
                Output(w).start_block(
                    &Block {
                        attempt,
                        message,
                        block,
                    },
                    *role,
                )?;
            }
            EventBody::TextDelta {
                message,
                block,
                utf8_offset,
                text,
            } => {
                Self::realtime_text_in(
                    w,
                    attempt,
                    message,
                    block,
                    *utf8_offset,
                    text,
                    &mut sample,
                )?;
            }
            EventBody::AttemptEnded { .. } => {
                if !bad_clock {
                    sample.kind = SampleKind::End;
                }
                Output(w).end(attempt, effective_at)?;
            }
            EventBody::Diagnostic { code, .. }
                if matches!(
                    code.as_str(),
                    "event_too_large" | "adapter_exited" | "reader_failed"
                ) =>
            {
                sample.kind = SampleKind::Gap;
            }
            EventBody::Usage { report }
                if report.measurement == Measurement::Final
                    && report.scope != UsageScope::Background
                    && report.tokens.output.is_some_and(|n| n > 0) =>
            {
                if !Output(w).has_text(attempt)? {
                    sample.kind = SampleKind::Gap;
                } // final accounting proves output existed, but no text was collected
            }
            // In particular Usage::Final is merely activity with zero units. A context meter is
            // not an output meter, even when the protocol names its measurement cumulative.
            _ => {}
        }
        Ok(sample)
    }

    /// Advance the block's byte checkpoint while counting only valid new assistant/thought text.
    fn realtime_text_in(
        w: &mut W<'_>,
        attempt: &AttemptId,
        message: &str,
        block: &str,
        utf8_offset: u64,
        text: &str,
        sample: &mut OutputSample,
    ) -> Result<(), StoreError> {
        let block = Block {
            attempt,
            message,
            block,
        };
        if let Some((role, end)) = Output(w).block_checkpoint(&block)? {
            let offset = i64::try_from(utf8_offset).ok();
            let next = offset.and_then(|o| o.checked_add(text.len() as i64));
            if let (Some(offset), Some(next)) = (offset, next) {
                if offset > end {
                    sample.kind = SampleKind::Gap; // missing prefix: never pretend this is a complete rate
                } else if next > end && matches!(role, Role::Assistant | Role::Thought) {
                    if let Some(suffix) = text.get((end - offset) as usize..) {
                        if !sample.bad_clock {
                            sample.units = steve_stats::realtime::text_units(suffix);
                            sample.kind = SampleKind::Text;
                        }
                    } else {
                        sample.kind = SampleKind::Gap;
                    } // overlapping UTF-8 code point
                }
                Output(w).advance_block(&block, next)?;
            } else {
                sample.kind = SampleKind::Gap;
            }
        } else {
            sample.kind = SampleKind::Gap; // no message/role baseline, including an upgrade mid-message
        }
        Ok(())
    }
}
