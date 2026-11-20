//! The production queue source over a migrated store; only the schedule consumer is replaced.
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::json;
use steve_digest::queue::{self, BOT_BATCH, BOT_WAKE_TABLES, BotRunner, BotSource};
#[path = "../../steve-channels/tests/support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use steve_runtime::due::contract::{self, Fixture};
use steve_runtime::due::{DueSource, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
use steve_runtime::testkit::{eventually_notified, eventually_sync};
use steve_store::{Store, StoreError, uid};
use tokio::sync::Notify;

const NOW: i64 = 1_800_000_000_000;

struct Consumer {
    store: Store,
    processed: Arc<Notify>,
}
impl BotRunner for Consumer {
    fn ready(&self, _: &Connection, _: &str) -> Result<Option<i64>, StoreError> {
        Ok(Some(i64::MIN))
    }
    fn start(&self, ws: &str, now: i64) -> Result<(), StoreError> {
        self.store.transaction(|tx| -> Result<(), StoreError> {
            let b = queue::batching(tx, ws)?;
            let (_, ids) = queue::lease(tx, ws, now, &b, 10)?;
            for id in ids {
                let p = queue::get(tx, ws, &id)?.unwrap();
                if p.facts["poison"] == true {
                    // A crashed Bot never acknowledges this packet: the real lease expires,
                    // is retried, and ends in the existing persisted failed state.
                    continue;
                }
                queue::packet_done(tx, &p, true, None, &b, now)?;
            }
            Ok(())
        })?;
        self.processed.notify_one();
        Ok(())
    }
}

struct Packets {
    env: TestEnv,
    source: Arc<BotSource<Consumer>>,
    processed: Arc<Notify>,
}
impl Packets {
    fn new() -> Self {
        let env = TestEnv::new(|_| {});
        env.clock.set(NOW);
        let processed = Arc::new(Notify::new());
        let source = Arc::new(BotSource::new(
            &env.modules,
            Consumer {
                store: env.store().clone(),
                processed: processed.clone(),
            },
        ));
        Self {
            env,
            source,
            processed,
        }
    }
    fn ids(&self, state: &str) -> Vec<String> {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, StoreError>(
                    c.prepare("SELECT id FROM digest_packets WHERE state = ? ORDER BY id")?
                        .query_map([state], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?,
                )
            })
            .unwrap()
    }
}
impl Fixture for Packets {
    type Source = BotSource<Consumer>;
    fn source(&self) -> &Self::Source {
        &self.source
    }
    fn batch(&self) -> usize {
        BOT_BATCH
    }
    fn add(&self, at: i64, poison: bool) -> String {
        self.env.store().transaction(|tx| {
            let ws = uid("ws");
            tx.execute("INSERT INTO workspaces (id,name,created) VALUES (?, 'fixture', 1)", [&ws])?;
            tx.execute("INSERT INTO settings (workspace_id,key,value,updated) VALUES (?, 'digest', ?, 1)",
                params![ws, json!({"every_min":1,"packets":1000,"max_attempts":3,"lease_min":1}).to_string()])?;
            queue::insert(tx, &queue::NewPacket {
                ws: &ws, conv: "space:fixture", seq: 1, kind: "window", trigger: "quiet",
                first_id: "m", last_id: "m", first_at: at, last_at: at, count: 1, priority: 1,
                facts: &json!({"poison":poison}), candidates: &json!([]),
            }, &[], at - 60_000)
        }).unwrap()
    }
    fn done(&self) -> Vec<String> {
        self.ids("done")
    }
    fn dead(&self) -> Vec<String> {
        self.ids("failed")
    }
}

#[tokio::test]
async fn packet_source_passes_the_shared_contract_with_real_lease_failures() {
    contract::check(Packets::new, NOW, 3).await;
}

fn backoff() -> Backoff {
    Backoff {
        base: Duration::from_secs(1),
        max: Duration::from_secs(60),
        factor: 2,
        jitter: Jitter::None,
        attempts: None,
        reset_after: None,
    }
}
async fn observed(p: &Packets, at: i64) {
    eventually_sync("due loop sleeping on the fake clock", || {
        p.env.clock.observed(at)
    })
    .await;
}

#[tokio::test]
async fn packet_age_runs_at_the_deadline_between_minute_ticks() {
    let p = Packets::new();
    let at = NOW + 17_123;
    let id = p.add(at, false);
    assert_eq!(p.source.next_due().await.unwrap(), Some(at));
    let task = tokio::spawn(due_loop(
        p.source.clone(),
        p.env.clock.clone(),
        Arc::new(Notify::new()),
        backoff(),
    ));
    observed(&p, NOW).await;
    p.env.clock.advance(17_122);
    observed(&p, at - 1).await;
    assert!(p.done().is_empty());
    p.env.clock.advance(1);
    eventually_notified("packet consumed", &p.processed, || async {
        p.done().contains(&id)
    })
    .await;
    assert_eq!(p.done(), vec![id]);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn a_committed_packet_wakes_an_idle_source_without_advancing_time() {
    let p = Packets::new();
    assert_eq!(p.source.next_due().await.unwrap(), None);
    let wake = p.env.store().commit_wake(BOT_WAKE_TABLES);
    let task = tokio::spawn(due_loop(
        p.source.clone(),
        p.env.clock.clone(),
        wake.clone(),
        backoff(),
    ));
    observed(&p, NOW).await;
    let id = p.add(NOW, false);
    eventually_notified("packet consumed", &p.processed, || async {
        p.done().contains(&id)
    })
    .await;
    assert_eq!(p.done(), vec![id]);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn expired_leases_fail_at_the_attempt_limit_even_without_a_runnable_bot() {
    struct Unbound;
    impl BotRunner for Unbound {
        fn ready(&self, _: &Connection, _: &str) -> Result<Option<i64>, StoreError> {
            Ok(None)
        }
        fn start(&self, _: &str, _: i64) -> Result<(), StoreError> {
            panic!("unbound Bot")
        }
    }
    let p = Packets::new();
    let id = p.add(NOW, true);
    p.source.run_due(NOW).await.unwrap();
    p.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE digest_packets SET attempts = 3 WHERE id = ?", [&id])?;
        c.execute("UPDATE settings SET value = '{\"enabled\":false,\"max_attempts\":3}' WHERE key = 'digest'", [])?;
            Ok(())
        })
        .unwrap();
    let source = BotSource::new(&p.env.modules, Unbound);
    assert_eq!(source.next_due().await.unwrap(), Some(NOW + 60_000));
    source.run_due(NOW + 59_999).await.unwrap();
    assert!(p.dead().is_empty());
    source.run_due(NOW + 60_000).await.unwrap();
    assert_eq!(p.dead(), vec![id]);
    assert_eq!(source.next_due().await.unwrap(), None);
}

#[tokio::test]
async fn packet_count_can_start_before_the_age_deadline() {
    let p = Packets::new();
    let id = p.add(NOW + 60_000, false);
    p.env.store().with_conn(|c| {
        c.execute("UPDATE settings SET value = '{\"packets\":1,\"every_min\":30}' WHERE key = 'digest'", [])?;
        Ok(())
    }).unwrap();
    assert_eq!(p.source.next_due().await.unwrap(), Some(NOW));
    p.source.run_due(NOW).await.unwrap();
    assert_eq!(p.done(), vec![id]);
}

#[tokio::test]
async fn packet_commits_do_not_self_wake_into_a_busy_loop() {
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use steve_runtime::{Clock, FakeClock, due::DueError};

    // Freeze domain wall time while independently advancing the loop's fallback waits.
    struct FixedWall(Arc<FakeClock>);
    impl Clock for FixedWall {
        fn now(&self) -> i64 {
            NOW
        }
        fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.0.sleep(d)
        }
    }
    struct Counted {
        source: Arc<BotSource<Consumer>>,
        runs: AtomicUsize,
        checks: AtomicUsize,
    }
    impl DueSource for Counted {
        fn name(&self) -> &'static str {
            self.source.name()
        }
        async fn next_due(&self) -> Result<Option<i64>, DueError> {
            let next = self.source.next_due().await;
            self.checks.fetch_add(1, Ordering::SeqCst);
            next
        }
        async fn run_due(&self, now: i64) -> Result<(), DueError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.source.run_due(now).await
        }
    }
    for poison in [false, true] {
        let p = Packets::new();
        let id = p.add(NOW, poison);
        let source = Arc::new(Counted {
            source: p.source.clone(),
            runs: AtomicUsize::new(0),
            checks: AtomicUsize::new(0),
        });
        let elapsed = Arc::new(FakeClock::new(0));
        let task = steve_runtime::spawn(
            "test.digest.bot",
            due_loop(
                source.clone(),
                Arc::new(FixedWall(elapsed.clone())),
                p.env.store().commit_wake(BOT_WAKE_TABLES),
                backoff(),
            ),
        );
        eventually_sync("packet written and source sleeping", || {
            source.checks.load(Ordering::SeqCst) >= 2 && elapsed.observed(0)
        })
        .await;
        assert_eq!(p.ids(if poison { "leased" } else { "done" }), vec![id]);
        for at in [60_000, 120_000, 180_000] {
            let before = source.checks.load(Ordering::SeqCst);
            elapsed.advance(60_000);
            eventually_sync("fallback rechecked and source sleeping again", || {
                source.checks.load(Ordering::SeqCst) > before && elapsed.observed(at)
            })
            .await;
            assert!(
                !task.is_finished(),
                "a failed loop cannot count as bounded work"
            );
            assert_eq!(p.env.clock.now(), NOW, "domain wall time stays frozen");
            assert_eq!(
                source.runs.load(Ordering::SeqCst),
                1,
                "own packet writes may recheck, but must not rerun without a new deadline"
            );
        }
        task.stop();
        task.await;
    }
}
