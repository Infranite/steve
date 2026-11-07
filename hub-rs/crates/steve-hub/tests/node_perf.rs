//! What a Node's text costs the Hub, through the real coordinator and store: one long message,
//! taken in and applied in batches as a Node delivers them. Run by hand, in release:
//! `cargo test --release -p steve-hub --test node_perf -- --ignored --nocapture`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steve_hub::wire::node_runtime::{Binding, NodeDomain, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{Store, StoreError};

struct Quiet;
impl NodeDomain for Quiet {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

/// Keeps every batch the Node delivers (unacknowledged), to hand it to the Hub in batches later.
struct Stash(Mutex<Vec<NodeEvent>>);

#[async_trait::async_trait]
impl EventSink for Stash {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.0.lock().unwrap().extend(batch.events);
        Ok(EventAck { durable: vec![] })
    }
}

/// CPU seconds of the whole process (the apply worker runs on blocking threads).
fn cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 * 1e-6;
    t(u.ru_utime) + t(u.ru_stime)
}

const S: &str = "s1";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn long_message_cost() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            Ok(())
        })
        .unwrap();
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Quiet));
    let hub = Sink(Arc::downgrade(&rt));
    node.connect(Arc::new(hub));
    rt.start();
    rt.bind(&Binding {
        session: S.into(),
        resource: format!("item:{S}"),
        prepare: PrepareKind::Scratch,
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();
    rt.prompt(S, "hello", "user", &[]).unwrap();
    let q = |sql: &str| -> i64 {
        store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while q("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 0 {
        assert!(Instant::now() < deadline, "no running attempt");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let exec: ExecutionRef = serde_json::from_str(
        &store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT execution FROM runtime_attempts",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap(),
    )
    .unwrap();

    // the Node numbers the whole message first; the Hub gets it in batches of 200 afterwards
    let total: u64 = std::env::var("PERF_DELTAS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6100);
    let stash = Arc::new(Stash(Mutex::new(vec![])));
    node.connect(stash.clone());
    let subject = Subject::Execution(exec.clone());
    node.emit(
        subject.clone(),
        EventBody::MessageStarted {
            message: "m1".into(),
            block: "b1".into(),
            role: Role::Assistant,
        },
    )
    .await
    .unwrap();
    let mut off = 0u64;
    for i in 0..total {
        let text = [
            "让我先看一下这个函数",
            " the `receive` path ",
            "，然后再改测试。",
        ][i as usize % 3]
            .to_string();
        let n = text.len() as u64;
        node.emit(
            subject.clone(),
            EventBody::TextDelta {
                message: "m1".into(),
                block: "b1".into(),
                utf8_offset: off,
                text,
            },
        )
        .await
        .unwrap();
        off += n;
    }
    let events = std::mem::take(&mut *stash.0.lock().unwrap());
    let hub = Sink(Arc::downgrade(&rt));
    let texts = || q("SELECT COUNT(*) FROM events WHERE kind='text' AND format='steve.v1'");
    let mut sent = 0usize;
    let deliver = |upto: usize, sent: &mut usize| {
        let mut batches = vec![];
        while *sent < upto {
            let end = (*sent + 200).min(upto);
            batches.push(EventBatch {
                events: events[*sent..end].to_vec(),
            });
            *sent = end;
        }
        batches
    };
    println!(
        "| delta in the message | Hub CPU µs per delta (receive + apply) | wall µs per delta |"
    );
    println!("|---|---|---|");
    // (already in the message, measured)
    for (at, n) in [(0usize, 100usize), (1000, 100), (3000, 100), (6000, 100)] {
        if at + n + 1 > events.len() {
            break;
        }
        for b in deliver(at + 1, &mut sent) {
            hub.commit(b).await.unwrap();
        }
        let want = at as i64;
        while texts() < want {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let (c0, w0) = (cpu(), Instant::now());
        for b in deliver(at + 1 + n, &mut sent) {
            hub.commit(b).await.unwrap();
        }
        let want = (at + n) as i64;
        while texts() < want {
            tokio::time::sleep(Duration::from_micros(200)).await;
        }
        let (c, w) = (cpu() - c0, w0.elapsed().as_secs_f64());
        println!(
            "| {at}–{} | {:.0} | {:.0} |",
            at + n,
            c * 1e6 / n as f64,
            w * 1e6 / n as f64
        );
    }
    // many short messages (100 deltas each), all handed over at once: throughput
    let messages = 20u64;
    for m in 0..messages {
        let message = format!("m{}", m + 2);
        node.emit(
            subject.clone(),
            EventBody::MessageStarted {
                message: message.clone(),
                block: "b1".into(),
                role: Role::Assistant,
            },
        )
        .await
        .unwrap();
        let mut off = 0u64;
        for i in 0..100u64 {
            let text = format!("chunk {i} 的文字，");
            let n = text.len() as u64;
            node.emit(
                subject.clone(),
                EventBody::TextDelta {
                    message: message.clone(),
                    block: "b1".into(),
                    utf8_offset: off,
                    text,
                },
            )
            .await
            .unwrap();
            off += n;
        }
    }
    let events = std::mem::take(&mut *stash.0.lock().unwrap());
    let left = (messages * 100) as i64;
    let have = texts();
    let (c0, w0) = (cpu(), Instant::now());
    for chunk in events.chunks(200) {
        hub.commit(EventBatch {
            events: chunk.to_vec(),
        })
        .await
        .unwrap();
    }
    while texts() < have + left {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let (c, w) = (cpu() - c0, w0.elapsed().as_secs_f64());
    println!(
        "\n{left} deltas in {messages} messages of 100: {:.0} µs CPU and {:.0} µs wall per delta, {:.0} deltas/s",
        c * 1e6 / left as f64,
        w * 1e6 / left as f64,
        left as f64 / w
    );
}
