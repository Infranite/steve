-- Unified WaitContract columns, constraints, indexes, and backfills across durable wait tables.
-- Named defaults match steve_types::wait:
--   dependency      = 72h  (259_200_000 ms) -> escalate_to_owner
--   decision        = 7d   (604_800_000 ms) -> close
--   mem candidate   = 30d  (2_592_000_000 ms) -> close
--   secret_use      = 24h  (86_400_000 ms) -> close
--   channel_draft   = 72h  (259_200_000 ms) -> close (or connection card_ttl_h when set)
--   item_operation  = 60m  (3_600_000 ms) -> escalate_to_owner
--   external_effect = 24h  (86_400_000 ms) -> escalate_to_owner
--   mail_draft      = 7d   (604_800_000 ms) -> close (unknown -> escalate_to_owner)
--   flow_wait       = 7d   (604_800_000 ms) -> close

-- 1. decisions
ALTER TABLE decisions ADD COLUMN wait_owner TEXT NOT NULL DEFAULT 'owner'
 CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0));
ALTER TABLE decisions ADD COLUMN wait_started_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE decisions ADD COLUMN wait_deadline INTEGER NOT NULL DEFAULT 0;
ALTER TABLE decisions ADD COLUMN wait_on_timeout TEXT NOT NULL DEFAULT 'close'
 CHECK (wait_on_timeout IN ('escalate_to_owner', 'close'));

UPDATE decisions
   SET wait_owner = 'owner',
       wait_started_at = created,
       wait_on_timeout = 'close',
       wait_deadline = CASE
         WHEN EXISTS (SELECT 1 FROM runtime_permissions rp WHERE rp.decision_id = decisions.id) THEN
           MAX(created, (SELECT rp.expires_at FROM runtime_permissions rp WHERE rp.decision_id = decisions.id))
         WHEN subject_type = 'mem' THEN
           created + 2592000000
         WHEN subject_type = 'secret_use' THEN
           created + 86400000
         WHEN subject_type = 'chan_message' AND kind = 'approve' THEN
           created + COALESCE(
             (SELECT cs.card_ttl_h * 3600000
                FROM chan_messages m
                JOIN spaces s ON s.id = m.space_id
                JOIN channel_settings cs ON cs.connection_id = s.connection_id AND cs.workspace_id = decisions.workspace_id
               WHERE m.id = decisions.subject_id
                 AND cs.card_ttl_h IS NOT NULL
                 AND cs.card_ttl_h > 0
               LIMIT 1),
             259200000
           )
         ELSE
           created + 604800000
       END;

CREATE INDEX decisions_wait_due ON decisions(wait_deadline, id) WHERE status = 'open';

-- 2. item_dependency_waits (rebuilt to add 'timed_out' state and WaitContract columns)
DROP TRIGGER item_dependencies_completion_gate;
ALTER TABLE item_dependency_waits RENAME TO previous_item_dependency_waits;
CREATE TABLE item_dependency_waits (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 session_id TEXT REFERENCES sessions(id),
 resume TEXT NOT NULL CHECK(resume IN ('automatic','notify','workspace')),
 reason TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','started','notified','timed_out')),
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL,
 wait_owner TEXT NOT NULL CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0)),
 wait_started_at INTEGER NOT NULL,
 wait_deadline INTEGER NOT NULL,
 wait_on_timeout TEXT NOT NULL CHECK(wait_on_timeout IN ('escalate_to_owner','close')),
 CHECK(wait_deadline >= wait_started_at),
 PRIMARY KEY(item_id,cycle)
);
INSERT INTO item_dependency_waits(
 item_id, cycle, session_id, resume, reason, state, created, updated,
 wait_owner, wait_started_at, wait_deadline, wait_on_timeout
)
SELECT
 w.item_id, w.cycle, w.session_id, w.resume, w.reason, w.state, w.created, w.updated,
 CASE WHEN i.bot_id IS NOT NULL AND length(trim(i.bot_id)) > 0 THEN 'bot:' || i.bot_id ELSE 'system:dependencies' END,
 w.created,
 w.created + 259200000,
 'escalate_to_owner'
FROM previous_item_dependency_waits w
LEFT JOIN items i ON i.id = w.item_id;
DROP TABLE previous_item_dependency_waits;
CREATE TRIGGER item_dependencies_completion_gate
BEFORE UPDATE OF phase,outcome,cycle ON items
WHEN NEW.phase='closed' AND NEW.outcome='completed' AND NEW.acceptance IS NOT 'owner'
 AND (EXISTS (
  SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
  WHERE d.item_id=NEW.id AND (p.phase IS NOT 'closed' OR p.outcome IS NOT 'completed')
 ) OR EXISTS (
  SELECT 1 FROM item_dependency_waits w
  WHERE w.item_id=NEW.id AND w.cycle=NEW.cycle AND w.state='waiting'
 ))
BEGIN
 SELECT RAISE(ABORT,'prerequisite items are not completed');
END;
CREATE INDEX item_dependency_waits_due ON item_dependency_waits(wait_deadline, item_id, cycle)
 WHERE state = 'waiting';

-- 3. item_operations
ALTER TABLE item_operations ADD COLUMN wait_owner TEXT NOT NULL DEFAULT 'system:operations'
 CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0));
ALTER TABLE item_operations ADD COLUMN wait_started_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE item_operations ADD COLUMN wait_deadline INTEGER NOT NULL DEFAULT 0;
ALTER TABLE item_operations ADD COLUMN wait_on_timeout TEXT NOT NULL DEFAULT 'escalate_to_owner'
 CHECK (wait_on_timeout IN ('escalate_to_owner', 'close'));
ALTER TABLE item_operations ADD COLUMN wait_escalated_at INTEGER;

UPDATE item_operations
   SET wait_owner = 'system:' || CASE WHEN length(trim(kind)) > 0 THEN kind ELSE 'operations' END,
       wait_started_at = created,
       wait_deadline = created + 3600000,
       wait_on_timeout = 'escalate_to_owner';

-- Historical pending operations on already-closed items can never finish and are waived on upgrade.
UPDATE item_operations
   SET status = 'waived',
       required = 0,
       updated = COALESCE((SELECT i.closed_at FROM items i WHERE i.id = item_operations.item_id), updated)
 WHERE status = 'pending'
   AND EXISTS (SELECT 1 FROM items i WHERE i.id = item_operations.item_id AND i.phase = 'closed');

CREATE INDEX item_operations_wait_due ON item_operations(wait_deadline, id)
 WHERE status IN ('pending', 'running', 'unknown') AND wait_escalated_at IS NULL;

-- 4. gh_effects
ALTER TABLE gh_effects ADD COLUMN wait_owner TEXT NOT NULL DEFAULT 'system:github'
 CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0));
ALTER TABLE gh_effects ADD COLUMN wait_started_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE gh_effects ADD COLUMN wait_deadline INTEGER NOT NULL DEFAULT 0;
ALTER TABLE gh_effects ADD COLUMN wait_on_timeout TEXT NOT NULL DEFAULT 'escalate_to_owner'
 CHECK (wait_on_timeout IN ('escalate_to_owner', 'close'));
ALTER TABLE gh_effects ADD COLUMN wait_escalated_at INTEGER;

UPDATE gh_effects
   SET wait_owner = 'system:github',
       wait_started_at = created,
       wait_deadline = created + 86400000,
       wait_on_timeout = 'escalate_to_owner';

CREATE INDEX gh_effects_wait_due ON gh_effects(wait_deadline, id)
 WHERE state IN ('pending', 'running', 'unknown') AND wait_escalated_at IS NULL;

-- 5. mail_drafts
ALTER TABLE mail_drafts ADD COLUMN wait_owner TEXT NOT NULL DEFAULT 'owner'
 CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0));
ALTER TABLE mail_drafts ADD COLUMN wait_started_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mail_drafts ADD COLUMN wait_deadline INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mail_drafts ADD COLUMN wait_on_timeout TEXT NOT NULL DEFAULT 'close'
 CHECK (wait_on_timeout IN ('escalate_to_owner', 'close'));
ALTER TABLE mail_drafts ADD COLUMN wait_escalated_at INTEGER;

UPDATE mail_drafts
   SET wait_owner = CASE WHEN status = 'unknown' THEN 'system:mail' ELSE 'owner' END,
       wait_started_at = created,
       wait_deadline = created + 604800000,
       wait_on_timeout = CASE WHEN status = 'unknown' THEN 'escalate_to_owner' ELSE 'close' END;

CREATE INDEX mail_drafts_wait_due ON mail_drafts(wait_deadline, id)
 WHERE status IN ('draft', 'pending', 'unknown') AND wait_escalated_at IS NULL;

-- 6. flow_waits
ALTER TABLE flow_waits ADD COLUMN wait_owner TEXT NOT NULL DEFAULT 'system:flow'
 CHECK (wait_owner = 'owner' OR ((wait_owner LIKE 'user:%' OR wait_owner LIKE 'bot:%' OR wait_owner LIKE 'system:%') AND length(trim(substr(wait_owner, instr(wait_owner, ':') + 1))) > 0));
ALTER TABLE flow_waits ADD COLUMN wait_started_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE flow_waits ADD COLUMN wait_deadline INTEGER NOT NULL DEFAULT 0;
ALTER TABLE flow_waits ADD COLUMN wait_on_timeout TEXT NOT NULL DEFAULT 'close'
 CHECK (wait_on_timeout IN ('escalate_to_owner', 'close'));

UPDATE flow_waits
   SET wait_owner = CASE WHEN mode = 'decision' THEN 'owner' ELSE 'system:flow' END,
       wait_started_at = CASE WHEN until IS NOT NULL AND typeof(until) = 'integer' AND until < created THEN until ELSE created END,
       until = COALESCE(until, created + 604800000),
       wait_deadline = CASE WHEN until IS NOT NULL AND typeof(until) = 'integer' THEN until ELSE created + 604800000 END,
       wait_on_timeout = 'close';

CREATE INDEX flow_waits_wait_due ON flow_waits(wait_deadline, run_id, node_id);
