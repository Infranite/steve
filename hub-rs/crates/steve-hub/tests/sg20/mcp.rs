use serde_json::{Value, json};
use steve_store::{NewItem, NewWorkspace};

use super::fixture::Env;

fn text(r: &Value) -> &str {
    r["content"][0]["text"].as_str().unwrap()
}

#[tokio::test]
async fn sg20_outside_mcp_rejects_invalid_arguments_before_writing() {
    let e = Env::new().await;
    let dep = e.item("default", "prerequisite");
    let ws = e
        .store
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Foreign".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    let foreign = e.item(&ws.id, "foreign prerequisite");
    let conversation = e
        .store
        .items()
        .create(
            "default",
            &NewItem {
                kind: Some("chat".into()),
                title: "conversation".into(),
                bot_id: "writer".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let before = e.store.items().all("default").unwrap().len();
    let cases = vec![
        (
            json!({"goal":"ok","unexpected":true}),
            "Invalid arguments: unexpected property.",
        ),
        (
            json!({}),
            "Invalid arguments: goal must be a non-empty string.",
        ),
        (
            json!({"goal":"  "}),
            "Invalid arguments: goal must be a non-empty string.",
        ),
        (
            json!({"goal":2}),
            "Invalid arguments: goal must be a non-empty string.",
        ),
        (
            json!({"goal":"ok","deps":null}),
            "Invalid arguments: deps must be an array of item IDs.",
        ),
        (
            json!({"goal":"ok","deps":[1]}),
            "Invalid arguments: deps must be an array of item IDs.",
        ),
        (
            json!({"goal":"ok","deps":[""]}),
            "Invalid arguments: invalid prerequisite item IDs.",
        ),
        (
            json!({"goal":"ok","deps":["x".repeat(81)]}),
            "Invalid arguments: invalid prerequisite item IDs.",
        ),
        (
            json!({"goal":"ok","deps":["界".repeat(27)]}),
            "Invalid arguments: invalid prerequisite item IDs.",
        ),
        (
            json!({"goal":"ok","deps":vec!["x";201]}),
            "Invalid arguments: invalid prerequisite item IDs.",
        ),
        (
            json!({"goal":"ok","deps":["x".repeat(80)]}),
            "Prerequisite item not found in this workspace.",
        ),
        (
            json!({"goal":"ok","deps":(0..200).map(|n| format!("missing-{n}")).collect::<Vec<_>>()}),
            "Prerequisite item not found in this workspace.",
        ),
        (
            json!({"goal":"ok","deps":[dep.id,dep.id]}),
            "Invalid arguments: duplicate prerequisite item IDs.",
        ),
        (
            json!({"goal":"ok","deps":["missing"],"team":""}),
            "Prerequisite item not found in this workspace.",
        ),
        (
            json!({"goal":"ok","deps":[foreign.id]}),
            "Prerequisite item not found in this workspace.",
        ),
        (
            json!({"goal":"ok","deps":[conversation.id]}),
            "Prerequisite item not found in this workspace.",
        ),
        (
            json!({"goal":"ok","team":""}),
            "Invalid arguments: team must be a non-empty string.",
        ),
        (
            json!({"goal":"ok","team":null}),
            "Invalid arguments: team must be a non-empty string.",
        ),
        (
            json!({"goal":"ok","team":17}),
            "Invalid arguments: team must be a non-empty string.",
        ),
        (
            json!({"goal":"ok","team":"absent","bot":"absent"}),
            "No unambiguous Bot Team by that id or name in this workspace.",
        ),
        (
            json!({"goal":"ok","title":"explicit","bot":"absent"}),
            "No Bot absent here. The Bots: Steve (steve), Writer (writer).",
        ),
    ];
    for (args, expected) in cases {
        let r = e.mcp(args.clone()).await;
        assert_eq!(r["isError"], true, "{args}: {r}");
        assert_eq!(text(&r), expected, "{args}");
        assert_eq!(e.store.items().all("default").unwrap().len(), before);
    }
    assert!(e.store.sessions().of_item(&dep.id).unwrap().is_empty());
    e.stop().await;
}

#[tokio::test]
async fn sg20_outside_mcp_bot_scope_and_team_ambiguity_precede_creation() {
    let e = Env::new().await;
    let ws = e
        .store
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Foreign".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    e.store.transaction(|tx| -> steve_store::Result<_> {
        for (id, name, enabled, deleted, workspace) in [
            ("disabled","Disabled",0,0,"default"), ("deleted","Deleted",1,1,"default"), ("foreign","Foreign",1,0,ws.id.as_str()),
        ] {
            tx.execute("INSERT INTO bots (id,workspace_id,name,enabled,deleted,harness,updated) VALUES (?1,?2,?3,?4,?5,'claude-code',1)",
                rusqlite::params![id,workspace,name,enabled,deleted])?;
        }
        for (id, name, workspace) in [("a","Same","default"), ("b","same","default"), ("outside","Only outside",ws.id.as_str())] {
            tx.execute("INSERT INTO bot_teams (id,workspace_id,name,members) VALUES (?1,?2,?3,'[]')",
                rusqlite::params![id,workspace,name])?;
        }
        Ok(())
    }).unwrap();
    for bot in ["disabled", "deleted", "foreign"] {
        let r = e
            .mcp(json!({"goal":"no write","title":"explicit","bot":bot}))
            .await;
        assert_eq!(r["isError"], true);
        assert_eq!(
            text(&r),
            format!("No Bot {bot} here. The Bots: Steve (steve), Writer (writer).")
        );
    }
    for team in ["SAME", "outside", "Only outside"] {
        let r = e
            .mcp(json!({"goal":"no write","title":"explicit","bot":"writer","team":team}))
            .await;
        assert_eq!(r["isError"], true);
        assert_eq!(
            text(&r),
            "No unambiguous Bot Team by that id or name in this workspace."
        );
    }
    assert!(e.store.items().all("default").unwrap().is_empty());
    e.stop().await;
}

#[tokio::test]
async fn sg20_outside_mcp_explicit_creation_keeps_source_dependencies_and_response() {
    let e = Env::new().await;
    let dep = e.item("default", "not completed");
    e.store.transaction(|tx| -> steve_store::Result<_> {
        tx.execute("INSERT INTO bot_teams (id,workspace_id,name,members) VALUES ('writers','default','Writers',?)",
            [json!([{"bot":"writer","nodes":null}]).to_string()])?;
        Ok(())
    }).unwrap();
    for (index, bot, team) in [
        (0, "writer", None),
        (1, "wRiTeR", Some("legacy")),
        (2, "writer", Some("writers")),
        (3, "writer", Some(" wRiTeRs ")),
    ] {
        let title = format!("Work {index}");
        let mut args = json!({"goal":"  complete the work  ","title":format!(" {title} "),"bot":bot,"deps":[dep.id]});
        if let Some(team) = team {
            args["team"] = json!(team);
        }
        let r = e.mcp(args).await;
        assert!(r["isError"].is_null(), "args index {index}: {r}");
        let item = e
            .store
            .items()
            .all("default")
            .unwrap()
            .into_iter()
            .find(|i| i.title == title)
            .unwrap();
        assert_eq!(
            text(&r),
            format!(
                "Created {} «{}» for writer. Status: block. Read it with steve_item_read to follow its progress and result.",
                item.id, title
            )
        );
        assert_eq!(item.workspace_id, "default");
        assert_eq!(item.source, ("MCP · SG20 caller".into(), "mcp".into()));
        let actor: Option<String> = e
            .store
            .reader(|c| -> steve_store::Result<_> {
                Ok(c.query_row(
                    "SELECT requested_by FROM items WHERE id=?",
                    [&item.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(actor.as_deref(), Some(e.user.as_str()));
        assert_eq!(e.store.items().goal(&item.id).unwrap(), "complete the work");
        assert_eq!(e.store.deps().of(&item.id).unwrap(), [dep.id.clone()]);
        assert!(e.store.sessions().of_item(&item.id).unwrap().is_empty());
        assert!(
            e.store
                .decisions()
                .of_item(&item.id)
                .unwrap()
                .iter()
                .all(|d| d.status != "open")
        );
        if index >= 2 {
            let dispatch = e
                .store
                .reader(|c| steve_store::bot_dispatch::item_team(c, &item.id))
                .unwrap()
                .unwrap();
            assert_eq!(dispatch.0.id, "writers");
            assert_eq!(dispatch.1, "explicit");
        }
    }
    e.stop().await;
}

#[tokio::test]
async fn sg20_outside_mcp_shared_bot_and_database_refusal_keep_the_boundary() {
    let e = Env::new().await;
    let ws = e
        .store
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Bot home".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    let dep = e.item("default", "blocked prerequisite");
    e.store.transaction(|tx| -> steve_store::Result<_> {
        tx.execute("INSERT INTO bots (id,workspace_id,name,harness,updated) VALUES ('shared',?,'Shared writer','claude-code',1)", [&ws.id])?;
        tx.execute("INSERT INTO bot_workspaces (bot_id,workspace_id,created) VALUES ('shared','default',1)", [])?;
        tx.execute_batch("CREATE TRIGGER sg20_refuse BEFORE INSERT ON items WHEN NEW.title='Refused' BEGIN SELECT RAISE(ABORT,'sg20 insert refused'); END;")?;
        Ok(())
    }).unwrap();
    let r = e
        .mcp(json!({"goal":"shared work","title":"Shared","bot":" sHaReD wRiTeR ","deps":[dep.id]}))
        .await;
    assert!(r["isError"].is_null(), "{r}");
    let item = e
        .store
        .items()
        .all("default")
        .unwrap()
        .into_iter()
        .find(|i| i.title == "Shared")
        .unwrap();
    assert_eq!(item.bot_id.as_deref(), Some("shared"));
    let before = e.store.items().all("default").unwrap();
    let r = e
        .mcp(json!({"goal":"refuse work","title":"Refused","bot":"writer","deps":[dep.id]}))
        .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("sg20 insert refused"), "{r}");
    assert_eq!(e.store.items().all("default").unwrap(), before);
    assert_eq!(e.store.deps().of(&item.id).unwrap(), [dep.id]);
    e.stop().await;
}
