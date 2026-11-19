use rusqlite::{Connection, params};
use steve_store_sqlite::{
    legacy,
    retention::{
        self as r, Budget, Policy,
        policies::{DAY, ITEM_BYTES, Kind, TOTAL_BYTES, TablePolicy},
    },
};
fn database() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    legacy::migrate_to_head(&c).unwrap();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c
}
fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn slice(c: &Connection, kind: Kind, now: i64) -> r::SliceReport<i64> {
    r::run_slice_timed(
        c,
        &TablePolicy::new(kind),
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
    let p = TablePolicy::new(Kind::Turns);
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
    assert_eq!(slice(&c, Kind::DiagnosticCapacity, 1).affected, 1);
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
    assert!(one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats") <= TOTAL_BYTES);
    assert_eq!(
        one(&c, "SELECT sum(bytes) FROM diagnostic_item_stats"),
        one(&c, "SELECT sum(bytes) FROM diagnostic_events")
    );
}
#[test]
fn keep_sess_settings_and_runtime_protection_are_resolved_per_slice() {
    let c = database();
    c.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,phase,archived,created,updated) VALUES('i','default',1,'I','done','','closed',1,0,0);
        INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES('s','default','i','fake','/tmp','ended',0,0);
        INSERT INTO events(session_id,ts,kind,data) VALUES('s',0,'user','{}');").unwrap();
    let p = TablePolicy::new(Kind::Payloads);
    let row = p.window(&c, None, 1).unwrap().rows.remove(0).0;
    assert_eq!(p.keep_ms(&c, &row).unwrap(), None);
    c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES('default','keepSess','\"90 天\"',0)",[]).unwrap();
    assert_eq!(p.keep_ms(&c, &row).unwrap(), Some(90 * DAY));
    assert!(!p.protect(&c, &row).unwrap());
    c.execute(
        "UPDATE settings SET value='\"1 年\"' WHERE key='keepSess'",
        [],
    )
    .unwrap();
    assert_eq!(p.keep_ms(&c, &row).unwrap(), Some(365 * DAY));
    c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES('turn','s','running',0,0)",[]).unwrap();
    assert!(p.protect(&c, &row).unwrap());
}
