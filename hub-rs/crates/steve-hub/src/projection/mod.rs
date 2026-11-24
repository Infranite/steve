//! The read projection: per workspace, the views of the hot entities (`steve_types::view`), kept in
//! memory and changed by what the store publishes after its commits. An event only marks the
//! entity it names dirty; a flush reads those views again from their owner (`steve_store::views`),
//! compares and sends what changed as one [`Patch`] frame through the bus. Nothing about the
//! business is worked out here.
//!
//! - **Coalescing**, per workspace: the first change after a quiet [`WINDOW`] flushes at once, the
//!   changes within one are flushed together at its end.
//! - **Revisions**: a patch carries the bus revision the projection had seen when it read
//!   (everything committed up to it is in what it read). Clients keep, per entity, the copy with
//!   the larger revision; a later commit makes a later patch, so the last one wins.
//! - **Snapshots** come from memory (no store call): the projection first catches up with the bus
//!   as it was when asked, and flushes that workspace.
//! - **Correctness**: every [`RECONCILE_EVERY`] the hot sets are read again and compared; a
//!   difference that was not a change on its way is a write that published no event (a defect),
//!   counted in `steve_projection_reconcile_diff_total` and corrected. Events the tap lost
//!   rebuild everything.
//!
//! The kinds' views go to the web client in place of their thin events ([`replaced`]).

mod flush;
mod kinds;
mod reconcile;
mod target;

use std::sync::Arc;
use std::time::Duration;

use steve_store::Store;
use steve_types::{BusEvent, Patch, Rev, Snapshot, SnapshotViews, ViewKind};
use tokio::sync::{mpsc, oneshot};

use crate::bus::{Bus, Commit};

pub(crate) use kinds::{RuntimeRoutes, read_by_id};

/// Changes within this long of a flush wait for the next one.
const WINDOW: Duration = Duration::from_millis(150);
/// A frame of at most about this many bytes; a larger patch goes in several, with one revision.
const FRAME_BYTES: usize = 256 * 1024;
const RECONCILE_EVERY: Duration = Duration::from_secs(600);
/// How long a snapshot waits for the projection to catch up with the bus.
const CATCH_UP: Duration = Duration::from_secs(2);

/// Whether `e`'s kind reaches clients as patches, so its thin event is not sent to them.
pub fn replaced(e: &BusEvent) -> bool {
    matches!(
        e,
        BusEvent::Domain(steve_types::DomainEvent::Work(
            steve_types::WorkEvent::Decision { .. }
                | steve_types::WorkEvent::Item { .. }
                | steve_types::WorkEvent::Session { .. }
        ))
    ) || matches!(e, BusEvent::Domain(steve_types::DomainEvent::Work(
        steve_types::WorkEvent::Message { space, .. }
    )) if space == "steve")
}

/// Whether `e` can change a view (the projection reads something again for it): the commits a
/// client waits to see in patches after its command (`x-steve-rev`).
pub fn changes_views(e: &BusEvent) -> bool {
    e.as_domain()
        .is_some_and(|d| !matches!(target::target(d), target::Dirty::None))
}

/// What a snapshot is made of: the hot views of one workspace, as of `rev`.
pub struct Snap {
    pub rev: u64,
    /// The revision of the workspace's last change (its ETag).
    pub changed: u64,
    pub cursor: String,
    pub views: SnapshotViews,
}

impl Snap {
    pub fn into_snapshot(self, epoch: &str, kinds: &[ViewKind]) -> Snapshot {
        let mut views = self.views;
        if !kinds.contains(&ViewKind::Decision) {
            views.decision = None;
        }
        if !kinds.contains(&ViewKind::Item) {
            views.item = None;
        }
        if !kinds.contains(&ViewKind::Session) {
            views.session = None;
        }
        if !kinds.contains(&ViewKind::Message) {
            views.message = None;
        }
        Snapshot {
            epoch: epoch.into(),
            rev: Rev(self.rev),
            cursor: self.cursor,
            views,
        }
    }
}

enum Ask {
    Snapshot {
        ws: String,
        /// the bus revision when it was asked: what it has to include
        upto: u64,
        reply: oneshot::Sender<Snap>,
    },
    /// Reconcile now (tests); answers the differences found.
    Reconcile { reply: oneshot::Sender<u64> },
}

type Tap = tokio::sync::broadcast::Receiver<Arc<Commit>>;

/// The projection's handle: asks go to its one task, which owns the views.
pub struct Projection {
    store: Store,
    bus: Bus,
    routes: RuntimeRoutes,
    asks: mpsc::Sender<Ask>,
    parts: std::sync::Mutex<Option<(Tap, mpsc::Receiver<Ask>)>>,
}

impl Projection {
    /// Subscribes to the bus now, so nothing published from here on is missed; [`Self::start`]
    /// loads and runs it.
    pub(crate) fn new(store: Store, bus: Bus, routes: RuntimeRoutes) -> Arc<Self> {
        let tap = bus.commits();
        let (asks, rx) = mpsc::channel(256);
        Arc::new(Self {
            store,
            bus,
            routes,
            asks,
            parts: std::sync::Mutex::new(Some((tap, rx))),
        })
    }

    /// The projection's task (once).
    pub fn start(&self) -> Option<steve_runtime::jobs::Task> {
        let (tap, asks) = self
            .parts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        let run = flush::Run::new(
            self.store.clone(),
            self.bus.clone(),
            tap,
            self.routes.clone(),
        );
        Some(steve_runtime::spawn("hub.projection", run.run(asks)))
    }

    /// The hot views of `ws`, including everything committed before this call; `None` when the
    /// projection could not catch up in time (or is not running).
    pub async fn snapshot(&self, ws: &str) -> Option<Snap> {
        let (reply, rx) = oneshot::channel();
        let ask = Ask::Snapshot {
            ws: ws.to_owned(),
            upto: self.bus.rev().0,
            reply,
        };
        self.asks.send(ask).await.ok()?;
        // loading at start may take a while; the task gives up on a catch-up itself
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .ok()?
            .ok()
    }

    /// Reconciles every workspace now; the number of differences.
    pub async fn reconcile(&self) -> Option<u64> {
        let (reply, rx) = oneshot::channel();
        self.asks.send(Ask::Reconcile { reply }).await.ok()?;
        rx.await.ok()
    }
}

/// One patch frame (or several, when large) for `ws`, as of `rev`.
fn publish(bus: &Bus, ws: &str, rev: u64, ops: Vec<(steve_types::PatchOp, usize)>) {
    let mut frame = Vec::new();
    let mut bytes = 0;
    for (op, size) in ops {
        if !frame.is_empty() && bytes + size > FRAME_BYTES {
            bus.publish_patch(Patch::new(ws.into(), std::mem::take(&mut frame)), Rev(rev));
            bytes = 0;
        }
        bytes += size;
        frame.push(op);
    }
    if !frame.is_empty() {
        bus.publish_patch(Patch::new(ws.into(), frame), Rev(rev));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use steve_store::{NewDecision, Views};
    use steve_types::DecisionView;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        bus: Bus,
        p: Arc<Projection>,
        _task: steve_runtime::jobs::Task,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let bus = Bus::with_window(4096, 8 << 20);
        let store = Store::open(
            dir.path().join("steve.sqlite3"),
            crate::wire::store::BusPublisher(bus.clone()),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        let p = Projection::new(store.clone(), bus.clone(), RuntimeRoutes::default());
        let task = p.start().unwrap();
        Fixture {
            _dir: dir,
            store,
            bus,
            p,
            _task: task,
        }
    }

    fn open_in(store: &Store, ws: &str) -> Vec<DecisionView> {
        let mut v: Vec<_> = store
            .read(|r| r.decision_views().open())
            .unwrap()
            .into_iter()
            .filter(|d| d.ws.as_str() == ws)
            .collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
        v
    }

    fn create(store: &Store, ws: &str, n: u64) -> String {
        store
            .decisions()
            .create(&NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "test".into(),
                subject_id: format!("s{n}"),
                question: format!("q{n}"),
                requested_by: "steve".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap()
            .id
    }

    /// The next patch frame changing `id` to `status`, as the stream sends it.
    async fn next_patch(
        rx: &mut tokio::sync::broadcast::Receiver<Arc<crate::bus::Delivery>>,
        id: &str,
        status: &str,
    ) -> Value {
        loop {
            let f = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("a patch in time")
                .unwrap();
            if matches!(f.envelope.event, BusEvent::Patch(_)) {
                let v: Value =
                    serde_json::from_slice(&crate::api::stream::encode_event(&f.envelope)).unwrap();
                if v["ops"][0]["id"] == id && v["ops"][0]["view"]["status"] == status {
                    return v;
                }
            }
        }
    }

    /// Patch frames published so far, as the stream sends them.
    fn patches(rx: &mut tokio::sync::broadcast::Receiver<Arc<crate::bus::Delivery>>) -> Vec<Value> {
        let mut out = vec![];
        while let Ok(f) = rx.try_recv() {
            if matches!(f.envelope.event, BusEvent::Patch(_)) {
                let bytes = crate::api::stream::encode_event(&f.envelope);
                out.push(serde_json::from_slice(&bytes).unwrap());
            }
        }
        out
    }

    /// Any sequence of writes: after each, the views kept incrementally are those a reload reads,
    /// a workspace's patch revisions never go down, and reconciling finds nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn incremental_views_equal_a_reload_after_every_write() {
        let f = fixture();
        let mut rx = f.bus.subscribe();
        let wss = ["default", "w2"];
        let mut ids: Vec<String> = vec![];
        let mut last_rev: HashMap<String, u64> = HashMap::new();
        let mut seed: u64 = 7;
        for step in 0..80u64 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let pick = (seed >> 33) as usize;
            let ws = wss[pick % 2];
            match (pick / 2) % 4 {
                0 | 1 if ids.len() < 12 => ids.push(create(&f.store, ws, step)),
                0 => {
                    let id = &ids[pick % ids.len()];
                    f.store
                        .decisions()
                        .resolve(
                            id,
                            &json!({"answer": step}),
                            steve_store::decision_repo::DecisionChange::Resolve,
                            Some(&steve_store::DecisionUndo::Domain),
                        )
                        .unwrap();
                }
                1 | 2 if !ids.is_empty() => {
                    let id = &ids[pick % ids.len()];
                    f.store.decisions().reopen(id).unwrap();
                }
                _ if !ids.is_empty() => {
                    let id = &ids[pick % ids.len()];
                    f.store
                        .decisions()
                        .set_result(id, &json!({"note": step}))
                        .unwrap();
                }
                _ => ids.push(create(&f.store, ws, step)),
            }
            for ws in wss {
                let snap = f.p.snapshot(ws).await.expect("caught up");
                assert_eq!(
                    snap.views.decision.unwrap(),
                    open_in(&f.store, ws),
                    "step {step}, {ws}"
                );
            }
            for p in patches(&mut rx) {
                let (ws, rev) = (p["ws"].as_str().unwrap(), p["rev"].as_u64().unwrap());
                let last = last_rev.entry(ws.into()).or_default();
                assert!(rev >= *last, "patch revisions of {ws} went down");
                *last = rev;
            }
        }
        assert_eq!(f.p.reconcile().await, Some(0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_thousand_item_change_sequences_match_hot_reads_and_patches() {
        let f = fixture();
        let ws = "default";
        let bot = f
            .store
            .bots()
            .create(
                ws,
                &steve_store::NewBot {
                    name: "Projection fixture".into(),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .unwrap();
        let item = f
            .store
            .items()
            .create(
                ws,
                &steve_store::NewItem {
                    title: "Repeated item changes".into(),
                    goal: "Verify projection".into(),
                    bot_id: bot.id,
                    tags: vec![f.store.tags().kinds(ws).unwrap().remove(0).id],
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        // The initial load establishes a baseline without publishing patches.
        f.p.snapshot(ws).await.expect("the projection is warm");
        let mut rx = f.bus.subscribe();
        // A stream client reads the initial hot state before expecting incremental changes.
        let initial = f.p.snapshot(ws).await.unwrap().views.item.unwrap();
        assert_eq!(initial[0].id.as_str(), item.id);
        let mut random = 7_u64;
        for sequence in 0..1_000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let current = f.store.items().get(&item.id).unwrap().unwrap();
            let phase = if random >> 63 == 0 {
                steve_types::ItemPhase::Active
            } else {
                steve_types::ItemPhase::Paused
            };
            f.store
                .write(|w| {
                    steve_store::lifecycle_write_in(
                        w,
                        &item.id,
                        &steve_store::LifecycleWrite {
                            phase,
                            cycle: current.cycle,
                            delivery_revision: current.delivery_revision,
                            ..Default::default()
                        },
                    )?;
                    Ok::<_, steve_store::StoreError>(())
                })
                .unwrap();
            f.store
                .items()
                .patch(
                    &item.id,
                    &steve_store::ItemPatch {
                        title: Some(format!("Item at step {sequence}")),
                        ..Default::default()
                    },
                )
                .unwrap();
            if random & 1 == 0 {
                let current = f.store.items().get(&item.id).unwrap().unwrap();
                f.store
                    .write(|w| {
                        steve_store::lifecycle_write_in(
                            w,
                            &item.id,
                            &steve_store::LifecycleWrite {
                                phase: steve_types::ItemPhase::Closed,
                                cycle: current.cycle,
                                delivery_revision: current.delivery_revision + 1,
                                acceptance: Some("owner".into()),
                                outcome: Some("completed".into()),
                                ..Default::default()
                            },
                        )?;
                        Ok::<_, steve_store::StoreError>(())
                    })
                    .unwrap();
            }
            let snapshot = f.p.snapshot(ws).await.unwrap();
            let snap = snapshot.views.item.unwrap();
            let read = f.store.read(|r| r.item_views().hot()).unwrap();
            assert_eq!(snap, read, "full read at sequence {sequence}");

            // The stream can still have older patches in flight when the snapshot is returned.
            // The item CAS revision identifies the write independently of the workspace's ETag.
            let revision = snap[0].revision;
            let replayed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let delivery = rx.recv().await.unwrap();
                    if !matches!(delivery.envelope.event, BusEvent::Patch(_)) {
                        continue;
                    }
                    let bytes = crate::api::stream::encode_event(&delivery.envelope);
                    let frame: Value = serde_json::from_slice(&bytes).unwrap();
                    if let Some(op) = frame["ops"].as_array().unwrap().iter().find(|op| {
                        op["type"] == "item"
                            && op["id"] == item.id
                            && op["view"]["revision"]
                                .as_i64()
                                .is_some_and(|seen| seen >= revision)
                    }) {
                        break op["view"].clone();
                    }
                }
            })
            .await
            .unwrap_or_else(|error| {
                panic!("patch for sequence {sequence}, item revision {revision}: {error}")
            });
            assert_eq!(
                replayed,
                serde_json::to_value(&snap[0]).unwrap(),
                "patch at sequence {sequence}"
            );
            if random & 1 == 0 {
                f.store
                    .write(|w| {
                        steve_store::lifecycle_write_in(
                            w,
                            &item.id,
                            &steve_store::LifecycleWrite {
                                phase: steve_types::ItemPhase::Active,
                                cycle: snap[0].cycle + 1,
                                ..Default::default()
                            },
                        )?;
                        Ok::<_, steve_store::StoreError>(())
                    })
                    .unwrap();
            }
        }
        assert_eq!(f.p.reconcile().await, Some(0));
    }

    /// The frame format, and what a write that published no event looks like: reconciling counts
    /// it, corrects the view and sends the correction.
    #[tokio::test(flavor = "multi_thread")]
    async fn patches_carry_changes_and_reconcile_corrects_a_silent_write() {
        let f = fixture();
        let mut rx = f.bus.subscribe();
        let id = create(&f.store, "default", 1);
        assert_eq!(
            f.p.snapshot("default")
                .await
                .unwrap()
                .views
                .decision
                .unwrap()
                .len(),
            1
        );
        f.store
            .decisions()
            .resolve(
                &id,
                &json!({"answer": "ok"}),
                steve_store::decision_repo::DecisionChange::Resolve,
                None,
            )
            .unwrap();
        let snap = f.p.snapshot("default").await.unwrap();
        assert!(snap.views.decision.unwrap().is_empty());
        let last = next_patch(&mut rx, &id, "resolved").await;
        assert_eq!(last["type"], "patch");
        assert_eq!(last["ws"], "default");
        assert!(last["rev"].as_u64().unwrap() <= snap.rev);
        assert_eq!(last["ops"][0]["type"], "decision");
        assert_eq!(last["ops"][0]["op"], "upsert");
        assert_eq!(last["ops"][0]["id"], id.as_str());
        assert_eq!(last["ops"][0]["view"]["status"], "resolved");
        assert_eq!(last["ops"][0]["view"]["result"]["answer"], "ok");

        // reopened behind the projection's back: no event
        steve_store::testkit::hub::views::silently_reopen_decision(&f.store, &id).unwrap();
        assert_eq!(f.p.reconcile().await, Some(1));
        assert_eq!(
            f.p.snapshot("default")
                .await
                .unwrap()
                .views
                .decision
                .unwrap()
                .len(),
            1
        );
        next_patch(&mut rx, &id, "open").await;
        assert_eq!(f.p.reconcile().await, Some(0));
    }
    #[tokio::test]
    async fn placement_facts_in_patches_match_a_fresh_read() {
        use steve_store::{NewBot, NewItem, NewSession, bot_dispatch};
        use steve_types::{BotPlacement, Millis, PlacementExplanation, PlacementReason};

        let f = fixture();
        let ws = "default";
        let bot = f
            .store
            .bots()
            .create(
                ws,
                &NewBot {
                    name: "Review Bot".into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .unwrap();
        let item = f
            .store
            .items()
            .create(
                ws,
                &NewItem {
                    title: "Review".into(),
                    bot_id: bot.id.clone(),
                    tags: vec![f.store.tags().kinds(ws).unwrap().remove(0).id],
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let session = f
            .store
            .sessions()
            .create(&NewSession {
                workspace_id: ws.into(),
                item_id: Some(item.id.clone()),
                bot_id: Some(bot.id.clone()),
                harness: "recorded-harness".into(),
                model: Some("recorded-model".into()),
                ..Default::default()
            })
            .unwrap();
        f.p.snapshot(ws).await.unwrap();
        let mut rx = f.bus.subscribe();
        let fact = BotPlacement {
            rule_version: 1,
            bot_revision: bot.revision,
            at: Millis(7),
            node: "local".into(),
            required_node: None,
            node_priority: 0,
            harness: session.harness.clone(),
            model: session.model.clone(),
            reasoning_effort: None,
            explanation: PlacementExplanation {
                bot: bot.id.as_str().into(),
                reason: PlacementReason::Specialty,
                matched: vec!["code review".into()],
                rejected: vec![],
                fallback: None,
            },
        };
        f.store
            .write(|w| bot_dispatch::record(w, ws, &item.id, Some(&session.id), Ok(&fact)))
            .unwrap();
        let (read_item, read_session) = f
            .store
            .read(|r| {
                Ok::<_, steve_store::StoreError>((
                    r.item_views()
                        .of(ws, std::slice::from_ref(&item.id))?
                        .remove(0),
                    r.session_views()
                        .of(ws, std::slice::from_ref(&session.id))?
                        .remove(0),
                ))
            })
            .unwrap();
        assert_eq!(read_item.placement, Some(fact.explanation.clone()));
        assert_eq!(read_session.placement, read_item.placement);
        assert_eq!(read_session.model.as_deref(), Some("recorded-model"));
        let snapshot = f.p.snapshot(ws).await.unwrap();
        assert!(snapshot.views.item.unwrap().contains(&read_item));
        assert!(snapshot.views.session.unwrap().contains(&read_session));
        let expected = [
            ("item", serde_json::to_value(read_item).unwrap()),
            ("session", serde_json::to_value(read_session).unwrap()),
        ];
        let mut seen = std::collections::HashSet::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while seen.len() != expected.len() {
                let delivery = rx.recv().await.unwrap();
                if matches!(delivery.envelope.event, BusEvent::Patch(_)) {
                    let patch: Value = serde_json::from_slice(&crate::api::stream::encode_event(
                        &delivery.envelope,
                    ))
                    .unwrap();
                    for op in patch["ops"].as_array().unwrap() {
                        for (kind, view) in &expected {
                            if op["type"] == *kind && op["view"] == *view {
                                seen.insert(*kind);
                            }
                        }
                    }
                }
            }
        })
        .await
        .expect("both placement views appear in SSE patches");
        assert!(
            f.store
                .write(|w| bot_dispatch::record(w, ws, &item.id, Some(&session.id), Ok(&fact)))
                .is_err()
        );
    }

    #[tokio::test]
    async fn changes_to_items_and_sessions_match_a_fresh_hot_read() {
        use steve_store::{ItemPatch, NewBot, NewItem, NewSession, SessionPatch};
        let f = fixture();
        let ws = "default";
        let kind = f.store.tags().kinds(ws).unwrap().remove(0).id;
        let bot = f
            .store
            .bots()
            .create(
                ws,
                &NewBot {
                    name: "Fixture".into(),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .unwrap();
        let mut pairs = Vec::new();
        for n in 0..4 {
            let item = f
                .store
                .items()
                .create(
                    ws,
                    &NewItem {
                        title: format!("Item {n}"),
                        goal: "A goal".into(),
                        bot_id: bot.id.clone(),
                        tags: vec![kind.clone()],
                        ..Default::default()
                    },
                )
                .unwrap()
                .unwrap();
            let sess = f
                .store
                .sessions()
                .create(&NewSession {
                    workspace_id: ws.into(),
                    item_id: Some(item.id.clone()),
                    harness: "fixture".into(),
                    ..Default::default()
                })
                .unwrap();
            pairs.push((item.id, sess.id));
        }
        let mut seed = 83u64;
        let mut seen = 0;
        for _ in 0..36 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let (item, sess) = &pairs[(seed >> 32) as usize % pairs.len()];
            match (seed >> 40) % 7 {
                0 | 1 => {
                    f.store
                        .items()
                        .patch(
                            item,
                            &ItemPatch {
                                archived: Some(seed & 1 == 0),
                                star: Some(seed & 8 == 0),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                }
                2 => {
                    f.store
                        .items()
                        .add_goal(item, &format!("Goal {seed}"))
                        .unwrap();
                }
                3 => {
                    f.store
                        .sessions()
                        .patch(
                            sess,
                            &SessionPatch {
                                status: Some(if seed & 1 == 0 { "ended" } else { "idle" }.into()),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                }
                4 => {
                    if f.store.sessions().get(sess).unwrap().unwrap().status != "ended" {
                        f.store.queued().add(sess, "queued", "user", &[]).unwrap();
                    }
                }
                5 => {
                    f.store.queued().take(sess).unwrap();
                }
                _ => {
                    f.store
                        .timeline()
                        .add(item, "owner", "comment", "Detail update", None, &[])
                        .unwrap();
                }
            }
            let snapshot = f.p.snapshot(ws).await.unwrap();
            let mut fresh = kinds::hot_all(&f.store, &RuntimeRoutes::default()).unwrap();
            fresh.sort_by(|a, b| (a.kind(), a.id()).cmp(&(b.kind(), b.id())));
            let current: Vec<steve_types::View> = snapshot
                .views
                .item
                .unwrap()
                .into_iter()
                .map(steve_types::View::Item)
                .chain(
                    snapshot
                        .views
                        .session
                        .unwrap()
                        .into_iter()
                        .map(steve_types::View::Session),
                )
                .collect();
            assert_eq!(current, fresh);
            assert!(snapshot.rev >= seen);
            seen = snapshot.rev;
        }
        assert_eq!(f.p.reconcile().await, Some(0));
    }

    /// Decision lifecycle acceptance: >= 1,000 resolve / undo / expire / supersede / dismiss / CAS conflict
    /// sequences plus tier matrix (`Decide` vs `Draft` vs `Folded`) match hot reads & patches.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_thousand_decision_sequences_match_projection_and_tier_matrix() {
        use steve_store::account_rows::NewAccount;
        use steve_store::channel_message_repo::outbound::{self as chan_out, NewMessage};
        use steve_store::channel_spaces::registration::{self as space_reg, Space as NewSpace};
        use steve_store::decision_repo::{DecisionChange, choice_receipt};
        use steve_store::{
            Accounts, ConnRow, Connections, Contacts, DecisionRows, DecisionSource, DecisionUndo,
            HealthState, NewDecision, NewParty, StoreError,
        };
        use steve_types::{DecisionStatus, DecisionTier, FoldReason};

        let f = fixture();
        let ws = "default";
        let now = steve_store::now();

        // Seed tier matrix fixtures: a chat space with an unstarred draft (Draft = 0 inbox
        // interruption), an owner approval card (Decide = 1 prompt), and an expired/superseded
        // card (Folded).
        let (acc_id, space_id) = f
            .store
            .write(|tx| {
                let p = Contacts::new(tx).insert(ws, &NewParty::new("contact", "Peer", "owner"))?;
                let acc = Accounts(tx).insert(
                    ws,
                    &NewAccount {
                        system_id: "feishu".into(),
                        family: "chat".into(),
                        party_id: p.id,
                        kind: "user".into(),
                        handle: "u_peer".into(),
                        handle_display: "Peer".into(),
                        display_name: "Peer".into(),
                        verified: true,
                        confidence: 1.0,
                        source: "manual".into(),
                        source_label: "test".into(),
                    },
                )?;
                Connections(tx).insert(&ConnRow {
                    id: "conn_tier_1".into(),
                    workspace_id: ws.into(),
                    account_id: acc.id.clone(),
                    adapter: "feishu".into(),
                    auth: "oauth".into(),
                    granted_scopes: vec![],
                    caps: vec![],
                    credential_item: None,
                    status: "ok".into(),
                    health: HealthState::default(),
                    config: json!({}),
                    last_sync: None,
                    created_by: "owner".into(),
                    revision: 1,
                    created: now,
                    updated: now,
                })?;
                space_reg::create(
                    tx,
                    &NewSpace {
                        ws,
                        system: "feishu",
                        connection: "conn_tier_1",
                        kind: "group",
                        external: "chat_tier_1",
                        name: "Tier Group",
                        member_count: 2,
                        has_external: false,
                        disclosure: "public",
                        platform: Some("feishu"),
                        now,
                    },
                )?;
                let space = steve_store::channel_spaces::rows::by_chat(tx, ws, "chat_tier_1")?
                    .pop()
                    .expect("space")
                    .id;
                Ok::<_, StoreError>((acc.id, space))
            })
            .unwrap();

        // Non-owner-interruption quiet draft (`chan_message` approve, unstarred) -> Draft tier.
        let draft_dec = f
            .store
            .write(|tx| {
                let (d, opened) = DecisionRows(tx).open_in(&NewDecision {
                    workspace_id: ws.into(),
                    kind: "approve".into(),
                    subject_type: "chan_message".into(),
                    subject_id: "cmsg_tier_draft".into(),
                    question: "Send reply?".into(),
                    draft: Some("quiet draft".into()),
                    options: Some(vec!["yes".into(), "no".into()]),
                    requested_by: "steve".into(),
                    source: Some(DecisionSource::Revision {
                        revision: "tier:draft:1".into(),
                    }),
                    ..Default::default()
                })?;
                assert!(opened);
                chan_out::insert(
                    tx,
                    &NewMessage {
                        id: "cmsg_tier_draft",
                        ws,
                        space: &space_id,
                        thread: None,
                        reply_to: None,
                        account: Some(&acc_id),
                        persona: None,
                        on_behalf: "self",
                        text: Some("quiet draft"),
                        status: "draft",
                        created: now,
                    },
                )?;
                chan_out::link_decision(tx, "cmsg_tier_draft", &d.id)?;
                Ok::<_, StoreError>(d)
            })
            .unwrap();

        // Owner-required approval (`approve` / `action`) -> Decide tier (1 prompt).
        let owner_dec = f
            .store
            .write(|tx| {
                let (d, opened) = DecisionRows(tx).open_in(&NewDecision {
                    workspace_id: ws.into(),
                    kind: "approve".into(),
                    subject_type: "action".into(),
                    subject_id: "act_tier_1".into(),
                    question: "Approve action?".into(),
                    options: Some(vec!["yes".into(), "no".into()]),
                    requested_by: "steve".into(),
                    source: Some(DecisionSource::Revision {
                        revision: "tier:owner:1".into(),
                    }),
                    ..Default::default()
                })?;
                assert!(opened);
                Ok::<_, StoreError>(d)
            })
            .unwrap();

        let snap = f.p.snapshot(ws).await.unwrap();
        let decisions = snap.views.decision.unwrap();
        let draft_view = decisions
            .iter()
            .find(|v| v.id.as_str() == draft_dec.id)
            .expect("draft in hot views");
        assert_eq!(draft_view.tier, DecisionTier::Draft);
        assert_eq!(draft_view.folded, None);
        assert!(
            draft_view.space_id.is_some(),
            "draft keeps conversation context"
        );
        let owner_view = decisions
            .iter()
            .find(|v| v.id.as_str() == owner_dec.id)
            .expect("owner approval in hot views");
        assert_eq!(owner_view.tier, DecisionTier::Decide);
        assert_eq!(
            decisions
                .iter()
                .filter(|v| v.tier == DecisionTier::Decide)
                .count(),
            1,
            "non-owner draft causes 0 inbox interruptions; owner approval causes 1 prompt"
        );

        let mut rx = f.bus.subscribe();
        let mut seed = 0x5EED_CAFE_u64;
        for seq in 0..1_000_u64 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let source = DecisionSource::Revision {
                revision: format!("seq:{seq}"),
            };
            let new = NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "action".into(),
                subject_id: format!("sub_{seq}"),
                question: format!("Step {seq}?"),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(source),
                ..Default::default()
            };
            // Natural-key dedupe: second open returns the same row without creating a duplicate.
            let d = f
                .store
                .write(|tx| {
                    let (a, o1) = DecisionRows(tx).open_in(&new)?;
                    let (b, o2) = DecisionRows(tx).open_in(&new)?;
                    assert!(o1 && !o2 && a.id == b.id);
                    Ok::<_, StoreError>(a)
                })
                .unwrap();

            match (seed >> 61) % 5 {
                0 => {
                    // Resolve + stale CAS conflict + undo (reopen) + re-resolve at rev 2.
                    f.store
                        .write(|tx| {
                            let mut rows = DecisionRows(tx);
                            let mut stale = d.clone();
                            stale.revision = d.revision + 7;
                            assert!(!rows.resolve_choice(
                                &stale,
                                "owner",
                                "yes",
                                "",
                                &json!({"choice": "yes"}),
                                Some(&DecisionUndo::Domain),
                            )?);
                            assert!(rows.resolve_choice(
                                &d,
                                "owner",
                                "yes",
                                "",
                                &json!({"choice": "yes"}),
                                Some(&DecisionUndo::Domain),
                            )?);
                            // Duplicate CAS write at rev 1 fails; original receipt stays intact.
                            assert!(!rows.resolve_choice(
                                &d,
                                "owner",
                                "no",
                                "",
                                &json!({"choice": "no"}),
                                None,
                            )?);
                            let resolved = rows.get(&d.id)?.unwrap();
                            rows.reopen(&resolved, &Value::Null)?;
                            let reopened = rows.get(&d.id)?.unwrap();
                            assert_eq!(reopened.revision, 2);
                            assert!(choice_receipt(tx, &d.id, 1)?.is_some());
                            assert!(DecisionRows(tx).resolve_choice(
                                &reopened,
                                "owner",
                                "no",
                                "",
                                &json!({"choice": "no"}),
                                None,
                            )?);
                            Ok::<_, StoreError>(())
                        })
                        .unwrap();
                }
                1 => {
                    // Expire is terminal: subsequent resolve/reopen are rejected.
                    f.store
                        .write(|tx| {
                            let mut rows = DecisionRows(tx);
                            assert!(rows.apply(
                                &d,
                                DecisionChange::Expire,
                                &json!({"expired": true}),
                                None,
                            )?);
                            let exp = rows.get(&d.id)?.unwrap();
                            assert_eq!(exp.status, DecisionStatus::Expired);
                            assert!(!rows.apply(
                                &exp,
                                DecisionChange::Reopen,
                                &Value::Null,
                                None
                            )?);
                            assert!(!rows.resolve_choice(
                                &exp,
                                "owner",
                                "yes",
                                "",
                                &json!({}),
                                None,
                            )?);
                            Ok::<_, StoreError>(())
                        })
                        .unwrap();
                    let view = f
                        .store
                        .read(|r| r.decision_views().of(ws, std::slice::from_ref(&d.id)))
                        .unwrap();
                    assert_eq!(view[0].tier, DecisionTier::Folded);
                    assert_eq!(view[0].folded, Some(FoldReason::Expired));
                }
                2 => {
                    // Supersede is terminal.
                    f.store
                        .write(|tx| {
                            let mut rows = DecisionRows(tx);
                            assert!(rows.apply(
                                &d,
                                DecisionChange::Supersede,
                                &json!({"superseded": true}),
                                None,
                            )?);
                            let sup = rows.get(&d.id)?.unwrap();
                            assert_eq!(sup.status, DecisionStatus::Superseded);
                            assert!(!rows.apply(
                                &sup,
                                DecisionChange::Reopen,
                                &Value::Null,
                                None
                            )?);
                            Ok::<_, StoreError>(())
                        })
                        .unwrap();
                }
                3 => {
                    // Dismiss + reopen + resolve.
                    f.store
                        .write(|tx| {
                            let mut rows = DecisionRows(tx);
                            assert!(rows.apply(
                                &d,
                                DecisionChange::Dismiss,
                                &json!({"dismissed": true}),
                                Some(&DecisionUndo::Domain),
                            )?);
                            let dis = rows.get(&d.id)?.unwrap();
                            rows.reopen(&dis, &Value::Null)?;
                            let reopened = rows.get(&d.id)?.unwrap();
                            assert!(rows.resolve_choice(
                                &reopened,
                                "owner",
                                "yes",
                                "",
                                &json!({"choice": "yes"}),
                                None,
                            )?);
                            Ok::<_, StoreError>(())
                        })
                        .unwrap();
                }
                _ => {
                    // Draft revise advances revision and invalidates stale rev 1 CAS.
                    f.store
                        .write(|tx| {
                            let mut rows = DecisionRows(tx);
                            assert!(rows.revise_draft(&d.id, 1, "revised")?);
                            assert!(!rows.resolve_choice(
                                &d,
                                "owner",
                                "yes",
                                "",
                                &json!({"choice": "yes"}),
                                None,
                            )?);
                            let latest = rows.get(&d.id)?.unwrap();
                            assert_eq!(latest.revision, 2);
                            assert!(rows.resolve_choice(
                                &latest,
                                "owner",
                                "yes",
                                "",
                                &json!({"choice": "yes"}),
                                None,
                            )?);
                            Ok::<_, StoreError>(())
                        })
                        .unwrap();
                }
            }

            // Periodically verify hot projection snapshot == fresh DB reload (and always on last).
            if seq % 50 == 49 || seq == 999 {
                let snapshot = f.p.snapshot(ws).await.unwrap();
                let projected = snapshot.views.decision.unwrap();
                let reloaded = f.store.read(|r| r.decision_views().open()).unwrap();
                assert_eq!(
                    projected, reloaded,
                    "decision projection mismatch at seq {seq}"
                );
            }
        }
        // Drain bus patches and confirm zero reconciliation drift against a fresh reload.
        let _ = patches(&mut rx);
        let snapshot = f.p.snapshot(ws).await.unwrap();
        let projected = snapshot.views.decision.unwrap();
        let reloaded = f.store.read(|r| r.decision_views().open()).unwrap();
        assert_eq!(projected, reloaded);
        assert_eq!(f.p.reconcile().await, Some(0));
    }
}
