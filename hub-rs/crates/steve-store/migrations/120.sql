ALTER TABLE bots ADD COLUMN specialties TEXT NOT NULL DEFAULT '[]';
CREATE TABLE bot_placements (
    seq INTEGER PRIMARY KEY,
    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    fact TEXT,
    blocked TEXT,
    CHECK ((fact IS NULL) <> (blocked IS NULL)),
    CHECK (session_id IS NOT NULL OR blocked IS NULL)
);
CREATE INDEX bot_placements_item ON bot_placements(item_id, seq DESC);
CREATE UNIQUE INDEX bot_placements_session ON bot_placements(session_id) WHERE session_id IS NOT NULL;
CREATE UNIQUE INDEX bot_placements_initial ON bot_placements(item_id) WHERE session_id IS NULL;

CREATE TABLE item_team_snapshots (
    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    team TEXT NOT NULL,
    source TEXT CHECK(source IN ('default', 'explicit', 'inherited'))
);
INSERT INTO item_team_snapshots (item_id, team, source)
    SELECT item_id, team, source FROM item_dispatch WHERE team IS NOT NULL;
DROP TABLE item_dispatch;

CREATE TABLE bot_proposals (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    actor TEXT NOT NULL,
    project_id TEXT,
    created INTEGER NOT NULL,
    draft TEXT NOT NULL,
    confirmed_bot TEXT REFERENCES bots(id)
);
