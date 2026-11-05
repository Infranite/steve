//! S1 acceptance plus the rules of §2 and the `/signals`, `/sources`
//! routes, on a temp store with the fake event adapter.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::adapter::{ActorRef, DropReason, Ingested, PushRequest, RawEvent};
use steve_flow::ingest::{self, LABEL_NO_FLOW, LABEL_RECORD};
use steve_flow::testkit::{FakeEventAdapter, FlowEnv};
use steve_flow::{AdapterError, EventAdapter, EventSink, retention, signals};
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use steve_store::ChangeEvent;

const DAY: i64 = 24 * 3_600_000;

fn setup(fake: FakeEventAdapter) -> (FlowEnv, Arc<FakeEventAdapter>) {
    let fake = Arc::new(fake);
    let e = FlowEnv::with_events(vec![fake.clone() as Arc<dyn EventAdapter>]);
    (e, fake)
}

fn polling() -> (FlowEnv, Arc<FakeEventAdapter>) {
    setup(FakeEventAdapter::poll_every(Duration::from_secs(120)))
}

async fn get(e: &FlowEnv, who: &str, path: &str) -> (u16, Value) {
    call(&e.router(), Some(&caller(WS, who)), "GET", path, None).await
}

async fn patch_source(e: &FlowEnv, body: Value) -> Value {
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        "/sources/fake_mail",
        Some(&body),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v
}

fn events(e: &FlowEnv) -> Vec<(String, String, String, String)> {
    e.env
        .store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT external_id, type, outcome, outcome_label FROM ext_events ORDER BY rowid",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap()
}

fn push_req(sig: &str, body: Value) -> PushRequest {
    let mut headers = HeaderMap::new();
    headers.insert("x-fake-signature", sig.parse().unwrap());
    PushRequest {
        headers,
        body: Bytes::from(body.to_string()),
    }
}

#[tokio::test]
async fn three_pushed_two_stored_all_recorded() {
    let (e, _) = setup(FakeEventAdapter::push());
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    // the IMPORTANT scope only records
    patch_source(
        &e,
        json!({"scopes": [{"connection_id": con, "ext_id": "IMPORTANT", "mode": "record"}]}),
    )
    .await;
    let body = json!([
        {"type": "mail.new", "id": "m1", "summary": "Sarah：周五评审能改到下午 2 点吗？"},
        {"type": "mail.new", "id": "m1", "summary": "Sarah：周五评审能改到下午 2 点吗？"},
        {"type": "mail.new", "id": "m2", "scope": "IMPORTANT", "summary": "季度账单"},
        {"type": "mail.bulk", "id": "m3", "summary": "订阅周刊"},
    ]);
    let out = ingest::push(&e.flow, WS, &con, &push_req("ok", body))
        .await
        .unwrap();
    assert!(matches!(out[0], Ingested::Stored { .. }));
    assert_eq!(
        out[1],
        Ingested::Duplicate {
            id: out[0].id().unwrap().into()
        }
    );
    assert!(matches!(out[2], Ingested::Stored { .. }));
    assert_eq!(out[3], Ingested::Dropped(DropReason::TypeOff));
    let rows = events(&e);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            "m1".into(),
            "mail.new".into(),
            "recorded".into(),
            LABEL_NO_FLOW.into()
        )
    );
    assert_eq!(
        rows[1],
        (
            "m2".into(),
            "mail.new".into(),
            "recorded".into(),
            LABEL_RECORD.into()
        )
    );
    // the dropped one still counts on the source page
    let (_, v) = get(&e, ME, "/sources").await;
    let bulk = v[0]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "mail.bulk")
        .unwrap()
        .clone();
    assert_eq!(bulk["on"], false);
    assert_eq!(bulk["n24"], 1);
}

#[tokio::test]
async fn bad_signature_writes_nothing() {
    let (e, _) = setup(FakeEventAdapter::push());
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let r = ingest::push(
        &e.flow,
        WS,
        &con,
        &push_req(
            "nope",
            json!([{"type": "mail.new", "id": "m1", "summary": "x"}]),
        ),
    )
    .await;
    assert_eq!(r, Err(AdapterError::BadSignature));
    assert!(events(&e).is_empty());
    // a polling connection does not take pushes
    let (p, _) = polling();
    let con2 = p.connection(WS, FakeEventAdapter::ID, None, ME);
    let r = ingest::push(&p.flow, WS, &con2, &push_req("ok", json!([]))).await;
    assert_eq!(r, Err(AdapterError::BadSignature));
}

#[tokio::test]
async fn paused_receives_nothing_and_resume_does_not_catch_up() {
    let (e, fake) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    // first poll sets the cursor at the end: no backlog
    fake.emit(FakeEventAdapter::event(
        "mail.new",
        "old",
        "INBOX",
        "旧邮件",
    ));
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 0);

    fake.emit(FakeEventAdapter::event("mail.new", "a", "INBOX", "第一封"));
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 1);

    let v = patch_source(&e, json!({"paused": true})).await;
    assert_eq!(v["paused"], true);
    fake.emit(FakeEventAdapter::event(
        "mail.new",
        "b",
        "INBOX",
        "暂停期间",
    ));
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 0);

    let v = patch_source(&e, json!({"paused": false})).await;
    assert_eq!(v["paused"], false);
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 0);
    fake.emit(FakeEventAdapter::event(
        "mail.new",
        "c",
        "INBOX",
        "恢复以后",
    ));
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 1);

    let ids: Vec<String> = events(&e).into_iter().map(|r| r.0).collect();
    assert_eq!(ids, ["a", "c"]);
    // direct ingest while paused is dropped as well
    patch_source(&e, json!({"paused": true})).await;
    let r = e
        .flow
        .ingest(
            WS,
            Some(&con),
            FakeEventAdapter::event("mail.new", "d", "INBOX", "x"),
        )
        .await
        .unwrap();
    assert_eq!(r, Ingested::Dropped(DropReason::Paused));
}

#[tokio::test]
async fn poller_backs_off_after_a_failure() {
    let (e, fake) = polling();
    e.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut p = steve_flow::Poller::new(e.flow.clone());
    assert_eq!(p.tick().await, 1);
    assert_eq!(p.tick().await, 0, "not due again yet");
    e.env.clock.advance(120_000);
    *fake.fail_next.lock().unwrap() =
        Some(steve_modules::accounts::connections::ProviderError::Transient("连不上".into()));
    assert_eq!(p.tick().await, 1);
    e.env.clock.advance(120_000);
    assert_eq!(
        p.tick().await,
        0,
        "backing off: 4 minutes after one failure"
    );
    e.env.clock.advance(120_000);
    assert_eq!(p.tick().await, 1);
}

#[tokio::test]
async fn raw_is_gone_after_retention_but_the_summary_stays() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let r = e
        .flow
        .ingest(
            WS,
            Some(&con),
            FakeEventAdapter::event("mail.new", "m1", "INBOX", "合同终稿"),
        )
        .await
        .unwrap();
    let id = r.id().unwrap().to_string();
    // a run that handled it keeps its node and timing, loses its data
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch(&format!(
                "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, event_id, status, context, started) VALUES ('run_x', 'default', 'flw_x', 1, 'event', '{id}', 'done', '{{\"event\":{{\"payload\":\"secret text\"}}}}', 1);
                 INSERT INTO flow_run_steps (run_id, seq, workspace_id, node_id, status, input, output, ms, started) VALUES ('run_x', 1, 'default', 't', 'ok', '{{\"a\":1}}', '{{\"b\":2}}', 0.5, 1);"
            ))?;
            Ok(())
        })
        .unwrap();
    let (st, v) = get(&e, ME, &format!("/signals/{id}/raw")).await;
    assert_eq!(st, 200);
    assert_eq!(v["payload"]["text"], "原文：合同终稿");

    e.env.clock.advance(6 * DAY);
    let s = retention::sweep(&e.flow, e.flow.now()).unwrap();
    assert_eq!(s.payloads, 0, "keepRaw defaults to 7 days");

    e.env.clock.advance(2 * DAY);
    let s = retention::sweep(&e.flow, e.flow.now()).unwrap();
    assert_eq!((s.payloads, s.runs), (1, 1));
    let (st, v) = get(&e, ME, &format!("/signals/{id}/raw")).await;
    assert_eq!(st, 410);
    assert_eq!(v["error"], signals::RAW_GONE);
    let (st, v) = get(&e, ME, &format!("/signals/{id}")).await;
    assert_eq!(st, 200);
    assert_eq!(v["summary"], "合同终稿");
    assert_eq!(v["raw_available"], false);
    let step = &v["runs"][0]["steps"][0];
    assert_eq!(step["input"], json!({"purged": true}));
    assert_eq!(step["output"], json!({"purged": true}));
    assert_eq!(step["node_id"], "t");
    assert_eq!(step["ms"], 0.5);
    let ctx: String = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT context FROM flow_runs WHERE id = 'run_x'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(ctx, retention::PURGED);

    // the event row goes after a year
    e.env.clock.advance(400 * DAY);
    let s = retention::sweep(&e.flow, e.flow.now()).unwrap();
    assert_eq!(s.events_deleted, 1);
}

#[tokio::test]
async fn keep_raw_setting_is_read_per_workspace() {
    let (e, _) = polling();
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'keepRaw', '\"30\"', 1), ('w2', 'keepRaw', '12', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    e.env
        .store()
        .with_conn(|c| {
            assert_eq!(retention::keep_raw_days(c, WS).unwrap(), 30);
            assert_eq!(
                retention::keep_raw_days(c, WS2).unwrap(),
                7,
                "not an allowed value"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(retention::keep_raw_setting()["def"], "7");
}

#[tokio::test]
async fn private_events_are_their_owners_only() {
    let (e, _) = polling();
    let mine = e.connection(WS, FakeEventAdapter::ID, Some(ME), ME);
    let shared = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let p = e
        .flow
        .ingest(
            WS,
            Some(&mine),
            FakeEventAdapter::event("mail.new", "p1", "INBOX", "私人邮件"),
        )
        .await
        .unwrap();
    let s = e
        .flow
        .ingest(
            WS,
            Some(&shared),
            FakeEventAdapter::event("mail.new", "s1", "INBOX", "公共邮箱"),
        )
        .await
        .unwrap();
    let (pid, sid) = (p.id().unwrap(), s.id().unwrap());

    let (st, _) = get(&e, OTHER, &format!("/signals/{pid}")).await;
    assert_eq!(st, 404);
    let (st, v) = get(&e, OTHER, &format!("/signals/{pid}/raw")).await;
    assert_eq!((st, v["error"].as_str()), (404, Some("没有这个信号")));
    let (st, _) = get(&e, OTHER, &format!("/signals/{sid}")).await;
    assert_eq!(st, 200);
    let (st, v) = get(&e, ME, &format!("/signals/{pid}")).await;
    assert_eq!(st, 200);
    assert_eq!(v["connection"]["id"], mine.as_str());

    let (_, v) = get(&e, OTHER, "/signals").await;
    assert_eq!(v["stats"]["total"], 1);
    assert_eq!(v["items"][0]["id"], sid);
    let (_, v) = get(&e, ME, "/signals").await;
    assert_eq!(v["stats"]["total"], 2);
    // another workspace sees neither
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/signals/{sid}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
    // the personal connection is not on the other user's source page
    let (_, v) = get(&e, OTHER, "/sources").await;
    assert_eq!(v[0]["connections"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn sender_is_identified_and_shown() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut ev = FakeEventAdapter::event("mail.new", "m1", "INBOX", "Sarah（Acme）：评审改时间");
    ev.actor = Some(ActorRef::handle(
        "mail",
        "sarah@acme.example",
        Some("Sarah Chen".into()),
    ));
    ev.group_key = Some("acme.example".into());
    let id = e
        .flow
        .ingest(WS, Some(&con), ev)
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    let (_, v) = get(&e, ME, &format!("/signals/{id}")).await;
    assert_eq!(v["actor"]["name"], "Sarah Chen");
    assert!(v["actor"]["party_id"].as_str().unwrap().starts_with("ct"));
    assert_eq!(v["group_key"], "acme.example");
}

#[tokio::test]
async fn list_filters_and_stats() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    for (id, s) in [("a", "合同终稿"), ("b", "周报"), ("c", "发票")] {
        e.flow
            .ingest(
                WS,
                Some(&con),
                FakeEventAdapter::event("mail.new", id, "INBOX", s),
            )
            .await
            .unwrap();
    }
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE ext_events SET outcome = 'need', outcome_label = '等你确认回复' WHERE external_id = 'a'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, v) = get(&e, ME, "/signals").await;
    assert_eq!(
        v["stats"],
        json!({"total": 3, "need": 1, "item": 0, "quiet": 2})
    );
    assert_eq!(v["items"][0]["summary"], "发票", "newest first");
    let (_, v) = get(&e, ME, "/signals?q=%E5%91%A8%E6%8A%A5").await; // 周报
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let (_, v) = get(&e, ME, "/signals?q=%E7%A1%AE%E8%AE%A4").await; // 确认 (outcome label)
    assert_eq!(v["items"][0]["summary"], "合同终稿");
    let (_, v) = get(&e, ME, "/signals?q=%E9%82%AE%E4%BB%B6").await; // 邮件 (source name)
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
    let (_, v) = get(&e, ME, "/signals?outcome=need").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["stats"]["total"], 3);
    let (_, v) = get(&e, ME, "/signals?source=github").await;
    assert_eq!(v["stats"]["total"], 0);
    let (st, _) = get(&e, ME, "/signals?range=1y").await;
    assert_eq!(st, 400);
    e.env.clock.advance(3 * DAY);
    let (_, v) = get(&e, ME, "/signals").await;
    assert_eq!(v["stats"]["total"], 0);
    let (_, v) = get(&e, ME, "/signals?range=7d").await;
    assert_eq!(v["stats"]["total"], 3);
}

/// An enabled flow with an event trigger on `mail` taking `types`.
fn flow_with_trigger(e: &FlowEnv, id: &str, types: Value, conns: Value) {
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO flows (id, workspace_id, kind, name, enabled, graph, version, origin, created_by, created, updated) VALUES (?, 'default', 'flow', '邮件分诊', 1, '{\"nodes\":[{\"id\":\"t\",\"kind\":\"trigger\"}],\"edges\":[]}', 2, 'me', 'usr_dev', 1, 1)",
                [id],
            )?;
            c.execute(
                "INSERT INTO flow_triggers (id, workspace_id, flow_id, node_id, kind, source, types, connections, enabled) VALUES (?, 'default', ?, 't', 'event', 'mail', ?, ?, 1)",
                params![format!("trg_{id}"), id, types.to_string(), conns.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn matching_triggers_open_runs_and_the_event_settles() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let other = e.connection(WS, FakeEventAdapter::ID, None, ME);
    flow_with_trigger(&e, "flw_a", json!(["mail.new"]), json!([]));
    flow_with_trigger(&e, "flw_b", json!(["mail.reply"]), json!([]));
    flow_with_trigger(&e, "flw_c", json!([]), json!([other]));

    // a reply is also a mail.new: flw_a and flw_b take it, flw_c is for the other connection
    let mut ev = FakeEventAdapter::event("mail.reply", "r1", "INBOX", "回复：报价");
    ev.also = vec!["mail.new".into()];
    let Ingested::Stored { id, runs } = e.flow.ingest(WS, Some(&con), ev).await.unwrap() else {
        panic!("not stored")
    };
    assert_eq!(runs.len(), 2);
    let (flows, version, event_ids): (Vec<String>, i64, Vec<String>) = e
        .env
        .store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT flow_id, flow_version, event_id FROM flow_runs ORDER BY flow_id",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<(String, i64, String)>>>()?;
            Ok((
                rows.iter().map(|r| r.0.clone()).collect(),
                rows[0].1,
                rows.iter().map(|r| r.2.clone()).collect(),
            ))
        })
        .unwrap();
    assert_eq!(flows, ["flw_a", "flw_b"]);
    assert_eq!(version, 2);
    assert!(event_ids.iter().all(|x| *x == id));

    // the graphs are only a trigger: both runs end without a result, so the event is `recorded`
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (_, v) = get(&e, ME, &format!("/signals/{id}")).await;
        if v["outcome"] == "recorded" && v["runs"][1]["status"] == "done" {
            assert_eq!(v["runs"].as_array().unwrap().len(), 2);
            assert_eq!(v["flow"]["name"], "邮件分诊");
            return;
        }
    }
    panic!("event never settled");
}

#[tokio::test]
async fn settle_takes_the_heaviest_outcome() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let id = e
        .flow
        .ingest(
            WS,
            Some(&con),
            FakeEventAdapter::event("mail.new", "m", "INBOX", "x"),
        )
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    let run = |rid: &str, status: &str, outcome: Option<&str>, label: &str, ctx: Value| {
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, event_id, status, outcome, outcome_label, context, work_item_id, started) VALUES (?, 'default', ?, 1, 'event', ?, ?, ?, ?, ?, ?, 1)",
                    params![rid, format!("flw_{rid}"), id, status, outcome, label, ctx.to_string(), (outcome == Some("item")).then_some("ST-9")],
                )?;
                Ok(())
            })
            .unwrap();
    };
    let settle = || {
        e.env
            .store()
            .transaction(|tx| signals::settle(tx, &id))
            .unwrap()
    };
    let row = || {
        e.env
            .store()
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT outcome, outcome_label, reason, run_id, work_item_id FROM ext_events WHERE id = ?",
                    [&id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                        ))
                    },
                )?)
            })
            .unwrap()
    };
    run("r1", "failed", None, "出错了", json!({}));
    run("r2", "running", None, "", json!({}));
    settle();
    assert_eq!(row().0, "pending", "a run is still going");
    e.env
        .store()
        .with_conn(|c| Ok(c.execute("UPDATE flow_runs SET status = 'done' WHERE id = 'r2'", [])?))
        .unwrap();
    settle();
    assert_eq!(row().0, "failed");
    run("r3", "done", Some("digest"), "进汇报", json!({}));
    run(
        "r4",
        "done",
        Some("item"),
        "建了事项",
        json!({"reason": "有截止日期"}),
    );
    run("r5", "done", Some("ignored"), "已忽略", json!({}));
    assert!(settle().is_some());
    assert_eq!(
        row(),
        (
            "item".into(),
            "建了事项".into(),
            "有截止日期".into(),
            Some("r4".into()),
            Some("ST-9".into())
        )
    );
    assert!(settle().is_none(), "unchanged");
}

#[tokio::test]
async fn waiting_event_nodes_are_found() {
    use async_trait::async_trait;
    use std::sync::Mutex;
    use steve_flow::{FlowCx, FlowError, RunExecutor, RunStatus};

    #[derive(Default)]
    struct Spy(Mutex<Vec<(String, String, String)>>);
    #[async_trait]
    impl RunExecutor for Spy {
        async fn drive(&self, _: &FlowCx, _: &str) -> Result<RunStatus, FlowError> {
            Ok(RunStatus::Done)
        }
        async fn resume_event(
            &self,
            _: &FlowCx,
            run: &str,
            node: &str,
            evt: &str,
        ) -> Result<RunStatus, FlowError> {
            self.0
                .lock()
                .unwrap()
                .push((run.into(), node.into(), evt.into()));
            Ok(RunStatus::Running)
        }
    }
    let fake = Arc::new(FakeEventAdapter::poll_every(Duration::from_secs(60)));
    let base = FlowEnv::with_events(vec![fake.clone() as Arc<dyn EventAdapter>]);
    let spy = Arc::new(Spy::default());
    let setup = steve_flow::FlowSetup::new(base.core.clone());
    let flow = setup.build(base.env.modules.clone(), spy.clone());
    let con = base.connection(WS, FakeEventAdapter::ID, None, ME);
    base.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES ('run_w', 'default', 'flw_w', 1, 'event', 'waiting', 1), ('run_v', 'default', 'flw_v', 1, 'event', 'waiting', 1);
                 INSERT INTO flow_waits (run_id, node_id, workspace_id, mode, match, created) VALUES ('run_w', 'w', 'default', 'event', '{\"source\":\"mail\",\"type\":\"mail.reply\"}', 1), ('run_v', 'v', 'default', 'event', '{\"source\":\"github\"}', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    // an event that must not drive a flow on (a bot the channel's loop guard stopped) wakes nothing
    let mut quiet = FakeEventAdapter::event("mail.reply", "m0", "INBOX", "回复");
    quiet.actor = None;
    quiet.wake = false;
    ingest::ingest(&flow, WS, Some(&con), quiet).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(spy.0.lock().unwrap().is_empty(), "not woken");
    // `flow` has no adapters: the type defaults are “on”
    let mut ev = FakeEventAdapter::event("mail.reply", "m", "INBOX", "回复");
    ev.actor = None;
    let r = ingest::ingest(&flow, WS, Some(&con), ev).await.unwrap();
    for _ in 0..50 {
        if !spy.0.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        *spy.0.lock().unwrap(),
        vec![("run_w".into(), "w".into(), r.id().unwrap().to_string())]
    );
}

#[tokio::test]
async fn sources_page_patch_and_refresh_scopes() {
    let (e, fake) = polling();
    let (_, v) = get(&e, ME, "/sources").await;
    assert_eq!(v[0]["adapter"], "fake_mail");
    assert_eq!(v[0]["paused"], false);
    assert_eq!(v[0]["connections"], json!([]));
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    flow_with_trigger(&e, "flw_a", json!(["mail.new"]), json!([]));

    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/sources/fake_mail/refresh-scopes",
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        v["scopes"],
        json!([
            {"connection_id": con, "ext_id": "INBOX", "name": "收件箱", "mode": "receive"},
            {"connection_id": con, "ext_id": "IMPORTANT", "name": "重要", "mode": "receive"},
        ])
    );
    let new = &v["types"][0];
    assert_eq!(
        (
            new["key"].as_str(),
            new["on"].as_bool(),
            new["flows"].as_i64()
        ),
        (Some("mail.new"), Some(true), Some(1))
    );

    let v = patch_source(
        &e,
        json!({"triage_bot": "ops", "types": {"mail.new": {"on": false}},
               "scopes": [{"connection_id": con, "ext_id": "IMPORTANT", "mode": "ignore"}]}),
    )
    .await;
    assert_eq!(v["triage_bot"], "ops");
    assert_eq!(v["types"][0]["on"], false);
    assert_eq!(v["scopes"][1]["mode"], "ignore");
    // a refresh keeps modes, drops vanished scopes, adds new ones as receive
    *fake.scopes.lock().unwrap() = vec![
        steve_flow::ScopeDef::new("IMPORTANT", "重要"),
        steve_flow::ScopeDef::new("WORK", "工作"),
    ];
    let (_, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/sources/fake_mail/refresh-scopes",
        None,
    )
    .await;
    assert_eq!(
        v["scopes"],
        json!([
            {"connection_id": con, "ext_id": "IMPORTANT", "name": "重要", "mode": "ignore"},
            {"connection_id": con, "ext_id": "WORK", "name": "工作", "mode": "receive"},
        ])
    );
    let r = e
        .flow
        .ingest(
            WS,
            Some(&con),
            FakeEventAdapter::event("mail.reply", "x", "IMPORTANT", "x"),
        )
        .await
        .unwrap();
    assert_eq!(r, Ingested::Dropped(DropReason::ScopeIgnored));

    // errors
    let r = |body: Value, path: &'static str| {
        let router = e.router();
        async move { call(&router, Some(&caller(WS, ME)), "PATCH", path, Some(&body)).await }
    };
    assert_eq!(r(json!({"paused": true}), "/sources/nope").await.0, 404);
    let (st, v) = r(json!({"triage_bot": "w2bot"}), "/sources/fake_mail").await;
    assert_eq!((st, v["error"].as_str()), (404, Some("没有这个 Bot")));
    let (st, v) = r(
        json!({"types": {"mail.nope": {"on": true}}}),
        "/sources/fake_mail",
    )
    .await;
    assert_eq!((st, v["error"].as_str()), (400, Some("参数不对：types")));
    let (st, _) = r(
        json!({"scopes": [{"connection_id": "con_x", "ext_id": "A", "mode": "record"}]}),
        "/sources/fake_mail",
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = r(json!({"triage_bot": null}), "/sources/fake_mail").await;
    assert_eq!(st, 200);
}

#[tokio::test]
async fn signal_events_are_coalesced() {
    let (e, _) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    e.env.take_events();
    let mut last = String::new();
    for i in 0..5 {
        last = e
            .flow
            .ingest(
                WS,
                Some(&con),
                FakeEventAdapter::event("mail.new", &format!("m{i}"), "INBOX", "x"),
            )
            .await
            .unwrap()
            .id()
            .unwrap()
            .to_string();
    }
    tokio::time::sleep(signals::COALESCE + Duration::from_millis(200)).await;
    let sig: Vec<ChangeEvent> = e
        .env
        .take_events()
        .into_iter()
        .filter(|x| matches!(x, ChangeEvent::Signal { .. }))
        .collect();
    assert_eq!(
        sig,
        vec![ChangeEvent::Signal {
            id: last,
            ws: WS.into()
        }]
    );
}

#[tokio::test]
async fn ingest_rules_without_a_connection() {
    let (e, _) = polling();
    // time/item/manual events have no connection; dedupe by source
    let ev = RawEvent::new("item", "item.done", "ST-3", 1, "ST-3 完成了");
    let a = e.flow.ingest(WS, None, ev.clone()).await.unwrap();
    let b = e.flow.ingest(WS, None, ev.clone()).await.unwrap();
    assert_eq!(
        b,
        Ingested::Duplicate {
            id: a.id().unwrap().into()
        }
    );
    // the same id in another workspace is another event
    let c = e.flow.ingest(WS2, None, ev).await.unwrap();
    assert!(matches!(c, Ingested::Stored { .. }));
    // record_only, summaries cut to 200 characters
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut ev = FakeEventAdapter::event("mail.new", "old", "INBOX", &"长".repeat(300));
    ev.record_only = Some("接入时同步的旧邮件".into());
    flow_with_trigger(&e, "flw_a", json!([]), json!([]));
    let r = e.flow.ingest(WS, Some(&con), ev).await.unwrap();
    assert_eq!(
        r,
        Ingested::Stored {
            id: r.id().unwrap().into(),
            runs: vec![]
        }
    );
    let (_, v) = get(&e, ME, &format!("/signals/{}", r.id().unwrap())).await;
    assert_eq!(v["outcome_label"], "接入时同步的旧邮件");
    assert_eq!(v["summary"].as_str().unwrap().chars().count(), 200);
    // a connection of another workspace
    let err = e
        .flow
        .ingest(
            WS2,
            Some(&con),
            FakeEventAdapter::event("mail.new", "z", "INBOX", "x"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "没有这个连接");
    let err = e
        .flow
        .ingest(WS, None, RawEvent::new("item", "", "x", 1, "x"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "参数不对：type_key");
}

/// Y1: an event that can't be stored stops the pass before the cursor moves; the next poll reads
/// the batch again, what was stored before it is a duplicate, nothing is lost. A malformed event is
/// skipped instead (it would hold the source forever).
#[tokio::test]
async fn a_failed_event_keeps_the_cursor_and_the_next_poll_reads_it_again() {
    let (e, fake) = polling();
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 0);
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER no_b BEFORE INSERT ON ext_events WHEN NEW.external_id = 'b'
                 BEGIN SELECT RAISE(ABORT, 'test store fault'); END",
            )?;
            Ok(())
        })
        .unwrap();
    for id in ["a", "b", "c"] {
        fake.emit(FakeEventAdapter::event("mail.new", id, "INBOX", id));
    }
    fake.emit(FakeEventAdapter::event("", "bad", "INBOX", "没有类型"));
    assert!(ingest::poll_connection(&e.flow, WS, &con).await.is_err());
    let ids = |e: &FlowEnv| -> Vec<String> { events(e).into_iter().map(|r| r.0).collect() };
    assert_eq!(ids(&e), ["a"], "stopped at b, c not read past it");
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER no_b")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 2);
    assert_eq!(
        ids(&e),
        ["a", "b", "c"],
        "a again is a duplicate, the malformed one skipped"
    );
    assert_eq!(ingest::poll_connection(&e.flow, WS, &con).await.unwrap(), 0);
}
