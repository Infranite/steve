-- Drop retired directory/worktree columns from bots and items.
ALTER TABLE bots DROP COLUMN project_dir;
ALTER TABLE bots DROP COLUMN worktree;
ALTER TABLE items DROP COLUMN project_dir;
