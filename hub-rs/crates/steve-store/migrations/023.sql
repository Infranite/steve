-- 23: Feishu / Lark app registrations (the official PersonalAgent bootstrap: `accounts/oauth/v1/app/registration`
-- begin → poll → client_id/client_secret). One row per attempt, so a Hub restart resumes it and a cancel and the
-- binding commit are ordered by the write lock. No secret is stored here: the `device_code` (a bearer ticket that can
-- fetch the app secret) is a vault item (`device_item`), written with the row and destroyed in the transaction that
-- leaves `pending`; the issued secret is `vault_item`, written in the transaction that moves the row to
-- `credentials_saved`. CREATE … IF NOT EXISTS only, so it can run twice.
CREATE TABLE IF NOT EXISTS chan_app_registrations (
  id TEXT PRIMARY KEY,                 -- reg_…
  workspace_id TEXT NOT NULL,
  initiator TEXT NOT NULL,             -- users.id of who started it: every route and the resume act as them
  brand_requested TEXT NOT NULL,       -- feishu | lark: the confirmation page's host
  brand TEXT NOT NULL,                 -- the brand polled now (switched at most once on the tenant's report)
  switched INTEGER NOT NULL DEFAULT 0,
  device_item TEXT,                    -- vault item holding the device_code, while pending only
  user_code TEXT,
  verification_uri_complete TEXT,
  interval_s INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,         -- Unix ms, fixed at begin; a resume never extends it
  state TEXT NOT NULL,                 -- pending|credentials_saved|verifying|succeeded|denied|expired|failed|cancelled
  error TEXT,                          -- short, ours (never the provider's text)
  error_kind TEXT,                     -- denied|expired|network|provider|verify_failed|conflict|member_gone|credentials_lost
  app_may_exist INTEGER NOT NULL DEFAULT 0,
  client_id TEXT,                      -- the App ID once issued
  vault_item TEXT,                     -- the credential item while credentials_saved/verifying; handed to the connection
  connection_id TEXT,
  account_id TEXT,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);
-- one active attempt per person and workspace (another person's never blocks or is returned)
CREATE UNIQUE INDEX IF NOT EXISTS chan_app_registrations_active ON chan_app_registrations(workspace_id, initiator)
  WHERE state IN ('pending', 'credentials_saved', 'verifying');
CREATE INDEX IF NOT EXISTS chan_app_registrations_resume ON chan_app_registrations(state)
  WHERE state IN ('pending', 'credentials_saved', 'verifying');
