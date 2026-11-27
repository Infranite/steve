-- 148: async Steve bot-dispatch jobs and clean up legacy keyword-scoring PlacementReason rows (#626).
CREATE TABLE IF NOT EXISTS item_dispatch_jobs (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending', 'running')),
  attempt_id TEXT,
  required_node TEXT,
  required_specialties TEXT NOT NULL DEFAULT '[]',
  project_explicit INTEGER NOT NULL DEFAULT 1 CHECK(project_explicit IN (0, 1)),
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_item_dispatch_jobs_state
  ON item_dispatch_jobs(state, created);

ALTER TABLE item_placement_waits ADD COLUMN required_node TEXT;
ALTER TABLE item_placement_waits ADD COLUMN required_specialties TEXT NOT NULL DEFAULT '[]';
ALTER TABLE item_placement_waits ADD COLUMN project_explicit INTEGER NOT NULL DEFAULT 1;

UPDATE bot_placements
   SET fact = json_remove(
         json_set(fact, '$.explanation.reason', 'existing_assignment'),
         '$.explanation.matched'
       )
 WHERE fact IS NOT NULL
   AND json_valid(fact)
   AND json_extract(fact, '$.explanation.reason') IN (
     'specialty',
     'description',
     'project_preference',
     'stable_order'
   );

UPDATE bot_placements
   SET fact = json_remove(fact, '$.explanation.matched')
 WHERE fact IS NOT NULL
   AND json_valid(fact)
   AND json_extract(fact, '$.explanation.matched') IS NOT NULL;

UPDATE messages
   SET meta = json_remove(
         json_set(meta, '$.offer.reason', 'existing_assignment'),
         '$.offer.matched'
       )
 WHERE meta IS NOT NULL
   AND json_valid(meta)
   AND json_extract(meta, '$.offer.reason') IN (
     'specialty',
     'description',
     'project_preference',
     'stable_order'
   );

UPDATE messages
   SET meta = json_remove(meta, '$.offer.matched')
 WHERE meta IS NOT NULL
   AND json_valid(meta)
   AND json_extract(meta, '$.offer.matched') IS NOT NULL;
