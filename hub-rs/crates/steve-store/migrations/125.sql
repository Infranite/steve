-- 125: unify person identity across login accounts, timeline actors, and settled decisions.
--
-- 1. Link every workspace member without an existing party to a contact in that workspace:
--    first reuse a unique unlinked active contact matching the user's name, then create one.
UPDATE parties
SET user_id = (
    SELECT wm.user_id
    FROM workspace_members wm
    JOIN users u ON u.id = wm.user_id
    WHERE wm.workspace_id = parties.workspace_id
      AND NOT EXISTS (
          SELECT 1 FROM parties p2
          WHERE p2.workspace_id = wm.workspace_id AND p2.user_id = wm.user_id
      )
      AND lower(trim(u.name)) = lower(trim(parties.name))
      AND (
          SELECT count(*) FROM workspace_members wm2
          JOIN users u2 ON u2.id = wm2.user_id
          WHERE wm2.workspace_id = parties.workspace_id
            AND NOT EXISTS (
                SELECT 1 FROM parties p3
                WHERE p3.workspace_id = wm2.workspace_id AND p3.user_id = wm2.user_id
            )
            AND lower(trim(u2.name)) = lower(trim(parties.name))
      ) = 1
    LIMIT 1
)
WHERE type = 'contact'
  AND status = 'active'
  AND user_id IS NULL
  AND (
      SELECT count(*) FROM parties p_same
      WHERE p_same.workspace_id = parties.workspace_id
        AND p_same.type = 'contact'
        AND p_same.status = 'active'
        AND p_same.user_id IS NULL
        AND lower(trim(p_same.name)) = lower(trim(parties.name))
  ) = 1
  AND EXISTS (
      SELECT 1
      FROM workspace_members wm
      JOIN users u ON u.id = wm.user_id
      WHERE wm.workspace_id = parties.workspace_id
        AND NOT EXISTS (
            SELECT 1 FROM parties p2
            WHERE p2.workspace_id = wm.workspace_id AND p2.user_id = wm.user_id
        )
        AND lower(trim(u.name)) = lower(trim(parties.name))
        AND (
            SELECT count(*) FROM workspace_members wm2
            JOIN users u2 ON u2.id = wm2.user_id
            WHERE wm2.workspace_id = parties.workspace_id
              AND NOT EXISTS (
                  SELECT 1 FROM parties p3
                  WHERE p3.workspace_id = wm2.workspace_id AND p3.user_id = wm2.user_id
              )
              AND lower(trim(u2.name)) = lower(trim(parties.name))
        ) = 1
  );

INSERT INTO parties (
    id, workspace_id, type, name, aliases, avatar, rels, important, note,
    contact_kind, org_kind, user_id, visibility, status, merged_into,
    source, source_ref, revision, created_by, created, updated
)
SELECT
    'ct_' || lower(hex(randomblob(8))),
    wm.workspace_id,
    'contact',
    coalesce(nullif(trim(u.name), ''), '我'),
    '[]',
    '{}',
    '["me"]',
    0,
    '',
    'real',
    NULL,
    wm.user_id,
    'workspace',
    'active',
    NULL,
    'manual',
    NULL,
    1,
    wm.user_id,
    wm.created,
    wm.created
FROM workspace_members wm
JOIN users u ON u.id = wm.user_id
WHERE NOT EXISTS (
    SELECT 1 FROM parties p
    WHERE p.workspace_id = wm.workspace_id AND p.user_id = wm.user_id
);

-- 2. Strongly typed actor column on decisions (replaces untyped inference in settled lists).
ALTER TABLE decisions ADD COLUMN resolved_by TEXT;

UPDATE decisions
SET resolved_by = json_extract(result, '$.decided_by')
WHERE status = 'resolved'
  AND typeof(json_extract(result, '$.decided_by')) = 'text'
  AND json_extract(result, '$.decided_by') <> '';

UPDATE decisions
SET resolved_by = (
    SELECT wm.user_id FROM workspace_members wm
    WHERE wm.workspace_id = decisions.workspace_id
    LIMIT 1
)
WHERE status = 'resolved'
  AND resolved_by IS NULL
  AND (
      SELECT count(*) FROM workspace_members wm
      WHERE wm.workspace_id = decisions.workspace_id
  ) = 1;

-- 3. Backfill legacy literal 'me' timeline actors in single-member workspaces.
UPDATE timeline
SET actor = (
    SELECT wm.user_id
    FROM workspace_members wm
    JOIN items i ON i.workspace_id = wm.workspace_id
    WHERE i.id = timeline.item_id
    LIMIT 1
)
WHERE actor = 'me'
  AND (
      SELECT count(*)
      FROM workspace_members wm
      JOIN items i ON i.workspace_id = wm.workspace_id
      WHERE i.id = timeline.item_id
  ) = 1;
