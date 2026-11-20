//! SG32: staged ingestion keeps admission/dedupe before writes and all data in the caller's Tx.
use serde_json::{Value, json};
use std::sync::Arc;
use steve_flow::adapter::{ActorRef, DropReason, Ingested, RawEvent};
use steve_flow::testkit::{FakeEventAdapter, FlowEnv};
use steve_flow::{FlowResult, flows, graph::Graph, ingest, sources};
use steve_modules::{
    ApiError,
    testkit::{ME, WS},
};
const NOW: i64 = 1_795_200_000_000;
fn env() -> FlowEnv {
    let e = FlowEnv::with_events(vec![Arc::new(FakeEventAdapter::push())]);
    e.env.clock.set(NOW);
    e
}
fn event(id: &str) -> RawEvent {
    FakeEventAdapter::event("mail.new", id, "INBOX", "fixture")
}
fn stage(e: &FlowEnv, conn: &str, ev: &RawEvent) -> Ingested {
    e.env
        .store()
        .transaction(|tx| ingest::ingest_in(tx, &e.flow, WS, conn, ev, NOW))
        .unwrap()
        .outcome()
}
fn count(e: &FlowEnv) -> i64 {
    e.env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT count(*) FROM ext_events",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}
#[tokio::test]
async fn sg32_ingest_dedupe_precedes_paused_type_and_scope_policy() {
    let e = env();
    let con = e.connection(WS, FakeEventAdapter::ID, Some(ME), ME);
    let ev = event("repeat");
    let id = match stage(&e, &con, &ev) {
        Ingested::Stored { id, .. } => id,
        x => panic!("{x:?}"),
    };
    let mut s = sources::SourceSettings::defaults(WS, &con);
    s.paused = true;
    e.env
        .store()
        .transaction(|tx| sources::save(tx, &s, NOW))
        .unwrap();
    assert_eq!(stage(&e, &con, &ev), Ingested::Duplicate { id });
    assert_eq!(count(&e), 1);
    assert!(matches!(
        stage(&e, &con, &event("new")),
        Ingested::Dropped(DropReason::Paused)
    ));
    assert_eq!(count(&e), 1);
    assert_eq!(e.flow.drops.count(&[con], "mail.new", NOW), 1);
}
#[tokio::test]
async fn sg32_ingest_scope_record_drop_and_visibility_snapshot_table() {
    for (mode, personal) in [
        ("receive", true),
        ("receive", false),
        ("record", true),
        ("ignore", true),
        ("type-off", true),
        ("record-only", true),
    ] {
        let e = env();
        let con = e.connection(WS, FakeEventAdapter::ID, personal.then_some(ME), ME);
        let mut s = sources::SourceSettings::defaults(WS, &con);
        if matches!(mode, "record" | "ignore") {
            s.scopes.push(sources::ScopeSetting {
                ext_id: "INBOX".into(),
                name: "Inbox".into(),
                mode: if mode == "record" {
                    sources::ScopeMode::Record
                } else {
                    sources::ScopeMode::Ignore
                },
            });
        }
        if mode == "type-off" {
            s.types.insert(
                "mail.new".into(),
                sources::TypeSetting {
                    on: Some(false),
                    ..Default::default()
                },
            );
        }
        e.env
            .store()
            .transaction(|tx| sources::save(tx, &s, NOW))
            .unwrap();
        let mut ev = event(mode);
        ev.summary = "界".repeat(201);
        ev.thread_key = Some("thread".into());
        ev.group_key = Some("group".into());
        ev.payload = if personal {
            json!({"kept":[1,true]})
        } else {
            Value::Null
        };
        if mode == "record-only" {
            ev.record_only = Some("backfill fixture".into());
        }
        let(account,party):(String,String)=e.env.store().reader(|c|Ok::<_,steve_store::StoreError>(c.query_row("SELECT a.id,a.party_id FROM accounts a JOIN connections c ON c.account_id=a.id WHERE c.id=?",[&con],|r|Ok((r.get(0)?,r.get(1)?)))?)).unwrap();
        ev.actor = Some(ActorRef::Known {
            account_id: account.clone(),
            party_id: party.clone(),
        });
        let got = stage(&e, &con, &ev);
        if mode == "ignore" || mode == "type-off" {
            assert!(matches!(got, Ingested::Dropped(_)));
            assert_eq!(count(&e), 0);
            continue;
        }
        let id = match got {
            Ingested::Stored { id, runs } => {
                assert!(runs.is_empty());
                id
            }
            x => panic!("{x:?}"),
        };
        let row: (String, String, Option<String>) = e
            .env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT visibility,outcome_label,payload FROM ext_events WHERE id=?",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!(row.0, if personal { "private" } else { "workspace" });
        assert_eq!(
            row.1,
            match mode {
                "record" => ingest::LABEL_RECORD,
                "record-only" => "backfill fixture",
                _ => ingest::LABEL_NO_FLOW,
            }
        );
        let v = e
            .env
            .store()
            .reader(|c| ingest::event_json(c, &id))
            .unwrap();
        assert_eq!(v["summary"], "界".repeat(200));
        assert_eq!(v["thread_key"], "thread");
        assert_eq!(v["actor_account_id"], account);
        assert_eq!(v["actor_party_id"], party);
        assert_eq!(v["payload"], ev.payload);
        assert_eq!(row.2.is_none(), !personal);
    }
}
#[tokio::test]
async fn sg32_ingest_trigger_context_skip_and_outer_rollback() {
    let e = env();
    let con = e.connection(WS, FakeEventAdapter::ID, Some(ME), ME);
    let g=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"mail","connection":con,"types":["mail.new"]}},{"id":"e","kind":"end","config":{}}],"edges":[{"from":"t","to":"e"}]})).unwrap();
    let flow = e
        .env
        .store()
        .transaction(|tx| {
            flows::create(
                tx,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name: "SG32 ingest",
                    description: "",
                    graph: &g,
                    origin: "me",
                    origin_label: "",
                    enabled: true,
                },
                NOW,
            )
        })
        .unwrap();
    let outcome = stage(&e, &con, &event("trigger"));
    let (id, ids) = match outcome {
        Ingested::Stored { id, runs } => (id, runs),
        x => panic!("{x:?}"),
    };
    assert_eq!(ids.len(), 1);
    let r = e
        .env
        .store()
        .reader(|c| steve_flow::runs::get(c, &ids[0]))
        .unwrap()
        .unwrap();
    assert_eq!(r.flow_id, flow);
    assert_eq!(r.event_id.as_deref(), Some(id.as_str()));
    assert_eq!(r.cursor.as_deref(), Some("t"));
    assert_eq!(r.context["event"]["id"], id);
    assert_eq!(r.context["steps"], json!({}));
    let mut skip = event("skip");
    skip.skip_flows.push(flow);
    assert!(matches!(stage(&e,&con,&skip),Ingested::Stored{runs,..} if runs.is_empty()));
    let before = count(&e);
    e.env.take_events();
    let out: FlowResult<()> = e.env.store().transaction(|tx| {
        ingest::ingest_in(tx, &e.flow, WS, &con, &event("rollback"), NOW)?;
        Err(ApiError::rule("fixture rollback").into())
    });
    assert!(out.is_err());
    assert_eq!(count(&e), before);
    assert!(e.env.take_events().is_empty());
}
