//! SG3 characterization: exercise the real engine/store boundary before splitting drive_inner.
//! Node effects use sentinel IDs; creating those objects is the node's responsibility.
use async_trait::async_trait;
use rusqlite::params;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_flow::exec::{MAX_RETRIES, RETRY_AFTER_MS, RunStatus};
use steve_flow::nodes::{Effects, Grant, NodeKind, NodeOutcome, RunCx, WaitSpec};
use steve_flow::testkit::FlowEnv;
use steve_flow::{FlowResult, engine, flows, graph::Graph, runs};
use steve_modules::testkit::{ME, WS};
use steve_store::NewDecision;

const NOW: i64 = 1_795_180_000_000;
struct Probe {
    kind: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
}
impl Probe {
    fn execute(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value, method: &str) -> NodeOutcome {
        let stored = cx
            .flow
            .store()
            .reader(|c| runs::get(c, &cx.run.id))
            .unwrap()
            .unwrap();
        let steps = cx
            .flow
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT count(*) FROM flow_run_steps WHERE run_id=?",
                    [&cx.run.id],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        self.calls.lock().unwrap().push(json!({"node":cx.node.id,"method":method,"input":input,
            "stored_input":stored.context["input"],"stored_cost":stored.cost,"steps_before":steps}));
        if cx.node.id != "p" {
            return NodeOutcome::next(input.clone());
        }
        if cfg["cancel"] == true {
            cx.flow
                .store()
                .transaction(|tx| {
                    let row = runs::get(tx, &cx.run.id)?.unwrap();
                    engine::cancel(tx, &row, NOW - 7)
                })
                .unwrap();
        }
        if cfg["decorate"] != false {
            cx.effects = Effects {
                outcome: Some("item".into()),
                label: Some("probe label".into()),
                item_id: Some("item-sentinel".into()),
                decision_id: Some("decision-sentinel".into()),
                reason: Some("reason-sentinel".into()),
                handled_by: Some("handler-sentinel".into()),
                cost: 0.25,
            };
        }
        match cfg["outcome"].as_str().unwrap_or("advance") {
            "retry" => NodeOutcome::Retry("again".into()),
            "fail" => NodeOutcome::Fail("broken".into()),
            "wait" | "decision" => NodeOutcome::Wait(Box::new(WaitSpec {
                mode: "delay",
                until: Some(NOW + 12_345),
                matcher: json!({"marker":"wait"}),
                output: json!({"pending":true}),
                decision: (cfg["outcome"] == "decision").then(|| NewDecision {
                    workspace_id: WS.into(),
                    kind: "approve".into(),
                    subject_type: "flow_step".into(),
                    subject_id: "replaced".into(),
                    item_id: None,
                    session_id: None,
                    question: "Continue probe?".into(),
                    draft: None,
                    options: Some(vec!["yes".into(), "no".into()]),
                    requested_by: ME.into(),
                }),
            })),
            _ => NodeOutcome::Next {
                output: json!({"phase":"advance","method":method}),
                branch: cfg["branch"].as_str().map(str::to_owned),
            },
        }
    }
}
#[async_trait]
impl NodeKind for Probe {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    fn effectful(&self, cfg: &Value) -> bool {
        cfg["effect"] == true
    }
    fn grant(&self, cfg: &Value) -> Option<Grant> {
        (cfg["grant"] == true).then(|| Grant::new("probe.effect", "host", "example.invalid"))
    }
    fn gate_note(&self, _: &RunCx<'_>, _: &Value) -> Option<String> {
        Some("probe note".into())
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        self.execute(cx, cfg, input, "run")
    }
    async fn dry_run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        self.execute(cx, cfg, input, "dry_run")
    }
}
struct Env {
    e: FlowEnv,
    id: String,
    calls: Arc<Mutex<Vec<Value>>>,
}
fn environment(kind: &'static str, cfg: Value, on_fail: &str, dry: bool, resume: Value) -> Env {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let e = FlowEnv::with(
        vec![],
        vec![
            Arc::new(Probe {
                kind,
                calls: calls.clone(),
            }),
            Arc::new(Probe {
                kind: "sg3_tail",
                calls: calls.clone(),
            }),
        ],
        vec![],
    );
    e.env.clock.set(NOW);
    let graph=Graph::from_value(&json!({"nodes":[
        {"id":"t","kind":"trigger","config":{"source":"manual"}},
        {"id":"p","kind":kind,"title":"probe","config":cfg,"on_fail":on_fail},
        {"id":"z","kind":"sg3_tail","config":{}},
        {"id":"q","kind":"sg3_tail","config":{}}
    ],"edges":[{"from":"t","to":"p"},{"from":"p","to":"z","label":"pass"},{"from":"p","to":"q","label":"later"}]})).unwrap();
    let id=e.env.store().transaction(|tx| -> FlowResult<_> {
        let flow=flows::create(tx,WS,ME,&flows::NewFlow {kind:"flow",name:"SG3 probe",description:"",graph:&graph,origin:"me",origin_label:"",enabled:false},NOW)?;
        let f=flows::own(tx,WS,&flow)?;
        let id=engine::open_run(tx,&f,"manual",json!({"input":{"name":"Ada"},"steps":{},"label_prefix":"prefix:",
            "approved":{},"source":"digest.bot","startup":{"format":1,"revision":{"flow":flow},"state":"started","attempts":0,"limit":3}}),None,dry,None,0,NOW)?;
        let mut row=runs::get(tx,&id)?.unwrap(); row.cursor=Some("p".into());row.attempts=1;row.retry_at=Some(NOW-1);row.error=Some("previous".into());
        if !resume.is_null() {row.context["resume"]=resume;}
        runs::save(tx,&row)?;Ok(id)
    }).unwrap();
    e.env.take_events();
    Env { e, id, calls }
}
fn row(e: &Env) -> runs::RunRow {
    e.e.env
        .store()
        .reader(|c| runs::get(c, &e.id))
        .unwrap()
        .unwrap()
}
fn edit(e: &Env, f: impl FnOnce(&mut runs::RunRow)) {
    e.e.env
        .store()
        .transaction(|tx| {
            let mut r = runs::get(tx, &e.id)?.unwrap();
            f(&mut r);
            runs::save(tx, &r)
        })
        .unwrap();
    e.e.env.take_events();
}
fn steps(e: &Env) -> Vec<Value> {
    e.e.env.store().reader(|c| {
        let mut s=c.prepare("SELECT node_id,status,input,output,branch,error,started FROM flow_run_steps WHERE run_id=? ORDER BY seq")?;
        Ok::<_,steve_store::StoreError>(s.query_map([&e.id],|r| {
            let input:String=r.get(2)?;let output:String=r.get(3)?;
            Ok(json!({"node":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"input":serde_json::from_str::<Value>(&input).unwrap(),
                "output":serde_json::from_str::<Value>(&output).unwrap(),"branch":r.get::<_,Option<String>>(4)?,"error":r.get::<_,Option<String>>(5)?,"started":r.get::<_,i64>(6)?}))
        })?.collect::<Result<_,_>>()?)
    }).unwrap()
}
fn counts(e: &Env) -> (i64, i64) {
    e.e.env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>((
                c.query_row(
                    "SELECT count(*) FROM flow_waits WHERE run_id=?",
                    [&e.id],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT count(*) FROM decisions WHERE subject_id=?",
                    [format!("{}:p", e.id)],
                    |r| r.get(0),
                )?,
            ))
        })
        .unwrap()
}
fn events(e: &Env) -> Vec<String> {
    e.e.env
        .take_events()
        .into_iter()
        .filter_map(|event| {
            let v = serde_json::to_value(event).unwrap();
            v["type"].as_str().map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn outcome_and_cancellation_table_preserves_steps_effects_and_retry_boundaries() {
    for (outcome, attempts, status, step_status) in [
        ("advance", 1, "done", "ok"),
        ("wait", 1, "waiting", "waiting"),
        ("decision", 1, "waiting", "waiting"),
        ("retry", 0, "waiting", "failed"),
        ("retry", MAX_RETRIES, "failed", "failed"),
        ("fail", 1, "failed", "failed"),
    ] {
        for cancelled in [false, true] {
            let e = environment(
                "sg3_probe",
                json!({"outcome":outcome,"cancel":cancelled}),
                "stop",
                false,
                Value::Null,
            );
            edit(&e, |r| r.attempts = attempts);
            let before = row(&e);
            let expected = if cancelled { "cancelled" } else { status };
            assert_eq!(
                engine::drive(&e.e.flow, &e.id).await.unwrap().as_str(),
                expected,
                "{outcome}/{attempts}/{cancelled}"
            );
            let r = row(&e);
            let ss = steps(&e);
            assert_eq!(r.status, expected);
            assert_eq!(ss[0]["status"], step_status);
            assert_eq!(ss[0]["input"], json!({"name":"Ada"}));
            assert_eq!(ss[0]["started"], NOW);
            assert_eq!(r.cost, 0.25);
            assert_eq!(r.outcome.as_deref(), Some("item"));
            assert_eq!(r.work_item_id.as_deref(), Some("item-sentinel"));
            assert_eq!(r.context["reason"], "reason-sentinel");
            assert_eq!(r.context["handled_by"], "handler-sentinel");
            assert_eq!(r.context["source"], before.context["source"]);
            assert_eq!(r.context["startup"], before.context["startup"]);
            assert!(r.context.get("resume").is_none());
            let opened = outcome == "decision" && !cancelled;
            let waiting = matches!(outcome, "wait" | "decision") && !cancelled;
            assert_eq!(counts(&e), (i64::from(waiting), i64::from(opened)));
            assert_eq!(
                r.context["held_outcome"],
                if opened { json!(true) } else { Value::Null }
            );
            if opened {
                let id = ss[0]["output"]["decision_id"].as_str().unwrap();
                assert_eq!(r.context["decision_id"], id);
                let subject: String =
                    e.e.env
                        .store()
                        .reader(|c| {
                            Ok::<_, steve_store::StoreError>(c.query_row(
                                "SELECT subject_id FROM decisions WHERE id=?",
                                [id],
                                |x| x.get(0),
                            )?)
                        })
                        .unwrap();
                assert_eq!(subject, format!("{}:p", e.id));
            } else {
                assert_eq!(r.context["decision_id"], "decision-sentinel");
            }
            let finished = !cancelled && outcome == "advance";
            assert_eq!(ss.len(), if finished { 2 } else { 1 });
            assert_eq!(r.cursor.as_deref(), if finished { None } else { Some("p") });
            assert_eq!(
                r.ended,
                if cancelled {
                    Some(NOW - 7)
                } else if expected == "waiting" {
                    None
                } else {
                    Some(NOW)
                }
            );
            let retry = !cancelled && outcome == "retry" && attempts < MAX_RETRIES;
            assert_eq!(
                r.attempts,
                if finished {
                    0
                } else if retry {
                    attempts + 1
                } else {
                    attempts
                }
            );
            assert_eq!(
                r.retry_at,
                if cancelled || finished {
                    None
                } else if retry {
                    Some(NOW + RETRY_AFTER_MS)
                } else {
                    Some(NOW - 1)
                }
            );
            let error = if finished {
                None
            } else if cancelled || waiting {
                Some("previous")
            } else if outcome == "retry" {
                Some("again")
            } else {
                Some("broken")
            };
            assert_eq!(r.error.as_deref(), error);
            let label = if !cancelled && outcome == "fail" {
                "prefix:broken"
            } else if !cancelled && outcome == "retry" && !retry {
                "prefix:again"
            } else {
                "prefix:probe label"
            };
            assert_eq!(r.outcome_label, label);
            let expected_events = if cancelled || finished {
                vec!["run", "flow", "run", "flow"]
            } else if opened {
                vec!["decision", "run", "flow"]
            } else {
                vec!["run", "flow"]
            };
            assert_eq!(events(&e), expected_events);
            if finished {
                let calls = e.calls.lock().unwrap();
                assert_eq!(calls[1]["node"], "z");
                assert_eq!(calls[1]["steps_before"], 1);
                assert_eq!(calls[1]["stored_cost"], 0.25);
                assert_eq!(calls[1]["stored_input"], ss[0]["output"]);
            }
        }
    }
}

#[tokio::test]
async fn resume_dry_run_and_failure_policy_table_keeps_precedence_and_node_dispatch() {
    // name, node kind, dry, effect, resume, on_fail, outcome, expected status, primary method
    for (name, kind, dry, effect, resume, on_fail, outcome, status, method) in [
        (
            "refusal wins over error",
            "sg3_probe",
            false,
            true,
            json!({"node":"p","gate":engine::GATE_NO,"error":"resume error"}),
            "stop",
            "advance",
            "done",
            None,
        ),
        (
            "error wins over approval",
            "sg3_probe",
            false,
            true,
            json!({"node":"p","gate":engine::GATE_YES,"error":"resume error"}),
            "stop",
            "advance",
            "failed",
            None,
        ),
        (
            "output resumes without rerunning effect",
            "sg3_probe",
            false,
            true,
            json!({"node":"p","output":{"saved":true}}),
            "stop",
            "advance",
            "done",
            None,
        ),
        (
            "other node resume is ignored",
            "sg3_probe",
            false,
            false,
            json!({"node":"other","gate":engine::GATE_NO}),
            "stop",
            "advance",
            "done",
            Some("run"),
        ),
        (
            "approved dry resume still executes",
            "sg3_probe",
            true,
            true,
            json!({"node":"p","gate":engine::GATE_YES}),
            "stop",
            "advance",
            "done",
            Some("run"),
        ),
        (
            "dry effect is skipped",
            "sg3_probe",
            true,
            true,
            Value::Null,
            "stop",
            "advance",
            "done",
            None,
        ),
        (
            "ordinary dry node uses dry_run",
            "sg3_probe",
            true,
            false,
            Value::Null,
            "stop",
            "advance",
            "done",
            Some("dry_run"),
        ),
        (
            "dry bot chooses preference",
            "bot",
            true,
            false,
            Value::Null,
            "stop",
            "advance",
            "done",
            None,
        ),
        (
            "dry approval chooses preference",
            "approve",
            true,
            false,
            Value::Null,
            "stop",
            "advance",
            "done",
            None,
        ),
        (
            "dry wait passes without waiting",
            "wait",
            true,
            false,
            Value::Null,
            "stop",
            "wait",
            "done",
            None,
        ),
        (
            "ungated effect waits for approval",
            "sg3_probe",
            false,
            true,
            Value::Null,
            "stop",
            "advance",
            "waiting",
            None,
        ),
        (
            "continue records failed step then advances",
            "sg3_probe",
            false,
            false,
            Value::Null,
            "continue",
            "fail",
            "done",
            Some("run"),
        ),
        (
            "failure branch selects edge",
            "sg3_probe",
            false,
            false,
            Value::Null,
            "branch:later",
            "fail",
            "done",
            Some("run"),
        ),
    ] {
        let e = environment(
            kind,
            json!({"effect":effect,"outcome":outcome,"timeout_label":"pass","goal":"{{input.name}}"}),
            on_fail,
            dry,
            resume,
        );
        edit(&e, |r| r.context["prefer"] = json!(["q"]));
        assert_eq!(
            engine::drive(&e.e.flow, &e.id).await.unwrap().as_str(),
            status,
            "{name}"
        );
        let r = row(&e);
        let ss = steps(&e);
        let calls = e.calls.lock().unwrap();
        let primary: Vec<_> = calls.iter().filter(|v| v["node"] == "p").collect();
        assert_eq!(primary.len(), usize::from(method.is_some()), "{name}");
        if let Some(method) = method {
            assert_eq!(primary[0]["method"], method, "{name}");
        }
        assert!(r.context.get("resume").is_none(), "{name}");
        if name == "refusal wins over error" {
            assert_eq!(ss[0]["status"], "skipped");
            assert_eq!(ss[0]["output"], json!({"refused":true}));
            assert_eq!(r.outcome_label, "你没有批准「probe」，没有执行");
            assert_eq!(r.cursor, None);
            assert_eq!(r.attempts, 1);
            assert_eq!(r.retry_at, Some(NOW - 1));
            assert_eq!(r.error.as_deref(), Some("previous"));
        } else if name == "error wins over approval" {
            assert_eq!(ss[0]["error"], "resume error");
            assert_eq!(r.outcome_label, "prefix:resume error");
        } else if name == "output resumes without rerunning effect" {
            assert_eq!(ss[0]["output"], json!({"saved":true}));
        }
        if name == "approved dry resume still executes" {
            assert_eq!(r.context["approved"]["p"], true);
        }
        if kind == "bot" || kind == "approve" || kind == "wait" || on_fail == "branch:later" {
            assert_eq!(ss[0]["branch"], "later", "{name}");
        }
        if on_fail != "stop" {
            assert_eq!(ss[0]["status"], "failed");
            assert_eq!(ss[0]["output"], json!({"error":"broken"}));
        }
        if status == "waiting" {
            assert_eq!(counts(&e), (1, 1));
            assert_eq!(r.outcome.as_deref(), Some("need"));
            assert_eq!(r.context["held_outcome"], true);
            assert_eq!(r.outcome_label, "prefix:等你批准：probe");
            let draft: String =
                e.e.env
                    .store()
                    .reader(|c| {
                        Ok::<_, steve_store::StoreError>(c.query_row(
                            "SELECT draft FROM decisions WHERE subject_id=?",
                            [format!("{}:p", e.id)],
                            |x| x.get(0),
                        )?)
                    })
                    .unwrap();
            assert!(draft.contains("Ada"));
        }
    }
}

#[tokio::test]
async fn failed_step_transaction_rolls_back_decisions_waits_effects_and_publications() {
    for outcome in ["advance", "decision", "retry", "fail"] {
        let e = environment(
            "sg3_probe",
            json!({"outcome":outcome}),
            "stop",
            false,
            Value::Null,
        );
        let before = row(&e);
        e.e.env.store().with_conn(|c| {c.execute_batch("CREATE TRIGGER reject_step_save BEFORE UPDATE ON flow_runs BEGIN SELECT RAISE(ABORT,'step save unavailable'); END")?;Ok(())}).unwrap();
        let error = engine::drive(&e.e.flow, &e.id).await.unwrap_err();
        assert!(error.to_string().contains("step save unavailable"));
        assert_eq!(row(&e), before, "{outcome}");
        assert!(steps(&e).is_empty());
        assert_eq!(counts(&e), (0, 0));
        assert!(events(&e).is_empty());
    }
}

#[tokio::test]
async fn early_exit_table_keeps_graph_validation_before_cursor_completion() {
    for (name, status, cursor, bad_graph, expected) in [
        (
            "terminal ignores malformed graph",
            "cancelled",
            Some("p"),
            true,
            RunStatus::Cancelled,
        ),
        (
            "valid graph with no cursor finishes",
            "running",
            None,
            false,
            RunStatus::Done,
        ),
        (
            "invalid snapshot falls back to current graph",
            "running",
            None,
            true,
            RunStatus::Done,
        ),
        (
            "invalid graph is checked before no cursor",
            "running",
            None,
            true,
            RunStatus::Failed,
        ),
        (
            "missing node fails",
            "running",
            Some("missing"),
            false,
            RunStatus::Failed,
        ),
    ] {
        let e = environment("sg3_probe", json!({}), "stop", false, Value::Null);
        edit(&e, |r| {
            r.status = status.into();
            r.cursor = cursor.map(str::to_owned);
        });
        if bad_graph {
            let flow = row(&e).flow_id;
            let bad = if expected == RunStatus::Failed {
                "{}"
            } else {
                "broken"
            };
            e.e.env
                .store()
                .with_conn(|c| {
                    c.execute(
                        "UPDATE flow_versions SET graph=?1 WHERE flow_id=?2",
                        params![bad, flow],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let before = row(&e);
        assert_eq!(
            engine::drive(&e.e.flow, &e.id).await.unwrap(),
            expected,
            "{name}"
        );
        let after = row(&e);
        assert!(steps(&e).is_empty());
        assert!(e.calls.lock().unwrap().is_empty());
        assert_eq!(after.attempts, before.attempts);
        assert_eq!(after.retry_at, before.retry_at);
        if status == "cancelled" {
            assert_eq!(after, before);
            assert!(events(&e).is_empty());
        } else if expected == RunStatus::Done {
            assert_eq!(after.error, before.error);
            assert_eq!(after.ended, Some(NOW));
        } else {
            assert!(after.error.is_some());
            assert_eq!(after.ended, Some(NOW));
        }
    }
}
