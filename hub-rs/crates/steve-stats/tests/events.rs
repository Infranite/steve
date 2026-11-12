//! The event explorer: turns, decisions and marks of a span, as a histogram and the newest events of a window.

use steve_stats::events::{Scope, events};
use steve_store::{NoPublisher, Store};

const T0: i64 = 1_793_491_200_000;
const MIN: i64 = 60_000;

fn m(x: i64) -> i64 {
    T0 + x * MIN
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    let sql = format!(
        "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('p1', 'default', 'one', '/tmp/p1', 0, 0), ('p2', 'default', 'two', '/tmp/p2', 0, 0);
         INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated, project_id) VALUES
           ('ST-1', 'default', 1, 'one', 'done', '执行', {c}, {c}, 'p1'), ('ST-2', 'default', 2, 'two', 'run', '执行', {c}, {c}, 'p2');
         INSERT INTO agent_turns (id, workspace_id, session_id, item_id, project_id, harness, started, ended, stop_reason, transient, tools, tool_errors) VALUES
           ('tn_1', 'default', 's1', 'ST-1', 'p1', 'claude-code', {t1}, {t1e}, 'end_turn', 0, 3, 0),
           ('tn_2', 'default', 's2', 'ST-2', 'p2', 'claude-code', {t2}, {t2e}, 'error', 0, 1, 1),
           ('tn_3', 'default', 's2', 'ST-2', 'p2', 'claude-code', {t3}, NULL, NULL, 0, 0, 0);
         INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, model, turn_id, session_id, item_id, input, output, cache_read, cache_write) VALUES
           ('default', {t1e}, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 'tn_1', 's1', 'ST-1', 100, 20, 1000, 0);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, result, requested_by, created, resolved) VALUES
           ('d1', 'default', 'approve', 'item', 'ST-1', 'ST-1', 'q', 'resolved', '{{\"decided_by\":\"yolo\"}}', 'dev', {d1}, {d1r}),
           ('d2', 'default', 'fail', 'item', 'ST-2', 'ST-2', 'q', 'open', NULL, 'dev', {d2}, NULL);
         INSERT INTO item_marks (workspace_id, item_id, ts, kind, from_v, to_v, actor) VALUES
           ('default', 'ST-1', {k1}, 'status', 'run', 'done', 'usr_1'),
           ('default', 'ST-2', {k2}, 'release', NULL, 'rolled_back', 'steve');",
        c = m(0),
        t1 = m(5),
        t1e = m(15),
        t2 = m(30),
        t2e = m(32),
        t3 = m(50),
        d1 = m(16),
        d1r = m(18),
        d2 = m(33),
        k1 = m(20),
        k2 = m(40),
    );
    s.with_conn(|c| Ok(c.execute_batch(&sql)?)).unwrap();
    (dir, s)
}

fn scope() -> Scope {
    Scope {
        workspace_id: "default".into(),
        ..Default::default()
    }
}

#[test]
fn the_span_counts_every_kind_and_lists_the_newest_first() {
    let (_d, s) = store();
    let e = s
        .with_conn(|c| Ok(events(c, &scope(), m(0), m(60), None, 12, m(55))?))
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            e.counts.turn,
            e.counts.decision,
            e.counts.mark,
            e.counts.bad
        ),
        (3, 2, 2, 3)
    );
    assert_eq!(e.total, 7);
    assert_eq!(e.histogram.step, 5 * MIN);
    assert_eq!(e.histogram.turn.iter().sum::<i64>(), 3);
    assert_eq!(
        e.histogram.turn[1], 1,
        "tn_1 at minute 5 falls in the second bucket"
    );
    assert_eq!(e.histogram.bad.iter().sum::<i64>(), 3);
    let ts: Vec<i64> = e.rows.iter().map(|x| x.ts).collect();
    assert!(ts.windows(2).all(|w| w[0] >= w[1]), "newest first");
    let running = &e.rows[0];
    assert_eq!(
        (
            running.kind,
            running.what.as_str(),
            running.open,
            running.dur_ms
        ),
        ("turn", "running", true, Some(5 * MIN))
    );
    let tn1 = e.rows.iter().find(|x| x.id == "tn_1").unwrap();
    assert_eq!(tn1.tokens, Some(1120));
    assert_eq!(tn1.no, Some(1));
    let d1 = e.rows.iter().find(|x| x.id == "d1").unwrap();
    assert_eq!((d1.by.as_deref(), d1.dur_ms), (Some("yolo"), Some(2 * MIN)));
    let release = e.rows.iter().find(|x| x.what == "release").unwrap();
    assert!(release.bad);
}

#[test]
fn a_window_narrows_the_list_not_the_histogram() {
    let (_d, s) = store();
    let e = s
        .with_conn(|c| {
            Ok(events(
                c,
                &scope(),
                m(0),
                m(60),
                Some((m(25), m(45))),
                12,
                m(55),
            )?)
        })
        .unwrap()
        .unwrap();
    assert_eq!((e.a, e.b), (m(25), m(45)));
    assert_eq!(e.total, 3, "tn_2, d2 and the rolled back release");
    assert!(e.rows.iter().all(|x| x.ts >= m(25) && x.ts < m(45)));
    let all: i64 = e
        .histogram
        .turn
        .iter()
        .chain(&e.histogram.decision)
        .chain(&e.histogram.mark)
        .sum();
    assert_eq!(all, 7);
}

#[test]
fn project_kind_and_item_narrow_everything() {
    let (_d, s) = store();
    let run = |sc: Scope| {
        s.with_conn(|c| Ok(events(c, &sc, m(0), m(60), None, 12, m(55))?))
            .unwrap()
            .unwrap()
    };
    let p1 = run(Scope {
        project: Some("p1".into()),
        ..scope()
    });
    assert_eq!(p1.total, 3);
    assert!(p1.rows.iter().all(|x| x.item_id.as_deref() == Some("ST-1")));
    let turns = run(Scope {
        kind: Some("turn".into()),
        ..scope()
    });
    assert_eq!(
        (turns.counts.turn, turns.counts.decision, turns.counts.mark),
        (3, 0, 0)
    );
    let st2 = run(Scope {
        item: Some("ST-2".into()),
        ..scope()
    });
    assert_eq!(st2.total, 4);
    assert!(
        s.with_conn(|c| Ok(events(c, &scope(), m(10), m(10), None, 12, m(55))?))
            .unwrap()
            .is_none()
    );
}
