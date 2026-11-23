use super::*;
use crate::testkit::{ME, TestEnv, WS};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;
use steve_runtime::testkit::eventually_sync;
use steve_runtime::{Clock, FakeClock};
use steve_store::testkit::modules::directory as fixture;

struct CountedClock {
    time: Arc<FakeClock>,
    reads: AtomicUsize,
    waits: Mutex<Vec<Duration>>,
}
impl Clock for CountedClock {
    fn now(&self) -> i64 {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.time.now()
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        lock(&self.waits).push(duration);
        self.time.sleep(duration)
    }
}
struct Env {
    env: TestEnv,
    sync: Arc<DirectorySync>,
    adapter: Arc<FakeDirectory>,
    clock: Arc<CountedClock>,
}
impl Env {
    fn new() -> Self {
        let env = TestEnv::new(crate::accounts::register);
        env.clock.set(1000);
        fixture::set_workspace_clocks(env.store(), "UTC").unwrap();
        let adapter = Arc::new(FakeDirectory::new(
            "feishu",
            Snapshot {
                departments: vec![Department {
                    id: "root".into(),
                    name: "Team".into(),
                    ..Default::default()
                }],
                users: vec![DirUser {
                    id: "alice".into(),
                    name: "Alice".into(),
                    department_ids: vec!["root".into()],
                    ..Default::default()
                }],
            },
        ));
        let clock = Arc::new(CountedClock {
            time: env.clock.clone(),
            reads: AtomicUsize::new(0),
            waits: Mutex::default(),
        });
        let modules = crate::Modules::with_clock(
            env.store().clone(),
            env.gateway.clone(),
            crate::Registry::new(),
            clock.clone(),
        );
        let table = TableConnections::new().with("feishu", "Test", {
            let adapter = adapter.clone();
            Arc::new(move |_| Ok(adapter.clone() as Arc<dyn DirectoryAdapter>))
        });
        let sync = DirectorySync::new(modules, Arc::new(table));
        Self {
            env,
            sync,
            adapter,
            clock,
        }
    }
    fn connection(&self, readable: bool) -> String {
        let id = uid("directory");
        self.env
            .store()
            .write(|w| -> ApiResult<()> {
                crate::contacts::service::ensure_self_org(w, WS, ME)?;
                Ok(())
            })
            .unwrap();
        fixture::seed_connection(
            self.env.store(),
            WS,
            &id,
            &format!("acc_{id}"),
            &id,
            if readable {
                r#"["directory.read"]"#
            } else {
                "[]"
            },
        )
        .unwrap();
        id
    }
    async fn start(&self) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
        // Poll the owned future once to install its Store subscription before any test write.
        let mut watch = Box::pin(self.sync.clone().watch());
        let state = poll_fn(|cx| Poll::Ready(watch.as_mut().poll(cx))).await;
        assert!(state.is_pending());
        let watcher = tokio::spawn(watch);
        let source = tokio::spawn(self.sync.clone().run());
        eventually_sync("directory baseline and first sleep", || {
            lock(&self.sync.state).observed_ready && self.env.clock.observed(self.env.clock.now())
        })
        .await;
        (source, watcher)
    }
    fn settled(&self, state: JobState) -> bool {
        lock(&self.sync.state)
            .jobs
            .iter()
            .any(|job| job.state == state)
    }
}
async fn stop(source: tokio::task::JoinHandle<()>, watcher: tokio::task::JoinHandle<()>) {
    source.abort();
    watcher.abort();
    assert!(source.await.unwrap_err().is_cancelled());
    assert!(watcher.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn real_connection_and_permission_commits_schedule_the_five_second_sync() {
    for readable in [true, false] {
        let e = Env::new();
        let (source, watcher) = e.start().await;
        let id = e.connection(readable);
        if !readable {
            eventually_sync("unreadable connection facts observed", || {
                lock(&e.sync.state).observed.contains_key(&id)
            })
            .await;
            assert!(lock(&e.sync.state).pending.is_empty());
            fixture::allow_directory(e.env.store(), &id).unwrap();
        }
        eventually_sync("committed directory connection is due soon", || {
            lock(&e.sync.state).pending.get(&(WS.into(), id.clone())) == Some(&6000)
        })
        .await;
        assert_eq!(
            e.env.clock.now(),
            1000,
            "no business event or fallback advance"
        );
        assert_eq!(e.adapter.reads(), 0);
        e.env.clock.advance(4999);
        eventually_sync("before directory quiet period", || {
            e.env.clock.observed(5999)
        })
        .await;
        assert_eq!(e.adapter.reads(), 0);
        e.env.clock.advance(1);
        eventually_sync("directory job completed", || {
            e.adapter.reads() == 1 && e.settled(JobState::Done)
        })
        .await;
        assert!(lock(&e.sync.state).pending.is_empty());
        stop(source, watcher).await;
    }
}

#[tokio::test]
async fn directory_success_and_failure_writes_cannot_self_schedule_more_jobs() {
    for fails in [false, true] {
        let e = Env::new();
        if fails {
            e.adapter
                .fail(DirectoryError::Unavailable("scripted failure".into()));
        }
        let (source, watcher) = e.start().await;
        let id = e.connection(true);
        eventually_sync("directory commit observed", || {
            lock(&e.sync.state).pending.get(&(WS.into(), id.clone())) == Some(&6000)
        })
        .await;
        e.env.clock.advance(5000);
        let terminal = if fails {
            JobState::Failed
        } else {
            JobState::Done
        };
        eventually_sync("one real directory writeback", || {
            e.adapter.reads() == 1 && e.settled(terminal)
        })
        .await;
        // No external writes during this window. last_sync, config.directory and permission
        // bookkeeping are committed by the actual worker above, not synthesized as events.
        for second in 1..=20 {
            e.env.clock.advance(1000);
            eventually_sync("self-written directory rows settle", || {
                e.env.clock.observed(6000 + second * 1000)
            })
            .await;
            assert_eq!(e.adapter.reads(), 1, "fails={fails}, second={second}");
            assert_eq!(lock(&e.sync.state).jobs.len(), 1);
            assert!(lock(&e.sync.state).pending.is_empty());
            assert!(
                e.clock.reads.load(Ordering::SeqCst) <= 256,
                "empty due rounds must also be bounded"
            );
        }
        stop(source, watcher).await;
    }
}

#[tokio::test]
async fn workspace_timezone_commit_replans_without_a_directory_event_or_job() {
    let e = Env::new();
    e.env.clock.set(21_585_000); // 05:59:45 UTC, fifteen seconds before daily work.
    lock(&e.sync.state).next_catch_up = 100_000_000;
    let (source, watcher) = e.start().await;
    assert_eq!(lock(&e.clock.waits).last(), Some(&Duration::from_secs(15)));
    fixture::set_workspace_clocks(e.env.store(), "Asia/Tokyo").unwrap();
    eventually_sync("timezone fact recomputes the daily deadline", || {
        lock(&e.clock.waits).last() == Some(&steve_runtime::due::FALLBACK)
    })
    .await;
    assert_eq!(e.env.clock.now(), 21_585_000);
    assert_eq!(e.adapter.reads(), 0);
    assert!(lock(&e.sync.state).pending.is_empty());
    stop(source, watcher).await;
}

#[tokio::test]
async fn existing_connections_still_wait_for_the_first_catch_up_after_boot() {
    let e = Env::new();
    e.connection(true);
    let (source, watcher) = e.start().await;
    assert!(lock(&e.sync.state).pending.is_empty());
    e.env.clock.advance(5000);
    eventually_sync("initial connections are not a synthetic event", || {
        e.env.clock.observed(6000)
    })
    .await;
    assert_eq!(e.adapter.reads(), 0);
    e.env.clock.advance(CATCH_UP_FIRST_MS - 5000);
    eventually_sync("original first catch-up", || {
        e.adapter.reads() == 1 && e.settled(JobState::Done)
    })
    .await;
    stop(source, watcher).await;
}

#[tokio::test]
async fn material_commits_extend_their_quiet_window_but_preserve_explicit_pending() {
    let e = Env::new();
    let (source, watcher) = e.start().await;
    let id = e.connection(true);
    let key = (WS.to_string(), id.clone());
    eventually_sync("first committed quiet period", || {
        lock(&e.sync.state).pending.get(&key) == Some(&6000)
    })
    .await;
    e.env.clock.advance(1000);
    fixture::bump_connection_revision(e.env.store(), &id).unwrap();
    eventually_sync(
        "material commit extends its own five-second quiet period",
        || lock(&e.sync.state).pending.get(&key) == Some(&7000),
    )
    .await;
    e.sync.changed(WS, &id);
    assert_eq!(
        lock(&e.sync.state).pending.get(&key),
        Some(&(2000 + DEBOUNCE_MS))
    );
    fixture::bump_connection_revision(e.env.store(), &id).unwrap();
    let signature = e
        .env
        .store()
        .read(|c| crate::accounts::connections::repo::change_signature(c, &id))
        .unwrap()
        .unwrap();
    eventually_sync("new connection fact is observed", || {
        lock(&e.sync.state).observed.get(&id) == Some(&signature)
    })
    .await;
    assert_eq!(
        lock(&e.sync.state).pending.get(&key),
        Some(&(2000 + DEBOUNCE_MS)),
        "a commit hint cannot replace the directory event's deadline"
    );
    assert!(!lock(&e.sync.state).inferred_pending.contains(&key));
    assert_eq!(e.adapter.reads(), 0);
    e.env.clock.advance(DEBOUNCE_MS);
    eventually_sync("explicit directory quiet period completes", || {
        e.adapter.reads() == 1 && e.settled(JobState::Done)
    })
    .await;
    stop(source, watcher).await;
}

#[tokio::test(start_paused = true)]
async fn workspace_commits_requery_and_cancelling_the_watch_releases_its_subscription() {
    let e = Env::new();
    let (source, watcher) = e.start().await;
    let before = lock(&e.clock.waits).len();
    fixture::rename_workspace(e.env.store(), WS, "changed workspace").unwrap();
    eventually_sync(
        "workspace commit makes the source prepare a new wait",
        || lock(&e.clock.waits).len() > before,
    )
    .await;
    assert_eq!(e.env.clock.now(), 1000);
    assert_eq!(e.adapter.reads(), 0);
    assert!(lock(&e.sync.state).pending.is_empty());
    watcher.abort();
    assert!(watcher.await.unwrap_err().is_cancelled());
    let waits = lock(&e.clock.waits).len();
    let reads = e.clock.reads.load(Ordering::SeqCst);
    fixture::rename_workspace(e.env.store(), WS, "after watch stopped").unwrap();
    // This is a paused Tokio negative window: a leaked forwarding task and the blocking
    // requery it wakes must get polled before the absence of another wait is asserted.
    steve_runtime::testkit::quiet_for(Duration::from_millis(20)).await;
    assert_eq!(lock(&e.clock.waits).len(), waits);
    assert_eq!(e.clock.reads.load(Ordering::SeqCst), reads);
    assert_eq!(e.adapter.reads(), 0);
    source.abort();
    assert!(source.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn watch_future_subscribes_before_its_first_poll() {
    let e = Env::new();
    let mut watch = Box::pin(e.sync.clone().watch());
    e.connection(true); // The Job future exists, but has never been polled.
    assert!(
        poll_fn(|cx| Poll::Ready(watch.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let mut wake = Box::pin(e.sync.wake.notified());
    assert!(
        poll_fn(|cx| Poll::Ready(wake.as_mut().poll(cx)))
            .await
            .is_ready(),
        "the real commit before the first poll must already be buffered"
    );
    drop(wake);
    drop(watch);
}

#[tokio::test]
async fn one_unreadable_connection_signature_does_not_block_other_due_connections() {
    use steve_runtime::due::DueSource;
    let e = Env::new();
    let due = DirectoryDue(e.sync.clone());
    due.next_due().await.unwrap();
    let bad = e.connection(false);
    fixture::damage_config(e.env.store(), &bad).unwrap();
    let good = e.connection(true);
    assert_eq!(due.next_due().await.unwrap(), Some(6000));
    let state = lock(&e.sync.state);
    assert_eq!(state.pending.len(), 1);
    assert_eq!(state.pending.get(&(WS.into(), good)), Some(&6000));
    assert!(!state.pending.contains_key(&(WS.into(), bad)));
}

#[tokio::test]
async fn observing_boot_facts_does_not_consume_the_first_explicit_connection_change() {
    use steve_runtime::due::DueSource;
    let e = Env::new();
    let id = e.connection(true);
    let key = (WS.to_string(), id.clone());
    let due = DirectoryDue(e.sync.clone());
    due.next_due().await.unwrap();
    assert!(lock(&e.sync.state).pending.is_empty());
    e.sync.connection_changed(WS, &id);
    assert_eq!(
        lock(&e.sync.state).pending.get(&key),
        Some(&6000),
        "the first explicit call keeps its pre-migration five-second scheduling semantics"
    );
    e.sync.changed(WS, &id);
    e.sync.connection_changed(WS, &id);
    assert_eq!(
        lock(&e.sync.state).pending.get(&key),
        Some(&(1000 + DEBOUNCE_MS)),
        "a repeated explicit call without material change must keep the directory quiet period"
    );
    assert_eq!(due.next_due().await.unwrap(), Some(1000 + DEBOUNCE_MS));
}
