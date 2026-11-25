-- 127: durable thread submit idempotency, reply tasks, reply receipts,
-- origin-level dispatch & outbound delivery refs, and typed notices.
-- Retires thread_resume, thread_runs, route_log, thread_items,
-- attention_final_reply trigger, settings.noticeDismissed, and settings.thread.pending.*.

CREATE TABLE thread_submit_keys (
  workspace_id TEXT NOT NULL,
  space TEXT NOT NULL,
  actor_key TEXT NOT NULL,
  submit_key TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  message_id TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'withdrawn')),
  input_seq INTEGER NOT NULL,
  input_version INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, space, actor_key, submit_key),
  UNIQUE (workspace_id, space, input_seq)
);

CREATE TABLE thread_reply_tasks (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  space TEXT NOT NULL,
  origin_message_id TEXT NOT NULL,
  reply_revision INTEGER NOT NULL DEFAULT 0,
  origin_version INTEGER NOT NULL DEFAULT 1,
  input_seq INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('queued', 'claimed', 'settled', 'withdrawn')),
  generation INTEGER NOT NULL DEFAULT 1,
  lease_token TEXT,
  token_hash TEXT,
  cancel_requested_at INTEGER,
  cancel_reason TEXT,
  user_id TEXT,
  project_id TEXT,
  conv_json TEXT,
  to_item TEXT,
  to_reply_to TEXT,
  files_json TEXT,
  mentions_json TEXT,
  reply_to_message_id TEXT,
  forced_kind TEXT,
  route_json TEXT,
  offer_json TEXT,
  prev_receipt_id TEXT,
  read_task_intent_key TEXT,
  receipt_id TEXT,
  created INTEGER NOT NULL,
  claimed_at INTEGER,
  updated INTEGER NOT NULL,
  UNIQUE (workspace_id, space, origin_message_id, reply_revision)
);
CREATE UNIQUE INDEX thread_reply_tasks_one_claimed
  ON thread_reply_tasks(workspace_id, space) WHERE status = 'claimed';
CREATE INDEX thread_reply_tasks_head
  ON thread_reply_tasks(workspace_id, space, input_seq) WHERE status IN ('queued', 'claimed');
CREATE INDEX thread_reply_tasks_token
  ON thread_reply_tasks(token_hash) WHERE token_hash IS NOT NULL;
CREATE INDEX thread_reply_tasks_origin
  ON thread_reply_tasks(workspace_id, origin_message_id, reply_revision);

CREATE TABLE thread_reply_receipts (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL UNIQUE REFERENCES thread_reply_tasks(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  space TEXT NOT NULL,
  origin_message_id TEXT NOT NULL,
  reply_revision INTEGER NOT NULL DEFAULT 0,
  outcome TEXT NOT NULL CHECK (outcome IN ('answered', 'handed_off', 'failed', 'unknown', 'stopped')),
  reply_message_id TEXT UNIQUE,
  error_code TEXT,
  route_kind TEXT NOT NULL,
  model_kind TEXT NOT NULL,
  route_guard TEXT,
  route_model TEXT,
  route_confidence REAL,
  route_signals TEXT NOT NULL DEFAULT '[]',
  route_fallback INTEGER NOT NULL DEFAULT 0,
  route_ms INTEGER NOT NULL DEFAULT 0,
  route_override TEXT,
  route_override_at INTEGER,
  route_outcome TEXT,
  item_id TEXT,
  dispatch_ref_id TEXT,
  delivery_ref_id TEXT,
  model_call_id TEXT,
  user_id TEXT,
  project_id TEXT,
  answer_model TEXT,
  harness TEXT,
  harness_session TEXT,
  tools INTEGER NOT NULL DEFAULT 0,
  turns INTEGER NOT NULL DEFAULT 0,
  first_text_ms INTEGER,
  error_detail TEXT,
  generation INTEGER NOT NULL DEFAULT 1,
  committed_at INTEGER NOT NULL,
  UNIQUE (workspace_id, space, origin_message_id, reply_revision)
);
CREATE INDEX thread_reply_receipts_resume
  ON thread_reply_receipts(workspace_id, space, outcome, committed_at DESC);
CREATE INDEX thread_reply_receipts_ws
  ON thread_reply_receipts(workspace_id, committed_at);

CREATE TABLE thread_dispatch_refs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  origin_message_id TEXT NOT NULL,
  effect_kind TEXT NOT NULL CHECK (effect_kind IN ('create_item', 'append_item')),
  target_item_id TEXT REFERENCES items(id) ON DELETE CASCADE,
  reply_message_id TEXT,
  from_question INTEGER NOT NULL DEFAULT 0,
  follow INTEGER NOT NULL DEFAULT 1,
  last_post INTEGER,
  state TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'unknown', 'rejected')),
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE (workspace_id, origin_message_id, effect_kind)
);
CREATE UNIQUE INDEX thread_dispatch_refs_item
  ON thread_dispatch_refs(target_item_id) WHERE effect_kind = 'create_item' AND target_item_id IS NOT NULL;
CREATE INDEX thread_dispatch_refs_ws
  ON thread_dispatch_refs(workspace_id, created);

CREATE TABLE thread_forwarded (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  origin_message_id TEXT NOT NULL,
  asked TEXT NOT NULL,
  created INTEGER NOT NULL
);

CREATE TABLE thread_delivery_refs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  space TEXT NOT NULL,
  origin_message_id TEXT NOT NULL,
  purpose TEXT NOT NULL,
  outbound_op_id TEXT NOT NULL,
  outbound_message_id TEXT,
  state TEXT NOT NULL CHECK (state IN ('pending', 'sent', 'drafted', 'skipped', 'failed', 'unknown')),
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE (workspace_id, space, origin_message_id, purpose)
);
CREATE INDEX thread_delivery_refs_origin
  ON thread_delivery_refs(workspace_id, space, origin_message_id);

CREATE TABLE thread_notices (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  causal_key TEXT,
  host_message_id TEXT,
  kind TEXT NOT NULL,
  level TEXT NOT NULL,
  item_id TEXT,
  text TEXT NOT NULL,
  closed_at INTEGER,
  closed_by TEXT,
  created INTEGER NOT NULL
);
CREATE INDEX thread_notices_ws
  ON thread_notices(workspace_id, created);
CREATE INDEX thread_notices_causal
  ON thread_notices(workspace_id, causal_key, closed_at);
CREATE INDEX thread_notices_host
  ON thread_notices(host_message_id) WHERE host_message_id IS NOT NULL;

DROP TRIGGER IF EXISTS attention_final_reply;

-- Migrate historical thread_runs (+ route_log metadata) into settled tasks + receipts.
-- Non-terminal historical runs become 'unknown' receipts and are never auto-replayed.
INSERT INTO thread_reply_tasks (
  id, workspace_id, space, origin_message_id, reply_revision, origin_version, input_seq,
  status, generation, user_id, project_id, prev_receipt_id, receipt_id,
  created, claimed_at, updated
)
SELECT
  'mig_task_' || r.id,
  r.workspace_id,
  COALESCE((SELECT m.space FROM messages m WHERE m.id = r.message_id), 'steve'),
  r.message_id,
  ROW_NUMBER() OVER (
    PARTITION BY r.workspace_id, COALESCE((SELECT m.space FROM messages m WHERE m.id = r.message_id), 'steve'), r.message_id
    ORDER BY r.created, r.id
  ) - 1,
  1,
  0,
  'settled',
  1,
  r.user_id,
  r.project_id,
  r.prev_run,
  r.id,
  r.created,
  r.started,
  COALESCE(r.ended, r.started, r.created)
FROM thread_runs r;

-- Backfill reply_message_id from thread_runs.reply_id, falling back to route_log.reply_id
-- (historical answer runs recorded reply_id on route_log while leaving thread_runs.reply_id NULL).
-- Rows remain NULL only when neither table has a non-empty reply_id, or when multiple runs
-- share the same reply_id and lose the UNIQUE tie-breaker (prefer explicit thread_runs.reply_id,
-- then status = 'done', then earliest created/id).
WITH run_candidates AS (
  SELECT
    r.*,
    COALESCE((SELECT m.space FROM messages m WHERE m.id = r.message_id), 'steve') AS space,
    COALESCE(NULLIF(r.reply_id, ''), NULLIF(rl.reply_id, '')) AS candidate_reply_id,
    rl.kind AS rl_kind,
    rl.model_kind AS rl_model_kind,
    rl.guard AS rl_guard,
    rl.model AS rl_model,
    rl.confidence AS rl_confidence,
    rl.signals AS rl_signals,
    rl.fallback AS rl_fallback,
    rl.ms AS rl_ms,
    rl.override AS rl_override,
    rl.override_at AS rl_override_at,
    rl.outcome AS rl_outcome,
    rl.item_id AS rl_item_id
  FROM thread_runs r
  LEFT JOIN route_log rl ON rl.message_id = r.message_id
),
runs AS (
  SELECT
    rc.*,
    ROW_NUMBER() OVER (
      PARTITION BY rc.workspace_id, rc.space, rc.message_id
      ORDER BY rc.created, rc.id
    ) - 1 AS reply_revision,
    CASE
      WHEN rc.candidate_reply_id IS NOT NULL
       AND ROW_NUMBER() OVER (
         PARTITION BY rc.candidate_reply_id
         ORDER BY
           CASE WHEN NULLIF(rc.reply_id, '') IS NOT NULL THEN 0 ELSE 1 END,
           CASE WHEN rc.status = 'done' THEN 0 ELSE 1 END,
           rc.created,
           rc.id
       ) = 1
      THEN rc.candidate_reply_id
      ELSE NULL
    END AS safe_reply_id
  FROM run_candidates rc
)
INSERT INTO thread_reply_receipts (
  id, task_id, workspace_id, space, origin_message_id, reply_revision,
  outcome, reply_message_id, error_code,
  route_kind, model_kind, route_guard, route_model, route_confidence,
  route_signals, route_fallback, route_ms, route_override, route_override_at,
  route_outcome, item_id, user_id, project_id, answer_model, harness,
  harness_session, tools, turns, first_text_ms, error_detail, generation, committed_at
)
SELECT
  r.id,
  'mig_task_' || r.id,
  r.workspace_id,
  r.space,
  r.message_id,
  r.reply_revision,
  CASE r.status
    WHEN 'done' THEN 'answered'
    WHEN 'stopped' THEN 'stopped'
    WHEN 'failed' THEN 'failed'
    WHEN 'budget' THEN 'failed'
    WHEN 'violated' THEN 'failed'
    ELSE 'unknown'
  END,
  r.safe_reply_id,
  CASE WHEN r.status IN ('failed', 'budget', 'violated') THEN r.status ELSE NULL END,
  COALESCE(r.rl_kind, 'research'),
  COALESCE(r.rl_model_kind, 'research'),
  r.rl_guard,
  r.rl_model,
  r.rl_confidence,
  COALESCE(r.rl_signals, '[]'),
  COALESCE(r.rl_fallback, 0),
  COALESCE(r.rl_ms, 0),
  r.rl_override,
  r.rl_override_at,
  r.rl_outcome,
  r.rl_item_id,
  r.user_id,
  r.project_id,
  r.model,
  r.harness,
  COALESCE(r.session, r.claude_session),
  r.tools,
  r.turns,
  r.first_text_ms,
  r.error,
  1,
  COALESCE(r.ended, r.started, r.created)
FROM runs r;

-- Migrate historical route_log rows that had no thread_runs entry.
INSERT INTO thread_reply_tasks (
  id, workspace_id, space, origin_message_id, reply_revision, origin_version, input_seq,
  status, generation, receipt_id, created, updated
)
SELECT
  'mig_route_task_' || rl.id,
  rl.workspace_id,
  COALESCE((SELECT m.space FROM messages m WHERE m.id = rl.message_id), 'steve'),
  rl.message_id,
  0,
  1,
  0,
  'settled',
  1,
  'mig_route_' || rl.id,
  rl.created,
  rl.created
FROM route_log rl
WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id);

-- Non-run route_log rows stay NULL only when route_log.reply_id is NULL/empty or collides
-- with an already-backfilled reply_message_id on the UNIQUE constraint.
WITH route_candidates AS (
  SELECT
    rl.*,
    COALESCE((SELECT m.space FROM messages m WHERE m.id = rl.message_id), 'steve') AS space,
    NULLIF(rl.reply_id, '') AS candidate_reply_id
  FROM route_log rl
  WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id)
),
routes AS (
  SELECT
    rc.*,
    CASE
      WHEN rc.candidate_reply_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM thread_reply_receipts x WHERE x.reply_message_id = rc.candidate_reply_id)
       AND ROW_NUMBER() OVER (PARTITION BY rc.candidate_reply_id ORDER BY rc.created, rc.id) = 1
      THEN rc.candidate_reply_id
      ELSE NULL
    END AS safe_reply_id
  FROM route_candidates rc
)
INSERT INTO thread_reply_receipts (
  id, task_id, workspace_id, space, origin_message_id, reply_revision,
  outcome, reply_message_id, route_kind, model_kind, route_guard, route_model,
  route_confidence, route_signals, route_fallback, route_ms, route_override,
  route_override_at, route_outcome, item_id, generation, committed_at
)
SELECT
  'mig_route_' || rl.id,
  'mig_route_task_' || rl.id,
  rl.workspace_id,
  rl.space,
  rl.message_id,
  0,
  CASE WHEN rl.kind IN ('item', 'supplement') THEN 'handed_off' ELSE 'answered' END,
  rl.safe_reply_id,
  rl.kind,
  rl.model_kind,
  rl.guard,
  rl.model,
  rl.confidence,
  rl.signals,
  rl.fallback,
  rl.ms,
  rl.override,
  rl.override_at,
  rl.outcome,
  rl.item_id,
  1,
  rl.created
FROM routes rl;

-- Rebuild thread_reply_commits to reference thread_reply_receipts(id) while preserving
-- exact seq values and the sqlite_sequence high-water mark for thread_reply_reads.through_seq.
CREATE TABLE _mig_125_commits (
  seq INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  receipt_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL
);
INSERT INTO _mig_125_commits(seq, message_id, receipt_id, workspace_id, user_id)
SELECT c.seq, c.message_id, c.run_id, c.workspace_id, c.user_id
FROM thread_reply_commits c
WHERE EXISTS (SELECT 1 FROM thread_reply_receipts r WHERE r.id = c.run_id)
  AND EXISTS (SELECT 1 FROM messages m WHERE m.id = c.message_id);

CREATE TABLE _mig_125_seq (v INTEGER NOT NULL);
INSERT INTO _mig_125_seq(v)
SELECT COALESCE(
  (SELECT seq FROM sqlite_sequence WHERE name = 'thread_reply_commits'),
  (SELECT MAX(seq) FROM _mig_125_commits),
  0
);

DROP TABLE thread_reply_commits;
CREATE TABLE thread_reply_commits (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
  receipt_id TEXT NOT NULL REFERENCES thread_reply_receipts(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL
);
CREATE INDEX thread_reply_commits_person
  ON thread_reply_commits(workspace_id, user_id, seq);

INSERT INTO thread_reply_commits(seq, message_id, receipt_id, workspace_id, user_id)
SELECT seq, message_id, receipt_id, workspace_id, user_id FROM _mig_125_commits;

DELETE FROM sqlite_sequence WHERE name = 'thread_reply_commits';
INSERT INTO sqlite_sequence(name, seq)
SELECT 'thread_reply_commits', v FROM _mig_125_seq WHERE v > 0;

DROP TABLE _mig_125_commits;
DROP TABLE _mig_125_seq;

-- Migrate thread_items into origin-scoped thread_dispatch_refs.
WITH legacy_items AS (
  SELECT
    ti.*,
    ROW_NUMBER() OVER (PARTITION BY ti.workspace_id, ti.origin_msg ORDER BY ti.created, ti.item_id) AS rn
  FROM thread_items ti
  WHERE EXISTS (SELECT 1 FROM items i WHERE i.id = ti.item_id)
)
INSERT INTO thread_dispatch_refs (
  id, workspace_id, origin_message_id, effect_kind, target_item_id,
  reply_message_id, from_question, follow, last_post, state, created, updated
)
SELECT
  'mig_disp_' || ti.item_id,
  ti.workspace_id,
  CASE WHEN ti.rn = 1 THEN ti.origin_msg ELSE ti.origin_msg || ':legacy:' || ti.item_id END,
  'create_item',
  ti.item_id,
  ti.reply_msg,
  ti.from_question,
  ti.follow,
  ti.last_post,
  'applied',
  ti.created,
  COALESCE(ti.last_post, ti.created)
FROM legacy_items ti;

-- Backfill thread_reply_receipts.dispatch_ref_id from migrated create_item dispatch refs
-- matched deterministically by (workspace_id, origin_message_id). When legacy thread_items
-- had multiple items for the same origin (suffixed ':legacy:<item_id>' on rn > 1), prefer
-- the ref whose target_item_id matches the receipt's item_id, else the earliest ref (rn = 1).
-- Receipts stay NULL when:
--   (a) the turn never created an item (inline answer/research/ask/config/open suggest);
--   (b) the turn appended/supplemented an existing item created by an earlier origin
--       (legacy thread_items only recorded create_item dispatches, never append_item);
--   (c) the legacy thread_items row referenced a deleted item and was skipped above.
UPDATE thread_reply_receipts
SET dispatch_ref_id = (
  SELECT d.id
  FROM thread_dispatch_refs d
  WHERE d.workspace_id = thread_reply_receipts.workspace_id
    AND d.effect_kind = 'create_item'
    AND d.origin_message_id IN (
      thread_reply_receipts.origin_message_id,
      CASE
        WHEN thread_reply_receipts.item_id IS NOT NULL
        THEN thread_reply_receipts.origin_message_id || ':legacy:' || thread_reply_receipts.item_id
      END
    )
  ORDER BY
    CASE
      WHEN thread_reply_receipts.item_id IS NOT NULL
       AND d.target_item_id = thread_reply_receipts.item_id
      THEN 0
      ELSE 1
    END,
    d.created,
    d.id
  LIMIT 1
)
WHERE dispatch_ref_id IS NULL;

-- Migrate messages(space='notify') and settings.noticeDismissed into typed thread_notices.
INSERT INTO thread_notices (
  id, workspace_id, causal_key, host_message_id, kind, level, item_id, text, closed_at, created
)
SELECT
  m.id,
  m.workspace_id,
  CASE
    WHEN COALESCE(json_extract(m.meta, '$.notice.kind'), 'release') = 'release' AND m.item_id IS NOT NULL
    THEN 'release:' || m.item_id
    ELSE NULL
  END,
  (
    SELECT h.id FROM messages h
     WHERE h.workspace_id = m.workspace_id
       AND h.space = 'steve'
       AND json_valid(h.meta)
       AND json_extract(h.meta, '$.notice.ref') = m.id
     ORDER BY h.created DESC, h.rowid DESC
     LIMIT 1
  ),
  COALESCE(json_extract(m.meta, '$.notice.kind'), 'release'),
  COALESCE(json_extract(m.meta, '$.notice.level'), 'success'),
  m.item_id,
  m.text,
  CASE
    WHEN EXISTS (
      SELECT 1 FROM settings s, json_each(s.value) j
       WHERE s.workspace_id = m.workspace_id
         AND s.key = 'noticeDismissed'
         AND json_valid(s.value)
         AND json_type(s.value) = 'array'
         AND j.value = m.id
    ) THEN m.created
    WHEN COALESCE(json_extract(m.meta, '$.notice.kind'), 'release') = 'release'
     AND COALESCE(json_extract(m.meta, '$.notice.level'), 'success') = 'fail'
     AND m.item_id IS NOT NULL
     AND EXISTS (
       SELECT 1 FROM messages later
        WHERE later.workspace_id = m.workspace_id
          AND later.space = 'notify'
          AND later.item_id = m.item_id
          AND COALESCE(json_extract(later.meta, '$.notice.kind'), 'release') = 'release'
          AND (later.created > m.created OR (later.created = m.created AND later.rowid > m.rowid))
     ) THEN m.created
    ELSE NULL
  END,
  m.created
FROM messages m
WHERE m.space = 'notify';

INSERT OR IGNORE INTO thread_notices (
  id, workspace_id, causal_key, kind, level, text, closed_at, created
)
SELECT
  s.workspace_id || ':alert:' || CAST(j.value AS TEXT),
  s.workspace_id,
  CAST(j.value AS TEXT),
  'alert',
  'info',
  '',
  0,
  0
FROM settings s, json_each(s.value) j
WHERE s.key = 'noticeDismissed'
  AND json_valid(s.value)
  AND json_type(s.value) = 'array'
  AND j.type = 'text'
  AND NOT EXISTS (
    SELECT 1 FROM thread_notices n WHERE n.workspace_id = s.workspace_id AND n.id = CAST(j.value AS TEXT)
  );

DELETE FROM messages WHERE space = 'notify';
DELETE FROM settings WHERE key LIKE 'thread.pending.%' OR key = 'noticeDismissed' OR key LIKE 'thread.postback.%';

DROP TABLE IF EXISTS thread_resume;
DROP TABLE IF EXISTS thread_runs;
DROP TABLE IF EXISTS route_log;
DROP TABLE IF EXISTS thread_items;
