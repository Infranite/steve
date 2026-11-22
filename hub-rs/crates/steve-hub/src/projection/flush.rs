//! The projection's task: takes events in, marks views dirty, flushes them per workspace and
//! answers snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use steve_store::{Store, Views};
use steve_types::{Op, PageOf, PatchOp, Rev, SnapshotViews, View, ViewKind};
use tokio::sync::{mpsc, oneshot};

use super::kinds::{self, meters};
use super::target::{Dirty, target};
use super::{Ask, CATCH_UP, RECONCILE_EVERY, Snap, Tap, WINDOW, publish};
use crate::bus::{Bus, Commit};

/// One view the projection keeps, with the hash and size of its encoding.
pub(super) struct Entry {
    pub(super) view: View,
    pub(super) hash: u64,
    bytes: usize,
}

pub(super) type Key = (ViewKind, String);

fn decision_key(id: String) -> Key {
    (ViewKind::Decision, id)
}

/// One workspace's views.
#[derive(Default)]
pub(super) struct Ws {
    pub(super) hot: HashMap<Key, Entry>,
    pub(super) dirty: BTreeSet<Key>,
    /// conversations (spaces) whose decisions are dirty, and whether all of them are
    chats: BTreeSet<String>,
    all_chats: bool,
    notices: bool,
    /// when the oldest change not flushed yet came in
    first_dirty: Option<Instant>,
    /// when it flushes next
    due: Option<Instant>,
    last_flush: Option<Instant>,
    retrying: bool,
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
    seen: u64,
    pub(super) routes: kinds::RuntimeRoutes,
    pub(super) tap: Tap,
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
    pub(super) fn new(store: Store, bus: Bus, tap: Tap, routes: kinds::RuntimeRoutes) -> Self {
        Self {
            store,
            bus,
            seen: 0,
            routes,
            tap,
            ws: HashMap::new(),
            waiting: Vec::new(),
        }
    }

    pub(super) fn seen(&self) -> u64 {
        self.seen
    }

    pub(super) fn saw(&mut self, rev: u64) {
        self.seen = self.seen.max(rev);
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
                    Ok(batch) => self.take(&batch),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.rebuild().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
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
            // A snapshot may bring forward a scheduled flush. Reading the snapshot itself
            // only copies the projection once it has caught up and its workspace is clean.
            for waiting in &self.waiting {
                if waiting.upto <= self.seen()
                    && let Some(w) = self.ws.get_mut(&waiting.ws)
                    && w.has_dirty()
                    && !w.retrying
                {
                    w.due = Some(Instant::now());
                }
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

    /// Mark all the changes in one commit before advancing the watermark.
    pub(super) fn take(&mut self, batch: &Commit) {
        if batch.rev.0 <= self.seen() {
            return;
        }
        for d in batch.events.iter().filter_map(|e| e.event.as_domain()) {
            let w = match target(d) {
                Dirty::One(ws, kind, id) => self.touch(ws).map(|w| {
                    w.dirty.insert((kind, id.to_owned()));
                    w
                }),
                Dirty::Chat(ws, space) => self.touch(ws).map(|w| {
                    w.chats.insert(space.to_owned());
                    w
                }),
                Dirty::Chats(ws) => self.touch(ws).map(|w| {
                    w.all_chats = true;
                    w
                }),
                Dirty::Notices(ws) => self.touch(ws).map(|w| {
                    w.notices = true;
                    w
                }),
                Dirty::SessionsOfNode(node) => {
                    let sessions: Vec<_> = self
                        .ws
                        .iter()
                        .flat_map(|(ws, w)| {
                            w.hot.values().filter_map(|e| match &e.view {
                                View::Session(s)
                                    if s.node_id.as_ref().is_some_and(|id| id.as_str() == node) =>
                                {
                                    Some((ws.clone(), s.id.to_string()))
                                }
                                _ => None,
                            })
                        })
                        .collect();
                    for (ws, id) in sessions {
                        self.mark(&ws, ViewKind::Session, &id);
                    }
                    None
                }
                Dirty::None => None,
            };
            if let Some(w) = w {
                w.first_dirty = Some(w.first_dirty.map_or(batch.at, |at| at.min(batch.at)));
            }
        }
        self.saw(batch.rev.0);
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
                self.waiting.push(Waiting {
                    ws,
                    upto,
                    reply,
                    until: Instant::now() + CATCH_UP,
                });
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
            if w.upto <= seen && !self.ws.get(&w.ws).is_some_and(Ws::has_dirty) {
                self.serve(&w.ws, w.reply);
            } else if w.until > now {
                self.waiting.push(w);
            }
            // otherwise it gave up: the asker answers busy
        }
    }

    /// A snapshot of `ws` after flushing what is dirty in it.
    fn serve(&self, ws: &str, reply: oneshot::Sender<Snap>) {
        let rev = self.seen();
        let cursor = self.bus.cursor().to_string();
        let (mut decisions, mut items, mut sessions) = (Vec::new(), Vec::new(), Vec::new());
        let mut messages = Vec::new();
        let changed = self.ws.get(ws).map_or(0, |w| {
            for e in w.hot.values() {
                match &e.view {
                    View::Decision(v) => decisions.push(v.clone()),
                    View::Item(v) => items.push(v.clone()),
                    View::Session(v) => sessions.push(v.clone()),
                    View::Message(v) => messages.push(v),
                }
            }
            w.changed
        });
        decisions.sort_by(|a, b| (a.created.0, &a.id).cmp(&(b.created.0, &b.id)));
        items.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        sessions.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        messages.sort_unstable_by(|a, b| (b.created, &b.id).cmp(&(a.created, &a.id)));
        let first = steve_store::views::FIRST_PAGE;
        let next = messages.get(first).map(|_| {
            let last = messages[first - 1];
            format!("{}:{}", last.created.0, last.id)
        });
        let _ = reply.send(Snap {
            rev,
            changed,
            cursor,
            views: SnapshotViews {
                decision: Some(decisions),
                item: Some(items),
                session: Some(sessions),
                message: Some(PageOf {
                    rev: Rev(rev),
                    views: messages.into_iter().take(first).cloned().collect(),
                    next,
                }),
            },
        });
    }

    /// Reads the dirty views of `ws` again and sends what changed.
    pub(super) async fn flush(&mut self, ws: &str) {
        let Some(w) = self.ws.get_mut(ws) else {
            return;
        };
        w.due = None;
        if !w.has_dirty() {
            return;
        }
        let mut dirty = std::mem::take(&mut w.dirty);
        let (chats, all_chats) = (
            std::mem::take(&mut w.chats),
            std::mem::take(&mut w.all_chats),
        );
        let notices = std::mem::take(&mut w.notices);
        let held_messages: BTreeSet<_> = w
            .hot
            .keys()
            .filter(|(k, _)| *k == ViewKind::Message)
            .map(|(_, id)| id.clone())
            .collect();
        let taken = (dirty.clone(), chats.clone(), all_chats);
        let r0 = self.seen();
        let started = Instant::now();
        let (store, name) = (self.store.clone(), ws.to_owned());
        let routes = self.routes.clone();
        let read = tokio::task::spawn_blocking(move || {
            store.read(|r| {
                let mut decisions = r.decision_views();
                if all_chats {
                    dirty.extend(
                        decisions
                            .open_about_chats(&name)?
                            .into_iter()
                            .map(decision_key),
                    );
                } else {
                    for space in chats {
                        dirty.extend(
                            decisions
                                .open_about_chat(&name, &space)?
                                .into_iter()
                                .map(decision_key),
                        );
                    }
                }
                if notices {
                    dirty.extend(
                        r.message_views()
                            .notices(&name)?
                            .into_iter()
                            .map(|id| (ViewKind::Message, id)),
                    );
                }
                let mut batches = BTreeMap::<ViewKind, Vec<String>>::new();
                for (kind, id) in dirty {
                    batches.entry(kind).or_default().push(id);
                }
                let mut reads = Vec::new();
                let tail = if let Some(ids) = batches.get_mut(&ViewKind::Message) {
                    let tail: BTreeSet<_> =
                        r.message_views().tail_ids(&name)?.into_iter().collect();
                    ids.extend(tail.difference(&held_messages).cloned());
                    ids.sort_unstable();
                    ids.dedup();
                    Some(tail)
                } else {
                    None
                };
                for (kind, ids) in batches {
                    let views = kinds::of(r, &routes, kind, &name, &ids)?;
                    reads.push((kind, ids, views));
                }
                Ok((reads, tail))
            })
        })
        .await;
        let w = self.ws.get_mut(ws).expect("taken above");
        let (reads, tail) = match read {
            Ok(Ok(result)) => result,
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
                w.notices |= notices;
                w.retrying = true;
                w.due = Some(Instant::now() + std::time::Duration::from_secs(1));
                return;
            }
        };
        w.retrying = false;
        w.last_flush = Some(Instant::now());
        let mut ops = Vec::new();
        for (kind, ids, views) in reads {
            let m = meters(kind);
            let mut found: HashMap<String, View> =
                views.into_iter().map(|v| (v.id().to_owned(), v)).collect();
            for id in &ids {
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
        if let Some(tail) = tail {
            w.hot
                .retain(|(kind, id), _| *kind != ViewKind::Message || tail.contains(id));
        }
        let first = w.first_dirty.take();
        if !ops.is_empty() {
            w.changed += 1;
            if let Some(first) = first {
                let delay = first.elapsed().as_secs_f64();
                for kind in ViewKind::ALL {
                    if ops.iter().any(|(op, _)| op.kind == kind) {
                        meters(kind).delay_seconds.record(delay);
                    }
                }
            }
            publish(&self.bus, ws, r0, ops);
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
    pub(super) fn has_dirty(&self) -> bool {
        !self.dirty.is_empty() || self.all_chats || !self.chats.is_empty() || self.notices
    }

    pub(super) fn may_change(&self, key: &Key) -> bool {
        self.dirty.contains(key)
            || (key.0 == ViewKind::Message && self.notices)
            || (key.0 == ViewKind::Decision && (self.all_chats || !self.chats.is_empty()))
    }

    pub(super) fn insert(&mut self, v: View) {
        let (hash, bytes) = encode(&v);
        let key = (v.kind(), v.id().to_owned());
        self.hot.insert(
            key,
            Entry {
                view: v,
                hash,
                bytes,
            },
        );
    }
}
