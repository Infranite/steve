use super::*;
use crate::adapter::{Batch, EventAdapter, EventTypeDef, ScopeDef};
use crate::testkit::{FakeEventAdapter, FlowEnv};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, ProviderError,
};
use steve_modules::testkit::{ME, WS};
use steve_runtime::Clock;
use steve_runtime::testkit::eventually_sync;
use tokio::sync::Notify;

#[tokio::test]
async fn poll_deadline_and_retry_table() {
    let fake = Arc::new(FakeEventAdapter::poll_every(Duration::from_secs(120)));
    let env = FlowEnv::with_events(vec![fake.clone()]);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut poller = Poller::new(env.flow.clone());
    let start = env.env.clock.now();
    for (at, fail, expected) in [
        (0, false, 1),
        (119_999, false, 0),
        (120_000, true, 1),
        (359_999, false, 0),
        (360_000, true, 1),
        (839_999, false, 0),
        (840_000, true, 1),
        (1_799_999, false, 0),
        (1_800_000, true, 1),
        (3_599_999, false, 0),
        (3_600_000, true, 1),
        (5_399_999, false, 0),
        (5_400_000, false, 1),
        (5_519_999, false, 0),
        (5_520_000, true, 1),
        (5_759_999, false, 0),
        (5_760_000, false, 1),
    ] {
        env.env.clock.set(start + at);
        if fail {
            *fake.fail_next.lock().unwrap() = Some(ProviderError::Transient("scripted".into()));
        }
        assert_eq!(poller.tick().await, expected, "at={at}, fail={fail}");
    }
    let mut restarted = Poller::new(env.flow.clone());
    assert_eq!(
        restarted.tick().await,
        1,
        "restart immediately reevaluates an existing live row"
    );
    assert!(
        env.env
            .store()
            .reader(|c| crate::sources::settings(c, WS, &id))
            .unwrap()
            .cursor
            .is_some()
    );
}

#[test]
fn retry_duration_boundaries() {
    for (every, failures, expected) in [
        (Duration::from_secs(120), 0, Duration::from_secs(240)),
        (Duration::from_secs(120), 1, Duration::from_secs(480)),
        (Duration::from_secs(120), 3, MAX_BACKOFF),
        (Duration::from_secs(120), 30, MAX_BACKOFF),
        (
            Duration::from_nanos(1),
            31,
            Duration::from_nanos(u64::from(u32::MAX)),
        ),
        (Duration::ZERO, 2, Duration::ZERO),
    ] {
        assert_eq!(retry_wait(every, failures), expected);
    }
}

#[tokio::test]
async fn poll_eligibility_and_live_cleanup_table() {
    let fake = Arc::new(FakeEventAdapter::poll_every(Duration::from_secs(120)));
    let env = FlowEnv::with_events(vec![fake]);
    let a = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let b = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut poller = Poller::new(env.flow.clone());
    env.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET caps='[]' WHERE id=?", [&b])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(poller.tick().await, 1);
    env.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET status='paused' WHERE id=?", [&a])?;
            c.execute(
                "UPDATE connections SET caps='[\"events\"]' WHERE id=?",
                [&b],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(poller.tick().await, 1);
    env.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET status='ok' WHERE id=?", [&a])?;
            c.execute("UPDATE connections SET caps='[]' WHERE id=?", [&b])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        poller.tick().await,
        1,
        "row omitted from the live set loses its memory deadline"
    );
    env.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE connections SET caps='[\"events\"]' WHERE id=?",
                [&b],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        poller.tick().await,
        0,
        "temporarily missing capability is still a live row"
    );
    env.env.clock.advance(120_000);
    assert_eq!(poller.tick().await, 2);
}

#[tokio::test]
async fn paused_source_advances_the_provider_cursor_without_backlog() {
    let fake = Arc::new(FakeEventAdapter::poll_every(Duration::from_secs(120)));
    let env = FlowEnv::with_events(vec![fake.clone()]);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut poller = Poller::new(env.flow.clone());
    assert_eq!(poller.tick().await, 1);
    env.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE source_settings SET paused=1 WHERE connection_id=?",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();
    fake.emit(FakeEventAdapter::event(
        "mail.new", "paused", "INBOX", "paused",
    ));
    env.env.clock.advance(120_000);
    assert_eq!(poller.tick().await, 1);
    assert_eq!(
        env.env
            .store()
            .reader(|c| crate::sources::settings(c, WS, &id))
            .unwrap()
            .cursor
            .as_deref(),
        Some("1")
    );
    env.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE source_settings SET paused=0 WHERE connection_id=?",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();
    fake.emit(FakeEventAdapter::event("mail.new", "live", "INBOX", "live"));
    env.env.clock.advance(120_000);
    assert_eq!(poller.tick().await, 1);
    let ids = env
        .env
        .store()
        .reader(|c| -> FlowResult<Vec<String>> {
            let mut rows = c.prepare("SELECT external_id FROM ext_events ORDER BY external_id")?;
            Ok(rows
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    assert_eq!(ids, ["live"]);
}

#[tokio::test]
async fn push_and_unknown_adapters_are_not_polled() {
    for adapters in [
        vec![Arc::new(FakeEventAdapter::push()) as Arc<dyn EventAdapter>],
        vec![],
    ] {
        let env = FlowEnv::with_events(adapters);
        env.connection(WS, FakeEventAdapter::ID, None, ME);
        assert_eq!(Poller::new(env.flow.clone()).tick().await, 0);
    }
}

struct Scripted {
    base: FakeEventAdapter,
    poison: Mutex<HashSet<String>>,
    panic: Mutex<HashSet<String>>,
    hold: Mutex<HashSet<String>>,
    calls: Mutex<Vec<String>>,
    active: Arc<AtomicUsize>,
    release: Notify,
}
impl Scripted {
    fn new() -> Arc<Self> {
        Self::with_period(Duration::from_secs(120))
    }
    fn with_period(every: Duration) -> Arc<Self> {
        Arc::new(Self {
            base: FakeEventAdapter::poll_every(every),
            poison: Mutex::default(),
            panic: Mutex::default(),
            hold: Mutex::default(),
            calls: Mutex::default(),
            active: Arc::default(),
            release: Notify::new(),
        })
    }
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ConnectionProvider for Scripted {
    fn id(&self) -> &str {
        FakeEventAdapter::ID
    }
    fn system(&self) -> &str {
        self.base.system()
    }
    fn name(&self) -> &str {
        self.base.name()
    }
    fn caps(&self) -> &[&'static str] {
        self.base.caps()
    }
    fn auth(&self) -> AuthKind {
        AuthKind::None
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        self.base.verify(cx).await
    }
}
#[async_trait]
impl EventAdapter for Scripted {
    fn source(&self) -> &'static str {
        self.base.source()
    }
    fn event_types(&self) -> &'static [EventTypeDef] {
        self.base.event_types()
    }
    async fn scopes(&self, cx: &ConnCx) -> Result<Vec<ScopeDef>, AdapterError> {
        self.base.scopes(cx).await
    }
    fn mode(&self) -> IngestMode {
        self.base.mode()
    }
    async fn poll(&self, cx: &ConnCx, cursor: Option<&str>) -> Result<Batch, AdapterError> {
        self.calls.lock().unwrap().push(cx.id().into());
        let panics = self.panic.lock().unwrap().contains(cx.id());
        assert!(!panics, "scripted poll panic");
        if self.poison.lock().unwrap().contains(cx.id()) {
            return Err(AdapterError::Malformed("scripted failure".into()));
        }
        let held = self.hold.lock().unwrap().contains(cx.id());
        if held {
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            self.release.notified().await;
        }
        self.base.poll(cx, cursor).await
    }
    async fn parse_push(
        &self,
        cx: &ConnCx,
        req: &PushRequest,
    ) -> Result<Vec<RawEvent>, AdapterError> {
        self.base.parse_push(cx, req).await
    }
}

#[tokio::test]
async fn panicked_row_is_retried_and_does_not_block_other_rows() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    let bad = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let good = env.connection(WS, FakeEventAdapter::ID, None, ME);
    adapter.panic.lock().unwrap().insert(bad.clone());
    let mut poller = Poller::new(env.flow.clone());
    assert_eq!(poller.tick().await, 2);
    assert!(
        env.env
            .store()
            .reader(|c| crate::sources::settings(c, WS, &good))
            .unwrap()
            .cursor
            .is_some()
    );
    adapter.panic.lock().unwrap().clear();
    assert_eq!(poller.tick().await, 1, "panic did not advance its deadline");
}

#[tokio::test]
async fn poll_parallelism_and_cancellation_are_bounded() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    for _ in 0..6 {
        let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
        adapter.hold.lock().unwrap().insert(id);
    }
    let mut poller = Poller::new(env.flow.clone());
    let task = tokio::spawn(async move { poller.tick().await });
    eventually_sync("four active provider polls", || {
        adapter.active.load(Ordering::SeqCst) == POLL_PARALLEL
    })
    .await;
    assert_eq!(adapter.calls.lock().unwrap().len(), POLL_PARALLEL);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    eventually_sync("cancel all poll children", || {
        adapter.active.load(Ordering::SeqCst) == 0
    })
    .await;
    assert_eq!(adapter.calls.lock().unwrap().len(), POLL_PARALLEL);
}

#[tokio::test]
async fn poll_timeout_is_malformed_and_drops_the_provider_future() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    adapter.hold.lock().unwrap().insert(id.clone());
    let result = poll_bounded(&env.flow, WS, &id, Duration::ZERO).await;
    assert_eq!(result, Err(AdapterError::Malformed("拉取超时".into())));
    assert_eq!(adapter.active.load(Ordering::SeqCst), 0);
}

fn paced_source(env: &FlowEnv) -> Poller {
    let mut source = Poller::new(env.flow.clone());
    source.cadence = Limiter::with_clock(cadence_rate(), env.env.clock.clone());
    source
}

struct ClockedSource {
    source: Poller,
    time: Arc<steve_runtime::FakeClock>,
}
impl DueSource for ClockedSource {
    fn name(&self) -> &'static str {
        self.source.name()
    }
    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        self.source.next_due().await
    }
    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        // The generic checker supplies logical world time. Advance the injected clock,
        // then delegate both deadline and execution decisions to the real source.
        self.time.set(now);
        self.source.run_due(now).await
    }
}
struct PollFixture {
    env: FlowEnv,
    adapter: Arc<Scripted>,
    source: ClockedSource,
}
impl PollFixture {
    fn new(now: i64) -> Self {
        let adapter = Scripted::new();
        let env = FlowEnv::with_events(vec![adapter.clone()]);
        env.env.clock.set(now);
        let source = ClockedSource {
            source: paced_source(&env),
            time: env.env.clock.clone(),
        };
        Self {
            env,
            adapter,
            source,
        }
    }
}
impl steve_runtime::due::contract::Fixture for PollFixture {
    type Source = ClockedSource;
    fn source(&self) -> &ClockedSource {
        &self.source
    }
    fn batch(&self) -> usize {
        POLL_PARALLEL
    }
    fn add(&self, at: i64, poison: bool) -> String {
        let id = self.env.connection(WS, FakeEventAdapter::ID, None, ME);
        self.source.source.due.defer(id.clone(), at);
        if poison {
            self.adapter.poison.lock().unwrap().insert(id.clone());
        }
        id
    }
    fn done(&self) -> Vec<String> {
        self.env
            .env
            .store()
            .reader(|c| -> FlowResult<_> {
                let mut st = c.prepare(
                    "SELECT id FROM connections WHERE last_sync IS NOT NULL ORDER BY id",
                )?;
                Ok(st
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .unwrap()
    }
    fn dead_letters(&self) -> bool {
        false
    }
    fn dead(&self) -> Vec<String> {
        vec![]
    }
}
#[tokio::test]
async fn poll_source_satisfies_real_storage_due_contract() {
    let now = 1_795_000_000_000;
    steve_runtime::due::contract::check(|| PollFixture::new(now), now, 4).await;
}

#[tokio::test]
async fn round_gap_is_after_work_even_for_zero_interval() {
    for work_ms in [0, 30_000] {
        let adapter = Scripted::with_period(Duration::ZERO);
        let env = FlowEnv::with_events(vec![adapter.clone()]);
        env.env.clock.set(0);
        let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
        adapter.hold.lock().unwrap().insert(id.clone());
        let source = Arc::new(paced_source(&env));
        assert_eq!(source.next_due().await.unwrap(), Some(0));
        assert_eq!(source.next_due().await.unwrap(), Some(0));
        assert!(
            source.cadence.is_empty(),
            "querying a deadline cannot consume the round quota"
        );
        let running = {
            let source = source.clone();
            tokio::spawn(async move { source.run_due(0).await })
        };
        eventually_sync("held poll", || adapter.active.load(Ordering::SeqCst) == 1).await;
        env.env.clock.advance(work_ms);
        adapter.hold.lock().unwrap().clear();
        adapter.release.notify_one();
        running.await.unwrap().unwrap();
        assert_eq!(source.due.not_before(&id), Some(0));
        let next = work_ms + 15_000;
        assert_eq!(source.next_due().await.unwrap(), Some(next));
        env.env.clock.set(next - 1);
        source.run_due(next - 1).await.unwrap();
        assert_eq!(adapter.calls.lock().unwrap().len(), 1);
        env.env.clock.set(next);
        source.run_due(next).await.unwrap();
        assert_eq!(adapter.calls.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn next_due_does_not_consume_an_elapsed_fence_before_clock_rewind() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter]);
    env.env.clock.set(0);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    let mut source = paced_source(&env);
    assert_eq!(source.tick().await, 1);
    env.env.clock.set(150_000);
    assert_eq!(source.next_due().await.unwrap(), Some(120_000));
    env.env.clock.set(60_000);
    assert_eq!(source.tick().await, 0);
    assert_eq!(source.due.not_before(&id), Some(120_000));
}

#[tokio::test]
async fn an_early_run_cannot_postpone_a_row_retry() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    env.env.clock.set(0);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    adapter.poison.lock().unwrap().insert(id.clone());
    let source = paced_source(&env);
    source.run_due(0).await.unwrap();
    assert_eq!(source.next_due().await.unwrap(), Some(240_000));
    env.env.clock.set(239_999);
    source.run_due(239_999).await.unwrap();
    assert_eq!(
        source.next_due().await.unwrap(),
        Some(240_000),
        "an empty early round cannot consume a pacing hit"
    );
    env.env.clock.set(240_000);
    source.run_due(240_000).await.unwrap();
    assert_eq!(source.next_due().await.unwrap(), Some(720_000));
    adapter.poison.lock().unwrap().clear();
    env.env.clock.set(720_000);
    source.run_due(720_000).await.unwrap();
    assert_eq!(source.next_due().await.unwrap(), Some(840_000));
    assert_eq!(source.failure_counts().get(&id), None);
}

struct ObservedClock {
    time: Arc<steve_runtime::FakeClock>,
    sleeps: Arc<AtomicUsize>,
}
impl Clock for ObservedClock {
    fn now(&self) -> i64 {
        self.time.now()
    }
    fn sleep(
        &self,
        d: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let sleeping = self.time.sleep(d);
        let sleeps = self.sleeps.clone();
        Box::pin(async move {
            sleeps.fetch_add(1, Ordering::SeqCst);
            sleeping.await;
        })
    }
}
fn change(id: &str) -> steve_types::Notification {
    Arc::new(steve_types::Envelope {
        event: steve_types::ChannelsEvent::Connection {
            id: id.into(),
            ws: WS.into(),
        }
        .into(),
        rev: steve_types::Rev(1),
    })
}

#[tokio::test]
async fn committed_wake_replans_without_erasing_the_row_fence_or_self_spinning() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    env.env.clock.set(0);
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let sleeps = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ObservedClock {
        time: env.env.clock.clone(),
        sleeps: sleeps.clone(),
    });
    let source = paced_source(&env).with_changes(rx);
    let running = tokio::spawn(source.run_on_clock(clock));
    eventually_sync("idle fallback wait", || {
        sleeps.load(Ordering::SeqCst) > 0 && env.env.clock.observed(0)
    })
    .await;
    let first = sleeps.load(Ordering::SeqCst);
    let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
    tx.send(change(&id)).await.unwrap();
    eventually_sync("committed connection wake", || {
        adapter.calls.lock().unwrap().len() == 1 && sleeps.load(Ordering::SeqCst) > first
    })
    .await;
    let before = sleeps.load(Ordering::SeqCst);
    for _ in 0..3 {
        tx.send(change(&id)).await.unwrap();
    }
    eventually_sync("own hints only replan", || {
        sleeps.load(Ordering::SeqCst) > before && tx.capacity() == 1
    })
    .await;
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
    env.env.clock.advance(119_999);
    eventually_sync("before row deadline", || {
        env.env.clock.observed(119_999) || adapter.calls.lock().unwrap().len() > 1
    })
    .await;
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
    env.env.clock.advance(1);
    eventually_sync("at row deadline", || {
        adapter.calls.lock().unwrap().len() == 2 && env.env.clock.observed(120_000)
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert!(tx.is_closed(), "the owned observer stops with its source");
}

#[tokio::test]
async fn whole_source_read_errors_keep_the_fifteen_second_retry() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    env.env.clock.set(0);
    env.connection(WS, FakeEventAdapter::ID, None, ME);
    env.env
        .store()
        .with_conn(|c| {
            c.execute_batch("ALTER TABLE connections RENAME TO offline_connections")?;
            Ok(())
        })
        .unwrap();
    let sleeps = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ObservedClock {
        time: env.env.clock.clone(),
        sleeps: sleeps.clone(),
    });
    let source = paced_source(&env);
    let running = tokio::spawn(source.run_on_clock(clock));
    eventually_sync("first failed source read", || {
        sleeps.load(Ordering::SeqCst) > 0 && env.env.clock.observed(0)
    })
    .await;
    for now in [15_000, 30_000] {
        let before = sleeps.load(Ordering::SeqCst);
        env.env.clock.advance(14_999);
        eventually_sync("source error before retry", || {
            env.env.clock.observed(now - 1)
        })
        .await;
        assert!(adapter.calls.lock().unwrap().is_empty());
        env.env.clock.advance(1);
        eventually_sync("constant source retry", || {
            sleeps.load(Ordering::SeqCst) > before && env.env.clock.observed(now)
        })
        .await;
    }
    env.env
        .store()
        .with_conn(|c| {
            c.execute_batch("ALTER TABLE offline_connections RENAME TO connections")?;
            Ok(())
        })
        .unwrap();
    env.env.clock.advance(14_999);
    eventually_sync("restored source still waits its interval", || {
        env.env.clock.observed(44_999)
    })
    .await;
    assert!(adapter.calls.lock().unwrap().is_empty());
    env.env.clock.advance(1);
    eventually_sync("source recovery", || {
        adapter.calls.lock().unwrap().len() == 1 && env.env.clock.observed(45_000)
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn named_run_cancellation_drops_every_inflight_poll() {
    let adapter = Scripted::new();
    let env = FlowEnv::with_events(vec![adapter.clone()]);
    for _ in 0..6 {
        let id = env.connection(WS, FakeEventAdapter::ID, None, ME);
        adapter.hold.lock().unwrap().insert(id);
    }
    let source = paced_source(&env);
    let clock = env.env.clock.clone();
    let running = tokio::spawn(source.run_on_clock(clock));
    eventually_sync("source in-flight batch", || {
        adapter.active.load(Ordering::SeqCst) == POLL_PARALLEL
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    eventually_sync("source child cancellation", || {
        adapter.active.load(Ordering::SeqCst) == 0
    })
    .await;
    assert_eq!(adapter.calls.lock().unwrap().len(), POLL_PARALLEL);
}
