-- Explicit owner completion is final even when prerequisites were left open.
DROP TRIGGER item_dependencies_completion_gate;
CREATE TRIGGER item_dependencies_completion_gate
BEFORE UPDATE OF phase,outcome,status,cycle ON items
WHEN NEW.phase='closed' AND NEW.outcome='completed' AND NEW.acceptance IS NOT 'owner'
 AND (EXISTS (
  SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
  WHERE d.item_id=NEW.id AND
   (p.id IS NULL OR p.status<>'done' OR p.phase<>'closed' OR p.outcome IS NOT 'completed')
 ) OR EXISTS (
  SELECT 1 FROM item_dependency_waits w
  WHERE w.item_id=NEW.id AND w.cycle=NEW.cycle AND w.state='waiting'
 ))
BEGIN
 SELECT RAISE(ABORT,'prerequisite items are not completed');
END;

-- These projections were used only by the model completion judge and the dry-run sweep.
DROP TABLE item_would_do;
DROP TABLE item_judgements;
DROP TABLE item_judge_candidates;
DROP TABLE item_waits;
DROP TABLE work_policy;

-- A migrated association keeps its identity and observed head. It cannot authorize closure.
CREATE TABLE item_delivery_refs (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 scope INTEGER NOT NULL,
 repository TEXT NOT NULL,
 number INTEGER NOT NULL CHECK(number>0),
 target TEXT NOT NULL,
 head TEXT NOT NULL,
 base TEXT,
 required INTEGER NOT NULL CHECK(required IN (0,1) AND (required=0 OR base IS NOT NULL)),
 PRIMARY KEY(item_id,cycle,scope,repository,number)
);
CREATE TABLE item_delivery_proofs (
 item_id TEXT NOT NULL,
 cycle INTEGER NOT NULL,
 scope INTEGER NOT NULL,
 revision INTEGER NOT NULL,
 repository TEXT NOT NULL,
 number INTEGER NOT NULL,
 target_head TEXT NOT NULL CHECK (length(target_head) IN (40,64) AND target_head NOT GLOB '*[^0-9a-f]*'),
 PRIMARY KEY(item_id,cycle,scope,revision,repository,number,target_head),
 FOREIGN KEY(item_id,cycle,scope,repository,number)
  REFERENCES item_delivery_refs(item_id,cycle,scope,repository,number) ON DELETE CASCADE
);
INSERT INTO item_delivery_refs(item_id,cycle,scope,repository,number,target,head,base,required)
 SELECT i.id,i.cycle,COALESCE((SELECT MAX(version) FROM item_goals WHERE item_id=i.id),0),
  p.repo_id,p.number,p.base_ref,p.head_sha,NULL,0
 FROM gh_prs p JOIN items i ON i.id=p.item_id AND i.workspace_id=p.workspace_id
 WHERE p.number>0 AND length(p.head_sha) IN (40,64) AND p.head_sha NOT GLOB '*[^0-9a-f]*'
 ON CONFLICT DO NOTHING;
DROP TABLE item_delivery_facts;

-- The old direct Finish recorded an explicit human acceptance without an acceptance card.
-- Preserve that fact, including when a pause or a wait prevented the old commit.
CREATE TEMP TABLE item_owner_finishes AS
 SELECT id,cycle,revision,COALESCE(json_extract(closing,'$.at'),updated) AS closed
 FROM items WHERE phase='closing' AND json_extract(closing,'$.kind')='complete'
  AND json_extract(closing,'$.reason')='你标记了完成'
  AND json_extract(closing,'$.policy')='human'
  AND json_extract(closing,'$.accepted')=1 AND json_extract(closing,'$.decision') IS NULL;
INSERT INTO item_cycles(item_id,cycle,revision,outcome,acceptance,reason,closed,source)
 SELECT id,cycle,revision,'completed','owner','owner_finished',closed,'migrated' FROM item_owner_finishes WHERE true
 ON CONFLICT(item_id,cycle) DO NOTHING;
UPDATE items SET phase='closed',status='done',outcome='completed',acceptance='owner',
 closed_reason='owner_finished',closed_at=(SELECT closed FROM item_owner_finishes f WHERE f.id=items.id),closing=NULL
 WHERE id IN (SELECT id FROM item_owner_finishes);
UPDATE items SET acceptance='owner',closed_reason='owner_finished'
 WHERE phase='closed' AND outcome='completed' AND closed_reason='你标记了完成' AND acceptance='human';
UPDATE item_cycles SET acceptance='owner',reason='owner_finished'
 WHERE outcome='completed' AND reason='你标记了完成' AND acceptance='human';

INSERT INTO session_dispatch (id,session_id,text,via,created,files,state,updated)
 SELECT q.id,q.session_id,q.text,q.via,q.created,q.files,'cancelled',f.closed
 FROM queued q JOIN sessions s ON s.id=q.session_id JOIN item_owner_finishes f ON f.id=s.item_id
 WHERE q.via NOT IN ('steve','resumed:steve')
 ON CONFLICT(id) DO NOTHING;
DELETE FROM queued WHERE session_id IN (
 SELECT s.id FROM sessions s JOIN item_owner_finishes f ON f.id=s.item_id
);
DELETE FROM session_resume WHERE session_id IN (
 SELECT s.id FROM sessions s JOIN item_owner_finishes f ON f.id=s.item_id
);
DELETE FROM item_dependency_waits WHERE state='waiting' AND EXISTS (
 SELECT 1 FROM item_owner_finishes f WHERE f.id=item_dependency_waits.item_id AND f.cycle=item_dependency_waits.cycle
);

-- The old automatic request did not pin a manifest or a current default-branch proof.
-- Retain its facts and exclude unstarted wrap-up from recovery.
CREATE TEMP TABLE item_discarded_closings AS
 SELECT id,cycle,revision,updated FROM items
 WHERE phase='closing' AND json_extract(closing,'$.kind')='complete';
UPDATE decisions SET status='expired',resolved=(
 SELECT updated FROM items i WHERE i.id=decisions.item_id
),result='{"superseded":"item_lifecycle_migrated"}'
 WHERE status='open' AND (
  (subject_type IN ('gh_pr_open','gh_pr_merge') AND requested_by='steve')
  OR item_id IN (SELECT id FROM item_owner_finishes)
  OR (item_id IN (SELECT id FROM item_discarded_closings)
   AND (kind='accept' OR subject_type IN ('release','pr_awaiting_merge','gh_pr_open','gh_pr_merge')))
 );
DELETE FROM item_operations WHERE kind='hooks.accepted';
UPDATE item_operations SET required=0,status=CASE WHEN status='pending' THEN 'waived' ELSE status END
 WHERE EXISTS (
  SELECT 1 FROM item_owner_finishes f WHERE f.id=item_operations.item_id AND f.cycle=item_operations.cycle
 ) OR EXISTS (
  SELECT 1 FROM item_discarded_closings d WHERE d.id=item_operations.item_id
   AND d.cycle=item_operations.cycle AND d.revision=item_operations.revision
 );
-- Bind still eligible explicit PR operations to their original item scope.
ALTER TABLE gh_effects ADD COLUMN item_cycle INTEGER;
ALTER TABLE gh_effects ADD COLUMN item_scope INTEGER;
UPDATE gh_effects SET (item_cycle,item_scope)=(
 SELECT i.cycle,COALESCE((SELECT MAX(version) FROM item_goals WHERE item_id=i.id),0)
 FROM decisions d JOIN items i ON i.id=d.item_id
 WHERE d.id=gh_effects.decision_id AND i.phase<>'closed'
   AND gh_effects.created >= COALESCE((SELECT MAX(closed) FROM item_cycles
      WHERE item_id=i.id AND cycle<i.cycle),0)
   AND gh_effects.created >= COALESCE((SELECT MAX(created) FROM item_goals WHERE item_id=i.id),0)
) WHERE kind IN ('open','merge') AND state IN ('pending','running','unknown');
UPDATE gh_effects SET state=CASE WHEN state='pending' THEN 'failed' ELSE state END,
 next_at=NULL,last_error='item lifecycle migrated'
 WHERE state IN ('pending','unknown') AND kind IN ('open','merge')
  AND decision_id IN (SELECT id FROM decisions
   WHERE (subject_type IN ('gh_pr_open','gh_pr_merge') AND requested_by='steve')
    OR item_id IN (SELECT id FROM item_owner_finishes UNION SELECT id FROM item_discarded_closings));
UPDATE gh_repos SET settings=json_remove(settings,'$.after_accept','$.finish_on','$.draft_pr');
-- Old outbox completions were missing the cycle and accepted head. Preserve other effects.
UPDATE outbox_events SET payload=json_remove(payload,'$.finish')
 WHERE topic='github.after' AND json_type(payload,'$.finish')='array';
DELETE FROM queued WHERE via IN ('steve','resumed:steve') AND session_id IN (
 SELECT s.id FROM sessions s JOIN item_discarded_closings d ON d.id=s.item_id
);
UPDATE items SET phase='active',revision=revision+1,closing=NULL
 WHERE id IN (SELECT id FROM item_discarded_closings);
UPDATE items SET closing=json_remove(closing,'$.policy')
 WHERE phase='closing' AND json_extract(closing,'$.kind') IN ('cancel','fail');
DROP TABLE item_owner_finishes;
DROP TABLE item_discarded_closings;
