-- Whether a session of a Node's owner loads the owner's personal harness configuration on that
-- machine (user-level AGENTS.md / CLAUDE.md, skills, subagents, commands). NULL follows the
-- workspace's `ownerContext` setting; 1 and 0 are the owner's choice for this Node.
ALTER TABLE nodes ADD COLUMN owner_context INTEGER CHECK(owner_context IN (0,1));
