//! Migration 62: a Bot's MCP servers, and the rows a session's harness gets.

mod common;

use common::*;
use steve_store::bot_mcp::{self, Server, Var};

fn server(id: &str, bot: &str, name: &str, transport: &str) -> Server {
    Server {
        id: id.into(),
        workspace_id: "default".into(),
        bot_id: bot.into(),
        name: name.into(),
        transport: transport.into(),
        command: (transport == "stdio").then(|| "npx".into()),
        args: if transport == "stdio" {
            vec!["-y".into(), "@x/server".into()]
        } else {
            vec![]
        },
        env: vec![],
        url: (transport != "stdio").then(|| "https://mcp.example.com/mcp".into()),
        headers: vec![],
        credential_item: None,
        enabled: true,
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
fn a_secret_value_never_reaches_the_row() {
    let t = setup();
    let mut s = server("bmcp_1", "dev", "github", "stdio");
    s.env = vec![
        Var {
            name: "LOG".into(),
            value: Some("debug".into()),
            secret: false,
        },
        Var {
            name: "TOKEN".into(),
            value: Some("ghp_leak".into()),
            secret: true,
        },
    ];
    s.credential_item = Some("vi_1".into());
    t.store.transaction(|tx| bot_mcp::insert(tx, &s)).unwrap();
    let raw: String = t
        .store
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT env FROM bot_mcp_servers",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(!raw.contains("ghp_leak"), "{raw}");
    assert_eq!(
        raw,
        r#"[{"name":"LOG","value":"debug"},{"name":"TOKEN","secret":true}]"#
    );
    let got = t
        .store
        .reader(|c| bot_mcp::get(c, "bmcp_1"))
        .unwrap()
        .unwrap();
    assert_eq!(got.env[1].value, None);
    assert_eq!(got.secret_labels(), vec!["env:TOKEN".to_string()]);
    assert_eq!(got.args, vec!["-y".to_string(), "@x/server".to_string()]);

    // an update drops a secret's value too
    let mut h = server("bmcp_2", "dev", "remote", "http");
    t.store.transaction(|tx| bot_mcp::insert(tx, &h)).unwrap();
    h.headers = vec![Var {
        name: "Authorization".into(),
        value: Some("Bearer x".into()),
        secret: true,
    }];
    h.updated = 5;
    t.store.transaction(|tx| bot_mcp::update(tx, &h)).unwrap();
    let got = t
        .store
        .reader(|c| bot_mcp::get(c, "bmcp_2"))
        .unwrap()
        .unwrap();
    assert_eq!(got.headers[0].value, None);
    assert_eq!(
        got.secret_labels(),
        vec!["header:Authorization".to_string()]
    );
    assert_eq!(got.updated, 5);
}

#[test]
fn a_session_gets_its_bots_enabled_servers_only() {
    let t = setup();
    t.store
        .transaction(|tx| {
            bot_mcp::insert(tx, &server("bmcp_a", "dev", "b-files", "stdio"))?;
            bot_mcp::insert(tx, &server("bmcp_b", "dev", "a-web", "sse"))?;
            bot_mcp::insert(tx, &server("bmcp_c", "doc", "other", "http"))?;
            bot_mcp::set_enabled(tx, "bmcp_b", false, 9)
        })
        .unwrap();
    let names = |s: &str| {
        t.store
            .reader(|c| bot_mcp::enabled_for_session(c, s))
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("s_dev"), vec!["b-files".to_string()]);
    assert!(names("s_none").is_empty());
    assert!(names("missing").is_empty());
    // listed by name, disabled ones included
    let all: Vec<_> = t
        .store
        .reader(|c| bot_mcp::list(c, "dev"))
        .unwrap()
        .into_iter()
        .map(|x| (x.name, x.enabled))
        .collect();
    assert_eq!(all, vec![("a-web".into(), false), ("b-files".into(), true)]);
    // names are unique per Bot, in any case
    assert!(
        t.store
            .reader(|c| bot_mcp::name_taken(c, "dev", "B-FILES", None))
            .unwrap()
    );
    assert!(
        !t.store
            .reader(|c| bot_mcp::name_taken(c, "dev", "b-files", Some("bmcp_a")))
            .unwrap()
    );
    assert!(
        !t.store
            .reader(|c| bot_mcp::name_taken(c, "doc", "b-files", None))
            .unwrap()
    );
    assert!(
        t.store
            .transaction(|tx| bot_mcp::delete(tx, "bmcp_a"))
            .unwrap()
    );
    assert!(names("s_dev").is_empty());
    assert_eq!(
        t.store
            .reader(|c| bot_mcp::bot_of_session(c, "s_dev"))
            .unwrap()
            .as_deref(),
        Some("dev")
    );
}
