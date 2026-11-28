//! 149: remove retired agent-robot-avatar `skin` key from `bots.avatar` JSON (#645)
//! while preserving `color`, `round`, and rows that never had `skin`.

use rusqlite::Connection;
use steve_store::{NoPublisher, Store, db};

#[test]
fn strips_legacy_skin_key_from_bot_avatars_and_preserves_color_and_round() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("json_remove(avatar, '$.skin')"))
        .expect("bot avatar skin cleanup migration registered") as i64;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);",
    )
    .unwrap();
    for (i, migration) in db::MIGRATIONS[..before as usize].iter().enumerate() {
        if i == 119 {
            for table in ["bots", "sessions"] {
                let present: bool = c
                    .query_row(
                        &format!(
                            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'reasoning_effort')"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                if !present {
                    c.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
                    ))
                    .unwrap();
                }
            }
        }
        c.execute_batch(migration).unwrap();
        if i + 1 == 122 {
            c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
        }
    }
    c.execute_batch(&format!(
        r##"INSERT INTO schema_version VALUES({before});
           INSERT INTO schema_compat VALUES({before});
           INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
           INSERT INTO bots(id,workspace_id,name,harness,avatar,updated) VALUES
             ('b_skin','default','WithSkin','claude-code','{{"color":"#8c7ae6","skin":"301","round":45}}',1),
             ('b_null','default','NullSkin','claude-code','{{"color":"#5fb39a","skin":null,"round":70}}',1),
             ('b_only','default','OnlySkin','claude-code','{{"skin":"901"}}',1),
             ('b_clean','default','Clean','claude-code','{{"color":"#e0a458","round":15}}',1);"##
    ))
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    assert_eq!(store.schema_compat().unwrap().schema, db::SCHEMA_VERSION);

    let c = Connection::open(&path).unwrap();
    let raw = |id: &str| -> String {
        c.query_row("SELECT avatar FROM bots WHERE id = ?", [id], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(raw("b_skin"), r##"{"color":"#8c7ae6","round":45}"##);
    assert_eq!(raw("b_null"), r##"{"color":"#5fb39a","round":70}"##);
    assert_eq!(raw("b_only"), r##"{}"##);
    assert_eq!(raw("b_clean"), r##"{"color":"#e0a458","round":15}"##);

    let bot = store.bots().get("b_skin").unwrap().unwrap();
    assert_eq!(bot.avatar.color.as_deref(), Some("#8c7ae6"));
    assert_eq!(bot.avatar.round.as_ref().and_then(|n| n.as_u64()), Some(45));
}
