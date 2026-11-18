use rusqlite::{Connection, params};
use steve_store::db;

#[test]
fn adds_receipt_trace_and_only_disables_the_untouched_issue_proposal() {
    assert_eq!(db::MIGRATIONS[106], include_str!("../migrations/107.sql"));
    let c = Connection::open_in_memory().unwrap();
    for m in &db::MIGRATIONS[..106] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES ('default','Fixture',1);
         CREATE TABLE schema_version(v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES(106);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES(106);
         INSERT INTO gh_deliveries(delivery_id, event, status, received)
             VALUES ('existing', 'issues', 'ok', 1);",
    )
    .unwrap();
    for (id, origin, version, updated) in [
        ("untouched", "template:github-issues", 1, 10),
        ("edited", "template:github-issues", 2, 11),
        ("toggled", "template:github-issues", 1, 11),
        ("other", "template:github", 1, 10),
    ] {
        c.execute(
            "INSERT INTO flows(id,workspace_id,name,enabled,graph,version,origin,created_by,created,updated)
                 VALUES (?1,'default',?1,1,'{}',?2,?3,'steve',10,?4)",
            params![id, version, origin, updated],
        )
        .unwrap();
        c.execute(
            "INSERT INTO flow_triggers(id,workspace_id,flow_id,node_id,kind,source,enabled)
                 VALUES (?1,'default',?1,'t','event','github',1)",
            [id],
        )
        .unwrap();
    }
    db::migrate(&c).unwrap();
    for id in ["untouched", "edited", "toggled", "other"] {
        let settings: (bool, bool) = c
            .query_row(
                "SELECT f.enabled, t.enabled FROM flows f JOIN flow_triggers t ON t.flow_id = f.id
                 WHERE f.id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let enabled = id != "untouched";
        assert_eq!(settings, (enabled, enabled), "{id}");
    }
    let delivery: (String, Option<String>) = c
        .query_row(
            "SELECT status, automation FROM gh_deliveries WHERE delivery_id = 'existing'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(delivery, ("ok".to_string(), None));
}
