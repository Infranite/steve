-- 10: GitHub: apps, repositories, pull requests, checks, issues, webhook deliveries.
-- Spec: steve-rs-plan/modules/github.md §2, DDL verbatim. Rust Hub only; additive, nothing existing is altered.

CREATE TABLE IF NOT EXISTS gh_apps (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,             -- ghapp_<app id>
  app_id INTEGER NOT NULL, slug TEXT NOT NULL, owner TEXT NOT NULL, client_id TEXT NOT NULL,
  credential_item TEXT NOT NULL,               -- 系统保险库条目：pem、webhook_secret、client_secret
  html_url TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE (app_id));

CREATE TABLE IF NOT EXISTS gh_repos (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL REFERENCES connections(id),
  gh_id INTEGER NOT NULL, owner TEXT NOT NULL, name TEXT NOT NULL, full_name TEXT NOT NULL,
  default_branch TEXT NOT NULL, private INTEGER NOT NULL,
  project_id TEXT REFERENCES projects(id),     -- 关联的项目（一个项目最多一个仓库）
  ingest TEXT NOT NULL DEFAULT 'app',          -- app（App Webhook）|repo_hook|poll
  hook_id INTEGER, last_delivery INTEGER, poll_state TEXT NOT NULL DEFAULT '{}',   -- {"pulls_etag":"…","issues_since":0}
  settings TEXT NOT NULL DEFAULT '{}',         -- §3.4
  enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (workspace_id, gh_id));
CREATE UNIQUE INDEX IF NOT EXISTS gh_repos_project ON gh_repos(workspace_id, project_id) WHERE project_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS gh_prs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repo_id TEXT NOT NULL REFERENCES gh_repos(id),
  number INTEGER NOT NULL, gh_id INTEGER NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL,
  state TEXT NOT NULL,                         -- open|closed|merged
  draft INTEGER NOT NULL DEFAULT 0,
  head_ref TEXT NOT NULL, head_sha TEXT NOT NULL, base_ref TEXT NOT NULL,
  author_login TEXT NOT NULL, author_account_id TEXT, opened_by TEXT NOT NULL DEFAULT 'other',   -- steve|me|other
  body_excerpt TEXT NOT NULL DEFAULT '',       -- ≤2000 字
  checks_state TEXT NOT NULL DEFAULT 'none',   -- none|pending|success|failure
  review_state TEXT NOT NULL DEFAULT 'none',   -- none|requested|approved|changes_requested|commented
  mergeable TEXT,                              -- clean|dirty|blocked|unknown
  item_id TEXT, link_source TEXT,              -- branch|ref|steve|manual
  created INTEGER NOT NULL, updated INTEGER NOT NULL, merged_at INTEGER, closed_at INTEGER,
  UNIQUE (repo_id, number));
CREATE INDEX IF NOT EXISTS gh_prs_item ON gh_prs(workspace_id, item_id);
CREATE INDEX IF NOT EXISTS gh_prs_head ON gh_prs(repo_id, head_sha);

CREATE TABLE IF NOT EXISTS gh_checks (
  repo_id TEXT NOT NULL REFERENCES gh_repos(id), workspace_id TEXT NOT NULL,
  head_sha TEXT NOT NULL, kind TEXT NOT NULL,  -- check_run|status
  ext_id TEXT NOT NULL,                        -- check run id；status 用 context 名
  name TEXT NOT NULL, status TEXT NOT NULL,    -- queued|in_progress|completed
  conclusion TEXT,                             -- success|failure|cancelled|skipped|neutral|timed_out|action_required
  url TEXT NOT NULL DEFAULT '', run_id INTEGER, job_id INTEGER,  -- Actions：工作流运行与 job
  summary TEXT NOT NULL DEFAULT '', started INTEGER, completed INTEGER, updated INTEGER NOT NULL,
  PRIMARY KEY (repo_id, head_sha, kind, ext_id));

CREATE TABLE IF NOT EXISTS gh_issues (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repo_id TEXT NOT NULL REFERENCES gh_repos(id),
  number INTEGER NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, state TEXT NOT NULL,
  labels TEXT NOT NULL DEFAULT '[]', author_login TEXT NOT NULL, author_account_id TEXT,
  item_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (repo_id, number));

CREATE TABLE IF NOT EXISTS gh_deliveries (
  delivery_id TEXT PRIMARY KEY,                -- X-GitHub-Delivery；轮询生成的用 poll:<repo>:<类型>:<id>:<updated>
  workspace_id TEXT, repo_id TEXT, event TEXT NOT NULL, action TEXT,
  status TEXT NOT NULL,                        -- ok|ignored|failed
  error TEXT, received INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS gh_deliveries_recv ON gh_deliveries(received);
