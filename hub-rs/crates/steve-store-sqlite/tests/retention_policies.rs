use rusqlite::{Connection, params};
use steve_store_sqlite::retention::{
    self as r, Budget,
    policies::{BACKGROUND_TOTAL_BYTES, DAY, Diagnostics, ITEM_BYTES, Kind, Turns},
};
fn database() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    steve_store_sqlite::migrate::initialize(
        &mut c,
        &steve_store_sqlite::layout::Layout::single_file(),
    )
    .unwrap();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c
}
fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn slice(c: &Connection, kind: Kind, now: i64) -> r::SliceReport<i64> {
    r::run_slice_timed(
        c,
        kind.policy().as_ref(),
        now,
        Budget::default(),
        None,
        || 0,
    )
    .unwrap()
}
#[test]
fn age_days_and_window_expiry_boundaries_and_latest_64_turns() {
    let c = database();
    let now = 200 * DAY as i64 + 1234;
    c.execute(
        "INSERT INTO node_vitals(node_id,at,data) VALUES('n',?,'{}'),('n',?,'{}')",
        params![now - DAY as i64 - 1, now - DAY as i64],
    )
    .unwrap();
    assert_eq!(slice(&c, Kind::Vitals, now).affected, 1);
    c.execute_batch("INSERT INTO tool_usage(bot_id,tool,day,searched,called) VALUES('b','old',109,1,0),('b','boundary',110,1,0);").unwrap();
    assert_eq!(slice(&c, Kind::Tools, now).affected, 1);
    assert_eq!(one(&c, "SELECT day FROM tool_usage"), 110);
    for table in ["feed_windows", "attention_windows"] {
        for (name, expires) in [("expired", now), ("live", now + 1)] {
            let sql = if table == "feed_windows" {
                "INSERT INTO feed_windows(token,workspace_id,user_id,unread,upper_seq,expires) VALUES(?,'default','u',0,0,?)"
            } else {
                "INSERT INTO attention_windows(token,workspace_id,user_id,kind,revision,upper_seq,position,expires) VALUES(?,'default','u','feed','r',0,0,?)"
            };
            c.execute(sql, params![name, expires]).unwrap();
        }
    }
    assert_eq!(slice(&c, Kind::Feed, now).affected, 1);
    assert_eq!(slice(&c, Kind::Attention, now).affected, 1);
    for n in 1..=70 {
        c.execute(
            "INSERT INTO session_turns(session_id,attempt,updated) VALUES('s',?,?)",
            params![format!("a{n}"), n],
        )
        .unwrap();
    }
    assert_eq!(slice(&c, Kind::Turns, now).affected, 6);
    assert_eq!(one(&c, "SELECT min(updated) FROM session_turns"), 7);
    // Reusing a declaration must not keep a stale ranking cache across slices.
    let p = Turns::new();
    r::run_slice_timed(&c, &p, now, Budget::default(), None, || 0).unwrap();
    c.execute(
        "INSERT INTO session_turns(session_id,attempt,updated) VALUES('s','new',71)",
        [],
    )
    .unwrap();
    assert_eq!(
        r::run_slice_timed(&c, &p, now, Budget::default(), None, || 0)
            .unwrap()
            .affected,
        1
    );
    assert_eq!(one(&c, "SELECT min(updated) FROM session_turns"), 8);
}
#[test]
fn outbox_receipts_paused_consumers_since_seq_and_leases_are_protected() {
    let c = database();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c.execute_batch("INSERT INTO outbox_consumers(id,topic,since_seq,enabled,registered) VALUES('paused','t',0,0,0),('new','t',99,1,0);
        INSERT INTO outbox_events(seq,id,workspace_id,topic,version,payload,created) VALUES(1,'a','default','t',1,'{}',0),(2,'b','default','t',1,'{}',0),(3,'c','default','t',1,'{}',0);
        INSERT INTO outbox_receipts(event_seq,consumer_id,workspace_id,done) VALUES(1,'paused','default',1),(3,'paused','default',1);
        INSERT INTO outbox_leases(event_seq,consumer_id,owner,updated) VALUES(3,'paused','fixture',0);").unwrap();
    assert_eq!(slice(&c, Kind::Outbox, 8 * DAY as i64).affected, 1);
    assert_eq!(
        one(&c, "SELECT count(*) FROM outbox_receipts WHERE event_seq=1"),
        0
    );
    assert_eq!(one(&c, "SELECT count(*) FROM outbox_events"), 2);
    c.execute("DELETE FROM outbox_leases", []).unwrap();
    c.execute("INSERT INTO outbox_receipts(event_seq,consumer_id,workspace_id,done) VALUES(2,'paused','default',1)",[]).unwrap();
    assert_eq!(slice(&c, Kind::Outbox, 8 * DAY as i64).affected, 2);
}
fn job(c: &Connection, id: &str) {
    c.execute("INSERT INTO runtime_jobs(job_id,kind,purpose,workspace_id,node_id,state,created,updated) VALUES(?,'think','test','default','n','finished',0,0)",[id]).unwrap();
}
fn attempt(c: &Connection, job: &str, id: &str) {
    c.execute("INSERT INTO runtime_job_attempts(attempt_id,job_id,ordinal,model,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES(?,?,0,'m','n',?,'{}','{}','digest','finished',0,0)",params![id,job,id]).unwrap();
}
#[test]
fn jobs_delete_whole_attempt_groups_and_keep_live_or_externally_depended_commands() {
    let c = database();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    for id in ["free", "live", "dependency"] {
        job(&c, id);
        attempt(&c, id, &format!("a-{id}"));
    }
    for (op, target, state, dependency) in [
        ("free", "a-free", "finished", None),
        ("live", "a-live", "active", None),
        ("dep", "a-dependency", "finished", None),
        ("outside", "a-live", "finished", Some("dep")),
    ] {
        c.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_job_attempt,state,dependency_operation,created,updated) VALUES(?,'n','test','{}','digest',?,?,?,0,0)",params![op,target,state,dependency]).unwrap();
    }
    let p = slice(&c, Kind::Jobs, 8 * DAY as i64);
    assert_eq!((p.affected, p.failures), (1, 0));
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM runtime_job_attempts WHERE job_id='free'"
        ),
        0
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM node_commands WHERE operation_id='free'"
        ),
        0
    );
    assert_eq!(one(&c, "SELECT count(*) FROM runtime_jobs"), 2);
}
fn diag(c: &Connection, item: &str, at: i64, bytes: i64) {
    c.execute("INSERT INTO diagnostic_events(workspace_id,item_id,at,kind,reason_code,severity,detail,data,bytes) VALUES('default',?,?,'event','reason','info',0,'{}',?)",params![item,at,bytes]).unwrap();
}
#[test]
fn diagnostic_expiry_and_capacity_update_bytes_and_gap_watermarks() {
    let c = database();
    c.execute(
        "INSERT INTO diagnostic_item_stats(item_id,bytes) VALUES('i',200)",
        [],
    )
    .unwrap();
    diag(&c, "i", 0, 100);
    diag(&c, "i", 0, 100);
    assert_eq!(slice(&c, Kind::DiagnosticAge, 8 * DAY as i64).affected, 2);
    assert_eq!(
        one(
            &c,
            "SELECT bytes FROM diagnostic_item_stats WHERE item_id='i'"
        ),
        0
    );
    assert_eq!(
        one(
            &c,
            "SELECT removed_through FROM diagnostic_item_stats WHERE item_id='i'"
        ),
        2
    );
    for _ in 0..3 {
        diag(&c, "i", 0, ITEM_BYTES / 2);
    }
    c.execute(
        "UPDATE diagnostic_item_stats SET bytes=? WHERE item_id='i'",
        [ITEM_BYTES / 2 * 3],
    )
    .unwrap();
    let foreground = Diagnostics::for_insert("default", "i", 0);
    assert_eq!(
        r::run_slice_timed(&c, &foreground, 1, r::DEFAULT_BUDGET, None, || 0)
            .unwrap()
            .affected,
        1
    );
    assert_eq!(
        one(
            &c,
            "SELECT bytes FROM diagnostic_item_stats WHERE item_id='i'"
        ),
        ITEM_BYTES
    );
    for n in 0..22 {
        let id = format!("i{n}");
        c.execute(
            "INSERT INTO diagnostic_item_stats(item_id,bytes) VALUES(?,?)",
            params![id, ITEM_BYTES / 2],
        )
        .unwrap();
        diag(&c, &id, 0, ITEM_BYTES / 2);
    }
    slice(&c, Kind::DiagnosticCapacity, 1);
    assert!(one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats") <= BACKGROUND_TOTAL_BYTES);
    assert_eq!(
        one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats"),
        one(&c, "SELECT sum(bytes) FROM diagnostic_events")
    );
}

#[test]
fn total_pressure_below_the_hard_cap_converges_to_background_headroom() {
    let c = database();
    // 95 MiB is legal for the writer but must activate the background 90 MiB threshold.
    for n in 0..19 {
        let id = format!("i{n}");
        c.execute(
            "INSERT INTO diagnostic_item_stats(item_id,bytes) VALUES(?,?)",
            params![id, ITEM_BYTES / 2],
        )
        .unwrap();
        diag(&c, &id, 0, ITEM_BYTES / 2);
    }
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let sums = Arc::new(AtomicUsize::new(0));
    let count = sums.clone();
    c.authorizer(Some(move |context: AuthContext<'_>| {
        if matches!(context.action, AuthAction::Function {function_name} if function_name.eq_ignore_ascii_case("sum")) {
            count.fetch_add(1,Ordering::SeqCst);
        }
        Authorization::Allow
    })).unwrap();
    let report = slice(&c, Kind::DiagnosticCapacity, 1);
    assert_eq!(
        sums.load(Ordering::SeqCst),
        1,
        "one aggregate per capacity window, not one per candidate"
    );
    c.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    assert_eq!(
        report.affected, 1,
        "cleanup runs before the hard 100 MiB limit"
    );
    assert_eq!(
        one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats"),
        BACKGROUND_TOTAL_BYTES
    );
    assert_eq!(
        one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats"),
        one(&c, "SELECT sum(bytes) FROM diagnostic_events")
    );
    c.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "diagnostic_events",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let quiet = slice(&c, Kind::DiagnosticCapacity, 1);
    assert_eq!(
        (quiet.inspected, quiet.affected, quiet.next),
        (0, 0, None),
        "under the threshold the event table is not even read"
    );
}
