//! 044: Steve's own project as the seed made it (`$STEVE_HOME/current/ops/release.sh`) releases through the
//! merge lane: no command of its own, done once merged and tagged; a rule of done already chosen stays, and
//! every other project's release is left as it was.

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_store::db;

const SEEDED: &str = "/srv/steve-home/current/ops/release.sh";

fn v43() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("steve.sqlite3")).unwrap();
    c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..43] {
        c.execute_batch(m).unwrap();
    }
    c.execute("INSERT INTO schema_version (v) VALUES (43)", [])
        .unwrap();
    c.execute_batch("INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);")
        .unwrap();
    (dir, c)
}

fn project(c: &Connection, id: &str, release: Option<Value>) {
    c.execute(
        "INSERT INTO projects (id, workspace_id, name, path, kind, release, created, updated) VALUES (?, 'default', ?, '/repo', 'git', ?, 1, 1)",
        rusqlite::params![id, id, release.map(|r| r.to_string())],
    )
    .unwrap();
}

fn release(c: &Connection, id: &str) -> Option<Value> {
    c.query_row("SELECT release FROM projects WHERE id = ?", [id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .unwrap()
    .map(|s| serde_json::from_str(&s).unwrap())
}

#[test]
fn the_seeded_steve_project_moves_to_the_lane_and_nothing_else_changes() {
    let (_dir, c) = v43();
    project(&c, "seeded", Some(json!({"kind": "script", "cmd": SEEDED})));
    project(
        &c,
        "tagged",
        Some(json!({"kind": "script", "cmd": SEEDED, "done_when": "tag"})),
    );
    project(
        &c,
        "merged",
        Some(json!({"kind": "script", "cmd": SEEDED, "done_when": "merge"})),
    );
    let edited = json!({"kind": "script", "cmd": format!("{SEEDED} --web-only")});
    project(&c, "edited", Some(edited.clone()));
    let other = json!({"kind": "script", "cmd": "/srv/app/deploy.sh"});
    project(&c, "other", Some(other.clone()));
    project(&c, "none", None);

    db::migrate(&c).unwrap();

    let typed = |id: &str| -> steve_store::Release {
        serde_json::from_value(release(&c, id).unwrap()).unwrap()
    };
    for (id, rule) in [("seeded", "tag"), ("tagged", "tag"), ("merged", "merge")] {
        let r = typed(id);
        assert_eq!(
            (r.kind.as_str(), r.cmd.as_str(), r.done_when.as_deref()),
            ("script", "", Some(rule)),
            "{id}"
        );
    }
    assert_eq!(release(&c, "edited"), Some(edited));
    assert_eq!(release(&c, "other"), Some(other));
    assert_eq!(release(&c, "none"), None);
    let v: i64 = c
        .query_row("SELECT v FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 44);
}
