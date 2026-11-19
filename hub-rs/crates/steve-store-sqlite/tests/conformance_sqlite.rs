//! Opt-in suite: RUSTFLAGS='--cfg sqlite_conformance' cargo nextest run -p
//! steve-store-sqlite --features steve-storage-api/conformance --test conformance_sqlite.
//! The conformance feature remains test-only without changing the production manifest.
#![allow(unexpected_cfgs)]
#![cfg(sqlite_conformance)]
#[path = "support/digest.rs"]
mod digest_fixture;
#[path = "uow_support.rs"]
mod support;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, Weak, mpsc},
};
use steve_storage_api::verify::block_on;
use steve_storage_api::{
    entity::*,
    query::*,
    uow::*,
    verify::{self, *},
    *,
};
use steve_store_sqlite::{domains::Registry, store::SqliteStore, uow::WallClock};
use support::*;

type Live = (
    Weak<SqliteStore<String>>,
    mpsc::Receiver<()>,
    Arc<std::sync::atomic::AtomicBool>,
);
struct Lifecycle {
    split: bool,
    time: Arc<Time>,
    dirs: Mutex<Vec<tempfile::TempDir>>,
    paths: Mutex<BTreeMap<usize, PathBuf>>,
    live: Mutex<BTreeMap<usize, Live>>,
}
impl Lifecycle {
    fn new(split: bool) -> Self {
        Self {
            split,
            time: Arc::new(Time::default()),
            dirs: Mutex::default(),
            paths: Mutex::default(),
            live: Mutex::default(),
        }
    }
    fn load(&self, path: &Path) -> Arc<dyn DomainStore> {
        let (ready, started) = mpsc::channel();
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let evidence = interrupted.clone();
        let mut registry = Registry::verification().unwrap();
        registry.register_query("verify.slow",Domain::Core,move |conn,_|{
            let mut ready=Some(ready.clone());
            conn.progress_handler(1000,Some(move || {if let Some(send)=ready.take(){let _=send.send(());}false})).unwrap();
            let result=conn.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",[],|r|r.get::<_,i64>(0));
            conn.progress_handler(0,None::<fn()->bool>).unwrap();
            if result.as_ref().is_err_and(|e|e.sqlite_error_code()==Some(rusqlite::ErrorCode::OperationInterrupted)){evidence.store(true,std::sync::atomic::Ordering::SeqCst);}
            result.map_err(|_|steve_storage_api::error(steve_types::Code::Stale,"query interrupted")).and_then(|n|encode(&n))
        }).unwrap();
        let store = Arc::new(open_store(path, Arc::new(registry), self.time.clone()));
        store.install_registered().unwrap();
        let weak = Arc::downgrade(&store);
        let store: Arc<dyn DomainStore> = store;
        self.live.lock().unwrap().insert(
            Arc::as_ptr(&store) as *const () as usize,
            (weak, started, interrupted),
        );
        self.paths
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&store) as *const () as usize, path.to_owned());
        store
    }
    fn path(&self, store: &Arc<dyn DomainStore>) -> PathBuf {
        self.paths.lock().unwrap()[&(Arc::as_ptr(store) as *const () as usize)].clone()
    }
}
impl Harness for Lifecycle {
    fn open(&self) -> Result<Arc<dyn DomainStore>, String> {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), self.split);
        let store = self.load(dir.path());
        self.dirs.lock().unwrap().push(dir);
        Ok(store)
    }
    fn digest_fixture(
        &self,
        _store: Arc<dyn DomainStore>,
    ) -> Result<Box<dyn verify::digest::Fixture>, String> {
        // A separate real-schema namespace avoids advertising generic export for
        // production tables whose codecs are intentionally not registered.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        prepare_layout(dir.path(), self.split);
        let mut registry = Registry::default();
        registry.register_digest().map_err(|e| e.to_string())?;
        let store = Arc::new(open_store(
            dir.path(),
            Arc::new(registry),
            self.time.clone(),
        ));
        let fixture = digest_fixture::backend::NativeFixture::new(store);
        self.dirs.lock().unwrap().push(dir);
        Ok(Box::new(fixture))
    }
    fn now(&self) -> Millis {
        self.time.now()
    }
    fn advance_clock(&self, n: i64) -> Result<(), String> {
        self.time.add(n);
        for (weak, _, _) in self.live.lock().unwrap().values() {
            if let Some(store) = weak.upgrade() {
                store.reap_snapshots();
            }
        }
        Ok(())
    }
    fn unavailable(&self, _: Capability) -> Option<String> {
        None
    }
    fn interrupt_expired_query(&self, store: Arc<dyn DomainStore>) -> Result<(), String> {
        let lease = block_on(store.begin_snapshot(Domain::Core, Lane::Background)).unwrap();
        let reader = store.clone();
        let id = lease.clone();
        let running = std::thread::spawn(move || {
            block_on(reader.lease_query(
                id,
                AnyQuery {
                    name: "verify.slow".into(),
                    domain: Domain::Core,
                    body: Payload::Json(encode(&()).unwrap()),
                },
            ))
        });
        let key = Arc::as_ptr(&store) as *const () as usize;
        self.live.lock().unwrap()[&key].1.recv().unwrap();
        self.advance_clock(2000)?;
        assert_eq!(
            running.join().unwrap().unwrap_err().code,
            steve_types::Code::Stale
        );
        assert!(
            self.live.lock().unwrap()[&key]
                .2
                .load(std::sync::atomic::Ordering::SeqCst),
            "SQLite must confirm SQLITE_INTERRUPT"
        );
        let real = self.live.lock().unwrap()[&key].0.upgrade().unwrap();
        let state = real.engine(Domain::Core).readers().state();
        assert_eq!(state.connections, state.idle_connections);
        let fresh = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
        block_on(store.lease_end(fresh)).unwrap();
        Ok(())
    }
    fn reopen(&self, store: Arc<dyn DomainStore>) -> Result<Arc<dyn DomainStore>, String> {
        let path = self.path(&store);
        drop(store);
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "durability_child", "--nocapture"])
            .env("SQLITE_DURABILITY_PATH", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "durability child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("SQLITE-DURABILITY-REOPENED"));
        Ok(self.load(&path))
    }
    fn crash_commit(
        &self,
        store: Arc<dyn DomainStore>,
        uow: UnitOfWork,
    ) -> Result<CrashOutcome, String> {
        let path = self.path(&store);
        drop(store);
        let input = path.join("crash-input.json");
        std::fs::write(&input, serde_json::to_vec(&uow).unwrap()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_child", "--nocapture"])
            .env("SQLITE_CRASH_PATH", &path)
            .env("SQLITE_CRASH_NOW", self.now().0.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            if output.read_line(&mut line).unwrap() == 0 {
                let status = child.wait().unwrap();
                return Err(format!("child exited before in-flight barrier: {status}"));
            }
            if line.contains("SQLITE-UOW-IN-FLIGHT") {
                break;
            }
        }
        let status = verify::kill_and_reap(&mut child).unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        std::fs::remove_file(input).unwrap();
        Ok(CrashOutcome {
            store: self.load(&path),
            killed_during_commit: true,
        })
    }
}
#[test]
fn durability_child() {
    let Some(path) = std::env::var_os("SQLITE_DURABILITY_PATH") else {
        return;
    };
    let registry = Arc::new(Registry::verification().unwrap());
    let store = open_store(
        Path::new(&path),
        registry.clone(),
        Arc::new(Time::default()),
    );
    // Read every fixture from its reopened physical writer's pool in a separate process.
    // The shared suite then checks exact values, cursors, facts and saved results in the parent.
    for e in registry.entities() {
        let q = Scan::<Row> {
            index: IndexName {
                entity: e.desc.name.clone(),
                name: String::new(),
            },
            range: KeyRange::default(),
            filter: vec![],
            order: Order::Asc,
            limit: 500,
            budget: ScanBudget(5000),
            resume: None,
            marker: std::marker::PhantomData,
        };
        block_on(store.query(AnyQuery {
            name: "scan".into(),
            domain: e.desc.domain,
            body: Payload::Json(encode(&q).unwrap()),
        }))
        .unwrap();
    }
    println!("SQLITE-DURABILITY-REOPENED");
}

#[test]
fn crash_child() {
    let Some(path) = std::env::var_os("SQLITE_CRASH_PATH") else {
        return;
    };
    let path = PathBuf::from(path);
    let mut registry = Registry::verification().unwrap();
    registry
        .register_op("verify.pause", Domain::Core, |conn, _| {
            assert!(
                !conn.is_autocommit(),
                "kill barrier must be inside the work transaction"
            );
            println!("\nSQLITE-UOW-IN-FLIGHT");
            std::io::stdout().flush().unwrap();
            // Never leave the transaction on stdin EOF: Child::wait closes stdin,
            // which could otherwise race delivery of SIGKILL and roll the unit back.
            loop {
                std::thread::park();
            }
        })
        .unwrap();
    let time = Arc::new(Time::default());
    time.add(
        std::env::var("SQLITE_CRASH_NOW")
            .unwrap()
            .parse::<i64>()
            .unwrap()
            - time.now().0,
    );
    let store = open_store(&path, Arc::new(registry), time);
    let mut u: UnitOfWork =
        serde_json::from_slice(&std::fs::read(path.join("crash-input.json")).unwrap()).unwrap();
    u.mutations.push(Mutation::Op(AnyOp {
        name: "verify.pause".into(),
        domain: Domain::Core,
        body: encode(&Row::default()).unwrap(),
    }));
    sign(&mut u);
    let _ = verify::block_on(store.commit(u));
}
fn run_suite(split: bool) {
    assert_eq!(
        steve_store_sqlite::domains::fixtures::descriptors(),
        verify::entities::descriptors()
            .into_iter()
            .filter(|e| !verify::digest::descriptors()
                .iter()
                .any(|d| d.name == e.name))
            .collect::<Vec<_>>()
    );
    let harness = Lifecycle::new(split);
    let report = verify::run_with_named(&harness, &verify::digest::named_checks());
    println!(
        "CONFORMANCE_RAW split={split}: {}",
        serde_json::to_string(&report).unwrap()
    );
    let failures: Vec<_> = report
        .items
        .iter()
        .filter(|item| item.status != Status::Passed)
        .map(|item| format!("{}: {:?} {:?}", item.name, item.status, item.detail))
        .collect();
    println!(
        "CONFORMANCE split={split}: {}",
        serde_json::to_string(&report).unwrap()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn single_file_contract_suite() {
    run_suite(false)
}
#[test]
fn three_file_contract_suite() {
    run_suite(true)
}
