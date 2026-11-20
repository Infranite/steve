-- Static SQL candidate, NOT a registered Store migration.
-- All retained rows participate, regardless of applied. Keep all other columns/watermarks.
WITH fixed(error) AS (VALUES('{"code":"internal","retry":{"kind":"never"},"reason":"node.error_detail_discarded","params":{},"safe_detail":"error details from before the upgrade were discarded"}')),
input(id,p) AS MATERIALIZED (SELECT rowid,payload FROM node_event_inbox),
position_0(id,p) AS (
  SELECT id, CASE WHEN json_valid(p) THEN
    CASE WHEN json_extract(p,'$.body.type')='operation_changed' AND json_extract(p,'$.body.receipt.state.state')='finished'
      AND json_type(p,'$.body.receipt.state.error') IS NOT NULL
      AND json_type(p,'$.body.receipt.state.error')<>'null'
    THEN json_set(p,'$.body.receipt.state.error',json((SELECT error FROM fixed))) ELSE p END
  ELSE p END FROM input
),
position_1(id,p) AS (
  SELECT id, CASE WHEN json_valid(p) THEN
    CASE WHEN json_extract(p,'$.body.type')='operation_changed' AND json_extract(p,'$.body.receipt.state.state')='finished' AND json_extract(p,'$.body.receipt.state.result.kind')='execution'
      AND json_type(p,'$.body.receipt.state.result.error') IS NOT NULL
      AND json_type(p,'$.body.receipt.state.result.error')<>'null'
    THEN json_set(p,'$.body.receipt.state.result.error',json((SELECT error FROM fixed))) ELSE p END
  ELSE p END FROM position_0
),
position_2(id,p) AS (
  SELECT id, CASE WHEN json_valid(p) THEN
    CASE WHEN json_extract(p,'$.body.type')='attempt_ended'
      AND json_type(p,'$.body.completion.error') IS NOT NULL
      AND json_type(p,'$.body.completion.error')<>'null'
    THEN json_set(p,'$.body.completion.error',json((SELECT error FROM fixed))) ELSE p END
  ELSE p END FROM position_1
)
UPDATE node_event_inbox AS target
SET payload=(SELECT p FROM position_2 WHERE id=target.rowid)
WHERE payload<>(SELECT p FROM position_2 WHERE id=target.rowid);
