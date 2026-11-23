-- Decisions retain their ID and historical outcome. Each new answer is tied to the
-- version the caller saw, and reopening a card leaves its previous receipt intact.
ALTER TABLE decisions ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;

CREATE TABLE decision_choice_receipts (
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    answer TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (decision_id, revision)
);

-- Retire cards that cannot apply to the current domain. Leave their links and
-- every recorded choice in place; neither case authorizes a new action.
UPDATE decisions AS old
   SET status = 'superseded', resolved = created
 WHERE status = 'open'
   AND (
     subject_type IN ('delivery_batch','mq_target','mq_batch','mq_train')
     OR (
       kind IN ('ask','fail') AND subject_type IN ('question','item','stall')
       AND EXISTS (
         SELECT 1 FROM decisions AS later
          WHERE later.workspace_id = old.workspace_id
            AND later.item_id = old.item_id AND later.session_id = old.session_id
            AND later.kind IN ('accept','approve') AND later.created > old.created
       )
     )
   );
