use rusqlite::{Connection, params};
use steve_stats::realtime::{self, Quality, Scope};
use steve_store::{NoPublisher, Store, db};
const AT: i64 = 1_800_000_000_000;

fn old_db() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL)")
        .unwrap();
    for sql in &db::MIGRATIONS[..73] {
        c.execute_batch(sql).unwrap();
    }
    c.execute_batch("INSERT INTO schema_version VALUES(73); INSERT INTO workspaces(id,name,created) VALUES('default','test',1);").unwrap();
    (d, path, c)
}
fn attempt(c: &Connection, id: &str) {
    c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES(?1,'default','custom','.','ended',?2,?3)",params![id,AT-100_000,AT]).unwrap();
    c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES(?1,'n','{}',?1,'r','idle',?2)",params![id,AT]).unwrap();
    c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES(?1,?1,'finished',?2,?3)",params![id,AT-100_000,AT]).unwrap();
    c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES(?1,?1,0,'n',?1,'{}','{}','d','finished',?2,?3)",params![id,AT-100_000,AT]).unwrap();
}
fn command(c: &Connection, id: &str, state: &str, sent: i64, payload: serde_json::Value) {
    c.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,state,receipt,maybe_sent,created,updated) VALUES(?1,'n','start_turn','{}','d',?1,?2,?3,?4,?5,?6)",params![id,state,serde_json::json!({"success":false,"payload":payload}).to_string(),sent,AT-100_000,AT-20_000]).unwrap();
}
fn state(c: &Connection, at: i64) -> Quality {
    realtime::read(
        c,
        Scope {
            workspace: "default",
            project: None,
            bot: None,
        },
        at,
    )
    .unwrap()
    .state
}
#[test]
fn schema73_applied_node_stop_bounds_unknown_without_erasing_history() {
    let (_d, path, c) = old_db();
    attempt(&c, "a");
    c.execute_batch(
        "INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES('n','s','e',1,1)",
    )
    .unwrap();
    let ev = serde_json::json!({"at":AT-20_000,"subject":{"of":"execution","type":"turn","session":"a","turn":"a","attempt":"a"},"body":{"type":"attempt_ended"}});
    c.execute(
        "INSERT INTO node_event_inbox VALUES('n','s','e',1,'stop',?1,1,?2)",
        params![ev.to_string(), AT - 20_000],
    )
    .unwrap();
    drop(c);
    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .reader(|c| {
            assert_eq!(state(c, AT - 11_000), Quality::Unavailable);
            assert_eq!(state(c, AT - 10_000), Quality::Idle);
            let r = realtime::read(
                c,
                Scope {
                    workspace: "default",
                    project: None,
                    bot: None,
                },
                AT,
            )
            .unwrap();
            assert_eq!(r.state, Quality::Idle);
            assert!(r.series.iter().any(|p| p.state == Quality::Unavailable));
            assert!(
                realtime::read(
                    c,
                    Scope {
                        workspace: "default",
                        project: None,
                        bot: None
                    },
                    AT + 49_000
                )
                .unwrap()
                .series
                .iter()
                .all(|p| p.state == Quality::Idle)
            );
            assert_eq!(db::compat(c)?.min_reader, db::SCHEMA_VERSION);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}
#[test]
fn schema73_terminal_receipt_is_evidence_but_status_and_unknown_receipt_are_not() {
    for (receipt, want) in [
        (
            serde_json::json!({"operation":"a","state":{"state":"finished","outcome":"succeeded"}}),
            Quality::Idle,
        ),
        (
            serde_json::json!({"operation":"a","state":{"state":"unknown","reason":"lost"}}),
            Quality::Unavailable,
        ),
        (
            serde_json::json!({"operation":"other","state":{"state":"finished","outcome":"succeeded"}}),
            Quality::Unavailable,
        ),
    ] {
        let (_d, path, c) = old_db();
        attempt(&c, "a");
        command(&c, "a", "finished", 1, receipt);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.reader(|c| {
            assert_eq!(state(c, AT), want);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}
#[test]
fn schema73_never_sent_rejection_has_no_unknown_execution() {
    let (_d, path, c) = old_db();
    attempt(&c, "a");
    command(
        &c,
        "a",
        "rejected",
        0,
        serde_json::json!({"reason":"cancelled before it was sent"}),
    );
    drop(c);
    let s = Store::open(path, NoPublisher).unwrap();
    s.reader(|c| {
        assert_eq!(state(c, AT - 99_000), Quality::Idle);
        assert_eq!(state(c, AT), Quality::Idle);
        Ok::<_, steve_store::StoreError>(())
    })
    .unwrap();
}

#[test]
fn unconfirmed_and_mismatched_old_stops_remain_unknown() {
    for (applied, node, session) in [(0, "n", "a"), (1, "other", "a"), (1, "n", "other")] {
        let (_d, path, c) = old_db();
        attempt(&c, "a");
        c.execute("INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES(?1,'s','e',1,?2)",params![node,applied]).unwrap();
        let ev = serde_json::json!({"subject":{"of":"execution","type":"turn","session":session,"turn":"a","attempt":"a"},"body":{"type":"attempt_ended"}});
        c.execute(
            "INSERT INTO node_event_inbox VALUES(?1,'s','e',1,'stop',?2,?3,?4)",
            params![node, ev.to_string(), applied, AT - 20_000],
        )
        .unwrap();
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.reader(|c| {
            assert_eq!(state(c, AT), Quality::Unavailable);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}

#[test]
fn possible_dispatch_or_unproven_refusal_cannot_erase_unknown() {
    for (sent, payload) in [
        (
            1,
            serde_json::json!({"reason":"cancelled before it was sent"}),
        ),
        (0, serde_json::json!({})),
    ] {
        let (_d, path, c) = old_db();
        attempt(&c, "a");
        command(&c, "a", "rejected", sent, payload);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.reader(|c| {
            assert_eq!(state(c, AT), Quality::Unavailable);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}

#[test]
fn a_new_terminal_receipt_closes_only_the_incomplete_baseline_and_replays_once() {
    use steve_store::runtime::{self, ReceiptState};
    for baseline in [None, Some(0), Some(1)] {
        let (_d, path, c) = old_db();
        attempt(&c, "a");
        command(&c, "a", "pending", 1, serde_json::json!({}));
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        if let Some(b) = baseline {
            s.with_conn(|c|{c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,node_id,started,baseline) VALUES('a','a','default','n',?1,?2)",params![AT-100_000,b])?;Ok(())}).unwrap();
        }
        let payload =
            serde_json::json!({"operation":"a","state":{"state":"finished","outcome":"succeeded"}});
        s.transaction(|tx| {
            assert!(runtime::merge_receipt_in(
                tx,
                "a",
                ReceiptState::Finished,
                true,
                &payload,
                AT - 20_000
            )?);
            assert!(!runtime::merge_receipt_in(
                tx,
                "a",
                ReceiptState::Finished,
                true,
                &payload,
                AT
            )?);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
        s.reader(|c| {
            assert_eq!(
                state(c, AT),
                if baseline == Some(1) {
                    Quality::Unavailable
                } else {
                    Quality::Idle
                }
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM realtime_output_attempts", [], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                1
            );
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}

#[test]
fn a_new_unsent_cancel_or_rejection_never_adds_unknown_coverage() {
    use steve_store::runtime::{self, ReceiptState};
    for payload in [
        serde_json::json!({"reason":"cancelled before it was sent"}),
        serde_json::json!({"code":"invalid_request","retry":"never"}),
    ] {
        let (_d, path, c) = old_db();
        attempt(&c, "a");
        command(&c, "a", "pending", 0, serde_json::json!({}));
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.transaction(|tx| {
            runtime::merge_receipt_in(tx, "a", ReceiptState::Rejected, false, &payload, AT)?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
        s.reader(|c| {
            assert_eq!(state(c, AT), Quality::Idle);
            assert_eq!(state(c, AT - 99_000), Quality::Idle);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}
