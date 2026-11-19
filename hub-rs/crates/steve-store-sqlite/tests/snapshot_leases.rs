#[path = "scan_support.rs"]
mod support;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
    mpsc,
};
use steve_storage_api::*;
use steve_store_sqlite::snapshot::{self, Clock, LeaseManager};
use steve_types::Code;
use support::*;
struct Manual(AtomicI64);
impl Clock for Manual {
    fn now(&self) -> Millis {
        Millis(self.0.load(Ordering::SeqCst))
    }
}
#[test]
fn a_batch_pins_one_snapshot_and_rejects_mixed_domains_and_body_entities() {
    let f = Fixture::new();
    f.seed(1);
    let mut calls = 0;
    let output = snapshot::read_batch_with(
        &f.engine,
        Domain::Core,
        vec![(f.desc.clone(), f.get(1)), (f.desc.clone(), f.get(1))],
        |c, e, q| {
            let result = steve_store_sqlite::scan::query(c, e, q)?;
            calls += 1;
            if calls == 1 {
                f.writer
                    .execute("UPDATE rec SET score=999,rev=2", [])
                    .unwrap();
            }
            Ok(result)
        },
    )
    .unwrap();
    for out in output {
        let row: Option<(Row, Rev)> = out.body.decode().unwrap();
        assert_eq!(row.unwrap().1, Rev(1));
    }
    assert_eq!(
        snapshot::read_batch(&f.engine, Domain::Comms, vec![(f.desc.clone(), f.get(1))])
            .unwrap_err()
            .code,
        Code::Invalid
    );
    let q = f.q(
        "get",
        serde_json::json!({"key":{"entity":"other","values":[Cell::Int(1)]}}),
    );
    assert_eq!(
        snapshot::read_batch(&f.engine, Domain::Core, vec![(f.desc.clone(), q)])
            .unwrap_err()
            .code,
        Code::Invalid
    );
    assert!(f.engine.read_conn().unwrap().is_autocommit());
}
#[test]
fn virtual_clock_expiry_returns_the_connection_and_ids_are_unforgeable() {
    let f = Fixture::new();
    f.seed(1);
    let clock = Arc::new(Manual(AtomicI64::new(1000)));
    let leases = LeaseManager::with_clock(f.engine.clone(), clock.clone()).unwrap();
    let lease = leases.begin(Domain::Core, Lane::Interactive).unwrap();
    assert_eq!(lease.expires_at, Millis(3000));
    f.writer.execute("UPDATE rec SET rev=2", []).unwrap();
    let row: Option<(Row, Rev)> = leases
        .query(&lease, f.desc.clone(), f.get(1))
        .unwrap()
        .body
        .decode()
        .unwrap();
    assert_eq!(row.unwrap().1, Rev(1));
    let mut forged = lease.clone();
    forged.expires_at = Millis(4000);
    assert_eq!(
        leases
            .query(&forged, f.desc.clone(), f.get(1))
            .unwrap_err()
            .code,
        Code::Stale
    );
    let mut q = f.get(1);
    q.domain = Domain::Comms;
    assert_eq!(
        leases.query(&lease, f.desc.clone(), q).unwrap_err().code,
        Code::Invalid
    );
    clock.0.store(3000, Ordering::SeqCst);
    leases.reap_expired();
    assert_eq!(
        leases
            .query(&lease, f.desc.clone(), f.get(1))
            .unwrap_err()
            .code,
        Code::Stale
    );
    assert_eq!(leases.end(&lease).unwrap_err().code, Code::Stale);
    let pool_state = f.engine.readers().state();
    assert_eq!(pool_state.connections, pool_state.idle_connections);
    let lease = leases.begin(Domain::Core, Lane::Admin).unwrap();
    leases.end(&lease).unwrap();
    assert!(f.engine.read_conn().unwrap().is_autocommit());
}
#[test]
fn expiry_interrupts_a_running_recursive_query_before_rollback_and_pool_return() {
    let f = Fixture::new();
    let clock = Arc::new(Manual(AtomicI64::new(0)));
    let leases = Arc::new(LeaseManager::with_clock(f.engine.clone(), clock.clone()).unwrap());
    let lease = leases.begin(Domain::Core, Lane::Background).unwrap();
    let (ready, started) = mpsc::sync_channel(1);
    let reader = leases.clone();
    let id = lease.clone();
    let running = std::thread::spawn(move || {
        reader.with_connection(&id,Domain::Core,move|conn|{
        let mut ready=Some(ready);
        conn.progress_handler(1000,Some(move||{if let Some(ready)=ready.take(){ready.send(()).unwrap();}false})).unwrap();
        let result=conn.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",[],|r|r.get::<_,i64>(0));
        conn.progress_handler(0,None::<fn()->bool>).unwrap();
        result.map_err(|_|error(Code::Internal,"statement interrupted"))
    })
    });
    started.recv().unwrap(); // SQLite's VM is executing, not merely queued on the worker.
    clock.0.store(2000, Ordering::SeqCst);
    leases.reap_expired();
    assert_eq!(running.join().unwrap().unwrap_err().code, Code::Stale);
    let pool_state = f.engine.readers().state();
    assert_eq!(pool_state.connections, pool_state.idle_connections);
    assert_eq!(
        leases
            .query(&lease, f.desc.clone(), f.get(1))
            .unwrap_err()
            .code,
        Code::Stale
    );
    let conn = f.engine.read_conn().unwrap();
    assert!(conn.is_autocommit());
    assert_eq!(
        conn.query_row("SELECT 7", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
}
#[test]
fn lease_queries_serialize_and_manager_drop_releases_idle_leases() {
    let f = Fixture::new();
    let leases = Arc::new(
        LeaseManager::with_clock(f.engine.clone(), Arc::new(Manual(AtomicI64::new(0)))).unwrap(),
    );
    let lease = leases.begin(Domain::Core, Lane::Admin).unwrap();
    let (ready, started) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let first = leases.clone();
    let id = lease.clone();
    let a = std::thread::spawn(move || {
        first.with_connection(&id, Domain::Core, move |_| {
            ready.send(()).unwrap();
            resume.recv().unwrap();
            Ok(1)
        })
    });
    started.recv().unwrap();
    let second = leases.clone();
    let id = lease.clone();
    let b = std::thread::spawn(move || second.with_connection(&id, Domain::Core, |_| Ok(2)));
    release.send(()).unwrap();
    assert_eq!(a.join().unwrap().unwrap(), 1);
    assert_eq!(b.join().unwrap().unwrap(), 2);
    drop(leases);
    let pool_state = f.engine.readers().state();
    assert_eq!(pool_state.connections, pool_state.idle_connections);
}
