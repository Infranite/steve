-- 95: whether a Node owner's sessions load the owner's personal harness configuration is decided per
-- Node, else by the Hub (`node.owner_context` in the Hub's settings, `on` | `off`), no longer by each
-- workspace's `ownerContext` setting: a Node belongs to the Hub and serves every workspace. The Hub's
-- default starts off when any workspace had turned it off, so no session starts loading a
-- configuration its owner had excluded; the workspace setting goes.
INSERT INTO settings(workspace_id, key, value, updated)
  SELECT 'default', 'node.owner_context', 'off',
    (SELECT MAX(updated) FROM settings WHERE key = 'ownerContext' AND value = '"off"')
  WHERE EXISTS (SELECT 1 FROM settings WHERE key = 'ownerContext' AND value = '"off"')
  ON CONFLICT(workspace_id, key) DO NOTHING;
DELETE FROM settings WHERE key = 'ownerContext';
