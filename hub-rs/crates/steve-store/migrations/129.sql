-- Converge bots.perm_mode onto the active modes enforced by approval routing
-- (plan, default, bypassPermissions).
UPDATE bots
   SET perm_mode = 'default'
 WHERE perm_mode NOT IN ('default', 'plan', 'bypassPermissions');
