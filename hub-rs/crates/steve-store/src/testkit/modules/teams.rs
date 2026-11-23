use super::{execute, scalar};
use crate::{Result, Store};

pub fn team_create_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'team.create'",
        [],
    )
}

pub fn team_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE type = 'team'",
        [],
    )
}

pub fn membership_count(store: &Store) -> Result<i64> {
    scalar(store, "SELECT COUNT(*) FROM team_members", [])
}

pub fn revision_patch(store: &Store, tid: &str) -> Result<String> {
    scalar(
        store,
        "SELECT patch FROM revisions WHERE object_id = ? ORDER BY id DESC LIMIT 1",
        rusqlite::params![tid],
    )
}

pub fn revision_version(store: &Store, tid: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT version FROM revisions WHERE object_id = ? ORDER BY id DESC LIMIT 1",
        rusqlite::params![tid],
    )
}

pub fn member_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'team.members'",
        [],
    )
}

pub fn seed_cross_workspace_tags(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_w2', 'w2', 'B', '客户', '', 'usr_dev', 1);",
    )
}

pub fn dissolution_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'team.dissolve'",
        [],
    )
}

pub fn dissolve_steve_team(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE parties SET status = 'dissolved' WHERE id = 'tm_steve'",
    )
}

pub fn swap_dissolved_teams(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE parties SET status = 'active' WHERE id = 'tm_steve'; UPDATE parties SET status = 'dissolved' WHERE id = 'tm_rd';",
    )
}

pub fn chart_team(store: &Store, id: &str, name: &str, parent: Option<&str>, members: &[&str]) {
    let parent = parent.map_or("NULL".to_string(), |p| format!("'{p}'"));
    let mut sql = format!(
        "INSERT INTO parties (id, workspace_id, type, name, source, created_by, created, updated) VALUES ('{id}', 'default', 'team', '{name}', 'sync', 'usr_dev', 1, 1);
         INSERT INTO team_details (party_id, workspace_id, kind, parent_id, sync_connection, external_id, synced) VALUES ('{id}', 'default', 'chart', {parent}, 'con_fs', 'dep_{id}', 1700000000000);"
    );
    for m in members {
        sql.push_str(&format!(
            "INSERT INTO team_members (workspace_id, team_id, contact_id, role, source, joined) VALUES ('default', '{id}', '{m}', 'member', 'sync', 1);"
        ));
    }
    execute(store, &sql).unwrap();
}
