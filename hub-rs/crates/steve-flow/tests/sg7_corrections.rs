//! Correction target and condition decisions, pinned before splitting the existing function.
use rusqlite::params;
use serde_json::json;
use steve_flow::{
    FlowResult,
    corrections::{self, Action, Scope},
    flows,
    graph::Graph,
    testkit::FlowEnv,
};
use steve_modules::testkit::{ME, WS, WS2, caller};

fn event(
    e: &FlowEnv,
    target: &str,
    group: Option<&str>,
    thread: Option<&str>,
    actor: Option<&str>,
) -> Option<String> {
    e.env.store().transaction(|tx|->FlowResult<_>{
        let flow=match target {
            "none"=>None,"missing"=>Some("flw_missing".into()),
            _=>{
                let graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"mail","types":["mail.new"]}},{"id":"a","kind":"action","config":{"action":"ignore","reason":"old"}}],"edges":[{"from":"t","to":"a"}]})).unwrap();
                let id=flows::create(tx,if target=="foreign"{WS2}else{WS},ME,&flows::NewFlow{kind:if target=="schedule"{"schedule"}else{"flow"},name:"original",description:"",graph:&graph,origin:"me",origin_label:"",enabled:false},1000)?;
                if target=="deleted"{tx.execute("UPDATE flows SET deleted=1 WHERE id=?",[&id])?;}
                Some(id)
            }
        };
        tx.execute("INSERT INTO ext_events(id,workspace_id,source,type,dedupe_key,thread_key,group_key,actor_account_id,occurred,received,summary,outcome,flow_id) VALUES ('ev_sg7',?,'mail','mail.new','sg7',?,?,?,?,?,'quoted summary','recorded',?)",
            params![WS,thread,group,actor,1000,1000,flow])?;
        Ok(flow)
    }).unwrap()
}
fn counts(e: &FlowEnv) -> (i64, i64, i64) {
    e.env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>((
                c.query_row("SELECT count(*) FROM signal_corrections", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM changesets", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM mems", [], |r| r.get(0))?,
            ))
        })
        .unwrap()
}

#[tokio::test]
async fn sg7_correction_targets_and_stored_plans_table() {
    for target in ["none", "missing", "flow", "schedule", "deleted", "foreign"] {
        for action in [Action::Ignore, Action::Remind] {
            let e = FlowEnv::new();
            let original = event(
                &e,
                target,
                Some("group \"quoted\""),
                Some("thread"),
                Some("sender"),
            );
            let before = original
                .as_ref()
                .and_then(|id| e.env.store().reader(|c| flows::get(c, id)).unwrap());
            let result = steve_i18n::scope(
                steve_i18n::EN,
                corrections::correct(&e.flow, &caller(WS, ME), "ev_sg7", action, Scope::Group),
            )
            .await
            .unwrap();
            assert!(result.item_id.is_none());
            let flow = result.flow_id.as_deref().unwrap();
            let current = e.env.store().reader(|c| flows::own(c, WS, flow)).unwrap();
            assert_eq!(
                current.version,
                if target == "flow" { 2 } else { 1 },
                "{target}"
            );
            assert_eq!(flow == original.as_deref().unwrap_or(""), target == "flow");
            if target != "flow" {
                if let Some(before) = before {
                    assert_eq!(
                        e.env
                            .store()
                            .reader(|c| flows::get(c, &before.id))
                            .unwrap()
                            .unwrap(),
                        before,
                        "{target}: leave rejected target alone"
                    );
                }
            }
            let graph = Graph::from_value(&current.graph).unwrap();
            let filter = graph.nodes.iter().find(|n| n.kind == "filter").unwrap();
            assert_eq!(
                filter.config["rules"][0]["when"],
                r#"event.group_key == "group \"quoted\"""#
            );
            let effect = graph
                .nodes
                .iter()
                .find(|n| n.id == format!("{}_a", filter.id))
                .unwrap();
            assert_eq!(
                effect.config["action"],
                if action == Action::Ignore {
                    "ignore"
                } else {
                    "notify"
                }
            );
            let cs = result.changeset_id.as_deref().unwrap();
            let stored = e
                .env
                .store()
                .reader(|c| steve_store::base::changesets::get(c, cs))
                .unwrap()
                .unwrap();
            let value = serde_json::to_value(stored).unwrap();
            assert_eq!(value["status"], "applied");
            assert!(
                value["title"].as_str().unwrap().contains("纠正"),
                "stored title stays zh-CN under English request: {value}"
            );
            let list = e
                .env
                .store()
                .reader(|c| corrections::list(c, WS, "ev_sg7"))
                .unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0]["id"], result.id);
            assert_eq!(list[0]["flow_id"], flow);
            assert_eq!(list[0]["changeset_id"], cs);
            assert_eq!(list[0]["scope"], "group");
            assert_eq!(list[0]["action"], action.code());
            let mem = list[0]["mem_id"].as_str().unwrap();
            let text: String = e
                .env
                .store()
                .reader(|c| {
                    Ok::<_, steve_store::StoreError>(c.query_row(
                        "SELECT text FROM mems WHERE id=?",
                        [mem],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert!(text.contains("quoted summary"));
            assert!(text.ends_with(if action == Action::Ignore {
                "忽略"
            } else {
                "提醒你"
            }));
            assert!(e.env.gateway.calls.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn sg7_correction_condition_precedence_and_rejection_table() {
    for (group, thread, actor, scope, expected) in [
        (
            Some("group"),
            Some("thread"),
            Some("sender"),
            Scope::Group,
            Some(r#"event.group_key == "group""#),
        ),
        (
            None,
            Some("thread"),
            Some("sender"),
            Scope::Group,
            Some(r#"event.actor_account_id == "sender""#),
        ),
        (
            Some("group"),
            Some("thread"),
            Some("sender"),
            Scope::One,
            Some(r#"event.thread_key == "thread""#),
        ),
        (None, None, None, Scope::Group, None),
        (Some("group"), None, Some("sender"), Scope::One, None),
        (Some(""), Some(""), Some(""), Scope::Group, None),
    ] {
        let e = FlowEnv::new();
        event(&e, "none", group, thread, actor);
        let before = counts(&e);
        let result =
            corrections::correct(&e.flow, &caller(WS, ME), "ev_sg7", Action::Ignore, scope).await;
        if let Some(condition) = expected {
            let r = result.unwrap();
            let f = e
                .env
                .store()
                .reader(|c| flows::get(c, r.flow_id.as_deref().unwrap()))
                .unwrap()
                .unwrap();
            let g = Graph::from_value(&f.graph).unwrap();
            assert_eq!(
                g.nodes.iter().find(|n| n.kind == "filter").unwrap().config["rules"][0]["when"],
                condition
            );
            assert_eq!(counts(&e).0, before.0 + 1);
        } else {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("认不出同类"), "{error}");
            assert_eq!(counts(&e), before);
            assert_eq!(
                e.env
                    .store()
                    .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                        "SELECT count(*) FROM flows WHERE deleted=0",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?))
                    .unwrap(),
                0
            );
        }
    }
}
