-- 91: the Hub no longer configures model providers for service sessions: a harness on a Node uses its own
-- sign-in there, for every principal. The setting that named a provider endpoint and a vault key per harness
-- for them (`model_access.v1`, written by 0.2.114) goes.
DELETE FROM settings WHERE key = 'model_access.v1';
