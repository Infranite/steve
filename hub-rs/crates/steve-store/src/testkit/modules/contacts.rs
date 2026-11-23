use super::{execute, scalar};
use crate::{Result, Store};

pub fn owner_party_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE user_id = 'usr_dev' AND workspace_id = 'default'",
        [],
    )
}

pub fn other_party_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE user_id = 'usr_other'",
        [],
    )
}

pub fn workspace_org_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE workspace_id = 'default' AND org_kind = 'self'",
        [],
    )
}

pub fn contact_create_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'party.create' AND detail LIKE '%contact%' AND detail NOT LIKE '%me%'",
        [],
    )
}

pub fn seed_unconfirmed_designer(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO parties (id, workspace_id, type, name, contact_kind, created_by, created, updated) VALUES ('ct_unc', 'default', 'contact', 'sarah.c.design', 'unconfirmed', 'usr_dev', 1, 1);",
    )
}

pub fn seed_vip_tag(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_vip', 'default', 'VIP', '客户', '', 'usr_dev', 1);",
    )
}

pub fn seed_cross_workspace_tags(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_w2', 'w2', 'B', '客户', '', 'usr_dev', 1);",
    )
}

pub fn seed_unconfirmed_sender(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES ('ct_unc', 'default', 'contact', 'sarah.c', 'unconfirmed', 'identify', 'usr_dev', 1, 1);",
    )
}

pub fn seed_contact_relations(store: &Store, cid: &str) -> Result<()> {
    execute(store, &format!("INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'open', 'new', 1, 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES ('default', 'item', 'ST-1', 'related', 'party', '{cid}', 'manual', 1);
             INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('tm_1', 'default', 'team', 'T', 'usr_dev', 1, 1);
             INSERT INTO team_details (party_id, workspace_id, kind) VALUES ('tm_1', 'default', 'virtual');
             INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES ('default', 'tm_1', '{cid}', 1);"))
}

pub fn attr_count(store: &Store, cid: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM attr_values WHERE party_id = ?",
        rusqlite::params![cid],
    )
}

pub fn memory_count(store: &Store, only: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM mems WHERE id = ?",
        rusqlite::params![only],
    )
}

pub fn team_membership_count(store: &Store, cid: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM team_members WHERE contact_id = ?",
        rusqlite::params![cid],
    )
}

pub fn party_link_count(store: &Store, cid: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM links WHERE subject_id = ? OR object_id = ?",
        rusqlite::params![cid, cid],
    )
}

pub fn deleted_party_count(store: &Store, cid: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE id = ? AND status = 'deleted'",
        rusqlite::params![cid],
    )
}

pub fn seed_contact_tag(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1);",
    )
}

pub fn seed_clock_precedence(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default','tz','\"Europe/London\"',1);
        INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated)
        VALUES ('default','usr_dev',1,'now','Asia/Tokyo',1,0,1)")
}

pub fn scrubbed_org_count(store: &Store, arg1: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE id = ? AND status = 'deleted' AND note = '' AND aliases = '[]'",
        rusqlite::params![arg1],
    )
}

pub fn privatize_org(store: &Store, arg1: &str) -> Result<()> {
    execute(
        store,
        &format!("UPDATE parties SET visibility='private' WHERE id='{arg1}'"),
    )
}

pub fn set_test_emails(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE users SET email = 'owner@example.test' WHERE id = 'usr_dev'; UPDATE users SET email = 'other@example.test' WHERE id = 'usr_other'",
    )
}

pub fn last_party_patch(store: &Store) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT patch FROM revisions WHERE object_type = 'party' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn party_deletion_audit(store: &Store) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT detail FROM audit WHERE action = 'party.delete'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn job_properties(store: &Store, org: &str) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT props FROM links WHERE predicate = 'works_at' AND object_id = ?",
            [org],
            |r| r.get(0),
        )?)
    })
}

pub fn workspace_name(store: &Store) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT name FROM workspaces WHERE id = 'default'",
            [],
            |r| r.get(0),
        )?)
    })
}
