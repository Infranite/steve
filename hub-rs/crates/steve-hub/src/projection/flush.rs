//! The projection's task: takes events in, marks views dirty, flushes them per workspace and
//! answers snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use steve_store::{Store, Views};
use steve_types::{BusEvent, Envelope, Op, PatchOp, View, ViewKind};
use tokio::sync::{mpsc, oneshot};

use super::kinds::{self, meters};
use super::target::{Dirty, target};
use super::{Ask, CATCH_UP, RECONCILE_EVERY, Snap, Tap, WINDOW, publish};
use crate::bus::Bus;

/// One view the projection keeps, with the hash and size of its encoding.
pub(super) struct Entry {
    pub(super) view: View,
    pub(super) hash: u64,
    bytes: usize,
}

pub(super) type Key = (ViewKind, String);

/// One workspace's views.
#[derive(Default)]
pub(super) struct Ws {
    pub(super) hot: HashMap<Key, Entry>,
    pub(super) dirty: BTreeSet<Key>,
    /// conversations (spaces) whose decisions are dirty, and whether all of them are
    chats: BTreeSet<String>,
    all_chats: bool,
    /// when the oldest change not flushed yet came in
    first_dirty: Option<Instant>,
    /// when it flushes next
    due: Option<Instant>,
    last_flush: Option<Instant>,
    /// the revision of its last change
    changed: u64,
}

struct Waiting {
    ws: String,
    upto: u64,
    reply: oneshot::Sender<Snap>,
    until: Instant,
}

pub(super) struct Run {
    pub(super) store: Store,
    pub(super) bus: Bus,
    seen: Arc<AtomicU64>,
    pub(super) tap: Tap,
    lost: Arc<AtomicU64>,
    lost_seen: u64,
    pub(super) ws: HashMap<String, Ws>,
    waiting: Vec<Waiting>,
}

pub(super) fn encode(v: &View) -> (u64, usize) {
    let bytes = serde_json::to_vec(v).expect("a view encodes");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    (h.finish(), bytes.len())
}

impl Run {
    pub(super) fn new(
        store: Store,
        bus: Bus,
        seen: Arc<AtomicU64>,
        tap: Tap,
        lost: Arc<AtomicU64>,
    ) -> Self {
        Self {
            store,
            bus,
            seen,
            tap,
            lost,
            lost_seen: 0,
            ws: HashMap::new(),
            waiting: Vec::new(),
        }
    }

    pub(super) fn seen(&self) -> u64 {
        self.seen.load(Ordering::Acquire)
    }

    pub(super) fn saw(&self, rev: u64) {
        self.seen.fetch_max(rev, Ordering::AcqRel);
    }

    pub(super) async fn run(mut self, mut asks: mpsc::Receiver<Ask>) {
        self.load().await;
        let mut reconcile = tokio::time::interval_at(
            tokio::time::Instant::now() + RECONCILE_EVERY,
            RECONCILE_EVERY,
        );
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let next = self
                .ws
                .values()
                .filter_map(|w| w.due)
                .chain(self.waiting.iter().map(|w| w.until))
                .min()
                .unwrap_or_else(|| Instant::now() + RECONCILE_EVERY);
            tokio::select! {
                e = self.tap.recv() => match e {
                    Some(e) => self.take(&e),
                    None => return,
                },
                a = asks.recv() => match a {
                    Some(a) => self.ask(a).await,
                    None => return,
                },
                _ = tokio::time::sleep_until(next.into()) => {}
                _ = reconcile.tick() => {
                    self.reconcile().await;
                }
            }
            let lost = self.lost.load(Ordering::Relaxed);
            if lost != self.lost_seen {
                self.lost_seen = lost;
                self.rebuild("tap_lost").await;
            }
            let now = Instant::now();
            let due: Vec<String> = self
                .ws
                .iter()
                .filter(|(_, w)| w.due.is_some_and(|d| d <= now))
                .map(|(k, _)| k.clone())
                .collect();
            for ws in due {
                self.flush(&ws).await;
            }
            self.answer_waiting().await;
        }
    }

    /// An event taken in: the view it names is dirty.
    pub(super) fn take(&mut self, e: &Envelope<BusEvent>) {
        self.saw(e.rev.0);
        if let BusEvent::Domain(d) = &e.event {
            match target(d) {
                Dirty::One(ws, kind, id) => self.mark(ws, kind, id),
                Dirty::Chat(ws, space) => {
                    if let Some(w) = self.touch(ws) {
                        w.chats.insert(space.to_owned());
                    }
                }
                Dirty::Chats(ws) => {
                    if let Some(w) = self.touch(ws) {
                        w.all_chats = true;
                    }
                }
                Dirty::None => {}
            }
        }
    }

    /// Workspace `ws`, scheduled to flush: the first change after a quiet window goes at once,
    /// the rest wait for its end.
    fn touch(&mut self, ws: &str) -> Option<&mut Ws> {
        if ws.is_empty() {
            return None;
        }
        let now = Instant::now();
        let w = self.ws.entry(ws.to_owned()).or_default();
        w.first_dirty.get_or_insert(now);
        if w.due.is_none() {
            w.due = Some(w.last_flush.map_or(now, |l| (l + WINDOW).max(now)));
        }
        Some(w)
    }

    pub(super) fn mark(&mut self, ws: &str, kind: ViewKind, id: &str) {
        if let Some(w) = self.touch(ws) {
            w.dirty.insert((kind, id.to_owned()));
        }
    }

    async fn ask(&mut self, a: Ask) {
        match a {
            Ask::Snapshot { ws, upto, reply } => {
                if self.seen() >= upto {
                    self.serve(&ws, reply).await;
                } else {
                    self.waiting.push(Waiting {
                        ws,
                        upto,
                        reply,
                        until: Instant::now() + CATCH_UP,
                    });
                }
            }
            Ask::Reconcile { reply } => {
                let _ = reply.send(self.reconcile().await);
            }
        }
    }

    async fn answer_waiting(&mut self) {
        if self.waiting.is_empty() {
            return;
        }
        let (now, seen) = (Instant::now(), self.seen());
        for w in std::mem::take(&mut self.waiting) {
            if w.upto <= seen {
                self.serve(&w.ws, w.reply).await;
            } else if w.until > now {
                self.waiting.push(w);
            }
            // otherwise it gave up: the asker answers busy
        }
    }

    /// A snapshot of `ws` after flushing what is dirty in it.
    async fn serve(&mut self, ws: &str, reply: oneshot::Sender<Snap>) {
        self.flush(ws).await;
        let rev = self.seen();
        let cursor = self.bus.cursor().to_string();
        let (changed, mut decisions) = match self.ws.get(ws) {
            Some(w) => (
                w.changed,
                w.hot
                    .values()
                    .map(|e| match &e.view {
                        View::Decision(d) => d.clone(),
                    })
                    .collect::<Vec<_>>(),
            ),
            None => (0, Vec::new()),
        };
        decisions.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
        let _ = reply.send(Snap {
            rev,
            changed,
            cursor,
            decisions,
        });
    }

    /// Reads the dirty views of `ws` again and sends what changed.
    pub(super) async fn flush(&mut self, ws: &str) {
        let Some(w) = self.ws.get_mut(ws) else {
            return;
        };
        w.due = None;
        if w.dirty.is_empty() && w.chats.is_empty() && !w.all_chats {
            return;
        }
        let mut dirty = std::mem::take(&mut w.dirty);
        let (chats, all_chats) = (
            std::mem::take(&mut w.chats),
            std::mem::take(&mut w.all_chats),
        );
        let taken = (dirty.clone(), chats.clone(), all_chats);
        let r0 = self.seen();
        let started = Instant::now();
        let (store, name) = (self.store.clone(), ws.to_owned());
        let read = tokio::task::spawn_blocking(move || {
            // the decisions of the conversations that changed
            let about = store.read(|r| {
                let mut ids = Vec::new();
                if all_chats {
                    ids.extend(r.decision_views().open_about_chats(&name)?);
                } else {
                    for space in &chats {
                        ids.extend(r.decision_views().open_about_chat(&name, space)?);
                    }
                }
                Ok(ids)
            })?;
            dirty.extend(about.into_iter().map(|id| (ViewKind::Decision, id)));
            let mut by_kind: BTreeMap<ViewKind, Vec<String>> = BTreeMap::new();
            for (k, id) in &dirty {
                by_kind.entry(*k).or_default().push(id.clone());
            }
            let views = by_kind
                .iter()
                .map(|(k, ids)| Ok((*k, kinds::read_by_id(&store, *k, &name, ids)?)))
                .collect::<steve_store::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>((dirty, by_kind, views))
        })
        .await;
        let w = self.ws.get_mut(ws).expect("taken above");
        let (by_kind, read) = match read {
            Ok(Ok((_, by_kind, read))) => (by_kind, read),
            failed => {
                let why = match failed {
                    Ok(Err(e)) => e.to_string(),
                    Err(e) => e.to_string(),
                    Ok(Ok(_)) => unreachable!(),
                };
                tracing::warn!(error = %why, "projection: reading views failed; retrying");
                w.dirty.extend(taken.0);
                w.chats.extend(taken.1);
                w.all_chats |= taken.2;
                w.due = Some(Instant::now() + std::time::Duration::from_secs(1));
                return;
            }
        };
        w.last_flush = Some(Instant::now());
        let mut ops = Vec::new();
        for (kind, views) in read {
            let mut found: HashMap<String, View> = views
                .into_iter()
                .map(|v| (kinds::id(&v).to_owned(), v))
                .collect();
            let ids = &by_kind[&kind];
            let m = meters(kind);
            for id in ids {
                if let Some((op, size)) = apply(w, kind, id, found.remove(id.as_str())) {
                    match op.op {
                        Op::Upsert => m.upserts.increment(1),
                        Op::Remove => m.removes.increment(1),
                    }
                    ops.push((op, size));
                }
            }
            m.flush_seconds.record(started.elapsed().as_secs_f64());
            m.flush_entities.record(ids.len() as f64);
        }
        let first = w.first_dirty.take();
        if !ops.is_empty() {
            w.changed = r0;
            publish(&self.bus, ws, r0, ops);
            if let Some(first) = first {
                crate::metrics::once!(histogram "steve_projection_delay_seconds")
                    .record(first.elapsed().as_secs_f64());
            }
        }
        self.gauges();
    }

    pub(super) fn gauges(&self) {
        for k in ViewKind::ALL {
            let (mut n, mut bytes) = (0usize, 0usize);
            for w in self.ws.values() {
                for ((kind, _), e) in &w.hot {
                    if *kind == k {
                        n += 1;
                        bytes += e.bytes;
                    }
                }
            }
            let m = meters(k);
            m.entities.set(n as f64);
            m.bytes.set(bytes as f64);
        }
    }
}

/// `w` with the view `id` of `kind` as just read (`None`: it no longer exists), and the change to
/// send, if any: a hot view is sent when it changed; one that is not hot is always sent (a client
/// may hold a copy from a page), and dropped here.
pub(super) fn apply(
    w: &mut Ws,
    kind: ViewKind,
    id: &str,
    v: Option<View>,
) -> Option<(PatchOp, usize)> {
    let key = (kind, id.to_owned());
    let Some(v) = v else {
        w.hot.remove(&key);
        return Some((
            PatchOp {
                kind,
                op: Op::Remove,
                id: id.to_owned(),
                view: None,
            },
            64,
        ));
    };
    let (hash, bytes) = encode(&v);
    if kinds::is_hot(&v) {
        if w.hot.get(&key).is_some_and(|e| e.hash == hash) {
            return None;
        }
        w.hot.insert(
            key,
            Entry {
                view: v.clone(),
                hash,
                bytes,
            },
        );
    } else {
        w.hot.remove(&key);
    }
    Some((
        PatchOp {
            kind,
            op: Op::Upsert,
            id: id.to_owned(),
            view: Some(v),
        },
        bytes + 48,
    ))
}

impl Ws {
    pub(super) fn insert(&mut self, v: View) {
        let (hash, bytes) = encode(&v);
        let key = (v.kind(), kinds::id(&v).to_owned());
        self.hot.insert(
            key,
            Entry {
                view: v,
                hash,
                bytes,
            },
        );
    }

    pub(super) fn set_changed(&mut self, rev: u64) {
        self.changed = rev;
    }
}
