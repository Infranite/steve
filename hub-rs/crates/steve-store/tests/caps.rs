//! The capabilities library: per-Bot selection, secret labels, pins, and tool usage.

mod common;

use common::*;
use steve_store::caps::{self, mcp, skills};

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
        .transaction(|tx| {
            mcp::insert(tx, &server("bmcp_a", "files", "stdio"))?;
            mcp::insert(tx, &server("bmcp_b", "remote", "http"))?;
            mcp::insert(tx, &server("bmcp_c", "unused", "sse"))?;
            skills::insert(tx, &skill("bskill_a", "review"))?;
            skills::insert(tx, &skill("bskill_b", "drafts"))?;
            caps::set(tx, "dev", "mcp", "bmcp_a", true, 1)?;
            caps::set(tx, "dev", "mcp", "bmcp_b", false, 1)?;
            caps::set(tx, "doc", "mcp", "bmcp_b", true, 1)?;
            caps::set(tx, "dev", "skill", "bskill_a", true, 1)?;
            caps::set(tx, "dev", "skill", "bskill_b", false, 1)?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    t.store
        .with_conn(|c| {
            let got: Vec<String> = mcp::enabled_for_session(c, "s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["files"]);
            assert!(mcp::enabled_for_session(c, "s_none")?.is_empty());
            let got: Vec<String> = skills::enabled_for_session(c, "s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["review"]);
            let used: Vec<String> = caps::of_entry(c, "mcp", "bmcp_b")?
                .into_iter()
                .map(|a| a.bot_id)
                .collect();
            assert_eq!(used, ["dev", "doc"]);
            assert!(mcp::name_taken(c, "default", "FILES", None)?);
            assert!(!mcp::name_taken(c, "default", "files", Some("bmcp_a"))?);
            Ok(())
        })
        .unwrap();
    // switching on, and deleting an entry takes it from every Bot
    t.store
        .transaction(|tx| {
            caps::set(tx, "dev", "mcp", "bmcp_b", true, 2)?;
            mcp::delete(tx, "bmcp_a")?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    t.store
        .with_conn(|c| {
            let got: Vec<String> = mcp::enabled_for_session(c, "s_dev")?
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(got, ["remote"]);
            assert!(caps::of_entry(c, "mcp", "bmcp_a")?.is_empty());
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
    t.store.transaction(|tx| mcp::insert(tx, &s)).unwrap();
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
        .with_conn(|c| mcp::get(c, "bmcp_1"))
        .unwrap()
        .unwrap();
    assert_eq!(back.secret_labels(), ["env:TOKEN"]);
}

/// Migration 67: a Bot's pins, gone with the entry or when the entry is taken off the Bot; usage
/// summed per tool over the last days, old rows dropped.
#[test]
fn pins_follow_their_entries_and_usage_sums_by_tool() {
    use caps::tools;
    let t = setup();
    t.store
        .transaction(|tx| {
            mcp::insert(tx, &server("bmcp_a", "files", "stdio"))?;
            caps::set(tx, "dev", "mcp", "bmcp_a", true, 1)?;
            assert!(tools::set_pin(tx, "dev", "mcp", "bmcp_a", true, 2)?);
            assert!(
                !tools::set_pin(tx, "dev", "mcp", "bmcp_a", true, 3)?,
                "once"
            );
            assert!(tools::set_pin(
                tx,
                "dev",
                "tool",
                "steve_workflow",
                true,
                4
            )?);
            assert!(tools::set_pin(tx, "doc", "mcp", "bmcp_a", true, 5)?);
            let kinds: Vec<String> = tools::pins(tx, "dev")?
                .into_iter()
                .map(|p| p.kind)
                .collect();
            assert_eq!(kinds, ["mcp", "tool"]);
            caps::remove(tx, "dev", "mcp", "bmcp_a")?;
            assert_eq!(
                tools::pins(tx, "dev")?.len(),
                1,
                "taken off the Bot: unpinned"
            );
            mcp::delete(tx, "bmcp_a")?;
            assert!(tools::pins(tx, "doc")?.is_empty(), "the entry is gone");

            let today = tools::day_of(1_700_000_000_000);
            tools::record(tx, "dev", today - 100, &["old"], 1, 0)?;
            tools::record(
                tx,
                "dev",
                today - 3,
                &["steve_workflow", "files__read"],
                1,
                0,
            )?;
            tools::record(tx, "dev", today, &["steve_workflow"], 0, 1)?;
            tools::record(tx, "dev", today, &["steve_workflow"], 0, 1)?;
            tools::record(tx, "doc", today, &["steve_workflow"], 0, 1)?;
            let u = tools::usage(tx, "dev", today, 30)?;
            assert_eq!(
                u.iter()
                    .map(|x| (x.tool.as_str(), x.searched, x.called))
                    .collect::<Vec<_>>(),
                [("steve_workflow", 1, 2), ("files__read", 1, 0)]
            );
            let old: i64 = tx.query_row(
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
