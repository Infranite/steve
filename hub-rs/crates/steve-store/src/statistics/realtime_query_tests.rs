//! Count SQLite instructions on the actual read queries, not elapsed time or a duplicated SQL
//! implementation. The telemetry holds the full history.
use super::*;
use crate::contract::Dialect;
use rusqlite::{Connection, StatementStatus, params};

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
        crate::db::migrate(&c).unwrap();
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
            tx.execute("WITH RECURSIVE ticks(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM ticks WHERE i<?2)
                INSERT INTO realtime_output_samples SELECT 'n',?1,'e',i,?1||'-'||i,?1,?3+i*50,?3+i*50,?3+i*50,'text',4 FROM ticks",params![id,n,start]).unwrap();
        }
        tx.commit().unwrap();
        for i in 0..concurrent {
            let id = format!("a{i}");
            let lo = AT - 69_000;
            let (gap_sql, _) = Dialect::Sqlite.render(&gap(&id)).unwrap();
            let (gap, gap_steps) = scalar(&c, &gap_sql, [&id]);
            assert_eq!(gap, None);
            assert!(gap_steps < 100, "gap lookup scanned history: {gap_steps}");
            let (preceding_sql, _) = Dialect::Sqlite.render(&preceding(&id, lo)).unwrap();
            let (preceding, preceding_steps) = scalar(&c, &preceding_sql, params![id, lo, 1_i64]);
            assert_eq!(preceding, Some(lo));
            assert!(preceding_steps < 100);
            let (window_sql, _) = Dialect::Sqlite.render(&window(&id, lo, AT)).unwrap();
            let mut st = c.prepare(&window_sql).unwrap();
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
            c.execute(
                "UPDATE realtime_output_samples SET kind='gap' WHERE stream=? AND seq=1",
                [&id],
            )
            .unwrap();
            let (old_gap, old_gap_steps) = scalar(&c, &gap_sql, [&id]);
            assert_eq!(old_gap, Some(start + 50));
            assert!(old_gap_steps < 100);
            c.execute(
                "UPDATE realtime_output_samples SET kind='text' WHERE stream=? AND seq=1",
                [&id],
            )
            .unwrap();
            let mut explain = c
                .prepare(&format!("EXPLAIN QUERY PLAN {window_sql}"))
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
                "QUERY_COST hours={hours} concurrent={concurrent} attempt={id} samples={n} rows={} gap={gap_steps} predecessor={preceding_steps} window={window_steps} old_gap={old_gap_steps} plan={plan:?}",
                rows.len()
            );
        }
    }
    assert!(
        window_costs.iter().max().unwrap() - window_costs.iter().min().unwrap() < 100,
        "equal tail windows must cost approximately the same: {window_costs:?}"
    );
}
