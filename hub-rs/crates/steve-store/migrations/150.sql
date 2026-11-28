-- 150: sink Bot.default_project from bots into workspace-scoped bot_workspaces (#582).
ALTER TABLE bot_workspaces ADD COLUMN default_project TEXT;

UPDATE bot_workspaces
   SET default_project = (
     SELECT b.default_project
       FROM bots b
       JOIN projects p ON p.id = b.default_project
      WHERE b.id = bot_workspaces.bot_id
        AND b.default_project IS NOT NULL
        AND b.default_project <> ''
        AND p.workspace_id = bot_workspaces.workspace_id
   )
 WHERE default_project IS NULL
   AND EXISTS (
     SELECT 1
       FROM bots b
       JOIN projects p ON p.id = b.default_project
      WHERE b.id = bot_workspaces.bot_id
        AND b.default_project IS NOT NULL
        AND b.default_project <> ''
        AND p.workspace_id = bot_workspaces.workspace_id
   );

UPDATE bot_workspaces
   SET default_project = (
     SELECT b.default_project
       FROM bots b
      WHERE b.id = bot_workspaces.bot_id
        AND b.default_project IS NOT NULL
        AND b.default_project <> ''
   )
 WHERE default_project IS NULL
   AND EXISTS (
     SELECT 1
       FROM bots b
      WHERE b.id = bot_workspaces.bot_id
        AND b.workspace_id = bot_workspaces.workspace_id
        AND b.default_project IS NOT NULL
        AND b.default_project <> ''
        AND NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = b.default_project)
   );

ALTER TABLE bots DROP COLUMN default_project;
