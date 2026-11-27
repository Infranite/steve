-- 148: async Steve bot-dispatch jobs and clean up legacy keyword-scoring PlacementReason rows (#626).
CREATE TABLE IF NOT EXISTS item_dispatch_jobs (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending', 'running')),
  attempt_id TEXT,
  required_node TEXT,
  required_specialties TEXT NOT NULL DEFAULT '[]',
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_item_dispatch_jobs_state
  ON item_dispatch_jobs(state, created);

UPDATE bot_placements
   SET fact = json_remove(
         json_set(fact, '$.explanation.reason', 'existing_assignment'),
         '$.explanation.matched'
       )
 WHERE fact IS NOT NULL
   AND json_extract(fact, '$.explanation.reason') IN (
     'specialty',
     'description',
     'project_preference',
     'stable_order'
   );

UPDATE bot_placements
   SET fact = json_remove(fact, '$.explanation.matched')
 WHERE fact IS NOT NULL
   AND json_extract(fact, '$.explanation.matched') IS NOT NULL;
