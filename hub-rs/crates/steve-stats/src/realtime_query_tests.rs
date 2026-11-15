//! Count SQLite instructions on the actual read queries, not elapsed time or a duplicated SQL
//! implementation. Both telemetry and the ordinary applied inbox contain the full history.
use super::*;
use rusqlite::StatementStatus;

const AT: i64 = 1_800_000_000_000;

fn scalar(c: &Connection, sql: &str, args: impl rusqlite::Params) -> (Option<i64>, i32) {
    let mut st = c.prepare(sql).unwrap();
    let value = st.query_row(args, |r| r.get(0)).unwrap();
    (value, st.get_status(StatementStatus::VmStep))
}

#[test]
fn realtime_queries_seek_bounded_windows_in_hour_four_hour_and_concurrent_histories() {
    let mut window_costs = vec![];
    for (hours, concurrent) in [(1, 1), (4, 1), (4, 3)] {
        let mut c = Connection::open_in_memory().unwrap();
        steve_store::db::migrate(&c).unwrap();
        let n = hours * 3_600 * 20; // actual Node coalescing cadence, 50ms
        let start = AT - n * 50;
        let tx = c.transaction().unwrap();
        for i in 0..concurrent {
            let id = format!("a{i}");
            tx.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','custom','.','running',?2,?2)", params![id,start]).unwrap();
            tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'n','{}',?1,'r','busy',?2)", params![id,start]).unwrap();
            tx.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?1,?1,'active',?2,?2)", params![id,start]).unwrap();
            tx.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES (?1,?1,0,'n',?1,'{}','{}','d','running',?2,?2)", params![id,start]).unwrap();
            tx.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,node_id,started) VALUES (?1,?1,'default','n',?2)", params![id,start]).unwrap();
            tx.execute("INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES ('n',?1,'e',?2,?2)",params![id,n]).unwrap();
            tx.execute("WITH RECURSIVE ticks(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM ticks WHERE i<?2)
                INSERT INTO realtime_output_samples SELECT 'n',?1,'e',i,?1||'-'||i,?1,?3+i*50,?3+i*50,?3+i*50,'text',4 FROM ticks",params![id,n,start]).unwrap();
            tx.execute("WITH RECURSIVE ticks(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM ticks WHERE i<?2)
                INSERT INTO node_event_inbox SELECT 'n',?1,'e',i,?1||'-'||i,json_object('subject',json_object('attempt',?1),'body',json_object('type','plan')),1,?3+i*50 FROM ticks",params![id,n,start]).unwrap();
        }
        tx.commit().unwrap();
        for i in 0..concurrent {
            let id = format!("a{i}");
            let lo = AT - 69_000;
            let (gap, gap_steps) = scalar(&c, GAP_SQL, [&id]);
            assert_eq!(gap, None);
            assert!(gap_steps < 100, "gap lookup scanned history: {gap_steps}");
            let (preceding, preceding_steps) = scalar(&c, PRECEDING_SQL, params![id, lo]);
            assert_eq!(preceding, Some(lo));
            assert!(preceding_steps < 100);
            let mut st = c.prepare(WINDOW_SQL).unwrap();
            let rows = st
                .query_map(params![id, lo, AT], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let window_steps = st.get_status(StatementStatus::VmStep);
            assert_eq!(rows.len(), 1_380);
            assert!(
                window_steps < 40_000,
                "window scanned history: {window_steps}"
            );
            window_costs.push(window_steps);
            let (pending, pending_steps) = scalar(&c, PENDING_SQL, params!["n", id]);
            assert_eq!(pending, None);
            assert!(
                pending_steps < 300,
                "healthy inbox scanned history: {pending_steps}"
            );
            c.execute(
                "UPDATE node_event_inbox SET applied=0 WHERE stream=? AND seq=?",
                params![id, n],
            )
            .unwrap();
            let (pending, unacked_steps) = scalar(&c, PENDING_SQL, params!["n", id]);
            assert_eq!(pending, Some(AT));
            assert!(unacked_steps < 300);
            c.execute(
                "UPDATE node_event_inbox SET applied=1 WHERE stream=? AND seq=?",
                params![id, n],
            )
            .unwrap();
            c.execute("UPDATE node_streams SET fault='gap' WHERE stream=?", [&id])
                .unwrap();
            let (fault, fault_steps) = scalar(&c, PENDING_SQL, params!["n", id]);
            assert_eq!(fault, Some(start + 50));
            assert!(
                fault_steps < 500,
                "fault lookup scanned history: {fault_steps}"
            );
            c.execute("UPDATE node_streams SET fault=NULL WHERE stream=?", [&id])
                .unwrap();
            c.execute(
                "UPDATE realtime_output_samples SET kind='gap' WHERE stream=? AND seq=1",
                [&id],
            )
            .unwrap();
            let (old_gap, old_gap_steps) = scalar(&c, GAP_SQL, [&id]);
            assert_eq!(old_gap, Some(start + 50));
            assert!(old_gap_steps < 100);
            c.execute(
                "UPDATE realtime_output_samples SET kind='text' WHERE stream=? AND seq=1",
                [&id],
            )
            .unwrap();
            let mut explain = c
                .prepare(&format!("EXPLAIN QUERY PLAN {WINDOW_SQL}"))
                .unwrap();
            let plan = explain
                .query_map(params![id, lo, AT], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                plan.iter()
                    .any(|p| p.contains("attempt_id=? AND at>? AND at<?")),
                "{plan:?}"
            );
            println!(
                "QUERY_COST hours={hours} concurrent={concurrent} attempt={id} samples={n} rows={} gap={gap_steps} predecessor={preceding_steps} window={window_steps} inbox={pending_steps} pending={unacked_steps} fault={fault_steps} old_gap={old_gap_steps} plan={plan:?}",
                rows.len()
            );
        }
        // Also call the complete production reader against the same persistent SQL shape.
        let r = read(
            &c,
            Scope {
                workspace: "default",
                project: None,
                bot: None,
            },
            AT,
        )
        .unwrap();
        assert_eq!(
            (r.state, r.tps, r.output_sessions),
            (
                Quality::Estimated,
                Some(20.0 * concurrent as f64),
                Some(concurrent as usize)
            )
        );
        assert!(r.series.iter().all(|p| p.tps == r.tps));
    }
    assert!(
        window_costs.iter().max().unwrap() - window_costs.iter().min().unwrap() < 100,
        "equal tail windows must cost approximately the same: {window_costs:?}"
    );
}
