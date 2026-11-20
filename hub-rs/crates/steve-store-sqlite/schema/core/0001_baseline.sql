-- Generated from the frozen legacy opening flow; STEVE_BASELINE_BLESS=1 regenerates.
CREATE TABLE a2a_agents (
  id TEXT PRIMARY KEY,                          -- a2a_…
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,                           -- the key of its tool (steve_a2a_<name>), once per workspace
  card_url TEXT NOT NULL,
  card TEXT NOT NULL DEFAULT '{}',
  auth TEXT NOT NULL DEFAULT '{"scheme":"none"}',
  credential_item TEXT,
  fetched INTEGER NOT NULL,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE access_tokens (
 id TEXT PRIMARY KEY,
 user_id TEXT NOT NULL REFERENCES users(id),
 name TEXT NOT NULL,
 hash TEXT NOT NULL UNIQUE,
 created INTEGER NOT NULL,
 expires INTEGER,
 last_used INTEGER,
 revoked INTEGER
);

CREATE TABLE agent_turns (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  session_id TEXT NOT NULL, item_id TEXT, bot_id TEXT, project_id TEXT, harness TEXT NOT NULL,
  user_seq INTEGER,
  attempt INTEGER NOT NULL DEFAULT 0,
  started INTEGER NOT NULL, ended INTEGER,
  stop_reason TEXT,
  error TEXT, transient INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0,
  tool_kinds TEXT NOT NULL DEFAULT '{}',
  permissions INTEGER NOT NULL DEFAULT 0,
  backfill INTEGER NOT NULL DEFAULT 0, node_attempt_id TEXT REFERENCES runtime_attempts(attempt_id), logical_turn_id TEXT REFERENCES runtime_turns(turn_id), model TEXT);

CREATE TABLE annotations (
  id TEXT PRIMARY KEY,                          -- ann_…
  item_id TEXT NOT NULL REFERENCES items(id),
  artifact_id TEXT NOT NULL REFERENCES artifacts(id),
  path TEXT NOT NULL DEFAULT '',                -- a file inside a folder output
  anchor TEXT NOT NULL,                         -- JSON: region {x,y,w,h} | text {quote,…} | page {page} | file
  note TEXT NOT NULL,
  status TEXT NOT NULL,                         -- draft | sending | sent
  send_key TEXT,
  timeline_id INTEGER,                          -- the item's line it was sent as
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  sent INTEGER);

CREATE TABLE artifact_roles (
  artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id),
  role TEXT NOT NULL);

CREATE TABLE artifacts (id TEXT PRIMARY KEY, item_id TEXT NOT NULL REFERENCES items(id), kind TEXT NOT NULL, title TEXT NOT NULL, location TEXT, created INTEGER NOT NULL);

CREATE TABLE attention_feed_scopes (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  revision TEXT NOT NULL DEFAULT (lower(hex(randomblob(16)))),
  unread_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_id,user_id),
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);

CREATE TABLE attention_feed_unread (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  seq INTEGER NOT NULL, entry_id TEXT NOT NULL REFERENCES feed_entries(id) ON DELETE CASCADE,
  PRIMARY KEY(workspace_id,user_id,seq),
  FOREIGN KEY(workspace_id,user_id) REFERENCES attention_feed_scopes(workspace_id,user_id) ON DELETE CASCADE
);

CREATE TABLE attention_meta (id INTEGER PRIMARY KEY CHECK(id=1), epoch TEXT NOT NULL);

CREATE TABLE attention_windows (
  token TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  kind TEXT NOT NULL, revision TEXT NOT NULL, upper_seq INTEGER NOT NULL,
  position INTEGER NOT NULL, expires INTEGER NOT NULL
);

CREATE TABLE attr_defs (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    applies TEXT NOT NULL,
    key TEXT NOT NULL, label TEXT NOT NULL,
    type TEXT NOT NULL,
    options TEXT NOT NULL DEFAULT '[]', default_level TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created INTEGER NOT NULL, UNIQUE (workspace_id, key));

-- debt:comms/fk/attr_values.(party_id) -> parties.(id)
CREATE TABLE attr_values (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, party_id TEXT NOT NULL REFERENCES parties(id),
    key TEXT NOT NULL, label TEXT NOT NULL,
    value TEXT NOT NULL,
    source TEXT NOT NULL, source_label TEXT NOT NULL DEFAULT '', source_ref TEXT,
    level TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'confirmed',
    decision_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE audit (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, ts INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, target TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '{}');

CREATE TABLE auth_sessions (token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), created INTEGER NOT NULL, expires INTEGER NOT NULL);

CREATE TABLE bot_capabilities (
  bot_id TEXT NOT NULL REFERENCES bots(id),
  kind TEXT NOT NULL,                           -- mcp | skill | a2a
  entry_id TEXT NOT NULL,                       -- mcp_servers.id | skills.id | a2a_agents.id
  enabled INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL,
  PRIMARY KEY (bot_id, kind, entry_id));

CREATE TABLE bot_memories (id TEXT PRIMARY KEY, bot_id TEXT NOT NULL REFERENCES bots(id), workspace_id TEXT NOT NULL, text TEXT NOT NULL, source TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE bot_node_bindings (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
  -- Ordered. An empty configured list is closed, not legacy.
  nodes TEXT NOT NULL,
  PRIMARY KEY(workspace_id, bot_id)
);

CREATE TABLE bot_team_defaults (
  workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
  team_id TEXT NOT NULL REFERENCES bot_teams(id) ON DELETE CASCADE
);

CREATE TABLE bot_teams (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  members TEXT NOT NULL
);

CREATE TABLE bot_tool_pins (
  bot_id TEXT NOT NULL REFERENCES bots(id),
  kind TEXT NOT NULL CHECK (kind IN ('tool', 'mcp', 'a2a')),
  target TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (bot_id, kind, target));

CREATE TABLE bot_workspaces (bot_id TEXT NOT NULL REFERENCES bots(id), workspace_id TEXT NOT NULL REFERENCES workspaces(id), duty TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL, PRIMARY KEY (bot_id, workspace_id));

CREATE TABLE bots (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, role TEXT NOT NULL DEFAULT '', main INTEGER NOT NULL DEFAULT 0,
    harness TEXT NOT NULL, model TEXT, rules TEXT NOT NULL DEFAULT '', accept_mode TEXT NOT NULL DEFAULT 'manual', perm_mode TEXT NOT NULL DEFAULT 'default', project_dir TEXT, worktree INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL, persona TEXT NOT NULL DEFAULT '', proactive INTEGER NOT NULL DEFAULT 1, avatar TEXT NOT NULL DEFAULT '{}', default_project TEXT, prompt TEXT, device TEXT, deleted INTEGER NOT NULL DEFAULT 0, owner TEXT, reasoning_effort TEXT);

CREATE TABLE changesets (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    target_type TEXT NOT NULL, target_id TEXT, target_revision INTEGER,
    title TEXT NOT NULL, href TEXT NOT NULL DEFAULT '',
    changes TEXT NOT NULL DEFAULT '[]', ops TEXT NOT NULL DEFAULT '[]', before TEXT,
    status TEXT NOT NULL DEFAULT 'draft',
    origin TEXT NOT NULL,
    request TEXT NOT NULL DEFAULT '', basis TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL, created INTEGER NOT NULL, applied INTEGER, undone INTEGER);

CREATE TABLE clients (
 id TEXT PRIMARY KEY,
 user_id TEXT NOT NULL REFERENCES users(id),
 token_id TEXT REFERENCES access_tokens(id),
 secret_hash TEXT NOT NULL,
 kind TEXT NOT NULL,
 name TEXT NOT NULL,
 platform TEXT NOT NULL DEFAULT '',
 version TEXT NOT NULL DEFAULT '',
 origin TEXT,
 created INTEGER NOT NULL,
 last_seen INTEGER,
 last_route TEXT,
 last_ip TEXT,
 revoked INTEGER
);

CREATE TABLE counters (workspace_id TEXT NOT NULL, name TEXT NOT NULL, value INTEGER NOT NULL, PRIMARY KEY (workspace_id, name));

CREATE TABLE decisions (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL, subject_type TEXT NOT NULL, subject_id TEXT NOT NULL, item_id TEXT,
    session_id TEXT, question TEXT NOT NULL, draft TEXT NOT NULL DEFAULT '', options TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL, result TEXT,
    requested_by TEXT NOT NULL, created INTEGER NOT NULL, resolved INTEGER, undo TEXT);

CREATE TABLE "deliveries" (
 id TEXT PRIMARY KEY,
 url TEXT NOT NULL UNIQUE,
 workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 item_id TEXT REFERENCES items(id) ON DELETE CASCADE,
 session_id TEXT,
 owner_id TEXT,
 title TEXT NOT NULL,
 name TEXT NOT NULL,
 mime TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(size >= 0),
 created INTEGER NOT NULL,
 entrypoint TEXT,
 sha256 TEXT NOT NULL,
 manifest TEXT NOT NULL,
 preview TEXT NOT NULL,
 interactive INTEGER NOT NULL DEFAULT 0 CHECK(interactive IN (0, 1)), reply INTEGER NOT NULL DEFAULT 0 CHECK(reply IN (0, 1)));

CREATE TABLE delivery_shares (
 token TEXT PRIMARY KEY,
 delivery_id TEXT NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE,
 created_by TEXT NOT NULL REFERENCES users(id),
 created INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 revoked_at INTEGER
);

CREATE TABLE "devices" (id TEXT PRIMARY KEY, name TEXT NOT NULL, roles TEXT NOT NULL,
  os TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT '正常',
  max_sessions INTEGER NOT NULL DEFAULT 32 CHECK(max_sessions BETWEEN 1 AND 256),
  seen INTEGER NOT NULL, created INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 0);

CREATE TABLE entity_tags (
    workspace_id TEXT NOT NULL, tag_id TEXT NOT NULL REFERENCES tags(id),
    object_type TEXT NOT NULL, object_id TEXT NOT NULL, created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, object_type, object_id, tag_id));

-- debt:comms/fk/feed_entries.(notification_id) -> personal_notifications.(id)
CREATE TABLE "feed_entries" (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  audience TEXT NOT NULL CHECK(audience IN ('workspace','user')),
  recipient_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('report','completion','starred','reminder','briefing')),
  source_type TEXT NOT NULL CHECK(source_type IN ('flow','notification')),
  source_key TEXT NOT NULL,
  source_id TEXT NOT NULL,
  run_id TEXT,
  node_id TEXT,
  item_id TEXT, -- source reference only, never a copied item title or completion body
  notification_id TEXT UNIQUE REFERENCES personal_notifications(id) ON DELETE SET NULL,
  content TEXT,
  result TEXT,
  published_at INTEGER NOT NULL,
  CHECK((audience='workspace' AND recipient_id IS NULL) OR (audience='user' AND recipient_id IS NOT NULL)),
  CHECK((kind='report' AND source_type='flow' AND audience='workspace' AND notification_id IS NULL AND content IS NOT NULL AND result IS NOT NULL AND run_id IS NOT NULL AND node_id IS NOT NULL)
     OR (kind IN ('completion','starred','reminder','briefing') AND source_type='notification' AND audience='user' AND content IS NULL AND result IS NULL)),
  UNIQUE(workspace_id,source_type,source_key)
);

CREATE TABLE feed_reads (
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  entry_id TEXT NOT NULL REFERENCES feed_entries(id) ON DELETE CASCADE,
  read_at INTEGER NOT NULL,
  PRIMARY KEY(workspace_id,user_id,entry_id),
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);

CREATE TABLE feed_windows (
  token TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  kind TEXT,
  source TEXT,
  unread INTEGER NOT NULL,
  upper_seq INTEGER NOT NULL,
  before_seq INTEGER,
  expires INTEGER NOT NULL,
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);

CREATE TABLE files (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, sha256 TEXT NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL, created_by TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE flow_run_steps (
  run_id TEXT NOT NULL REFERENCES flow_runs(id), seq INTEGER NOT NULL, workspace_id TEXT NOT NULL,
  node_id TEXT NOT NULL, status TEXT NOT NULL, -- ok|skipped|waiting|failed
  branch TEXT, input TEXT, output TEXT, ms REAL NOT NULL DEFAULT 0, error TEXT, started INTEGER NOT NULL,
  PRIMARY KEY (run_id, seq));

CREATE TABLE flow_runs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, flow_id TEXT NOT NULL, flow_version INTEGER NOT NULL,
  trigger_kind TEXT NOT NULL, event_id TEXT, dry_run INTEGER NOT NULL DEFAULT 0, parent_run TEXT, depth INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,                        -- running|waiting|done|failed|cancelled
  outcome TEXT, outcome_label TEXT NOT NULL DEFAULT '',
  cursor TEXT,                                 -- 下一个要执行的节点 id（恢复用）
  context TEXT NOT NULL DEFAULT '{}',          -- {event, steps:{<id>:output}}，敏感值打码后才写
  error TEXT, retry_at INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
  work_item_id TEXT, cost REAL NOT NULL DEFAULT 0,
  started INTEGER NOT NULL, ended INTEGER);

CREATE TABLE flow_triggers (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, flow_id TEXT NOT NULL REFERENCES flows(id), node_id TEXT NOT NULL,
  kind TEXT NOT NULL,                          -- event|message|time|item|manual
  source TEXT, types TEXT NOT NULL DEFAULT '[]', connections TEXT NOT NULL DEFAULT '[]', scopes TEXT NOT NULL DEFAULT '[]', spaces TEXT NOT NULL DEFAULT '[]',
  cron TEXT, tz TEXT, next_at INTEGER, missed TEXT,          -- missed: catch_up_once|skip
  enabled INTEGER NOT NULL);

CREATE TABLE flow_versions (
  flow_id TEXT NOT NULL REFERENCES flows(id), workspace_id TEXT NOT NULL, version INTEGER NOT NULL,
  graph TEXT NOT NULL, author TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL,
  PRIMARY KEY (flow_id, version));

CREATE TABLE flow_waits (
  run_id TEXT NOT NULL REFERENCES flow_runs(id), node_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- delay|until|event|decision
  until INTEGER, match TEXT, decision_id TEXT, created INTEGER NOT NULL,
  PRIMARY KEY (run_id, node_id));

CREATE TABLE flows (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  kind TEXT NOT NULL DEFAULT 'flow',           -- flow|schedule|response（渠道响应规则转成的流程）
  name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 0,
  graph TEXT NOT NULL,                         -- 当前版本的图（§3.1）
  version INTEGER NOT NULL DEFAULT 1,
  origin TEXT NOT NULL,                        -- me|steve|plugin:<id>|template:<id>|correction
  origin_label TEXT NOT NULL DEFAULT '',       -- “你画的”“Steve 起草，你确认”“GitHub 插件模板”
  schedule TEXT,                               -- kind=schedule 的表单字段（§5）
  deleted INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE gateway_attempts (
 attempt_id TEXT PRIMARY KEY, call_id TEXT NOT NULL REFERENCES gateway_calls(call_id),
 ordinal INTEGER NOT NULL, previous_attempt TEXT REFERENCES gateway_attempts(attempt_id),
 harness TEXT NOT NULL, model TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER,
 ok INTEGER, error TEXT, UNIQUE(call_id, ordinal));

CREATE TABLE gateway_calls (
 call_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT, bot_id TEXT, project_id TEXT,
 purpose TEXT NOT NULL, started INTEGER NOT NULL, day TEXT NOT NULL,
 ended INTEGER, ok INTEGER NOT NULL DEFAULT 0, model TEXT NOT NULL DEFAULT '',
 harness TEXT NOT NULL DEFAULT '', attempts INTEGER NOT NULL DEFAULT 0,
 duration_ms INTEGER NOT NULL DEFAULT 0, failures TEXT NOT NULL DEFAULT '[]',
 fallback_usage TEXT NOT NULL DEFAULT '[]', projection_dirty INTEGER NOT NULL DEFAULT 1);

CREATE TABLE gateway_job_accounts (
 job_attempt_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id),
 finish_on_end INTEGER NOT NULL DEFAULT 0 CHECK(finish_on_end IN (0,1)), node_id TEXT, job_id TEXT);

CREATE TABLE gateway_local_inbox (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id), report_id TEXT NOT NULL,
 observed_at INTEGER NOT NULL, payload TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(attempt_id,report_id));

CREATE TABLE gateway_local_ready (
 attempt_id TEXT PRIMARY KEY REFERENCES gateway_attempts(attempt_id), tried INTEGER NOT NULL DEFAULT 0);

CREATE TABLE gateway_result_ready (
 ticket INTEGER PRIMARY KEY AUTOINCREMENT,
 call_id TEXT NOT NULL UNIQUE REFERENCES gateway_calls(call_id) ON DELETE CASCADE);

CREATE TABLE gateway_usage_meters (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id),
 meter_epoch TEXT NOT NULL, model TEXT NOT NULL, scope TEXT NOT NULL, payload TEXT NOT NULL,
 PRIMARY KEY(attempt_id, meter_epoch, model, scope));

CREATE TABLE gateway_usage_reports (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id), report_id TEXT NOT NULL,
 source_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(attempt_id, report_id));

CREATE TABLE grants (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  subject_type TEXT NOT NULL, subject_id TEXT NOT NULL,             -- bot|persona|plugin|device
  capability TEXT NOT NULL,                    -- secret.use（其他模块以后复用：http.effect、cmd.run……）
  resource_type TEXT NOT NULL, resource_id TEXT NOT NULL,           -- vault_item|host|project
  scope TEXT NOT NULL DEFAULT '{}',            -- {"project_id":"prj_…"} 或 {"site":"github.com"}
  until INTEGER NOT NULL, created_by TEXT NOT NULL, decision_id TEXT,
  revoked INTEGER, created INTEGER NOT NULL);

CREATE TABLE item_cycles (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 revision INTEGER NOT NULL,
 outcome TEXT NOT NULL,
 acceptance TEXT,
 reason TEXT,
 closed INTEGER NOT NULL,
 source TEXT NOT NULL DEFAULT 'live',
 PRIMARY KEY (item_id, cycle));

CREATE TABLE item_delivery_facts (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT,
  turn_id TEXT,
  kind TEXT NOT NULL CHECK(kind IN ('commit','tree','pr','ci','build','deploy','review','approval')),
  -- the commit the fact is about (a tree, a PR, a check, a review or an approval of that head)
  head TEXT,
  -- what it names: a tree hash, a PR number, a check, a build, a version, a reviewer or approver
  ref TEXT,
  state TEXT NOT NULL,
  provenance TEXT NOT NULL CHECK(provenance IN ('verified','reported','unknown')),
  source TEXT NOT NULL,
  observed_at INTEGER NOT NULL
);

CREATE TABLE item_dependency_waits (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 session_id TEXT NOT NULL REFERENCES sessions(id),
 resume TEXT NOT NULL CHECK(resume IN ('automatic','notify','workspace')),
 reason TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','started','notified')),
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL,
 PRIMARY KEY(item_id,cycle)
);

CREATE TABLE item_deps (item_id TEXT NOT NULL REFERENCES items(id), dep_id TEXT NOT NULL REFERENCES items(id), created INTEGER NOT NULL, PRIMARY KEY (item_id, dep_id));

CREATE TABLE item_dispatch (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  -- A snapshot of the Team at creation. Current Bot bindings and workspace/Node grants still apply.
  team TEXT,
  source TEXT CHECK(source IN ('default', 'explicit', 'inherited')),
  last_decision TEXT
);

CREATE TABLE item_goals (item_id TEXT NOT NULL REFERENCES items(id), version INTEGER NOT NULL, text TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (item_id, version));

CREATE TABLE item_judge_candidates (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  scope_revision INTEGER NOT NULL,
  terminal_attempt TEXT,
  registered_by TEXT NOT NULL CHECK(registered_by IN ('turn_end','sweep')),
  state TEXT NOT NULL DEFAULT 'pending'
    CHECK(state IN ('pending','judging','judged','superseded','failed')),
  settled TEXT CHECK(settled IS NULL OR settled IN ('evaluable','busy','unknown')),
  settled_reason TEXT,
  claim TEXT,
  claimed_at INTEGER,
  input_seq INTEGER,
  fingerprint TEXT,
  failures INTEGER NOT NULL DEFAULT 0 CHECK(failures>=0),
  last_error TEXT,
  next_check INTEGER NOT NULL,
  judgement_id TEXT,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision)
);

CREATE TABLE item_judgements (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  scope_revision INTEGER NOT NULL DEFAULT 0,
  verdict TEXT NOT NULL CHECK(verdict IN ('done','not_done','waiting','needs_you','unknown')),
  basis TEXT NOT NULL DEFAULT '{}',
  harness TEXT,
  model TEXT,
  created INTEGER NOT NULL, candidate_id TEXT, decided_by TEXT, would TEXT, independent TEXT, fingerprint TEXT,
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision)
);

CREATE TABLE item_marks (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  from_v TEXT, to_v TEXT,
  actor TEXT NOT NULL,
  ref TEXT,
  detail TEXT NOT NULL DEFAULT '{}', backfill INTEGER NOT NULL DEFAULT 0);

CREATE TABLE item_node_tags (
 item_id TEXT PRIMARY KEY,
 tag TEXT NOT NULL);

CREATE TABLE item_operations (
 id TEXT PRIMARY KEY,
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 revision INTEGER NOT NULL,
 kind TEXT NOT NULL,
 effect_id TEXT,
 required INTEGER NOT NULL DEFAULT 0,
 status TEXT NOT NULL CHECK(status IN ('pending','running','succeeded','failed','unknown','waived')),
 label TEXT NOT NULL DEFAULT '',
 evidence TEXT NOT NULL DEFAULT '{}',
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);

CREATE TABLE item_stats (
  item_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  created INTEGER NOT NULL, first_turn INTEGER, first_deliver INTEGER, last_deliver INTEGER,
  accepted INTEGER, done_at INTEGER, dropped_at INTEGER,
  lead_ms INTEGER, cycle_ms INTEGER, wait_ms INTEGER, agent_ms INTEGER,
  turns INTEGER NOT NULL DEFAULT 0, delivers INTEGER NOT NULL DEFAULT 0, reworks INTEGER NOT NULL DEFAULT 0,
  first_pass INTEGER,
  fails INTEGER NOT NULL DEFAULT 0, stalls INTEGER NOT NULL DEFAULT 0,
  releases INTEGER NOT NULL DEFAULT 0, release_fails INTEGER NOT NULL DEFAULT 0,
  decisions INTEGER NOT NULL DEFAULT 0, yolo_decided INTEGER NOT NULL DEFAULT 0,
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, models TEXT NOT NULL DEFAULT '{}',
  dirty INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE item_tags (item_id TEXT NOT NULL REFERENCES items(id), tag_id TEXT NOT NULL REFERENCES tags(id), PRIMARY KEY (item_id, tag_id));

CREATE TABLE item_waits (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT,
  waits_on TEXT NOT NULL CHECK(waits_on IN
    ('you','external','item','transient','quota','pr','ci','build','deploy','time')),
  what TEXT NOT NULL,
  ref TEXT,
  next_check INTEGER NOT NULL,
  fallback_at INTEGER,
  wakes INTEGER NOT NULL DEFAULT 0 CHECK(wakes>=0),
  cap INTEGER CHECK(cap IS NULL OR cap>=0),
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  resolved_at INTEGER,
  resolution TEXT
);

CREATE TABLE item_would_do (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  -- '' when the item has no session or its session no logical turn on a Node
  session_id TEXT NOT NULL DEFAULT '',
  turn_id TEXT NOT NULL DEFAULT '',
  -- the item's scope revision (the version of its goal; 0: none) it was decided on
  scope_revision INTEGER NOT NULL DEFAULT 0,
  -- what the decision is about when its reason names one thing: an operation, a card or a wait id
  -- ('' when it is about the item as a whole)
  target TEXT NOT NULL DEFAULT '',
  action TEXT NOT NULL CHECK(action IN ('wake','card','remind')),
  reason TEXT NOT NULL,
  detail TEXT,
  -- the derived state's row and code it was decided on
  row INTEGER NOT NULL,
  code TEXT NOT NULL,
  -- when it came due (the policy's clock), and when the sweep first saw it due
  due_at INTEGER NOT NULL,
  first_seen INTEGER NOT NULL,
  cleared_at INTEGER,
  episodes INTEGER NOT NULL DEFAULT 1 CHECK(episodes>=1),
  -- 0: only observed (its switch was off); a later release sets it when it carries the action out
  executed INTEGER NOT NULL DEFAULT 0 CHECK(executed IN (0,1)),
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision,action,reason,target)
);

CREATE TABLE items (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, no INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'task', title TEXT NOT NULL,
    status TEXT NOT NULL, stage TEXT NOT NULL, bot_id TEXT REFERENCES bots(id), project_dir TEXT, cwd TEXT, branch TEXT,
    plan TEXT NOT NULL DEFAULT '[]', now TEXT NOT NULL DEFAULT '', accept_mode TEXT, priority TEXT NOT NULL DEFAULT '中',
    archived INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL, project_id TEXT REFERENCES projects(id), star INTEGER NOT NULL DEFAULT 0, due TEXT, snooze TEXT, source TEXT NOT NULL DEFAULT '["对话","chat"]', parent TEXT, requested_by TEXT, design_gate INTEGER NOT NULL DEFAULT 0, phase TEXT NOT NULL DEFAULT 'active', cycle INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 0, outcome TEXT, acceptance TEXT, closing TEXT, closed_at INTEGER, closed_reason TEXT, UNIQUE (workspace_id, no));

CREATE TABLE legacy_spread_nodes (
  node_id TEXT PRIMARY KEY REFERENCES nodes(node_id) ON DELETE CASCADE
);

CREATE TABLE links (workspace_id TEXT NOT NULL, subject_type TEXT NOT NULL, subject_id TEXT NOT NULL, predicate TEXT NOT NULL, object_type TEXT NOT NULL, object_id TEXT NOT NULL,
    props TEXT NOT NULL DEFAULT '{}', source TEXT NOT NULL, confidence REAL NOT NULL DEFAULT 1, created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, subject_type, subject_id, predicate, object_type, object_id));

CREATE TABLE mcp_servers (
  id TEXT PRIMARY KEY,                          -- bmcp_…
  workspace_id TEXT NOT NULL,                   -- its library; its credential item is in this vault
  name TEXT NOT NULL,                           -- the MCP server name the harness sees, once per workspace
  transport TEXT NOT NULL,                      -- stdio | http | sse
  command TEXT,                                 -- stdio
  args TEXT NOT NULL DEFAULT '[]',              -- stdio, JSON list of strings
  env TEXT NOT NULL DEFAULT '[]',               -- stdio: [{"name","value"} | {"name","secret":true}]
  url TEXT,                                     -- http | sse
  headers TEXT NOT NULL DEFAULT '[]',           -- http | sse, like env
  credential_item TEXT,                         -- vault_items.id holding the secret values, if any
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE mem_about (
    mem_id TEXT NOT NULL REFERENCES mems(id), workspace_id TEXT NOT NULL,
    object_type TEXT NOT NULL, object_id TEXT NOT NULL,
    PRIMARY KEY (mem_id, object_type, object_id));

CREATE TABLE mem_candidates (
  mem_id TEXT PRIMARY KEY REFERENCES mems(id), workspace_id TEXT NOT NULL,
  origin TEXT NOT NULL,                        -- item|chat|mail|tool|space
  origin_id TEXT NOT NULL, origin_label TEXT NOT NULL,          -- “ST-12 交付时”“10-31 的对话”
  evidence TEXT NOT NULL DEFAULT '',           -- 原话摘录（≤300 字）
  supersedes TEXT,                             -- 与之冲突的已确认记忆；确认后它变 stale
  created INTEGER NOT NULL);

CREATE TABLE mem_changes (
  id TEXT PRIMARY KEY,                          -- mch_…
  workspace_id TEXT NOT NULL,
  run_id TEXT NOT NULL,                         -- the item (or session) of the agent run that made it
  writer TEXT NOT NULL,                         -- the bot
  op TEXT NOT NULL,                             -- merge|resolve|flag|stale|about|source
  mem_id TEXT NOT NULL,                         -- the memory changed; merge: the new candidate
  targets TEXT NOT NULL DEFAULT '[]',           -- the other memories involved (merged, retired, in conflict)
  before TEXT NOT NULL DEFAULT '{}',            -- what undo puts back
  after TEXT NOT NULL DEFAULT '{}',
  reason TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'applied',        -- applied|undone; flag: open|kept|dismissed; merge follows its candidate
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE mem_ranges (
  mem_id TEXT NOT NULL REFERENCES mems(id) ON DELETE CASCADE, workspace_id TEXT NOT NULL,
  conv TEXT NOT NULL, first_id TEXT NOT NULL, last_id TEXT NOT NULL,
  first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, packet_id TEXT,
  PRIMARY KEY (mem_id, conv, first_id));

CREATE TABLE mem_supersedes (
  mem_id TEXT NOT NULL,                         -- the candidate
  old_id TEXT NOT NULL,                         -- a confirmed memory it replaces once confirmed
  PRIMARY KEY (mem_id, old_id));

CREATE TABLE mems (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source_id TEXT, source_label TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL DEFAULT 'private',
    level TEXT NOT NULL DEFAULT 'conclusion',
    status TEXT NOT NULL DEFAULT 'confirmed',
    valid_until INTEGER, writer TEXT NOT NULL,
    decision_id TEXT, created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE messages (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, space TEXT NOT NULL, author TEXT NOT NULL, text TEXT NOT NULL, item_id TEXT, created INTEGER NOT NULL, meta TEXT);

CREATE TABLE node_audit (
 at INTEGER NOT NULL,
 node_id TEXT,
 actor TEXT,
 what TEXT NOT NULL,
 safe_detail TEXT);

CREATE TABLE node_commands (
 operation_id TEXT PRIMARY KEY, node_id TEXT NOT NULL, method TEXT NOT NULL,
 request TEXT NOT NULL, digest TEXT NOT NULL,
 target_attempt TEXT REFERENCES runtime_attempts(attempt_id),
 dependency_operation TEXT REFERENCES node_commands(operation_id),
 state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','active','unknown','finished','rejected')),
 receipt TEXT, revision INTEGER NOT NULL DEFAULT 0,
 retry_at INTEGER NOT NULL DEFAULT 0, lease_owner TEXT, lease_until INTEGER,
 lease_generation INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL, maybe_sent INTEGER NOT NULL DEFAULT 0 CHECK(maybe_sent IN (0,1)), target_job_attempt TEXT REFERENCES runtime_job_attempts(attempt_id)
 CHECK(target_job_attempt IS NULL OR target_attempt IS NULL));

CREATE TABLE node_env (
 node_id TEXT NOT NULL,
 principal TEXT NOT NULL,
 key TEXT NOT NULL,
 value TEXT,
 vault_ref TEXT,
 note TEXT NOT NULL DEFAULT '',
 updated_by TEXT NOT NULL,
 updated INTEGER NOT NULL,
 CHECK((value IS NULL) <> (vault_ref IS NULL)),
 PRIMARY KEY(node_id, principal, key));

CREATE TABLE node_grants (
 id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL REFERENCES nodes(node_id),
 grantee_kind TEXT NOT NULL CHECK(grantee_kind IN ('user','workspace','team')),
 grantee TEXT NOT NULL,
 roots TEXT NOT NULL DEFAULT '[]',
 bots TEXT NOT NULL DEFAULT '"*"',
 max_concurrent INTEGER CHECK(max_concurrent IS NULL OR max_concurrent > 0),
 expires INTEGER,
 created_by TEXT NOT NULL,
 created INTEGER NOT NULL,
 accepted INTEGER,
 revoked INTEGER,
 UNIQUE(node_id, grantee_kind, grantee));

CREATE TABLE node_pairings (
 code_hash TEXT PRIMARY KEY,
 created_by TEXT NOT NULL,
 created INTEGER NOT NULL,
 expires INTEGER NOT NULL,
 claimed_at INTEGER,
 node_id TEXT, confirmed INTEGER NOT NULL DEFAULT 0);

CREATE TABLE node_removals (
 operation_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 requested_by TEXT NOT NULL,
 grace_secs INTEGER NOT NULL,
 phase TEXT NOT NULL CHECK(phase IN ('requested','draining','cancelling','ready','confirming','removed','unconfirmed','failed','manual')),
 error TEXT,
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);

CREATE TABLE node_roots (
 root_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 path TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('ro','rw')),
 owner_root INTEGER NOT NULL DEFAULT 0 CHECK(owner_root IN (0,1)),
 created INTEGER NOT NULL,
 UNIQUE(node_id, path));

CREATE TABLE node_streams (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL,
 durable INTEGER NOT NULL DEFAULT 0 CHECK(durable>=0),
 applied INTEGER NOT NULL DEFAULT 0 CHECK(applied>=0 AND applied<=durable), fault TEXT CHECK(fault IS NULL OR fault IN ('conflict','gap','ahead')),
 PRIMARY KEY(node_id,stream,epoch));

CREATE TABLE node_tags (
 node_id TEXT NOT NULL,
 tag TEXT NOT NULL,
 PRIMARY KEY (node_id, tag));

CREATE TABLE node_upgrades (
 operation_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 version TEXT NOT NULL,
 commit_sha TEXT NOT NULL,
 target TEXT NOT NULL,
 from_version TEXT,
 artifact TEXT NOT NULL,
 file TEXT NOT NULL DEFAULT '',
 sha256 TEXT NOT NULL DEFAULT '',
 size INTEGER NOT NULL DEFAULT 0,
 requested_by TEXT NOT NULL,
 force INTEGER NOT NULL DEFAULT 0 CHECK(force IN (0,1)),
 phase TEXT NOT NULL,
 error TEXT,
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);

CREATE TABLE nodes (
 node_id TEXT PRIMARY KEY,
 owner TEXT NOT NULL,
 name TEXT NOT NULL,
 pubkey BLOB NOT NULL CHECK(length(pubkey)=32),
 fingerprint TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('pending','active','paused','revoked')),
 generation INTEGER NOT NULL DEFAULT 0,
 incarnation INTEGER,
 local INTEGER NOT NULL DEFAULT 0 CHECK(local IN (0,1)),
 capabilities TEXT,
 created INTEGER NOT NULL,
 last_seen INTEGER,
 revoked_at INTEGER,
 revoked_by TEXT, max_sessions INTEGER NOT NULL DEFAULT 32 CHECK(max_sessions BETWEEN 1 AND 256), owner_context INTEGER CHECK(owner_context IN (0,1)), upgrade_policy TEXT
  CHECK(upgrade_policy IS NULL OR upgrade_policy IN ('auto','notify','off')), host TEXT, link TEXT);

CREATE TABLE notification_policies (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  mode TEXT NOT NULL CHECK(mode IN ('now','digest','log')),
  party_id TEXT,
  space_id TEXT,
  timezone TEXT NOT NULL,
  quiet_start INTEGER,
  quiet_end INTEGER,
  consented_at INTEGER NOT NULL,
  consent_since_seq INTEGER NOT NULL,
  updated INTEGER NOT NULL, starred_in_quiet INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_id,user_id),
  CHECK((quiet_start IS NULL AND quiet_end IS NULL) OR
        (quiet_start BETWEEN 0 AND 1439 AND quiet_end BETWEEN 0 AND 1439 AND quiet_start <> quiet_end))
);

CREATE TABLE outbox_consumers (
  id TEXT PRIMARY KEY,                 -- automation.item_trigger, work.tidy
  topic TEXT NOT NULL,                 -- the one topic it subscribes to
  since_seq INTEGER NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  registered INTEGER NOT NULL);

CREATE TABLE outbox_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,             -- evt_… (stable; consumers key their own idempotency on it)
  workspace_id TEXT NOT NULL,
  topic TEXT NOT NULL,                 -- work.item.done
  version INTEGER NOT NULL,            -- payload schema version of the topic
  payload TEXT NOT NULL,               -- JSON
  created INTEGER NOT NULL);

CREATE TABLE outbox_leases (
  event_seq INTEGER NOT NULL REFERENCES outbox_events(seq),
  consumer_id TEXT NOT NULL,
  owner TEXT,                          -- the dispatcher instance (process start id) holding the lease
  lease_until INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  next_at INTEGER NOT NULL DEFAULT 0,
  dead INTEGER NOT NULL DEFAULT 0,
  updated INTEGER NOT NULL, failing_since INTEGER,
  PRIMARY KEY (event_seq, consumer_id));

CREATE TABLE outbox_receipts (
  event_seq INTEGER NOT NULL REFERENCES outbox_events(seq),
  consumer_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  done INTEGER NOT NULL,
  outcome TEXT,                        -- what the delivery did (没有监听, evt_…), also when it changed nothing
  PRIMARY KEY (event_seq, consumer_id));

CREATE TABLE passkeys (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, user_id TEXT NOT NULL REFERENCES users(id),
  name TEXT NOT NULL, rp_id TEXT NOT NULL, credential TEXT NOT NULL,   -- webauthn-rs 序列化的 Passkey（公钥，不是机密）
  created INTEGER NOT NULL, last_used INTEGER);

CREATE TABLE price_catalog (
  provider TEXT NOT NULL,                       -- Models.dev provider id (`anthropic`, `openai`, …)
  model TEXT NOT NULL,                          -- the model id under it, lower-cased
  since INTEGER NOT NULL,
  input REAL NOT NULL, output REAL NOT NULL,    -- USD per million tokens
  cache_read REAL, cache_write REAL,            -- NULL = not listed
  tiered INTEGER NOT NULL DEFAULT 0,            -- also lists long-context or tier prices (not applied)
  name TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (provider, model, since));

CREATE TABLE price_sync (
  source TEXT PRIMARY KEY,
  synced INTEGER, tried INTEGER, error TEXT,
  etag TEXT,
  models INTEGER NOT NULL DEFAULT 0);

CREATE TABLE projects (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
    device_id TEXT NOT NULL DEFAULT 'hub', path TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'dir', worktree INTEGER NOT NULL DEFAULT 0, default_bot TEXT,
    release TEXT, archived INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL, UNIQUE (workspace_id, name));

CREATE TABLE queued (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id), text TEXT NOT NULL, via TEXT NOT NULL, created INTEGER NOT NULL, files TEXT);

CREATE TABLE revisions (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, object_type TEXT NOT NULL, object_id TEXT NOT NULL, version INTEGER NOT NULL, patch TEXT NOT NULL, author TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE route_log (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  reply_id TEXT,
  model TEXT, model_kind TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence REAL, signals TEXT NOT NULL DEFAULT '[]', guard TEXT,
  fallback INTEGER NOT NULL DEFAULT 0, item_id TEXT, ms INTEGER NOT NULL,
  override TEXT,
  override_at INTEGER, outcome TEXT,
  created INTEGER NOT NULL);

CREATE TABLE runtime_attempts (
 attempt_id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES runtime_turns(turn_id),
 ordinal INTEGER NOT NULL CHECK(ordinal>=0), node_id TEXT NOT NULL,
 operation_id TEXT NOT NULL UNIQUE, execution TEXT NOT NULL, spec TEXT NOT NULL, digest TEXT NOT NULL,
 state TEXT NOT NULL, receipt TEXT, completion TEXT, cancel_requested INTEGER NOT NULL DEFAULT 0,
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(turn_id,ordinal));

CREATE TABLE runtime_job_attempts (
 attempt_id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES runtime_jobs(job_id),
 ordinal INTEGER NOT NULL CHECK(ordinal>=0), model TEXT NOT NULL, node_id TEXT NOT NULL,
 operation_id TEXT NOT NULL UNIQUE, execution TEXT NOT NULL, spec TEXT NOT NULL, digest TEXT NOT NULL,
 state TEXT NOT NULL DEFAULT 'planned' CHECK(state IN ('planned','accepted','running','unknown','finished')),
 receipt TEXT, completion TEXT,
 cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(job_id,ordinal));

CREATE TABLE runtime_jobs (
 job_id TEXT PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('think','research')),
 purpose TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 node_id TEXT NOT NULL,
 thread_run_id TEXT,
 state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','finished')),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE runtime_permissions (
 decision_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 session_id TEXT NOT NULL REFERENCES runtime_sessions(session_id),
 attempt_id TEXT NOT NULL REFERENCES runtime_attempts(attempt_id),
 permission_id TEXT NOT NULL,
 request TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','answering','applied','expired','refused')),
 answer_operation TEXT REFERENCES node_commands(operation_id),
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(node_id, attempt_id, permission_id));

CREATE TABLE runtime_sessions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id),
 backend TEXT NOT NULL DEFAULT 'node' CHECK(backend='node'),
 node_id TEXT NOT NULL, workspace_ref TEXT NOT NULL, node_session_id TEXT NOT NULL,
 config_revision TEXT NOT NULL, phase TEXT NOT NULL,
 active_attempt TEXT, release_pending INTEGER NOT NULL DEFAULT 0 CHECK(release_pending IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL, desired_config TEXT, node_revision TEXT, principal TEXT, audience TEXT, state_dir TEXT, view_digest TEXT,
 UNIQUE(node_id,node_session_id));

CREATE TABLE runtime_turns (
 turn_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES runtime_sessions(session_id),
 state TEXT NOT NULL, active_attempt TEXT, retry_ordinal INTEGER NOT NULL DEFAULT 0,
 revision INTEGER NOT NULL DEFAULT 0, applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE secret_uses (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL,
  actor_type TEXT NOT NULL, actor_id TEXT NOT NULL,                 -- bot|me|steve|flow
  session_id TEXT, work_item_id TEXT, run_id TEXT,
  purpose TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- env|file|stdin|internal|view|copy|browser_fill
  command TEXT,                                -- 命令原文（不含机密；含机密的部分打码）
  fields TEXT NOT NULL DEFAULT '[]',           -- 用到的字段 label
  decision_id TEXT, grant_id TEXT, device_id TEXT NOT NULL DEFAULT 'hub',
  status TEXT NOT NULL,                        -- pending|denied|running|ok|failed|expired
  exit_code INTEGER, created INTEGER NOT NULL, finished INTEGER);

CREATE TABLE session_compactions (
  session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  upto INTEGER NOT NULL,
  events INTEGER NOT NULL,
  at INTEGER NOT NULL);

CREATE TABLE session_dispatch (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 text TEXT NOT NULL, via TEXT NOT NULL, created INTEGER NOT NULL, files TEXT,
 state TEXT NOT NULL CHECK(state IN ('claimed','submitted','completed','interrupted','cancelled','error')),
 updated INTEGER NOT NULL, turn_id TEXT REFERENCES runtime_turns(turn_id), backend TEXT NOT NULL DEFAULT 'legacy' CHECK(backend IN ('legacy','node')));

CREATE TABLE session_resume (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 created INTEGER NOT NULL);

CREATE TABLE session_stops (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 attempt TEXT,
 stop_reason TEXT,
 error TEXT,
 yours INTEGER NOT NULL DEFAULT 0 CHECK(yours IN (0,1)),
 stop_asked TEXT,
 asked_seq INTEGER,
 ended INTEGER,
 seq INTEGER,
 updated INTEGER NOT NULL);

CREATE TABLE sessions (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT REFERENCES items(id), no INTEGER NOT NULL DEFAULT 1, bot_id TEXT, harness TEXT NOT NULL, model TEXT,
    cwd TEXT NOT NULL, acp_session_id TEXT, status TEXT NOT NULL, used INTEGER, size INTEGER, cost REAL, token_hash TEXT, title TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL, updated INTEGER NOT NULL, device_id TEXT NOT NULL DEFAULT 'hub', source TEXT NOT NULL DEFAULT 'Steve', readonly INTEGER NOT NULL DEFAULT 0, import_key TEXT, first_tool_at INTEGER, reasoning_effort TEXT);

CREATE TABLE settings (workspace_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (workspace_id, key));

CREATE TABLE signal_corrections (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, event_id TEXT NOT NULL,
  scope TEXT NOT NULL,                         -- one|group
  action TEXT NOT NULL,                        -- remind|ignore|to_item
  flow_id TEXT, changeset_id TEXT, mem_id TEXT, work_item_id TEXT,
  created_by TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE skills (
  id TEXT PRIMARY KEY,                          -- bskill_…
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,                           -- the skill's directory and front matter name
  description TEXT NOT NULL,                    -- when to use it
  content TEXT NOT NULL DEFAULT '',             -- the SKILL.md body (Markdown)
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE stat_daily (
  workspace_id TEXT NOT NULL, day TEXT NOT NULL,
  source TEXT NOT NULL, purpose TEXT NOT NULL, harness TEXT NOT NULL, model TEXT NOT NULL,
  bot_id TEXT NOT NULL DEFAULT '', project_id TEXT NOT NULL DEFAULT '',
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0, reasoning INTEGER NOT NULL DEFAULT 0,
  hit_base INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, calls INTEGER NOT NULL DEFAULT 0, failed_calls INTEGER NOT NULL DEFAULT 0,
  turns INTEGER NOT NULL DEFAULT 0, turn_ms INTEGER NOT NULL DEFAULT 0, turn_errors INTEGER NOT NULL DEFAULT 0,
  retries INTEGER NOT NULL DEFAULT 0, interrupted INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0, tool_kinds TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (workspace_id, day, source, purpose, harness, model, bot_id, project_id));

-- debt:comms/fk/stewards.(person_id) -> parties.(id)
CREATE TABLE stewards (
  workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL,      -- account|connection|device|bot|persona|party|space
  object_id TEXT NOT NULL,
  person_id TEXT NOT NULL REFERENCES parties(id),
  role TEXT NOT NULL DEFAULT 'owner',   -- owner(负责人)|maintainer(维护人)|user(使用人)
  source TEXT NOT NULL,           -- manual|connect|sync|chat|inferred|seed
  confidence REAL NOT NULL DEFAULT 1, status TEXT NOT NULL DEFAULT 'confirmed',  -- confirmed|proposed
  source_ref TEXT, decision_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, object_type, object_id, person_id, role));

CREATE TABLE systems (
  id TEXT NOT NULL, workspace_id TEXT NOT NULL,                     -- '*' = 内置
  name TEXT NOT NULL, kind TEXT NOT NULL,      -- comm|calendar|code|cloud|finance|steve
  family TEXT NOT NULL,                        -- 句柄族：email|phone|feishu_user|feishu_app|slack_user|github_user|aws_account|card|other
  plugin TEXT,                                 -- 提供适配器的插件（内置写 builtin:<name>）
  handle_rule TEXT NOT NULL DEFAULT '{}',      -- {"normalize":"email|phone|lower|none","pattern":"…"}
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, id));

CREATE TABLE tags (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, cat TEXT NOT NULL, description TEXT NOT NULL, created_by TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE (workspace_id, name));

CREATE TABLE thread_items (
  item_id TEXT PRIMARY KEY REFERENCES items(id), workspace_id TEXT NOT NULL,
  origin_msg TEXT NOT NULL, reply_msg TEXT, from_question INTEGER NOT NULL DEFAULT 0,
  follow INTEGER NOT NULL DEFAULT 1, last_post INTEGER, created INTEGER NOT NULL);

CREATE TABLE thread_reply_commits (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL REFERENCES thread_runs(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL
);

CREATE TABLE thread_reply_reads (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL, space TEXT NOT NULL CHECK(space='steve'),
  through_seq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_id,user_id,space)
);

CREATE TABLE thread_resume (
 run_id TEXT PRIMARY KEY REFERENCES thread_runs(id), created INTEGER NOT NULL,
 attempts INTEGER NOT NULL DEFAULT 0);

CREATE TABLE thread_runs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  message_id TEXT NOT NULL, reply_id TEXT, prev_run TEXT,
  user_id TEXT, project_id TEXT, model TEXT, claude_session TEXT,
  status TEXT NOT NULL,
  token_hash TEXT,
  tools INTEGER NOT NULL DEFAULT 0, turns INTEGER NOT NULL DEFAULT 0,
  first_text_ms INTEGER, error TEXT,
  created INTEGER NOT NULL, started INTEGER, ended INTEGER, session TEXT, harness TEXT);

CREATE TABLE time_alarm_dirty (
  workspace_id TEXT NOT NULL,
  subject TEXT NOT NULL,
  PRIMARY KEY(workspace_id, subject)
);

CREATE TABLE timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, item_id TEXT NOT NULL REFERENCES items(id), ts INTEGER NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL, session_id TEXT, files TEXT);

CREATE TABLE usage_meters (
  turn_id TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
  meter_epoch TEXT NOT NULL, model TEXT NOT NULL, scope TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY(turn_id, meter_epoch, model, scope)
);

CREATE TABLE usage_records (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL,
  ts INTEGER NOT NULL, day TEXT NOT NULL,
  source TEXT NOT NULL,
  purpose TEXT NOT NULL,
  harness TEXT NOT NULL,
  model TEXT NOT NULL DEFAULT '',
  turn_id TEXT, session_id TEXT, item_id TEXT, bot_id TEXT, project_id TEXT,
  input INTEGER, output INTEGER, cache_read INTEGER, cache_write INTEGER, reasoning INTEGER,
  cost_reported REAL,
  duration_ms INTEGER, ok INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 1,
  detail TEXT NOT NULL DEFAULT '{}', live_turn_id TEXT REFERENCES agent_turns(id) ON DELETE CASCADE, live_call_id TEXT REFERENCES gateway_calls(call_id));

CREATE TABLE usage_reports (
  turn_id TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
  report_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY(turn_id, report_id)
);

CREATE TABLE users (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), subject TEXT NOT NULL UNIQUE, name TEXT NOT NULL, email TEXT, role TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE vault_fields (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL REFERENCES vault_items(id),
  label TEXT NOT NULL, type TEXT NOT NULL,     -- text|password|otp|card_number|date|email|url|file
  secret INTEGER NOT NULL,
  value_plain TEXT,                            -- 只有非机密字段
  value_ct BLOB,                               -- 只有机密字段：nonce(24) || 密文
  hint TEXT NOT NULL DEFAULT '',               -- 卡号：•••• 4821；其他机密：空
  sort INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL);

-- debt:comms/fk/vault_items.(account_id) -> accounts.(id)
CREATE TABLE vault_items (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, vault_id TEXT NOT NULL REFERENCES vaults(id),
  template TEXT NOT NULL, title TEXT NOT NULL, site TEXT NOT NULL DEFAULT '',
  account_id TEXT REFERENCES accounts(id),
  sensitivity TEXT NOT NULL,                   -- normal|sensitive|high
  dek_wrapped BLOB, kek_version INTEGER,       -- builtin 后端才有；删除条目时置空 = 销毁
  external_ref TEXT,                           -- 外部后端的引用
  status TEXT NOT NULL DEFAULT 'active',       -- active|deleted
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE vault_keys (
  workspace_id TEXT NOT NULL, version INTEGER NOT NULL,
  keychain_account TEXT NOT NULL,              -- 钥匙串条目的 account 名
  fingerprint TEXT NOT NULL,                   -- hex(sha256("steve/kek-fp" || KEK))[:16]
  status TEXT NOT NULL,                        -- active|retiring|retired
  created INTEGER NOT NULL, retired INTEGER, PRIMARY KEY (workspace_id, version));

CREATE TABLE vault_templates (
  id TEXT NOT NULL, workspace_id TEXT NOT NULL, name TEXT NOT NULL,
  fields TEXT NOT NULL,                        -- [{"label":"密码","type":"password","secret":true}]
  default_sensitivity TEXT NOT NULL, source TEXT NOT NULL, PRIMARY KEY (workspace_id, id));

CREATE TABLE vaults (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL,
  scope TEXT NOT NULL,                         -- personal|shared|system（system 存连接凭据，界面不列出）
  owner_user TEXT,                             -- personal 才有
  backend TEXT NOT NULL DEFAULT 'builtin',     -- builtin|external:<plugin>
  created INTEGER NOT NULL);

CREATE TABLE view_shadow (
 session_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 at INTEGER NOT NULL,
 digest TEXT NOT NULL,
 view TEXT NOT NULL,
 extra_count INTEGER,
 extra TEXT,
 missing_count INTEGER,
 missing TEXT);

CREATE TABLE work_policy (
  name TEXT PRIMARY KEY,
  value INTEGER NOT NULL,
  updated INTEGER NOT NULL
);

CREATE TABLE "workflow_attempts" (
 item_id TEXT NOT NULL REFERENCES items(id),
 attempt INTEGER NOT NULL CHECK(attempt >= 0),
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 unit_id TEXT NOT NULL, snapshot TEXT NOT NULL, created INTEGER NOT NULL,
 PRIMARY KEY(item_id, attempt));

CREATE TABLE workflow_history (
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 revision INTEGER NOT NULL, action TEXT NOT NULL, reason TEXT NOT NULL,
 unit_id TEXT, detail TEXT NOT NULL, created INTEGER NOT NULL,
 PRIMARY KEY(workflow_id,revision));

CREATE TABLE workflow_units (
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 id TEXT NOT NULL, position INTEGER NOT NULL, spec TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','dispatching','running','succeeded','failed','cancelled')),
 item_id TEXT UNIQUE REFERENCES items(id),
 session_id TEXT REFERENCES sessions(id),
 attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
 summary TEXT NOT NULL DEFAULT '', outputs TEXT NOT NULL DEFAULT '[]',
 checks TEXT NOT NULL DEFAULT '[]', reason TEXT,
 PRIMARY KEY(workflow_id,id));

CREATE TABLE workflows (
 item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL CHECK(revision > 0),
 cycle INTEGER NOT NULL DEFAULT 1 CHECK(cycle > 0),
 goal_version INTEGER NOT NULL DEFAULT 1 CHECK(goal_version >= 0),
 paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
 max_parallel INTEGER NOT NULL CHECK(max_parallel > 0),
 max_attempts INTEGER NOT NULL CHECK(max_attempts > 0),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE workspace_members (workspace_id TEXT NOT NULL REFERENCES workspaces(id), user_id TEXT NOT NULL REFERENCES users(id), role TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (workspace_id, user_id));

CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created INTEGER NOT NULL, description TEXT NOT NULL DEFAULT '', item_prefix TEXT NOT NULL DEFAULT 'ST', sort INTEGER NOT NULL DEFAULT 0);

CREATE TABLE yolo_actions (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, window_id TEXT,   -- ya_…
  decision_id TEXT, item_id TEXT, session_id TEXT,
  kind TEXT NOT NULL, subject_type TEXT NOT NULL,
  rule TEXT NOT NULL,                          -- tool.worktree|action.local|accept.checks|…|limit.<n>|presence
  outcome TEXT NOT NULL,                       -- scheduled|decided|declined|muted|cancelled|failed
  choice TEXT, text TEXT, reason TEXT NOT NULL,
  checks TEXT NOT NULL DEFAULT '{}', cost REAL,
  due_at INTEGER, done_at INTEGER,
  undo TEXT,                                   -- {kind: decision|rollback|recall|reanswer|pause, …}
  undone INTEGER, created INTEGER NOT NULL);

CREATE TABLE yolo_scopes (
  workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL,                   -- workspace|project|item
  object_id TEXT NOT NULL,                     -- workspace 行 = workspace_id
  mode TEXT NOT NULL,                          -- on|off|inherit（workspace 只有 on|off）
  paused TEXT,                                 -- 熔断 {code, reason, at}，只用于 workspace 行
  reason TEXT NOT NULL DEFAULT '',             -- 例如“没有 worktree”
  set_by TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, object_type, object_id));

CREATE TABLE yolo_windows (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,           -- yw_…
  started INTEGER NOT NULL, ended INTEGER,
  ended_why TEXT,                                            -- off|tripped
  include_open INTEGER NOT NULL DEFAULT 0,                   -- 打开时选了“也处理现有的待处理卡”
  counters TEXT NOT NULL DEFAULT '{}');

CREATE INDEX a2a_agents_ws ON a2a_agents(workspace_id, name);

CREATE INDEX access_tokens_user ON access_tokens(user_id);

CREATE INDEX agent_turns_item ON agent_turns(item_id, started);

CREATE UNIQUE INDEX agent_turns_node_attempt ON agent_turns(node_attempt_id) WHERE node_attempt_id IS NOT NULL;

CREATE INDEX agent_turns_open ON agent_turns(ended) WHERE ended IS NULL;

CREATE INDEX agent_turns_session ON agent_turns(session_id, started);

CREATE INDEX agent_turns_ws ON agent_turns(workspace_id, started);

CREATE INDEX annotations_item ON annotations(item_id, created);

CREATE INDEX artifacts_item ON artifacts(item_id, created);

CREATE INDEX attention_feed_unread_entry ON attention_feed_unread(entry_id);

CREATE INDEX attention_windows_expiry ON attention_windows(expires);

CREATE INDEX attention_windows_scope ON attention_windows(workspace_id,user_id,kind,revision,position,expires);

CREATE INDEX attr_values_party ON attr_values(workspace_id, party_id, key);

CREATE INDEX audit_target ON audit(target, action, id);

CREATE INDEX bot_capabilities_entry ON bot_capabilities(kind, entry_id);

CREATE INDEX bot_memories_bot ON bot_memories(bot_id, workspace_id, created);

CREATE INDEX bot_teams_workspace ON bot_teams(workspace_id);

CREATE INDEX changesets_ws ON changesets(workspace_id, status, created);

CREATE INDEX clients_user ON clients(user_id);

CREATE INDEX decisions_chan_answered ON decisions(resolved) WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved';

CREATE INDEX decisions_chan_open ON decisions(created) WHERE subject_type = 'chan_message' AND status = 'open';

CREATE INDEX decisions_created_item ON decisions(created) WHERE item_id IS NOT NULL;

CREATE INDEX decisions_item ON decisions(item_id, created);

CREATE INDEX decisions_open ON decisions(workspace_id, status, created);

CREATE INDEX decisions_resolved_item ON decisions(resolved) WHERE item_id IS NOT NULL;

CREATE INDEX decisions_subject ON decisions(subject_type, subject_id, status);

CREATE INDEX deliveries_item ON deliveries(item_id, created);

CREATE INDEX deliveries_owner ON deliveries(workspace_id, owner_id, created);

CREATE INDEX delivery_shares_delivery ON delivery_shares(delivery_id, created);

CREATE INDEX entity_tags_tag ON entity_tags(tag_id);

CREATE INDEX feed_recipient ON feed_entries(workspace_id,recipient_id,seq);

CREATE INDEX feed_scope ON feed_entries(workspace_id,kind,seq);

CREATE INDEX feed_windows_expiry ON feed_windows(expires);

CREATE INDEX feed_windows_scope ON feed_windows(workspace_id,user_id,kind,source,unread,upper_seq,before_seq,expires);

CREATE INDEX files_ws ON files(workspace_id, sha256);

CREATE INDEX flow_runs_event ON flow_runs(event_id) WHERE event_id IS NOT NULL;

CREATE INDEX flow_runs_flow ON flow_runs(workspace_id, flow_id, started);

CREATE INDEX flow_runs_flow_started ON flow_runs(flow_id, started);

CREATE INDEX flow_runs_retry ON flow_runs(status, retry_at);

CREATE INDEX flow_triggers_event ON flow_triggers(workspace_id, kind, source, enabled);

CREATE INDEX flow_triggers_time ON flow_triggers(kind, enabled, next_at);

CREATE INDEX flow_waits_until ON flow_waits(until);

CREATE INDEX flows_ws ON flows(workspace_id, kind, deleted);

CREATE INDEX gateway_local_pending ON gateway_local_inbox(attempt_id) WHERE applied=0;

CREATE INDEX gateway_local_round ON gateway_local_ready(tried);

CREATE INDEX gateway_projection_pending ON gateway_calls(projection_dirty) WHERE projection_dirty=1;

CREATE INDEX grants_lookup ON grants(workspace_id, capability, resource_id, subject_id);

CREATE INDEX item_delivery_facts_item ON item_delivery_facts(item_id,cycle,observed_at);

CREATE INDEX item_deps_dep ON item_deps(dep_id);

CREATE INDEX item_judge_candidates_due ON item_judge_candidates(next_check)
  WHERE state IN ('pending','judging');

CREATE INDEX item_marks_item ON item_marks(item_id, ts);

CREATE INDEX item_marks_ws ON item_marks(workspace_id, kind, ts);

CREATE UNIQUE INDEX item_operations_effect ON item_operations(item_id, kind, effect_id) WHERE effect_id IS NOT NULL;

CREATE INDEX item_operations_item ON item_operations(item_id, cycle);

CREATE INDEX item_stats_dirty ON item_stats(dirty) WHERE dirty = 1;

CREATE INDEX item_stats_done ON item_stats(workspace_id, done_at);

CREATE INDEX item_tags_tag ON item_tags(tag_id);

CREATE INDEX item_waits_open ON item_waits(item_id,cycle) WHERE resolved_at IS NULL;

CREATE INDEX item_would_do_item ON item_would_do(item_id,first_seen);

CREATE INDEX items_closing ON items(phase) WHERE phase = 'closing';

CREATE INDEX links_object ON links(workspace_id, object_type, object_id);

CREATE INDEX mcp_servers_ws ON mcp_servers(workspace_id, name);

CREATE INDEX mem_about_obj ON mem_about(workspace_id, object_type, object_id);

CREATE INDEX mem_changes_mem ON mem_changes(mem_id);

CREATE INDEX mem_changes_run ON mem_changes(workspace_id, run_id, created);

CREATE INDEX mem_ranges_conv ON mem_ranges(workspace_id, conv, first_at);

CREATE INDEX mems_ws ON mems(workspace_id, status, updated);

CREATE INDEX messages_space ON messages(workspace_id, space, created);

CREATE INDEX node_audit_node ON node_audit(node_id, at);

CREATE INDEX node_commands_job_attempt ON node_commands(target_job_attempt) WHERE target_job_attempt IS NOT NULL;

CREATE INDEX node_commands_ready ON node_commands(state,retry_at,lease_until);

CREATE INDEX node_commands_session_room ON node_commands(node_id, state)
  WHERE method='start_turn' AND state IN ('pending','accepted','active','unknown');

CREATE INDEX node_removals_node ON node_removals(node_id, created);

CREATE INDEX node_streams_faulted ON node_streams(node_id) WHERE fault IS NOT NULL;

CREATE INDEX node_upgrades_node ON node_upgrades(node_id, created);

CREATE INDEX nodes_owner ON nodes(owner);

CREATE INDEX outbox_events_topic ON outbox_events(topic, seq);

CREATE INDEX outbox_leases_dead ON outbox_leases(dead) WHERE dead = 1;

CREATE INDEX queued_session ON queued(session_id, created);

CREATE INDEX revisions_object ON revisions(object_type, object_id, version);

CREATE UNIQUE INDEX route_log_msg ON route_log(message_id);

CREATE INDEX route_log_ws ON route_log(workspace_id, created);

CREATE INDEX runtime_job_attempts_open ON runtime_job_attempts(node_id) WHERE state <> 'finished';

CREATE INDEX runtime_jobs_finished ON runtime_jobs(updated) WHERE state='finished';

CREATE UNIQUE INDEX runtime_one_open_attempt ON runtime_attempts(turn_id) WHERE state <> 'finished';

CREATE UNIQUE INDEX runtime_one_open_job_attempt ON runtime_job_attempts(job_id) WHERE state <> 'finished';

CREATE UNIQUE INDEX runtime_one_open_turn ON runtime_turns(session_id) WHERE state <> 'finished';

CREATE INDEX runtime_permissions_answer ON runtime_permissions(answer_operation) WHERE answer_operation IS NOT NULL;

CREATE INDEX runtime_permissions_attempt ON runtime_permissions(attempt_id, state);

CREATE INDEX runtime_permissions_due ON runtime_permissions(expires_at) WHERE state='waiting';

CREATE INDEX runtime_turns_session ON runtime_turns(session_id,created);

CREATE INDEX secret_uses_created ON secret_uses(created);

CREATE INDEX secret_uses_item ON secret_uses(workspace_id, item_id, created);

CREATE INDEX secret_uses_status ON secret_uses(status, created);

CREATE INDEX session_dispatch_pending ON session_dispatch(session_id,state);

CREATE INDEX session_dispatch_turn ON session_dispatch(turn_id,backend,state);

CREATE INDEX sessions_item ON sessions(item_id, no);

CREATE INDEX sessions_token ON sessions(token_hash) WHERE token_hash IS NOT NULL;

CREATE INDEX sessions_updated ON sessions(updated);

CREATE INDEX sessions_ws_updated ON sessions(workspace_id, updated);

CREATE INDEX skills_ws ON skills(workspace_id, name);

CREATE INDEX stewards_person ON stewards(workspace_id, person_id);

CREATE INDEX thread_items_ws ON thread_items(workspace_id, created);

CREATE INDEX thread_reply_commits_person ON thread_reply_commits(workspace_id,user_id,seq);

CREATE INDEX thread_runs_token ON thread_runs(token_hash) WHERE token_hash IS NOT NULL;

CREATE INDEX thread_runs_ws ON thread_runs(workspace_id, status, created);

CREATE INDEX timeline_item ON timeline(item_id, id);

CREATE INDEX timeline_me_ts ON timeline(ts, item_id) WHERE kind = 'me' AND actor = 'me';

CREATE INDEX usage_item ON usage_records(item_id, ts);

CREATE INDEX usage_live_call ON usage_records(live_call_id) WHERE live_call_id IS NOT NULL;

CREATE INDEX usage_live_turn ON usage_records(live_turn_id) WHERE live_turn_id IS NOT NULL;

CREATE INDEX usage_session ON usage_records(session_id, ts);

CREATE INDEX usage_turn ON usage_records(turn_id) WHERE turn_id IS NOT NULL;

CREATE INDEX usage_ws_day ON usage_records(workspace_id, day);

CREATE INDEX vault_fields_item ON vault_fields(item_id, sort);

CREATE INDEX vault_items_ws ON vault_items(workspace_id, vault_id, status);

CREATE INDEX workflow_attempts_graph ON workflow_attempts(workflow_id, created);

CREATE INDEX workflow_units_active ON workflow_units(workflow_id,status);

CREATE INDEX yolo_actions_decision ON yolo_actions(decision_id, created) WHERE decision_id IS NOT NULL;

CREATE INDEX yolo_actions_due ON yolo_actions(outcome, due_at);

CREATE INDEX yolo_actions_item ON yolo_actions(item_id, created);

CREATE INDEX yolo_actions_window ON yolo_actions(window_id, outcome);

CREATE INDEX yolo_actions_ws ON yolo_actions(workspace_id, created);

CREATE INDEX yolo_windows_ws ON yolo_windows(workspace_id, started);

CREATE TRIGGER attention_feed_count_add AFTER INSERT ON attention_feed_unread BEGIN
  UPDATE attention_feed_scopes SET unread_count=unread_count+1,revision=lower(hex(randomblob(16)))
    WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id;
END;

CREATE TRIGGER attention_feed_count_remove AFTER DELETE ON attention_feed_unread BEGIN
  UPDATE attention_feed_scopes SET unread_count=unread_count-1,revision=lower(hex(randomblob(16)))
    WHERE workspace_id=OLD.workspace_id AND user_id=OLD.user_id;
END;

CREATE TRIGGER attention_feed_entry_add AFTER INSERT ON feed_entries BEGIN
  INSERT INTO attention_feed_unread SELECT NEW.workspace_id,m.user_id,NEW.seq,NEW.id FROM workspace_members m
    WHERE m.workspace_id=NEW.workspace_id AND (NEW.audience='workspace' OR NEW.recipient_id=m.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=NEW.workspace_id AND r.user_id=m.user_id AND r.entry_id=NEW.id);
END;

CREATE TRIGGER attention_feed_entry_before_update BEFORE UPDATE ON feed_entries BEGIN
  DELETE FROM attention_feed_unread WHERE entry_id=OLD.id;
END;

CREATE TRIGGER attention_feed_entry_update AFTER UPDATE ON feed_entries BEGIN
  INSERT INTO attention_feed_unread SELECT NEW.workspace_id,m.user_id,NEW.seq,NEW.id FROM workspace_members m
    WHERE m.workspace_id=NEW.workspace_id AND (NEW.audience='workspace' OR NEW.recipient_id=m.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=NEW.workspace_id AND r.user_id=m.user_id AND r.entry_id=NEW.id);
END;

CREATE TRIGGER attention_feed_member_add AFTER INSERT ON workspace_members BEGIN
  INSERT INTO attention_feed_scopes(workspace_id,user_id) VALUES(NEW.workspace_id,NEW.user_id);
  INSERT INTO attention_feed_unread SELECT e.workspace_id,NEW.user_id,e.seq,e.id FROM feed_entries e
    WHERE e.workspace_id=NEW.workspace_id AND (e.audience='workspace' OR e.recipient_id=NEW.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=e.workspace_id AND r.user_id=NEW.user_id AND r.entry_id=e.id);
END;

CREATE TRIGGER attention_feed_read_add AFTER INSERT ON feed_reads BEGIN
  DELETE FROM attention_feed_unread WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id
    AND seq=(SELECT seq FROM feed_entries WHERE id=NEW.entry_id);
END;

CREATE TRIGGER attention_feed_read_remove AFTER DELETE ON feed_reads BEGIN
  INSERT OR IGNORE INTO attention_feed_unread SELECT e.workspace_id,OLD.user_id,e.seq,e.id FROM feed_entries e
    JOIN workspace_members m ON m.workspace_id=e.workspace_id AND m.user_id=OLD.user_id
    WHERE e.id=OLD.entry_id AND e.workspace_id=OLD.workspace_id AND (e.audience='workspace' OR e.recipient_id=OLD.user_id);
END;

CREATE TRIGGER attention_feed_read_update AFTER UPDATE ON feed_reads BEGIN
  INSERT OR IGNORE INTO attention_feed_unread SELECT e.workspace_id,OLD.user_id,e.seq,e.id FROM feed_entries e
    JOIN workspace_members m ON m.workspace_id=e.workspace_id AND m.user_id=OLD.user_id
    WHERE e.id=OLD.entry_id AND e.workspace_id=OLD.workspace_id AND (e.audience='workspace' OR e.recipient_id=OLD.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=e.workspace_id AND r.user_id=OLD.user_id AND r.entry_id=e.id);
  DELETE FROM attention_feed_unread WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id
    AND seq=(SELECT seq FROM feed_entries WHERE id=NEW.entry_id);
END;

CREATE TRIGGER attention_final_reply AFTER UPDATE ON thread_runs
WHEN NEW.status IN ('done','stopped','budget','violated','failed') AND NEW.user_id IS NOT NULL
  AND (OLD.status<>NEW.status OR OLD.reply_id IS NOT NEW.reply_id)
BEGIN
  INSERT OR IGNORE INTO thread_reply_commits(message_id,run_id,workspace_id,user_id)
  SELECT m.id, NEW.id, NEW.workspace_id, NEW.user_id FROM messages m
  JOIN messages origin ON origin.id=NEW.message_id
  WHERE m.id=NEW.reply_id AND m.workspace_id=NEW.workspace_id AND m.space='steve' AND m.author='steve'
    AND origin.workspace_id=NEW.workspace_id AND origin.space='steve' AND origin.author='me'
    AND json_valid(m.meta) AND json_extract(m.meta,'$.run')=NEW.id
    AND json_extract(m.meta,'$.run_receipt')=1 AND json_extract(m.meta,'$.origin')=NEW.message_id;
END;

CREATE TRIGGER brief_alarms_flow_delete AFTER DELETE ON flows
WHEN OLD.kind = 'schedule' AND CASE WHEN json_valid(OLD.schedule) THEN json_extract(OLD.schedule, '$.mode') END = 'brief' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (OLD.workspace_id, 'brief:' || OLD.id)
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_flow_insert AFTER INSERT ON flows
WHEN NEW.kind = 'schedule' AND CASE WHEN json_valid(NEW.schedule) THEN json_extract(NEW.schedule, '$.mode') END = 'brief' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'brief:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_flow_update AFTER UPDATE OF enabled, deleted, schedule, workspace_id ON flows
WHEN NEW.kind = 'schedule' AND (CASE WHEN json_valid(NEW.schedule) THEN json_extract(NEW.schedule, '$.mode') END = 'brief'
  OR CASE WHEN json_valid(OLD.schedule) THEN json_extract(OLD.schedule, '$.mode') END = 'brief') BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'brief:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_member_delete AFTER DELETE ON workspace_members BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = OLD.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = OLD.user_id
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_policy_insert AFTER INSERT ON notification_policies BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = NEW.user_id
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_policy_update AFTER UPDATE OF timezone ON notification_policies
WHEN NEW.timezone IS NOT OLD.timezone BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = NEW.user_id
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_tz_delete AFTER DELETE ON settings WHEN OLD.key = 'tz' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = OLD.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_tz_insert AFTER INSERT ON settings WHEN NEW.key = 'tz' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER brief_alarms_tz_update AFTER UPDATE OF value ON settings
WHEN NEW.key = 'tz' AND NEW.value IS NOT OLD.value BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER gateway_result_clean AFTER UPDATE OF projection_dirty ON gateway_calls
 WHEN NEW.projection_dirty=0 BEGIN
 DELETE FROM gateway_result_ready WHERE call_id=NEW.call_id;
END;

CREATE TRIGGER gateway_result_dirty AFTER UPDATE OF projection_dirty ON gateway_calls
 WHEN NEW.projection_dirty=1 BEGIN
 INSERT OR IGNORE INTO gateway_result_ready(call_id) VALUES(NEW.call_id);
END;

CREATE TRIGGER gateway_result_insert AFTER INSERT ON gateway_calls
 WHEN NEW.projection_dirty=1 BEGIN
 INSERT OR IGNORE INTO gateway_result_ready(call_id) VALUES(NEW.call_id);
END;

CREATE TRIGGER item_dependencies_completion_gate
BEFORE UPDATE OF phase,outcome,status,cycle ON items
WHEN NEW.phase='closed' AND NEW.outcome='completed'
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

-- Final fresh-install seeds; historical data backfills are not replayed.
INSERT INTO attention_meta VALUES (1, lower(hex(randomblob(16))));
INSERT INTO "attr_defs" VALUES ('ad_address','*','both','address','地址','address','[]','reasoning','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_birthday','*','contact','birthday','生日','date','[]','reasoning','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_call','*','contact','call','偏好的称呼','text','[]','public','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_contract','*','org','contract','合同','text','[]','reasoning','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_lang','*','contact','lang','语言','text','[]','public','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_phone','*','both','phone','电话','phone','[]','reasoning','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_tz','*','contact','tz','时区','text','[]','public','builtin','active',0);
INSERT INTO "attr_defs" VALUES ('ad_website','*','org','website','网站','url','[]','public','builtin','active',0);
INSERT INTO sqlite_sequence(name,seq) VALUES ('audit',0);
INSERT INTO sqlite_sequence(name,seq) VALUES ('feed_entries',0);
INSERT INTO sqlite_sequence(name,seq) VALUES ('gateway_result_ready',0);
INSERT INTO sqlite_sequence(name,seq) VALUES ('item_marks',0);
