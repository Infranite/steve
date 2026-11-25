-- Remove retired dynamic work-graph tables, steve_workflow tool pins, and stale close-request workflow revisions.
DROP TABLE IF EXISTS workflow_review_findings;
DROP TABLE IF EXISTS workflow_units;
DROP TABLE IF EXISTS workflow_history;
DROP TABLE IF EXISTS workflow_attempts;
DROP TABLE IF EXISTS workflows;

DELETE FROM bot_tool_pins WHERE kind = 'tool' AND target = 'steve_workflow';

UPDATE items
   SET closing = json_remove(closing, '$.workflow_revision')
 WHERE closing IS NOT NULL
   AND json_valid(closing)
   AND json_type(closing, '$.workflow_revision') IS NOT NULL;
