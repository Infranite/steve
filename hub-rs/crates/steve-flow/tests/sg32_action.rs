//! SG32: item creation preserves provenance links, transaction rollback and effects.
use serde_json::{Value, json};
use steve_flow::{
    FlowResult, engine, flows,
    graph::Graph,
    nodes::{NodeOutcome, RunCx, action::item_create_event},
    runs,
    testkit::FlowEnv,
};
use steve_modules::testkit::{ME, WS};
use steve_store::base::{ObjRef, links};
fn run_row(e: &FlowEnv, cfg: &Value) -> (Graph, runs::RunRow) {
    let graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},
        {"id":"a","kind":"action","title":"HTTP probe","config":cfg}],"edges":[{"from":"t","to":"a"}]})).unwrap();
    let row = e
        .env
        .store()
        .transaction(|tx| -> FlowResult<_> {
            let id = flows::create(
                tx,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name: "HTTP table",
                    description: "",
                    graph: &graph,
                    origin: "me",
                    origin_label: "",
                    enabled: false,
                },
                1,
            )?;
            let f = flows::own(tx, WS, &id)?;
            let id = engine::open_run(
                tx,
                &f,
                "manual",
                json!({"steps":{},"input":{}}),
                None,
                false,
                None,
                0,
                1,
            )?;
            Ok(runs::get(tx, &id)?.unwrap())
        })
        .unwrap();
    (graph, row)
}

#[tokio::test]
async fn sg32_item_create_provenance_and_failure_table() {
    for mode in [
        "manual",
        "external",
        "minimal",
        "link-failure",
        "create-failure",
    ] {
        let e = FlowEnv::new();
        if mode == "link-failure" {
            e.env.store().with_conn(|c|{c.execute_batch("CREATE TRIGGER sg32_link_refusal BEFORE INSERT ON links WHEN NEW.object_type='event' BEGIN SELECT RAISE(ABORT,'fixture link refused'); END;")?;Ok(())}).unwrap();
        }
        if mode == "create-failure" {
            e.env.store().with_conn(|c|{c.execute_batch("CREATE TRIGGER sg32_item_refusal BEFORE INSERT ON items BEGIN SELECT RAISE(ABORT,'fixture create refused'); END;")?;Ok(())}).unwrap();
        }
        let cfg = json!({"action":"item.create","title":"{{input.title}}","goal":"Do {{input.text}}","mode":"propose","accept":"auto","node_tag":"fixture"});
        let (graph, mut run) = run_row(&e, &cfg);
        let run_id = run.id.clone();
        run.context["event"] = if mode == "minimal" {
            json!({})
        } else {
            json!({"id":"sg32-event","source":if mode=="external"{"mail"}else{"manual"},"summary":"event summary","actor_party_id":"sg32-party"})
        };
        let mut cx = RunCx {
            flow: &e.flow,
            run,
            graph: &graph,
            node: graph.node("a").unwrap(),
            dry_run: false,
            sender: Value::Null,
            effects: Default::default(),
        };
        let out = item_create_event(
            &mut cx,
            &cfg,
            &json!({"title":"fixture title","text":"untrusted text"}),
        )
        .await;
        if mode == "create-failure" {
            assert!(
                matches!(out,NodeOutcome::Fail(ref e) if e.contains("建事项失败：")&&e.contains("fixture create refused")),
                "{out:?}"
            );
            assert!(cx.effects.item_id.is_none());
            continue;
        }
        let NodeOutcome::Next { output, .. } = out else {
            panic!("{out:?}")
        };
        let id = output["item_id"].as_str().unwrap();
        let req = e.core.created().pop().unwrap();
        assert_eq!(req.title, "fixture title");
        assert_eq!(req.propose, true);
        assert!(req.accept_auto);
        assert_eq!(req.run_id, run_id);
        assert_eq!(req.requester, None);
        assert!(req.goal.contains("untrusted text"));
        if mode == "external" {
            assert_ne!(req.goal, "Do untrusted text");
        } else {
            assert_eq!(req.goal, "Do untrusted text");
        }
        assert_eq!(cx.effects.outcome.as_deref(), Some("item"));
        assert_eq!(cx.effects.item_id.as_deref(), Some(id));
        assert!(cx.effects.label.unwrap().starts_with("提了方案"));
        assert_eq!(
            e.env
                .store()
                .reader(|c| steve_store::nodes::item_tag(c, id))
                .unwrap()
                .as_deref(),
            Some("fixture")
        );
        let ls = e
            .env
            .store()
            .reader(|c| links::from(c, WS, &ObjRef::new("item", id), None))
            .unwrap();
        let mut actual: Vec<_> = ls
            .iter()
            .map(|l| {
                (
                    l.predicate.as_str(),
                    l.object_type.as_str(),
                    l.object_id.as_str(),
                )
            })
            .collect();
        actual.sort();
        let mut want = if mode == "link-failure" {
            vec![]
        } else if mode == "minimal" {
            vec![("from", "flow_run", run_id.as_str())]
        } else {
            vec![
                ("from", "flow_run", run_id.as_str()),
                ("from", "event", "sg32-event"),
                ("raised_by", "party", "sg32-party"),
            ]
        };
        want.sort();
        assert_eq!(actual, want, "{mode}");
    }
}
