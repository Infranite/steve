//! 019: one space per (installation, chat). A database shaped like production (one Feishu app with its bot and
//! your user connection, 10 space rows of which 3 chats were seen by both) migrates to 7 spaces; every reference
//! follows the kept (oldest) row, both identities are recorded, the merges are traced, settings you made win,
//! and running it again changes nothing.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::db;

const SEED: &str = "
INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);
INSERT INTO systems (id, workspace_id, name, kind, family, created) VALUES ('feishu_app', '*', '飞书', 'comm', 'feishu', 1), ('feishu', '*', '飞书', 'comm', 'feishu', 1);
INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES
  ('ct_me', 'default', 'contact', '李鹏翔', 'u', 1, 1), ('org', 'default', 'org', '公司', 'u', 1, 1), ('ct_a', 'default', 'contact', '张三', 'u', 1, 1);
INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
  ('acc_bot', 'default', 'feishu_app', 'feishu', 'org', 'app_bot', 'ou_bot', 'Steve', 'connect', 1, 1),
  ('acc_me', 'default', 'feishu', 'feishu', 'ct_me', 'personal', 'ou_me', '李鹏翔', 'connect', 1, 1),
  ('acc_a', 'default', 'feishu', 'feishu', 'ct_a', 'personal', 'ou_a', '张三', 'channel', 1, 1);
INSERT INTO connections (id, workspace_id, account_id, adapter, auth, config, created_by, created, updated) VALUES
  ('con_bot', 'default', 'acc_bot', 'feishu_app', 'credential', '{}', 'u', 1, 1),
  ('con_me', 'default', 'acc_me', 'feishu_user', 'oauth', '{\"app_connection\":\"con_bot\"}', 'u', 2, 2);
INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, created, updated, last_message) VALUES
  ('spc_g1b', 'default', 'feishu_app', 'con_bot', 'group', 'oc_g1', '运维群', 10, 10, 100),
  ('spc_g1m', 'default', 'feishu', 'con_me', 'group', 'oc_g1', '运维群', 5, 5, 120),
  ('spc_g2b', 'default', 'feishu_app', 'con_bot', 'group', 'oc_g2', '告警通知', 11, 11, 50),
  ('spc_g2m', 'default', 'feishu', 'con_me', 'group', 'oc_g2', '告警通知', 12, 12, 60),
  ('spc_g3b', 'default', 'feishu_app', 'con_bot', 'group', 'oc_g3', '', 13, 13, NULL),
  ('spc_g3m', 'default', 'feishu', 'con_me', 'group', 'oc_g3', '产品群', 14, 14, 70),
  ('spc_dmb', 'default', 'feishu_app', 'con_bot', 'dm', 'oc_dm_bot', '私聊', 15, 15, 80),
  ('spc_self', 'default', 'feishu', 'con_me', 'dm', 'oc_self', '李鹏翔', 16, 16, 90),
  ('spc_g4m', 'default', 'feishu', 'con_me', 'group', 'oc_g4', '只有我', 17, 17, 30),
  ('spc_g5b', 'default', 'feishu_app', 'con_bot', 'group', 'oc_g5', '只有机器人', 18, 18, 20);
INSERT INTO space_settings (space_id, workspace_id, disclosure, updated, configured, muted)
  SELECT id, 'default', 'conclusion', 1, id = 'spc_g1b', id = 'spc_g2m' FROM spaces;
INSERT INTO space_members (workspace_id, space_id, account_id, role, joined) VALUES
  ('default', 'spc_g1b', 'acc_bot', 'bot', 1), ('default', 'spc_g1b', 'acc_a', 'member', 1),
  ('default', 'spc_g1m', 'acc_me', 'bot', 1), ('default', 'spc_g1m', 'acc_a', 'member', 1);
INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, created) VALUES
  ('cm1', 'default', 'spc_g1b', 'om_1', 'in', 'hi', 1), ('cm2', 'default', 'spc_g1m', 'om_1', 'in', 'hi', 1),
  ('cm3', 'default', 'spc_g1m', 'om_2', 'out', '收到', 2), ('cm4', 'default', 'spc_g1b', 'om_2', 'in', '收到', 2),
  ('cm5', 'default', 'spc_g2m', 'om_3', 'in', 'x', 3);
INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created)
  VALUES ('d1', 'default', 'ask', 'space', 'spc_g2m', '?', 'open', 'steve', 1);
INSERT INTO ext_events (id, workspace_id, source, type, dedupe_key, group_key, space_id, occurred, received, summary)
  VALUES ('e1', 'default', 'im', 'im.message', 'k1', 'spc_g1b', 'spc_g1b', 1, 1, 's');
INSERT INTO persona_bindings (workspace_id, account_id, space_id, persona_id, created) SELECT 'default', 'acc_bot', 'spc_g1b', 'per_x', 1 WHERE 0;
INSERT INTO settings VALUES ('default', 'notifyVia', '\"spc_dmb\"', 1), ('default', 'other', '\"spc_g3m\"', 1);
";

fn v18() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("steve.sqlite3")).unwrap();
    c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..18] {
        c.execute_batch(m).unwrap();
    }
    c.execute("INSERT INTO schema_version (v) VALUES (18)", [])
        .unwrap();
    c.execute_batch(SEED).unwrap();
    (dir, c)
}

fn strings(c: &Connection, sql: &str) -> Vec<String> {
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn sibling_spaces_merge_into_one_per_installation_and_chat() {
    let (_dir, c) = v18();
    assert_eq!(count(&c, "SELECT count(*) FROM spaces"), 10);
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
    // before 10, after 7: g1, g2, g3 were each two rows
    assert_eq!(count(&c, "SELECT count(*) FROM spaces"), 7);
    assert_eq!(
        strings(&c, "SELECT id FROM spaces ORDER BY id"),
        [
            "spc_dmb", "spc_g1m", "spc_g2b", "spc_g3b", "spc_g4m", "spc_g5b", "spc_self"
        ]
    );
    assert_eq!(
        strings(
            &c,
            "SELECT from_id || '>' || into_id FROM space_merges ORDER BY from_id"
        ),
        ["spc_g1b>spc_g1m", "spc_g2m>spc_g2b", "spc_g3m>spc_g3b"]
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM audit WHERE action = 'space.merge'"
        ),
        3
    );
    // both identities of the merged chats, one of the others
    assert_eq!(count(&c, "SELECT count(*) FROM space_identities"), 10);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM space_identities WHERE space_id = 'spc_g1m'"
        ),
        2
    );
    // the kept row knows what the other knew
    assert_eq!(
        strings(
            &c,
            "SELECT name || ' ' || last_message FROM spaces WHERE id IN ('spc_g1m', 'spc_g3b') ORDER BY id"
        ),
        ["运维群 120", "产品群 70"]
    );
    // messages: one space, the second copy of a provider message keeps its row under a marked id
    assert_eq!(
        strings(
            &c,
            "SELECT id || ' ' || space_id || ' ' || external_id FROM chan_messages ORDER BY id"
        ),
        [
            "cm1 spc_g1m om_1#dup:cm1",
            "cm2 spc_g1m om_1",
            "cm3 spc_g1m om_2",
            "cm4 spc_g1m om_2#dup:cm4",
            "cm5 spc_g2b om_3"
        ]
    );
    assert_eq!(
        strings(
            &c,
            "SELECT space_id || ' ' || account_id FROM space_members ORDER BY 1"
        ),
        ["spc_g1m acc_a", "spc_g1m acc_bot", "spc_g1m acc_me"]
    );
    // settings you made win (g1: configured on the bot's copy; g2: muted on yours)
    assert_eq!(
        strings(
            &c,
            "SELECT space_id || ' ' || configured || ' ' || muted FROM space_settings WHERE space_id IN ('spc_g1m', 'spc_g2b') ORDER BY 1"
        ),
        ["spc_g1m 1 0", "spc_g2b 0 1"]
    );
    assert_eq!(count(&c, "SELECT count(*) FROM space_settings"), 7);
    assert_eq!(strings(&c, "SELECT subject_id FROM decisions"), ["spc_g2b"]);
    assert_eq!(
        strings(&c, "SELECT space_id || ' ' || group_key FROM ext_events"),
        ["spc_g1m spc_g1m"]
    );
    assert_eq!(
        strings(&c, "SELECT value FROM settings ORDER BY key"),
        ["\"spc_dmb\"", "\"spc_g3b\""]
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );

    // again: nothing changes
    let before = strings(
        &c,
        "SELECT id || name || coalesce(last_message, '') FROM spaces ORDER BY id",
    );
    c.execute_batch(db::MIGRATIONS[18]).unwrap();
    assert_eq!(
        strings(
            &c,
            "SELECT id || name || coalesce(last_message, '') FROM spaces ORDER BY id"
        ),
        before
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM audit WHERE action = 'space.merge'"
        ),
        3
    );
    assert_eq!(count(&c, "SELECT count(*) FROM space_identities"), 10);
}
