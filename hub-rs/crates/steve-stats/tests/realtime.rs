use rusqlite::params;
use steve_stats::realtime::{self, Quality, Scope};
use steve_store::{NoPublisher, Store};

const AT: i64 = 1_800_000_000_000;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        Self { _dir: dir, store }
    }
    fn attempt(&self, id: &str, session: &str, ws: &str, project: &str, bot: &str, start: i64) {
        self.store.with_conn(|c| {
            c.execute("INSERT OR IGNORE INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,?,'test','.','running',?,?)",params![session,ws,start,start])?;
            c.execute("INSERT OR IGNORE INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?,'n','{}',?,'r','busy',?)",params![session,session,start])?;
            c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'finished',?,?)",params![id,session,start,start])?;
            c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES (?,?,0,'n',?,'{}','{}','d','running',?,?)",params![id,id,id,start,start])?;
            c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started) VALUES (?,?,?,?,?,'n',?)",params![id,session,ws,project,bot,start])?;
            Ok(())
        }).unwrap();
        self.sample(id, 1, start, 0, "activity");
    }
    fn sample(&self, id: &str, seq: i64, at: i64, units: i64, kind: &str) {
        self.store.with_conn(|c| {
            c.execute("INSERT INTO realtime_output_samples(node_id,stream,epoch,seq,event_id,attempt_id,at,source_at,received_at,kind,units) VALUES ('n',?,'e',?,?,?,?,?,?,?,?)",params![id,seq,format!("{id}-{seq}"),id,at,at,AT,kind,units])?;
            Ok(())
        }).unwrap();
    }
    fn end(&self, id: &str, at: i64) {
        self.store
            .with_conn(|c| {
                c.execute(
                    "UPDATE realtime_output_attempts SET ended=? WHERE attempt_id=?",
                    params![at, id],
                )?;
                c.execute(
                    "UPDATE runtime_attempts SET state='finished',updated=? WHERE attempt_id=?",
                    params![at, id],
                )?;
                Ok(())
            })
            .unwrap();
    }
    fn read(
        &self,
        at: i64,
        ws: &str,
        project: Option<&str>,
        bot: Option<&str>,
    ) -> realtime::Realtime {
        self.store
            .reader(|c| {
                realtime::read(
                    c,
                    Scope {
                        workspace: ws,
                        project,
                        bot,
                    },
                    at,
                )
            })
            .unwrap()
    }
}

#[test]
fn one_session_boundaries_stop_and_sixty_equal_windows() {
    let f = Fixture::new();
    f.attempt("a", "s", "default", "p", "b", AT - 70_000);
    f.sample("a", 2, AT - 10_000, 400, "text"); // excluded at current point
    f.sample("a", 3, AT - 9_999, 80, "text");
    f.sample("a", 4, AT, 120, "text");
    f.end("a", AT + 1_000);
    let r = f.read(AT + 999, "default", None, None);
    assert_eq!(r.at, AT);
    assert_eq!((r.window_ms, r.step_ms), (10_000, 1_000));
    assert_eq!(
        (r.tps, r.output_sessions, r.state),
        (Some(5.0), Some(1), Quality::Estimated)
    );
    assert_eq!(
        (
            r.coverage.measured,
            r.coverage.estimated,
            r.coverage.unavailable
        ),
        (0, 1, 0)
    );
    assert_eq!(r.series.len(), 60);
    assert_eq!(r.series.first().unwrap().at, AT - 59_000);
    assert_eq!(r.series.last().unwrap().tps, r.tps);
    for pair in r.series.windows(2) {
        assert_eq!(pair[1].at - pair[0].at, 1_000);
    }
    // The last delta stays in the window after completion, then leaves at exactly +10s.
    assert_eq!(f.read(AT + 9_000, "default", None, None).tps, Some(3.0));
    let idle = f.read(AT + 10_000, "default", None, None);
    assert_eq!(
        (idle.state, idle.tps, idle.output_sessions),
        (Quality::Idle, Some(0.0), Some(0))
    );
}

#[test]
fn concurrent_sessions_project_bot_intersection_and_workspace_isolation() {
    let f = Fixture::new();
    for (id, session, ws, project, bot, units) in [
        ("a", "s1", "default", "p1", "b1", 40),
        ("b", "s2", "default", "p1", "b2", 80),
        ("c", "s3", "default", "p2", "b1", 120),
        ("d", "s4", "elsewhere", "p1", "b1", 400_000),
    ] {
        f.attempt(id, session, ws, project, bot, AT - 20_000);
        f.sample(id, 2, AT - 1_000, units, "text");
    }
    let all = f.read(AT, "default", None, None);
    assert_eq!(
        (all.tps, all.output_sessions, all.coverage.estimated),
        (Some(6.0), Some(3), 3)
    );
    assert_eq!(f.read(AT, "default", Some("p1"), None).tps, Some(3.0));
    assert_eq!(f.read(AT, "default", None, Some("b1")).tps, Some(4.0));
    assert_eq!(f.read(AT, "default", Some("p1"), Some("b1")).tps, Some(1.0));
    assert_eq!(f.read(AT, "elsewhere", None, None).tps, Some(10_000.0));
    assert_eq!(f.read(AT, "empty", None, None).state, Quality::Idle);
    // Two non-overlapping attempts of one session still count as one covered/output session.
    f.end("a", AT - 500);
    f.attempt("e", "s1", "default", "p1", "b1", AT - 499);
    f.sample("e", 2, AT, 40, "text");
    let r = f.read(AT, "default", Some("p1"), Some("b1"));
    assert_eq!(
        (r.tps, r.output_sessions, r.coverage.estimated),
        (Some(2.0), Some(1), 1)
    );
}

#[test]
fn silence_missing_and_partial_never_become_false_zero() {
    let f = Fixture::new();
    f.attempt("a", "s1", "default", "p", "b", AT - 20_000);
    f.sample("a", 2, AT - 1_000, 40, "text");
    let stale = f.read(AT + 3_000, "default", None, None);
    assert_eq!(
        (stale.state, stale.tps, stale.output_sessions),
        (Quality::Partial, None, None)
    );
    assert_eq!(stale.estimated_tps, Some(1.0));
    let no_output = f.read(AT + 10_000, "default", None, None);
    assert_eq!(
        (no_output.state, no_output.tps),
        (Quality::Unavailable, None)
    );
    f.attempt("b", "s2", "default", "p", "b", AT - 2_000);
    f.sample("b", 2, AT, 80, "text");
    f.sample("a", 3, AT - 500, 0, "gap");
    let r = f.read(AT, "default", None, None);
    assert_eq!((r.coverage.estimated, r.coverage.unavailable), (1, 1));
    assert_eq!(
        (r.state, r.tps, r.output_sessions, r.estimated_tps),
        (Quality::Partial, None, None, Some(3.0))
    );
    // A started attempt with fresh non-output activity has known zero visible output.
    let fresh = f.read(AT - 20_000, "default", None, None);
    assert_eq!((fresh.state, fresh.tps), (Quality::Idle, Some(0.0)));
}

#[test]
fn late_samples_use_source_time_and_reopen_reads_the_same_record() {
    let f = Fixture::new();
    f.attempt("a", "s", "default", "p", "b", AT - 80_000);
    f.sample("a", 2, AT - 11_000, 800_000, "text"); // arrived now; never spikes the current window
    f.sample("a", 3, AT - 2_000, 40, "text");
    f.end("a", AT - 1_000);
    let r = f.read(AT, "default", None, None);
    assert_eq!(r.tps, Some(1.0));
    assert_eq!(r.series[48].tps, Some(20_000.0)); // point at -11s includes the late text
    let reopened = Store::open(f.store.path(), NoPublisher).unwrap();
    let reread = reopened
        .reader(|c| {
            realtime::read(
                c,
                Scope {
                    workspace: "default",
                    project: None,
                    bot: None,
                },
                AT,
            )
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(r).unwrap(),
        serde_json::to_value(reread).unwrap()
    );
}

#[test]
fn estimator_is_independent_of_chunking_and_utf8_bytes() {
    let chunks = ["a", "bcd", "中", "文", "🙂", "\n"];
    assert_eq!(
        realtime::text_units(&chunks.concat()),
        chunks.iter().map(|s| realtime::text_units(s)).sum::<i64>()
    );
    assert_eq!(realtime::text_units("abcd中文🙂\n"), 17);
}

#[test]
fn missing_baselines_and_workspace_changes_do_not_expose_old_samples() {
    let f = Fixture::new();
    f.attempt("a", "s", "default", "p", "b", AT - 1_000);
    f.sample("a", 2, AT, 40, "text");
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE sessions SET workspace_id='elsewhere' WHERE id='s'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.read(AT, "default", None, None).tps, Some(0.0));
    let moved = f.read(AT, "elsewhere", None, None);
    assert_eq!(
        (moved.state, moved.tps, moved.estimated_tps),
        (Quality::Unavailable, None, None)
    );
    // A pre-collection attempt is an execution intent with no trustworthy text baseline.
    f.attempt("b", "s2", "default", "p", "b", AT - 1_000);
    f.store
        .with_conn(|c| {
            c.execute(
                "DELETE FROM realtime_output_attempts WHERE attempt_id='b'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.read(AT, "default", None, None).state,
        Quality::Unavailable
    );
}

#[test]
fn a_hub_settle_without_a_node_end_cannot_age_into_false_idle() {
    let f = Fixture::new();
    f.attempt("a", "s", "default", "p", "b", AT - 20_000);
    f.sample("a", 2, AT - 1_000, 40, "text");
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET state='finished',updated=? WHERE attempt_id='a'",
                [AT],
            )?;
            Ok(())
        })
        .unwrap();
    let missing = f.read(AT + 30_000, "default", None, None);
    assert_eq!(
        (missing.state, missing.tps, missing.output_sessions),
        (Quality::Unavailable, None, None)
    );
    f.end("a", AT + 30_000); // the Node's end, independently recorded, resolves the unknown interval
    assert_eq!(
        f.read(AT + 40_000, "default", None, None).state,
        Quality::Idle
    );
}

#[test]
fn upgrade_without_a_baseline_and_hub_only_settle_stays_unknown() {
    let f = Fixture::new();
    f.attempt("old", "s", "default", "p", "b", AT - 20_000);
    f.store
        .with_conn(|c| {
            c.execute(
                "DELETE FROM realtime_output_attempts WHERE attempt_id='old'",
                [],
            )?;
            c.execute(
                "UPDATE runtime_attempts SET state='finished',updated=? WHERE attempt_id='old'",
                [AT],
            )?;
            Ok(())
        })
        .unwrap();
    for offset in [0, 9_000, 10_000, 11_000, 70_000, 600_000] {
        let r = f.read(AT + offset, "default", None, None);
        assert_eq!(
            (r.state, r.tps, r.output_sessions, r.coverage.unavailable),
            (Quality::Unavailable, None, None, 1),
            "Hub settle is not Node stop evidence, +{offset}ms"
        );
    }
    assert_eq!(
        f.read(AT - 21_000, "default", None, None).state,
        Quality::Idle
    );
}

#[test]
fn ended_legacy_sessions_keep_unknown_history_until_its_windows_leave_the_trend() {
    let f = Fixture::new();
    f.store.with_conn(|c| {
        c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('legacy','default','custom','.','running',?,?)", params![AT-60_000,AT-1_000])?;
        Ok(())
    }).unwrap();
    assert_eq!(
        f.read(AT, "default", None, None).state,
        Quality::Unavailable
    );
    f.store
        .with_conn(|c| {
            c.execute("UPDATE sessions SET status='ended' WHERE id='legacy'", [])?;
            Ok(())
        })
        .unwrap();
    let r = f.read(AT, "default", None, None);
    assert_eq!(
        (r.state, r.tps, r.coverage.unavailable),
        (Quality::Unavailable, None, 1)
    );
    assert!(r.series.iter().all(|p| p.state == Quality::Unavailable));
    assert_eq!(
        f.read(AT + 8_000, "default", None, None).state,
        Quality::Unavailable
    );
    let tail = f.read(AT + 9_000, "default", None, None);
    assert_eq!((tail.state, tail.tps), (Quality::Idle, Some(0.0)));
    assert_eq!(tail.series[0].state, Quality::Unavailable);
    let last_unknown = f.read(AT + 67_000, "default", None, None);
    assert_eq!(last_unknown.series[0].state, Quality::Unavailable);
    assert!(
        last_unknown.series[1..]
            .iter()
            .all(|p| p.state == Quality::Idle)
    );
    assert!(
        f.read(AT + 68_000, "default", None, None)
            .series
            .iter()
            .all(|p| p.state == Quality::Idle)
    );
    assert_eq!(
        f.read(AT - 61_000, "default", None, None).state,
        Quality::Idle
    );
    assert_eq!(f.read(AT, "elsewhere", None, None).state, Quality::Idle);
}
