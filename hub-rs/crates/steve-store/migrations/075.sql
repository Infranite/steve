-- A stream's complete collection baseline is independent of a Node-confirmed stop. Upgrading
-- mid-attempt may observe the latter without the former; Hub lifecycle updates prove neither.
ALTER TABLE realtime_output_attempts ADD COLUMN baseline INTEGER NOT NULL DEFAULT 1
 CHECK(baseline IN (0,1));

-- The first gap is a persistent coverage boundary, including when it precedes the visible tail.
-- The ordinary (attempt_id,at) index cannot efficiently find a gap in a long, healthy stream.
CREATE INDEX realtime_output_gaps ON realtime_output_samples(attempt_id,at) WHERE kind='gap';

-- Applied inbox history must not make coverage polls linear either. Pending events have a
-- separate seek; a faulted stream can seek its first received event for this attempt.
CREATE INDEX node_inbox_pending_attempt
 ON node_event_inbox(node_id,json_extract(payload,'$.subject.attempt'),received) WHERE applied=0;
CREATE INDEX node_inbox_attempt_time
 ON node_event_inbox(node_id,json_extract(payload,'$.subject.attempt'),stream,epoch,received);

-- Repair previously recorded backwards stop times without losing source_at audit evidence.
-- MAX(at) uses the window index; never let a stop hide later persisted output or missing data.
UPDATE realtime_output_attempts
 SET ended=MAX(ended,COALESCE((SELECT MAX(at) FROM realtime_output_samples s
                              WHERE s.attempt_id=realtime_output_attempts.attempt_id),ended))
 WHERE ended IS NOT NULL;
