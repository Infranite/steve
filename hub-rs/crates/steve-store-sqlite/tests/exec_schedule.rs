use std::time::Duration;
use steve_store_sqlite::exec::{Head, LANES, Lane, Scheduler, index, target};
fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
fn head(enqueued: u64) -> Head {
    Head {
        enqueued: ms(enqueued),
        ready_at: ms(enqueued),
    }
}
#[test]
fn most_overdue_head_precedes_drr_and_target_is_relative_to_enqueue() {
    let mut s = Scheduler::default();
    // Interactive 60-50=10 late; admin 201-200=1 late; ingest 40-20=20 late.
    let heads = [
        Some(head(940)),
        Some(head(799)),
        Some(head(960)),
        Some(head(0)),
    ];
    assert_eq!(s.choose(ms(1000), heads), Some(Lane::Ingest));
    let heads = [heads[0], heads[1], None, heads[3]];
    assert_eq!(s.choose(ms(1000), heads), Some(Lane::Interactive));
    // Background is exactly at its target, not overdue; admin is 1 ms over.
    assert_eq!(
        s.choose(ms(1000), [None, heads[1], None, heads[3]]),
        Some(Lane::Admin)
    );
    for lane in LANES {
        assert_eq!(target(lane).as_millis(), [50, 200, 20, 1000][index(lane)]);
    }
}
#[test]
fn drr_delivers_weighted_actual_service_and_charges_long_holds() {
    let mut s = Scheduler::default();
    let heads = [Some(head(0)); 4];
    let mut counts = [0; 4];
    for _ in 0..11000 {
        let lane = s.choose(ms(0), heads).unwrap();
        counts[index(lane)] += 1;
        s.charge(lane, ms(1));
    }
    assert_eq!(counts, [4000, 4000, 2000, 1000]);
    let mut s = Scheduler::default();
    assert_eq!(s.choose(ms(0), heads), Some(Lane::Interactive));
    s.charge(Lane::Interactive, ms(100));
    let mut counts = [0; 4];
    for _ in 0..110 {
        let lane = s.choose(ms(0), heads).unwrap();
        counts[index(lane)] += 1;
        s.charge(lane, ms(1));
    }
    assert_eq!(counts[0], 0, "a long holder must repay its time deficit");
}
#[test]
fn ingest_readiness_empty_lanes_and_zero_resolution_clock_progress() {
    let mut s = Scheduler::default();
    let heads = [
        None,
        None,
        Some(Head {
            enqueued: ms(0),
            ready_at: ms(20),
        }),
        None,
    ];
    assert_eq!(s.choose(ms(19), heads), None);
    assert_eq!(s.choose(ms(20), heads), Some(Lane::Ingest));
    s.charge(Lane::Ingest, ms(1000000000));
    assert_eq!(s.choose(ms(0), [None; 4]), None);
    assert_eq!(
        s.choose(ms(20), heads),
        Some(Lane::Ingest),
        "empty lanes reset their deficit"
    );
    s.charge(Lane::Ingest, Duration::ZERO);
}
