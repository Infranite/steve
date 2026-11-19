#[path = "exec_support.rs"]
mod support;
use std::sync::{Arc, mpsc};
use steve_storage_api::uow::CommitError;
use steve_store_sqlite::exec::{Lane, QUEUE_LIMIT, Step};
use support::*;

#[test]
fn each_lane_rejects_the_1025th_unit_but_root_grants_are_not_capped() {
    let (e, _, _) = executor();
    let held = hold(&e, Lane::Background);
    let mut receivers = Vec::new();
    for lane in [
        Lane::Interactive,
        Lane::Admin,
        Lane::Ingest,
        Lane::Background,
    ] {
        for id in 0..QUEUE_LIMIT {
            receivers.push(
                e.submit(
                    lane,
                    Job {
                        id,
                        ms: 0,
                        fail: false,
                    },
                )
                .unwrap(),
            );
        }
        assert_eq!(e.queue_depth(lane), QUEUE_LIMIT);
        assert!(matches!(
            e.submit(lane, job(QUEUE_LIMIT)),
            Err(CommitError::Retryable { .. })
        ));
    }
    let (grant, ready) = queued_grant(e.clone(), Lane::Interactive);
    assert_eq!(e.queue_depth(Lane::Interactive), QUEUE_LIMIT + 1);
    held.finish();
    ready.recv().unwrap();
    grant.finish();
    // All ingest batches are full, so this test needs no real or virtual waiting window.
    for rx in receivers {
        assert!(rx.blocking_recv().unwrap().is_ok());
    }
}
#[test]
fn ingest_batches_cap_at_128_and_remainder_waits_from_head_enqueue() {
    let (e, time, runner) = executor();
    let held = hold(&e, Lane::Admin);
    let mut receivers = Vec::new();
    for id in 0..129 {
        receivers.push(
            e.submit(
                Lane::Ingest,
                Job {
                    id,
                    ms: 0,
                    fail: false,
                },
            )
            .unwrap(),
        );
    }
    let mut tail = receivers.pop().unwrap();
    held.finish();
    for (id, rx) in receivers.into_iter().enumerate() {
        assert_eq!(rx.blocking_recv().unwrap().unwrap(), id);
    }
    let _g = e.grant(Lane::Admin).unwrap();
    drop(_g); // worker has considered the unready remainder
    assert!(tail.try_recv().is_err());
    assert_eq!(runner.batches.lock().unwrap()[0].len(), 128);
    time.advance(19);
    e.wake();
    let _g = e.grant(Lane::Admin).unwrap();
    drop(_g);
    assert!(tail.try_recv().is_err());
    time.advance(1);
    e.wake();
    assert_eq!(tail.blocking_recv().unwrap().unwrap(), 128);
    assert_eq!(
        runner
            .batches
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![128, 1]
    );
}
#[test]
fn ingest_window_is_relative_and_other_lanes_continue_while_it_collects() {
    let (e, time, runner) = executor();
    time.advance(37);
    let mut a = e
        .submit(
            Lane::Ingest,
            Job {
                id: 1,
                ms: 0,
                fail: false,
            },
        )
        .unwrap();
    let mut b = e
        .submit(
            Lane::Ingest,
            Job {
                id: 2,
                ms: 0,
                fail: false,
            },
        )
        .unwrap();
    drop(e.grant(Lane::Interactive).unwrap());
    assert!(a.try_recv().is_err());
    time.advance(19);
    e.wake();
    drop(e.grant(Lane::Admin).unwrap());
    assert!(a.try_recv().is_err());
    assert!(b.try_recv().is_err());
    time.advance(1);
    e.wake();
    assert_eq!(a.blocking_recv().unwrap().unwrap(), 1);
    assert_eq!(b.blocking_recv().unwrap().unwrap(), 2);
    assert_eq!(*runner.batches.lock().unwrap(), vec![vec![1, 2]]);
}
#[test]
fn interactive_does_not_wait_and_batch_outcomes_remain_independent() {
    let (e, _, runner) = executor();
    let held = hold(&e, Lane::Admin);
    let a = e.submit(Lane::Interactive, job(1)).unwrap();
    let b = e
        .submit(
            Lane::Interactive,
            Job {
                id: 2,
                fail: true,
                ms: 1,
            },
        )
        .unwrap();
    let c = e.submit(Lane::Interactive, job(3)).unwrap();
    held.finish();
    assert_eq!(a.blocking_recv().unwrap().unwrap(), 1);
    assert!(b.blocking_recv().unwrap().is_err());
    assert_eq!(c.blocking_recv().unwrap().unwrap(), 3);
    assert_eq!(*runner.batches.lock().unwrap(), vec![vec![1, 2, 3]]);
}
#[test]
fn slices_yield_to_waiting_interactive_work_and_report_overruns() {
    let probe = Probe::default();
    assert!(metrics::set_global_recorder(probe.clone()).is_ok());
    let (e, time, runner) = executor();
    let batches = runner.batches.clone();
    let (started, ack) = mpsc::channel();
    let (proceed, permission) = mpsc::channel();
    let mut count = 0;
    let done = e
        .slice(move |_| {
            count += 1;
            batches.lock().unwrap().push(vec![100 + count]);
            if count == 1 {
                started.send(()).unwrap();
                permission.recv().unwrap();
                time.advance(21);
                Step::More
            } else {
                Step::Done
            }
        })
        .unwrap();
    ack.recv().unwrap();
    let urgent = e.submit(Lane::Interactive, job(1)).unwrap();
    proceed.send(()).unwrap();
    urgent.blocking_recv().unwrap().unwrap();
    done.blocking_recv().unwrap().unwrap();
    assert_eq!(
        *runner.batches.lock().unwrap(),
        vec![vec![101], vec![1], vec![102]]
    );
    assert_eq!(probe.total("steve_store_slice_overruns_total"), 1.);
}

struct Panics;
impl steve_store_sqlite::exec::BatchRunner<()> for Panics {
    type Job = bool;
    type Output = ();
    fn run(&self, _: &(), jobs: Vec<bool>) -> Vec<Result<(), CommitError>> {
        assert!(!jobs[0], "runner panic");
        vec![Ok(())]
    }
}
#[test]
fn runner_panic_and_shutdown_do_not_strand_receivers_or_the_writer() {
    let e = steve_store_sqlite::exec::Executor::new(
        "panic-test",
        (),
        Arc::new(Time::default()),
        Panics,
        || false,
    )
    .unwrap();
    assert!(
        e.submit(Lane::Admin, true)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .is_err()
    );
    assert!(
        e.submit(Lane::Admin, false)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .is_ok()
    );
    let grant = e.grant(Lane::Interactive).unwrap();
    // A grant is independent of the last executor handle; shutdown must not join its own holder.
    drop(e);
    drop(grant);
    let (e, _, _) = executor();
    let held = hold(&e, Lane::Admin);
    let pending = e.submit(Lane::Interactive, job(7)).unwrap();
    // The holder owns a handle until it exits, so submit is still drained normally here.
    held.finish();
    assert_eq!(pending.blocking_recv().unwrap().unwrap(), 7);
    let pending = e
        .submit(
            Lane::Ingest,
            Job {
                id: 8,
                ms: 0,
                fail: false,
            },
        )
        .unwrap();
    drop(e);
    assert!(pending.blocking_recv().is_err());
}
