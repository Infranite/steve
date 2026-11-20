use rusqlite::{Connection, params};
use steve_stats::realtime::{self, Quality, Scope};
use steve_store::{NoPublisher, Store};
const AT: i64 = 1_800_000_000_000;

fn current_db() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("steve.sqlite3");
    let mut c = Connection::open(&path).unwrap();
    steve_store_sqlite::migrate::initialize(
        &mut c,
        &steve_store_sqlite::layout::Layout::single_file(),
    )
    .unwrap();
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('default','test',1);")
        .unwrap();
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
fn a_new_terminal_receipt_closes_only_the_incomplete_baseline_and_replays_once() {
    use steve_store::runtime::{self, ReceiptState};
    for baseline in [None, Some(0), Some(1)] {
        let (_d, path, c) = current_db();
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
        let (_d, path, c) = current_db();
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
