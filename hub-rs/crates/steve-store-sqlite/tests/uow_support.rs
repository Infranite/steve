#![allow(dead_code)]
use rusqlite::Connection;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
};
use steve_storage_api::{uow::*, *};
use steve_store_sqlite::{
    domains::Registry,
    exec::BatchRunner,
    publish::Attached,
    uow::{SqliteBatchRunner, WallClock},
};
pub struct Time(AtomicI64);
impl Default for Time {
    fn default() -> Self {
        Self(AtomicI64::new(1_000_000))
    }
}
impl Time {
    pub fn add(&self, n: i64) {
        self.0.fetch_add(n, Ordering::SeqCst);
    }
}
impl WallClock for Time {
    fn now(&self) -> Millis {
        Millis(self.0.load(Ordering::SeqCst))
    }
}
impl steve_store_sqlite::snapshot::Clock for Time {
    fn now(&self) -> Millis {
        WallClock::now(self)
    }
}
pub type Events = Arc<Mutex<Vec<(Rev, Vec<String>)>>>;
pub fn row(id: &str, n: i64) -> Row {
    Row(vec![
        ("id".into(), Cell::Text(id.into())),
        ("value".into(), Cell::Int(n)),
        ("label".into(), Cell::Null),
        ("at".into(), Cell::Millis(0)),
        ("data".into(), Cell::Null),
        ("bytes".into(), Cell::Null),
        ("real".into(), Cell::Null),
        ("flag".into(), Cell::Null),
    ])
}
pub fn set(row: &mut Row, name: &str, value: Cell) {
    let field = row.0.iter_mut().find(|(n, _)| n == name).unwrap();
    field.1 = value;
}
pub fn insert(id: &str, n: i64) -> Mutation {
    Mutation::Insert {
        entity: "verify_core".into(),
        row: row(id, n),
    }
}
pub fn update(id: &str, n: i64) -> Mutation {
    Mutation::Update {
        entity: "verify_core".into(),
        key: vec![Cell::Text(id.into())],
        set: Row(vec![("value".into(), Cell::Int(n))]),
    }
}
pub fn unit(id: &str, mutations: Vec<Mutation>) -> UnitOfWork {
    let mut u = UnitOfWork {
        domain: Domain::Core,
        id: IdempotencyKey {
            issued_at: Millis(1_000_000),
            key: id.into(),
        },
        digest: RequestDigest([0; 32]),
        lane: Lane::Interactive,
        idempotency: Idempotency::Recorded,
        preconditions: vec![],
        mutations,
        facts: vec![],
    };
    sign(&mut u);
    u
}
pub fn sign(u: &mut UnitOfWork) {
    u.digest = u.request_digest().unwrap();
}
pub fn schema(c: &Connection, registry: &Registry) {
    c.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    c.execute_batch("CREATE TABLE outbox_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,workspace_id TEXT NOT NULL,topic TEXT NOT NULL,version INTEGER NOT NULL,payload TEXT NOT NULL,created INTEGER NOT NULL);").unwrap();
    registry.install(c).unwrap();
}
pub fn fixture() -> (Connection, SqliteBatchRunner<String>, Arc<Time>, Events) {
    let c = Connection::open_in_memory().unwrap();
    let registry = Arc::new(Registry::verification().unwrap());
    schema(&c, &registry);
    let time = Arc::new(Time::default());
    let events: Events = Arc::default();
    let log = events.clone();
    let runner = SqliteBatchRunner::new(
        &c,
        registry,
        time.clone(),
        Arc::new(move |rev, events| log.lock().unwrap().push((rev, events))),
    )
    .unwrap();
    (c, runner, time, events)
}
pub fn commit(
    c: &Connection,
    r: &SqliteBatchRunner<String>,
    u: UnitOfWork,
) -> Result<Committed, CommitError> {
    r.run(c, vec![Attached::new(u, vec!["event".into()])])
        .pop()
        .unwrap()
}
pub fn value(c: &Connection, id: &str) -> Option<i64> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT value FROM verify_core WHERE id=?", [id], |r| {
        r.get(0)
    })
    .optional()
    .unwrap()
}
pub fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

pub fn prepare_layout(dir: &std::path::Path, split: bool) {
    use steve_store_sqlite::layout::{CORE_FILE, Layout};
    let mut layout = Layout::single_file();
    if split {
        for (d, file) in [
            (Domain::Stream, "stream.sqlite3"),
            (Domain::Comms, "comms.sqlite3"),
        ] {
            layout.domains.insert(d, file.into());
            layout.files.insert(file.into(), format!("fixture-{file}"));
        }
    }
    std::fs::create_dir_all(dir).unwrap();
    for file in layout.files.keys() {
        let c = Connection::open(dir.join(file)).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(steve_store_sqlite::schema::CONTRACT)
            .unwrap();
        c.execute_batch("CREATE TABLE outbox_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,workspace_id TEXT NOT NULL,topic TEXT NOT NULL,version INTEGER NOT NULL,payload TEXT NOT NULL,created INTEGER NOT NULL);").unwrap();
        steve_store_sqlite::migrate::stamp_current(&c).unwrap();
        layout.write_file_meta(&c, file).unwrap();
    }
    assert!(dir.join(CORE_FILE).exists());
}
pub fn open_sync_store(
    dir: &std::path::Path,
    registry: Arc<Registry>,
    time: Arc<Time>,
) -> steve_store_sqlite::store::SqliteStore<String> {
    let engine = loop {
        match steve_store_sqlite::open::open_admin(dir) {
            Ok(e) => break e,
            Err(e) if e.code == steve_types::Code::Busy => std::thread::yield_now(),
            Err(e) => panic!("open fixture: {e}"),
        }
    };
    let mut engines = Vec::new();
    for file in engine.layout().files.keys() {
        if engine.path() != engine.data_dir().join(file) {
            engines.push(engine.open_peer(file).unwrap());
        }
    }
    engines.push(engine);
    steve_store_sqlite::store::SqliteStore::new_sync(
        engines,
        registry,
        time.clone(),
        Arc::new(|_, _: Vec<String>| {}),
        || tokio::runtime::Handle::try_current().is_ok(),
    )
    .unwrap()
}

pub fn open_store(
    dir: &std::path::Path,
    registry: Arc<Registry>,
    time: Arc<Time>,
) -> steve_store_sqlite::store::SqliteStore<String> {
    open_sync_store(dir, registry, time.clone())
        .with_runtime(
            tokio::runtime::Handle::try_current().unwrap_or_else(|_| runtime().handle().clone()),
        )
        .unwrap()
        .with_snapshot_clock(time)
        .unwrap()
}

pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Notify(std::thread::Thread);
    impl std::task::Wake for Notify {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Notify(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

/// One Tokio runtime per test process supplies the store's real blocking pool.
/// The conformance suite still polls futures on its independent standard threads.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap()
    })
}
