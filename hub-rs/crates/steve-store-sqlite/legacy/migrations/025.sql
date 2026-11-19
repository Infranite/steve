-- What the provider lookups found about one person's account (contacts profile): when it was last looked
-- up, when it is due again (1 h → 6 h → 24 h → 72 h after lookups that left gaps), where the answer came
-- from, and why each missing field is missing (`gaps`: field → {"reason", "scope"?}). Read by the contact
-- page to say what is unknown and why; never a copy of the data itself.
CREATE TABLE IF NOT EXISTS account_profiles (
 account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL,
 checked INTEGER, next_check INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
 source TEXT, gaps TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS account_profiles_due ON account_profiles(workspace_id, next_check);
