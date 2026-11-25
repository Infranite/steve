//! The event explorer: turns, decisions and marks of a span, as a histogram and the newest events of a window.

use steve_stats::events::{Scope, events};
use steve_store::testkit::stats::seed_events;
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
    seed_events(&s, m).unwrap();
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
        .read::<_, steve_store::StoreError>(|c| {
            Ok(events(c, &scope(), m(0), m(60), None, 12, m(55))?)
        })
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
    assert_eq!((d1.by.as_deref(), d1.dur_ms), (Some("me"), Some(2 * MIN)));
    let release = e.rows.iter().find(|x| x.what == "release").unwrap();
    assert!(release.bad);
}

#[test]
fn a_window_narrows_the_list_not_the_histogram() {
    let (_d, s) = store();
    let e = s
        .read::<_, steve_store::StoreError>(|c| {
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
        s.read::<_, steve_store::StoreError>(|c| Ok(events(c, &sc, m(0), m(60), None, 12, m(55))?))
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
        s.read::<_, steve_store::StoreError>(|c| Ok(events(
            c,
            &scope(),
            m(10),
            m(10),
            None,
            12,
            m(55)
        )?))
        .unwrap()
        .is_none()
    );
}
