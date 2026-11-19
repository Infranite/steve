-- An adapter receipt owns only the decision and the reference to a standard flow event.
-- Run, step and item state are projected from their own tables, never copied into this receipt.
ALTER TABLE gh_deliveries ADD COLUMN automation TEXT;

CREATE INDEX gh_deliveries_automation_event
    ON gh_deliveries(workspace_id, json_extract(automation, '$.event_id'))
    WHERE automation IS NOT NULL;
CREATE INDEX gh_deliveries_automation_key
    ON gh_deliveries(repo_id, json_extract(automation, '$.key'))
    WHERE automation IS NOT NULL;

-- Retire only the untouched auto-installed proposal, not a user-edited/enabled rule.
UPDATE flow_triggers SET enabled = 0
 WHERE flow_id IN (SELECT id FROM flows WHERE origin = 'template:github-issues'
     AND version = 1 AND updated = created AND enabled = 1 AND deleted = 0);
UPDATE flows SET enabled = 0, updated = updated + 1
 WHERE origin = 'template:github-issues'
    AND version = 1 AND updated = created AND enabled = 1 AND deleted = 0;
