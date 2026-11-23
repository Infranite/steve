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
                        .resolve(id, &json!({"answer": step}), "resolved", &json!({"x": 1}))
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
            .resolve(&id, &json!({"answer": "ok"}), "resolved", &Value::Null)
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
}
