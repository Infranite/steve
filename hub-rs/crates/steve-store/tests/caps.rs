//! Migration 65: the capabilities library, assigned per Bot; what a session gets; and the move of
//! 62's and 63's per-Bot rows into it.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::caps::{self, mcp, skills};
use steve_store::db;

fn server(id: &str, name: &str, transport: &str) -> mcp::Server {
    mcp::Server {
        id: id.into(),
        workspace_id: "default".into(),
        name: name.into(),
        transport: transport.into(),
        command: (transport == "stdio").then(|| "npx".into()),
        args: vec![],
        env: vec![],
        url: (transport != "stdio").then(|| "https://mcp.example.com/mcp".into()),
        headers: vec![],
        credential_item: None,
        created: 1,
        updated: 1,
    }
}

fn skill(id: &str, name: &str) -> skills::Skill {
    skills::Skill {
        id: id.into(),
        workspace_id: "default".into(),
        name: name.into(),
        description: format!("Use for {name}"),
        content: "# Steps".into(),
        created: 1,
        updated: 1,
    }
}

fn setup() -> T {
    let t = fresh();
    t.store
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1), ('doc', 'default', '文档', 'codex', 1);
                 INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s_dev', 'default', 'dev', 'claude-code', '.', 'active', 1, 1), ('s_none', 'default', NULL, 'claude-code', '.', 'active', 1, 1);",
            )?;
            Ok(())
        })
        .unwrap();
    t
}

#[test]
fn a_session_gets_the_entries_its_bot_has_switched_on() {
    let t = setup();
    t.store
        .write(|tx| {
            mcp::Servers(tx).insert(&server("bmcp_a", "files", "stdio"))?;
            mcp::Servers(tx).insert(&server("bmcp_b", "remote", "http"))?;
            mcp::Servers(tx).insert(&server("bmcp_c", "unused", "sse"))?;
            skills::Skills(tx).insert(&skill("bskill_a", "review"))?;
            skills::Skills(tx).insert(&skill("bskill_b", "drafts"))?;
            caps::Assignments(tx).set("dev", "mcp", "bmcp_a", true, 1)?;
            caps::Assignments(tx).set("dev", "mcp", "bmcp_b", false, 1)?;
            caps::Assignments(tx).set("doc", "mcp", "bmcp_b", true, 1)?;
            caps::Assignments(tx).set("dev", "skill", "bskill_a", true, 1)?;
            caps::Assignments(tx).set("dev", "skill", "bskill_b", false, 1)?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    t.store
        .read(|c| {
            let got: Vec<String> = mcp::Servers(c)
                .enabled_for_session("s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["files"]);
            assert!(mcp::Servers(c).enabled_for_session("s_none")?.is_empty());
            let got: Vec<String> = skills::Skills(c)
                .enabled_for_session("s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["review"]);
            let used: Vec<String> = caps::Assignments(c)
                .of_entry("mcp", "bmcp_b")?
                .into_iter()
                .map(|a| a.bot_id)
                .collect();
            assert_eq!(used, ["dev", "doc"]);
            assert!(mcp::Servers(c).name_taken("default", "FILES", None)?);
            assert!(!mcp::Servers(c).name_taken("default", "files", Some("bmcp_a"))?);
            Ok(())
        })
        .unwrap();
    // switching on, and deleting an entry takes it from every Bot
    t.store
        .write(|tx| {
            caps::Assignments(tx).set("dev", "mcp", "bmcp_b", true, 2)?;
            mcp::Servers(tx).delete("bmcp_a")?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    t.store
        .read(|c| {
            let got: Vec<String> = mcp::Servers(c)
                .enabled_for_session("s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["remote"]);
            assert!(caps::Assignments(c).of_entry("mcp", "bmcp_a")?.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_secret_value_never_reaches_the_row() {
    let t = setup();
    let mut s = server("bmcp_1", "github", "stdio");
    s.env = vec![
        mcp::Var {
            name: "LOG".into(),
            value: Some("debug".into()),
            secret: false,
        },
        mcp::Var {
            name: "TOKEN".into(),
            value: Some("ghp_leak".into()),
            secret: true,
        },
    ];
    t.store.write(|tx| mcp::Servers(tx).insert(&s)).unwrap();
    let raw: String = t
        .store
        .with_conn(|c| {
            Ok(
                c.query_row("SELECT env FROM mcp_servers WHERE id = 'bmcp_1'", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert!(!raw.contains("ghp_leak"), "{raw}");
    let back = t
        .store
        .read(|c| mcp::Servers(c).get("bmcp_1"))
        .unwrap()
        .unwrap();
    assert_eq!(back.secret_labels(), ["env:TOKEN"]);
}

#[test]
fn the_per_bot_rows_of_62_and_63_move_into_the_library() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("caps.sqlite3");
    let c = Connection::open(&file).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(64);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..64] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1), ('doc', 'default', '文档', 'codex', 1);
         INSERT INTO bot_mcp_servers (id, workspace_id, bot_id, name, transport, command, env, credential_item, enabled, created, updated)
           VALUES ('bmcp_1', 'default', 'dev', 'files', 'stdio', 'npx', '[{\"name\":\"TOKEN\",\"secret\":true}]', 'vi_1', 1, 1, 1),
                  ('bmcp_2', 'default', 'doc', 'Files', 'stdio', 'uvx', '[]', NULL, 0, 2, 2);
         INSERT INTO bot_skills (id, workspace_id, bot_id, name, description, content, enabled, created, updated)
           VALUES ('bskill_1', 'default', 'dev', 'review', 'Use for review', 'body', 1, 1, 1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    steve_store::Store::read_file(&file, |r| {
        let s = mcp::Servers(r).get("bmcp_1")?.unwrap();
        assert_eq!(
            (s.name.as_str(), s.credential_item.as_deref()),
            ("files", Some("vi_1"))
        );
        assert_eq!(s.secret_labels(), ["env:TOKEN"]);
        // the second "files" of the workspace is renamed, and keeps its switch for its Bot
        assert_eq!(mcp::Servers(r).get("bmcp_2")?.unwrap().name, "Files-2");
        let a = caps::Assignments(r).get("doc", "mcp", "bmcp_2")?.unwrap();
        assert!(!a.enabled);
        assert!(caps::Assignments(r).is_enabled("dev", "mcp", "bmcp_1")?);
        assert!(caps::Assignments(r).is_enabled("dev", "skill", "bskill_1")?);
        assert_eq!(skills::Skills(r).get("bskill_1")?.unwrap().content, "body");
        Ok(())
    })
    .unwrap();
    let old: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('bot_mcp_servers', 'bot_skills')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old, 0);
}

/// Migration 67: a Bot's pins, gone with the entry or when the entry is taken off the Bot; usage
/// summed per tool over the last days, old rows dropped.
#[test]
fn pins_follow_their_entries_and_usage_sums_by_tool() {
    use caps::tools;
    let t = setup();
    t.store
        .write(|tx| {
            mcp::Servers(tx).insert(&server("bmcp_a", "files", "stdio"))?;
            caps::Assignments(tx).set("dev", "mcp", "bmcp_a", true, 1)?;
            assert!(tools::ToolRows(tx).set_pin("dev", "mcp", "bmcp_a", true, 2)?);
            assert!(
                !tools::ToolRows(tx).set_pin("dev", "mcp", "bmcp_a", true, 3)?,
                "once"
            );
            assert!(tools::ToolRows(tx).set_pin("dev", "tool", "steve_workflow", true, 4)?);
            assert!(tools::ToolRows(tx).set_pin("doc", "mcp", "bmcp_a", true, 5)?);
            let kinds: Vec<String> = tools::ToolRows(tx)
                .pins("dev")?
                .into_iter()
                .map(|p| p.kind)
                .collect();
            assert_eq!(kinds, ["mcp", "tool"]);
            caps::Assignments(tx).remove("dev", "mcp", "bmcp_a")?;
            assert_eq!(
                tools::ToolRows(tx).pins("dev")?.len(),
                1,
                "taken off the Bot: unpinned"
            );
            mcp::Servers(tx).delete("bmcp_a")?;
            assert!(
                tools::ToolRows(tx).pins("doc")?.is_empty(),
                "the entry is gone"
            );

            let today = tools::day_of(1_700_000_000_000);
            tools::ToolRows(tx).record("dev", today - 100, &["old"], 1, 0)?;
            tools::ToolRows(tx).record(
                "dev",
                today - 3,
                &["steve_workflow", "files__read"],
                1,
                0,
            )?;
            tools::ToolRows(tx).record("dev", today, &["steve_workflow"], 0, 1)?;
            tools::ToolRows(tx).record("dev", today, &["steve_workflow"], 0, 1)?;
            tools::ToolRows(tx).record("doc", today, &["steve_workflow"], 0, 1)?;
            let u = tools::ToolRows(tx).usage("dev", today, 30)?;
            assert_eq!(
                u.iter()
                    .map(|x| (x.tool.as_str(), x.searched, x.called))
                    .collect::<Vec<_>>(),
                [("steve_workflow", 1, 2), ("files__read", 1, 0)]
            );
            let old: i64 = tx.legacy().query_row(
                "SELECT COUNT(*) FROM tool_usage WHERE tool = 'old'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(old, 1, "recording no longer prunes under the writer");
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    retain(
        &t.store,
        steve_store_sqlite::retention::policies::Kind::Tools,
        1_700_000_000_000,
    );
    assert_eq!(
        t.store
            .reader(|c| Ok::<_, steve_store::StoreError>(count(
                c,
                "SELECT count(*) FROM tool_usage WHERE tool='old'"
            )))
            .unwrap(),
        0
    );
}
