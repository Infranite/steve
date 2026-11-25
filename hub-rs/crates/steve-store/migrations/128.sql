-- Steve is the workspace coordinator, never a Team member. Strip any main bot rows from
-- bot_teams.members, rewrite historical steve_fallback placement facts to coordinator_deferred,
-- and track items waiting for an executor so one workspace decision card covers them all.
UPDATE bot_teams
   SET members = COALESCE(
     (SELECT json_group_array(json(m.value))
        FROM json_each(bot_teams.members) AS m
       WHERE NOT EXISTS (
         SELECT 1 FROM bots b
          WHERE b.id = json_extract(m.value, '$.bot')
            AND b.main = 1
       )),
     '[]'
   )
 WHERE json_valid(members)
   AND EXISTS (
     SELECT 1
       FROM json_each(bot_teams.members) AS m
       JOIN bots b ON b.id = json_extract(m.value, '$.bot')
      WHERE b.main = 1
   );

UPDATE bot_placements
   SET fact = json_set(
     fact,
     '$.explanation.reason', 'coordinator_deferred',
     '$.explanation.deferred_by', 'setting'
   )
 WHERE fact IS NOT NULL
   AND json_valid(fact)
   AND json_extract(fact, '$.explanation.reason') = 'steve_fallback';

CREATE TABLE item_placement_waits (
    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    blocked TEXT NOT NULL,
    proposal_id TEXT REFERENCES bot_proposals(id) ON DELETE SET NULL,
    created INTEGER NOT NULL
);
CREATE INDEX item_placement_waits_ws ON item_placement_waits(workspace_id, created);
