//! Test-only fault injection after the first actual Vault release; no production injection seam.
use serde_json::json;
use std::sync::{Arc, Mutex};
use steve_flow::corrections::{self, Action, Scope};
use steve_flow::exec::RunStatus;
use steve_flow::{
    FlowResult, NodeKind, engine, flows, graph::Graph, nodes::http::Http, testkit::FlowEnv,
};
use steve_modules::{
    testkit::{ME, WS, caller},
    vault::{Vault, items::NewItem, keystore::FileKeyStore},
};
use steve_store::testkit::flow as fixtures;
use steve_store::{DecisionRows, FlowWaits, RunRows};

struct SecretsFixture {
    _keys: tempfile::TempDir,
    e: FlowEnv,
    first: String,
    second: String,
}
fn fixture(high: bool) -> SecretsFixture {
    let keys = tempfile::tempdir().unwrap();
    let keypath = keys.path().to_path_buf();
    let slot = Arc::new(Mutex::new(None));
    let captured = slot.clone();
    let e = FlowEnv::with_late_nodes(vec![], vec![], move |env| {
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(&keypath).unwrap()),
        );
        *captured.lock().unwrap() = Some(vault.clone());
        vec![Arc::new(Http::new(vault)) as Arc<dyn NodeKind>]
    });
    let vault = slot.lock().unwrap().take().unwrap();
    vault.init(WS, ME).unwrap();
    let shared = vault
        .list_vaults(WS, ME)
        .unwrap()
        .into_iter()
        .find(|v| v.scope == "shared")
        .unwrap()
        .id;
    let create = |title: &str, sensitivity: &str| {
        let input:NewItem=serde_json::from_value(json!({"vault_id":shared,"template":"api_credential","title":title,"sensitivity":sensitivity,
            "fields":[{"label":"token","type":"password","value":"synthetic-test-value"}]})).unwrap();
        vault.create_item(WS, ME, input).unwrap().id
    };
    let first = create("first item", "normal");
    let second = create("second item", if high { "high" } else { "normal" });
    SecretsFixture {
        _keys: keys,
        e,
        first,
        second,
    }
}
fn open(f: &SecretsFixture, grant: bool) -> String {
    let graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},
        {"id":"h","kind":"http","title":"Use two items","config":{"url":"https://example.invalid/never-sent","headers":{
            "X-First":format!("{{{{secrets.{}.token}}}}",f.first),"X-Second":format!("{{{{secrets.{}.token}}}}",f.second)}}}],"edges":[{"from":"t","to":"h"}]})).unwrap();
    f.e.env
        .store()
        .write(|w| -> FlowResult<_> {
            let id = flows::create(
                w,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name: "TS5 secrets",
                    description: "",
                    graph: &graph,
                    origin: "me",
                    origin_label: "",
                    enabled: false,
                },
                1,
            )?;
            if grant {
                for item in [&f.first, &f.second] {
                    fixtures::flow_grant_in(
                        w,
                        fixtures::FlowGrantFixture {
                            id: &steve_store::uid("gr"),
                            ws: WS,
                            flow: &id,
                            capability: "secret.use",
                            resource_type: "vault_item",
                            resource: item,
                            until: i64::MAX,
                            actor: ME,
                        },
                    )?;
                }
            }
            let flow = flows::own(w, WS, &id)?;
            engine::open_run(
                w,
                &flow,
                "manual",
                json!({"steps": {}}),
                None,
                false,
                None,
                0,
                1,
            )
        })
        .unwrap()
}

#[tokio::test]
async fn partial_secret_release_is_finished_as_failed_when_the_second_release_changes() {
    for fault in ["needs-approval", "error"] {
        let f = fixture(false);
        let run = open(&f, true);
        fixtures::secret_release_fault(
            f.e.env.store(),
            &f.first,
            &f.second,
            fault == "needs-approval",
        )
        .unwrap();
        assert_eq!(
            engine::drive(&f.e.flow, &run).await.unwrap(),
            RunStatus::Failed
        );
        let row =
            f.e.env
                .store()
                .read(|r| steve_store::RunRows(r).get(&run))
                .unwrap()
                .unwrap();
        let uses = fixtures::secret_use_results(f.e.env.store()).unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].0, f.first);
        assert_eq!(uses[0].1, "failed");
        assert!(uses[0].2.is_some());
        assert_eq!(uses[0].3.as_deref(), Some(run.as_str()));
        let step =
            f.e.env
                .store()
                .read(|r| RunRows(r).steps(&run))
                .unwrap()
                .into_iter()
                .find(|step| step.node_id == "h")
                .unwrap();
        let (status, error) = (step.status, step.error);
        assert_eq!(status, "failed");
        assert_eq!(row.error, error);
        if fault == "needs-approval" {
            assert_eq!(error.as_deref(), Some("机密的授权刚刚失效了，请重新运行"));
        } else {
            let error = error.unwrap();
            assert!(error.starts_with(&format!("机密「{}」：", f.second)));
            assert!(error.contains("条目"));
        }
        assert!(!row.context.to_string().contains("synthetic-test-value"));
    }
}

#[tokio::test]
async fn a_mixed_grantable_and_high_pair_clears_the_persistent_grant_offer() {
    for high in [false, true] {
        let f = fixture(high);
        let run = open(&f, false);
        assert_eq!(
            engine::drive(&f.e.flow, &run).await.unwrap(),
            RunStatus::Waiting
        );
        let (matcher, options) =
            f.e.env
                .store()
                .read(|r| {
                    let wait = FlowWaits(r).get(&run, "h")?.unwrap();
                    let decision = DecisionRows(r).get(&wait.decision.unwrap())?.unwrap();
                    Ok((wait.matcher, decision.options))
                })
                .unwrap();
        assert_eq!(options.contains(&engine::GATE_ALWAYS.to_string()), !high);
        if high {
            assert!(
                matcher.get("grants").is_none(),
                "mixed high items cannot grant the batch: {matcher}"
            );
        } else {
            assert_eq!(matcher["grants"].as_array().unwrap().len(), 2);
        }
        assert_eq!(
            fixtures::secret_use_results(f.e.env.store()).unwrap().len(),
            0
        );
    }
}

#[tokio::test]
async fn correction_ops_remain_flow_then_rule_and_change_cards_keep_stored_text() {
    for existing in [false, true] {
        for action in [Action::Ignore, Action::Remind] {
            let e = FlowEnv::new();
            e.env
                .store()
                .write(|w| -> FlowResult<()> {
                    let target = if existing {
                        let g = Graph::starter("manual");
                        Some(flows::create(
                            w,
                            WS,
                            ME,
                            &flows::NewFlow {
                                kind: "flow",
                                name: "Original",
                                description: "",
                                graph: &g,
                                origin: "me",
                                origin_label: "",
                                enabled: false,
                            },
                            1,
                        )?)
                    } else {
                        None
                    };
                    fixtures::correction_signal(w, WS, target.as_deref())?;
                    Ok(())
                })
                .unwrap();
            let corrected =
                corrections::correct(&e.flow, &caller(WS, ME), "ts5-event", action, Scope::Group)
                    .await
                    .unwrap();
            let cs = e
                .env
                .store()
                .read(|r| {
                    steve_store::base::changesets::ChangeSets(r)
                        .get(corrected.changeset_id.as_deref().unwrap())
                })
                .unwrap()
                .unwrap();
            assert_eq!(
                cs.ops
                    .iter()
                    .map(|op| op["op"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [corrections::OP_FLOW, corrections::OP_RULE]
            );
            let before = cs.before.as_ref().unwrap();
            assert_eq!(
                before
                    .iter()
                    .map(|b| b["op"]["op"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [corrections::OP_FLOW, corrections::OP_RULE]
            );
            assert_eq!(cs.changes.len(), 2);
            assert_eq!(
                cs.changes[0].label,
                if existing {
                    "流程「Original」"
                } else {
                    "新流程「纠正：邮件 · mail.new」"
                }
            );
            assert_eq!(cs.changes[0].from, if existing { "" } else { "（没有）" });
            let verb = if action == Action::Ignore {
                "忽略"
            } else {
                "提醒你"
            };
            assert_eq!(
                cs.changes[0].to,
                format!("例外：同组（ts5-group） → {verb}")
            );
            assert_eq!(cs.changes[1].label, "规则记忆");
            assert_eq!(cs.changes[1].from, "");
            assert_eq!(
                cs.changes[1].to,
                format!("邮件里「Fixture summary」这类以后{verb}")
            );
        }
    }
}
