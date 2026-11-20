use serde_json::{Value, json};
use steve_store::{
    ModelUsage, NoPublisher, Store, StoreError, Tokens, WS,
    gateway_ledger::{CallBegin, CallFinish},
};

fn usage(model: &str, input: Option<i64>, cost: Option<f64>, detail: Value) -> ModelUsage {
    ModelUsage {
        model: model.into(),
        tokens: Tokens {
            input,
            ..Default::default()
        },
        cost_reported: cost,
        detail,
        ..Default::default()
    }
}
fn open() -> (tempfile::TempDir, Store) {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(&t.path().join("fixture.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    (t, s)
}
fn rows(s: &Store) -> Vec<Value> {
    s.reader::<_,StoreError>(|c|Ok(c.prepare("SELECT model,input,cost_reported,attempts,duration_ms,detail,ok,source,purpose FROM usage_records WHERE live_call_id='sg44' ORDER BY id")?
        .query_map([],|r|{let detail:String=r.get(5)?;Ok(json!({"model":r.get::<_,String>(0)?,"input":r.get::<_,Option<i64>>(1)?,"cost":r.get::<_,Option<f64>>(2)?,"attempts":r.get::<_,i64>(3)?,"duration":r.get::<_,Option<i64>>(4)?,"detail":serde_json::from_str::<Value>(&detail).unwrap(),"ok":r.get::<_,bool>(6)?,"source":r.get::<_,String>(7)?,"purpose":r.get::<_,String>(8)?}))})?.collect::<rusqlite::Result<Vec<_>>>()?)).unwrap()
}
fn begin(s: &Store) {
    s.stats()
        .gateway_begin(&CallBegin {
            id: "sg44".into(),
            workspace: WS.into(),
            purpose: "sg44".into(),
            started: 1_793_491_200_000,
            day: "2026-11-01".into(),
            ..Default::default()
        })
        .unwrap();
}
#[test]
fn sg44_projection_folds_cost_companions_and_keeps_zero_evidence_off_request_rows() {
    let (_t, s) = open();
    begin(&s);
    let finish = CallFinish {
        ok: true,
        model: "answer".into(),
        harness: "fixture".into(),
        attempts: 2,
        duration_ms: 77,
        failures: vec!["first attempt failed".into()],
        usage: vec![
            usage("answer", Some(10), None, json!("original")),
            usage(
                "",
                None,
                Some(0.25),
                json!({"accounting_tokens":false,"fee":"context"}),
            ),
            usage("second", Some(3), None, json!({})),
            usage("zero", Some(0), Some(0.0), json!({})),
        ],
    };
    s.stats().gateway_finish("sg44", &finish).unwrap();
    let r = rows(&s);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0]["model"], "answer");
    assert_eq!(r[0]["input"], 10);
    assert_eq!(r[0]["cost"], 0.25);
    assert_eq!(r[0]["attempts"], 2);
    assert_eq!(r[0]["duration"], 77);
    assert_eq!(r[0]["detail"]["reported_detail"], "original");
    assert_eq!(r[0]["detail"]["cost_companions"][0]["cost_reported"], 0.25);
    assert_eq!(r[0]["detail"]["failures"], json!(["first attempt failed"]));
    assert_eq!(r[0]["detail"]["unspent_usage"][0]["model"], "zero");
    assert!(r[0]["detail"].get("unspent_accounting").is_none());
    assert_eq!(r[1]["model"], "second");
    assert_eq!(r[1]["attempts"], 1);
    assert!(r[1]["duration"].is_null());
    assert!(r[1]["cost"].is_null());
    for row in &r {
        assert_eq!(row["ok"], true);
        assert_eq!(row["source"], "gateway");
        assert_eq!(row["purpose"], "sg44");
        assert_eq!(row["detail"]["pending"], false);
        assert_eq!(row["detail"]["accounting_tokens"], true);
    }
    s.stats()
        .gateway_finish(
            "sg44",
            &CallFinish {
                ok: false,
                ..finish.clone()
            },
        )
        .unwrap();
    assert_eq!(rows(&s), r, "the first caller result remains final");
}

#[test]
fn sg44_projection_zero_precedence_cost_only_unknown_and_fault_recovery() {
    for case in [
        "answer-zero",
        "single-alias",
        "retry-unknown",
        "no-report",
        "cost-only",
    ] {
        let (_t, s) = open();
        begin(&s);
        let usages = match case {
            "answer-zero" => vec![
                usage("alias", Some(0), None, json!({"tag":"alias"})),
                usage("answer", Some(0), Some(0.0), json!({"tag":"answer"})),
            ],
            "single-alias" | "retry-unknown" => {
                vec![usage("alias", Some(0), Some(0.0), json!({"tag":"alias"}))]
            }
            "cost-only" => vec![usage(
                "answer",
                None,
                Some(1.0),
                json!({"accounting_tokens":false}),
            )],
            _ => vec![],
        };
        let f = CallFinish {
            ok: true,
            model: "answer".into(),
            harness: "fixture".into(),
            attempts: if case == "retry-unknown" { 2 } else { 1 },
            duration_ms: 9,
            usage: usages,
            ..Default::default()
        };
        s.stats().gateway_finish("sg44", &f).unwrap();
        let r = rows(&s);
        assert_eq!(r.len(), 1, "{case}");
        assert_eq!(
            r[0]["model"],
            if case == "single-alias" {
                "alias"
            } else {
                "answer"
            }
        );
        if case == "answer-zero" {
            assert_eq!(r[0]["detail"]["tag"], "answer");
            assert_eq!(r[0]["detail"]["unspent_usage"][0]["model"], "alias");
        }
        if ["retry-unknown", "no-report"].contains(&case) {
            assert_eq!(r[0]["detail"]["scope"], "none");
        }
        if case == "single-alias" {
            assert_eq!(r[0]["cost"], 0.0);
            assert_eq!(r[0]["detail"]["tag"], "alias");
        }
        if case == "cost-only" {
            assert_eq!(r[0]["cost"], 1.0);
            assert_eq!(r[0]["detail"]["accounting_tokens"], true);
        }
    }
    let (_t, s) = open();
    begin(&s);
    s.stats()
        .gateway_attempt("sg44", "attempt", "fixture", "answer")
        .unwrap();
    let before = rows(&s);
    s.transaction::<_,StoreError>(|tx|{tx.execute_batch("CREATE TRIGGER sg44_fault BEFORE INSERT ON usage_records BEGIN SELECT RAISE(ABORT,'sg44 projection fault'); END")?;Ok(())}).unwrap();
    let f = CallFinish {
        ok: false,
        model: "answer".into(),
        harness: "fixture".into(),
        attempts: 1,
        duration_ms: 5,
        usage: vec![usage("answer", Some(8), None, json!({}))],
        ..Default::default()
    };
    assert!(s.stats().gateway_finish("sg44", &f).is_err());
    assert_eq!(rows(&s), before, "projection replacement is atomic");
    s.transaction::<_, StoreError>(|tx| {
        tx.execute_batch("DROP TRIGGER sg44_fault")?;
        Ok(())
    })
    .unwrap();
    s.stats().gateway_apply_local(1).unwrap();
    let after = rows(&s);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["input"], 8);
    assert_eq!(after[0]["ok"], false);
    assert_eq!(after[0]["detail"]["pending"], false);
}
