#[path = "uow_support.rs"]
mod support;
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};
use steve_storage_api::*;
use support::*;
fn block_on<F: Future>(f: F) -> F::Output {
    struct Notify(std::thread::Thread);
    impl Wake for Notify {
        fn wake(self: Arc<Self>) {
            self.0.unpark()
        }
    }
    let w = Waker::from(Arc::new(Notify(std::thread::current())));
    let mut c = Context::from_waker(&w);
    let mut f = std::pin::pin!(f);
    loop {
        match f.as_mut().poll(&mut c) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}
#[test]
fn engine_executor_domain_store_commit_and_legacy_bridge_in_both_layouts() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let s = open_store(
            dir.path(),
            Arc::new(steve_store_sqlite::domains::Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
        s.install_registered().unwrap();
        let u = unit("store", vec![insert("a", 1)]);
        let result = block_on(s.commit(u.clone())).unwrap();
        assert!(block_on(s.commit(u)).unwrap().replayed);
        let results = block_on(s.commit_many(vec![
            unit("before", vec![insert("b", 2)]),
            unit("unique", vec![insert("a", 9)]),
            unit("after", vec![insert("c", 3)]),
        ]));
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_ok());
        assert!(result.rev < results[0].as_ref().unwrap().rev);
        s.with_root(Domain::Core, Lane::Admin, |c, runner, rev| {
            assert_eq!(value(c, "c"), Some(3));
            assert!(matches!(
                s.submit_with_events(unit("rejected", vec![]), vec![]),
                Err(CommitError::UowInsideGrant)
            ));
            c.execute_batch("BEGIN IMMEDIATE").unwrap();
            let applied = runner.apply_in(
                c,
                steve_store_sqlite::publish::Attached::new(
                    unit("bridge", vec![insert("root", 4)]),
                    vec![],
                ),
                rev,
            )?;
            c.execute_batch("COMMIT").unwrap();
            drop(applied);
            Ok(())
        })
        .unwrap();
        let comms = {
            let mut u = unit(
                "comms",
                vec![uow::Mutation::Insert {
                    entity: "verify_comms".into(),
                    row: row("x", 5),
                }],
            );
            u.domain = Domain::Comms;
            sign(&mut u);
            u
        };
        block_on(s.commit(comms)).unwrap();
        assert!(
            s.hello()
                .domains
                .iter()
                .all(|d| d.features.contains("cas") && !d.features.contains("snapshot_read"))
        );
        assert!(block_on(s.begin_snapshot(Domain::Core, Lane::Admin)).is_err());
    }
}

#[test]
fn root_callback_panic_cleans_sql_without_poisoning_engine_mutex() {
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let s = open_store(
        dir.path(),
        Arc::new(steve_store_sqlite::domains::Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    s.install_registered().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), CommitError> = s.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch(
                "BEGIN IMMEDIATE; INSERT INTO verify_core(id,value,rev) VALUES('discard',1,1)",
            )
            .unwrap();
            panic!("caller panic");
        });
    }));
    assert!(result.is_err());
    s.with_root(Domain::Core, Lane::Admin, |c, _, _| {
        assert!(c.is_autocommit());
        assert_eq!(value(c, "discard"), None);
        Ok(())
    })
    .unwrap();
    block_on(s.commit(unit("after-root-panic", vec![insert("healthy", 1)]))).unwrap();
}
