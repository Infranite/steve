use steve_stats::realtime::{self, Quality, Scope};
use steve_store::testkit::stats as fixture;
use steve_store::{NoPublisher, Store, db};
const AT: i64 = 1_800_000_000_000;

fn old_db() -> (tempfile::TempDir, std::path::PathBuf, fixture::RealtimeV73) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("steve.sqlite3");
    let old = fixture::RealtimeV73::new(&path, AT);
    (d, path, old)
}
fn state(c: &mut impl steve_store::Read, at: i64) -> Quality {
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
    c.stop("n", "a", 1, Some(AT - 20_000), AT - 20_000);
    drop(c);
    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .read(|c| {
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
            assert_eq!(
                db::inspect_schema(store.path())?.min_reader,
                db::SCHEMA_VERSION
            );
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
        c.command("a", "finished", 1, receipt, AT);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.read(|c| {
            assert_eq!(state(c, AT), want);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}
#[test]
fn schema73_never_sent_rejection_has_no_unknown_execution() {
    let (_d, path, c) = old_db();
    c.command(
        "a",
        "rejected",
        0,
        serde_json::json!({"reason":"cancelled before it was sent"}),
        AT,
    );
    drop(c);
    let s = Store::open(path, NoPublisher).unwrap();
    s.read(|c| {
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
        c.stop(node, session, applied, None, AT - 20_000);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.read(|c| {
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
        c.command("a", "rejected", sent, payload, AT);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        s.read(|c| {
            assert_eq!(state(c, AT), Quality::Unavailable);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}

#[test]
fn a_new_terminal_receipt_closes_only_the_incomplete_baseline_and_replays_once() {
    for baseline in [None, Some(0), Some(1)] {
        let (_d, path, c) = old_db();
        c.command("a", "pending", 1, serde_json::json!({}), AT);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        if let Some(b) = baseline {
            fixture::seed_baseline(&s, AT - 100_000, b).unwrap();
        }
        let payload =
            serde_json::json!({"operation":"a","state":{"state":"finished","outcome":"succeeded"}});
        fixture::merge_terminal_receipt(&s, &payload, AT).unwrap();
        s.read(|c| {
            assert_eq!(
                state(c, AT),
                if baseline == Some(1) {
                    Quality::Unavailable
                } else {
                    Quality::Idle
                }
            );
            assert_eq!(steve_store::statistics::RealtimeReads(c).record_count()?, 1);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}

#[test]
fn a_new_unsent_cancel_or_rejection_never_adds_unknown_coverage() {
    for payload in [
        serde_json::json!({"reason":"cancelled before it was sent"}),
        serde_json::json!({"code":"invalid_request","retry":"never"}),
    ] {
        let (_d, path, c) = old_db();
        c.command("a", "pending", 0, serde_json::json!({}), AT);
        drop(c);
        let s = Store::open(path, NoPublisher).unwrap();
        fixture::merge_unsent_rejection(&s, &payload, AT).unwrap();
        s.read(|c| {
            assert_eq!(state(c, AT), Quality::Idle);
            assert_eq!(state(c, AT - 99_000), Quality::Idle);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    }
}
