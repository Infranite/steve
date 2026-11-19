//! The production queue source over a migrated store; only the schedule consumer is replaced.
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::json;
use steve_digest::queue::{self, BOT_BATCH, BotRunner, BotSource};
use steve_modules::testkit::TestEnv;
use steve_runtime::due::contract::{self, Fixture};
use steve_runtime::due::{DueSource, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
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
        self.store.transaction(|tx| {
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
    tokio::time::timeout(Duration::from_secs(30), async {
        while !p.env.clock.observed(at) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("due loop sleeping on the fake clock");
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
    tokio::time::timeout(Duration::from_secs(30), p.processed.notified())
        .await
        .unwrap();
    assert_eq!(p.done(), vec![id]);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn a_committed_packet_wakes_an_idle_source_without_advancing_time() {
    let p = Packets::new();
    assert_eq!(p.source.next_due().await.unwrap(), None);
    let wake = Arc::new(Notify::new());
    let task = tokio::spawn(due_loop(
        p.source.clone(),
        p.env.clock.clone(),
        wake.clone(),
        backoff(),
    ));
    observed(&p, NOW).await;
    let id = p.add(NOW, false);
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(30), p.processed.notified())
        .await
        .unwrap();
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
