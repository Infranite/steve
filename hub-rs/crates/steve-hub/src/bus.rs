//! In-process domain events (bus.ts). Store writes and the orchestrator publish here; every SSE stream subscribes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

/// One bus event; serialized it is exactly the SSE `data:` payload of the TS Hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    #[serde(rename = "item")]
    Item { id: String },
    /// `item_id`: `None` = key absent, `Some(None)` = `null`.
    #[serde(rename = "decision")]
    Decision {
        id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        item_id: Option<Option<String>>,
    },
    #[serde(rename = "session")]
    Session {
        id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        item_id: Option<Option<String>>,
    },
    /// A session event; `data` is the stored object as written (not re-read).
    #[serde(rename = "event")]
    Event {
        session_id: String,
        seq: i64,
        kind: String,
        data: serde_json::Value,
    },
    #[serde(rename = "message")]
    Message { id: String },
    /// `phase`: `route` → `answer` | `config` | `item` | `supplement` while one message is handled.
    #[serde(rename = "steve.thinking")]
    Thinking {
        on: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ws: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// The bot id may be `"*"` (all memories of a workspace cleared).
    #[serde(rename = "bot")]
    Bot { id: String },
    #[serde(rename = "project")]
    Project { id: String },
    #[serde(rename = "tag")]
    Tag { id: String },
    #[serde(rename = "workspace")]
    Workspace { id: String },
    #[serde(rename = "settings")]
    Settings { id: String },
    #[serde(rename = "device")]
    Device { id: String },
}

/// `Option<Option<T>>` that tells "absent" from `null`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// Who may see a frame: every subscriber, the subscribers of one workspace, or nobody (a session event of a
/// session that doesn't exist; TS compared `undefined` with the workspace id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Ws(String),
    Nobody,
}

/// One event as it goes out on every SSE stream: serialized (and slimmed) once, with its audience.
#[derive(Debug)]
pub struct Frame {
    /// `data: <json>\n\n`
    pub bytes: Bytes,
    pub scope: Scope,
}

impl Frame {
    pub fn visible_to(&self, ws: &str) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Ws(w) => w == ws,
            Scope::Nobody => false,
        }
    }
}

/// Events that change nothing `/api/state` reads, so they leave [`Bus::generation`] alone: transcript chunks
/// (stored only in `events`; a turn's end publishes `session`), a chat answer being written and its research
/// tool calls (the stored reply publishes `message`), and statistics (counted by their own generation).
const STATELESS: &[&str] = &["event", "steve.draft", "steve.tool", "stats"];

type Resolver = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
type Taps = Arc<Mutex<Vec<(Vec<String>, tokio::sync::mpsc::UnboundedSender<Value>)>>>;

struct Inner {
    /// bumped by every publish that can change `/api/state`, synchronously: anything computed from the database
    /// before a write is stale after it
    generation: std::sync::atomic::AtomicU64,
    /// bumped by every `stats` event before it is coalesced: cached statistics older than this are stale
    stats_generation: std::sync::atomic::AtomicU64,
    tx: std::sync::mpsc::Sender<Value>,
    out: broadcast::Sender<Arc<Frame>>,
    taps: Taps,
    resolver: Arc<OnceLock<Resolver>>,
}

/// Fan-out of bus events. Publishing is synchronous and never blocks (it can run under the store's write
/// lock); one dispatcher thread, in publish order, resolves a session event's workspace (cached; sessions
/// never move), slims its data (`evOut`), serializes once and broadcasts the bytes. A subscriber that falls
/// more than [`Bus::CAPACITY`] frames behind is told it lagged (the SSE layer asks the web to refetch).
#[derive(Clone)]
pub struct Bus(Arc<Inner>);

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub const CAPACITY: usize = 4096;

    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Value>();
        let out = broadcast::channel(Self::CAPACITY).0;
        let resolver: Arc<OnceLock<Resolver>> = Arc::new(OnceLock::new());
        let taps: Taps = Arc::default();
        let (o, r, t) = (out.clone(), resolver.clone(), taps.clone());
        let spawned = std::thread::Builder::new()
            .name("steve-bus".into())
            .spawn(move || dispatch(rx, o, r, t));
        if let Err(e) = spawned {
            tracing::error!("bus dispatcher: {e}");
        }
        Bus(Arc::new(Inner {
            generation: std::sync::atomic::AtomicU64::new(0),
            stats_generation: std::sync::atomic::AtomicU64::new(0),
            tx,
            out,
            taps,
            resolver,
        }))
    }

    /// Every published event whose `type` is one of `types`, as JSON, in publish order (module jobs that
    /// react to store changes, e.g. steve-knowledge's `sync::on_event`).
    pub fn tap(&self, types: &[&str]) -> tokio::sync::mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut t = self.0.taps.lock().unwrap_or_else(|e| e.into_inner());
        t.push((types.iter().map(|s| s.to_string()).collect(), tx));
        rx
    }

    /// How to find a session's workspace (set once storage is open).
    pub fn set_session_workspace(
        &self,
        f: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) {
        let _ = self.0.resolver.set(Box::new(f));
    }

    /// Delivers `e` to every current subscriber that may see it, in publish order.
    pub fn publish(&self, e: HubEvent) {
        if let Ok(v) = serde_json::to_value(e) {
            self.publish_value(v);
        }
    }

    /// Any event shaped `{type, …}` (steve-store's and steve-core's events serialize to the same JSON).
    pub fn publish_value(&self, v: Value) {
        let ty = v.get("type").and_then(Value::as_str);
        if !ty.is_some_and(|t| STATELESS.contains(&t)) {
            self.0
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if ty == Some("stats") {
            self.0
                .stats_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = self.0.tx.send(v);
    }

    /// How many events were published so far.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many `stats` events were published so far (before coalescing).
    pub fn stats_generation(&self) -> u64 {
        self.0
            .stats_generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.0.out.subscribe()
    }
}

fn dispatch(
    rx: std::sync::mpsc::Receiver<Value>,
    out: broadcast::Sender<Arc<Frame>>,
    resolver: Arc<OnceLock<Resolver>>,
    taps: Taps,
) {
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut stats = StatsThrottle::default();
    loop {
        let next = match stats.deadline() {
            None => rx
                .recv()
                .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())),
        };
        let mut v = match next {
            Ok(v) => v,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                for v in stats.due(std::time::Instant::now()) {
                    send(&out, &v, Scope::Ws(ws_of(&v)));
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        {
            // a closed receiver drops its tap
            let mut t = taps.lock().unwrap_or_else(|e| e.into_inner());
            t.retain(|(types, tx)| !types.contains(&ty) || tx.send(v.clone()).is_ok());
        }
        if ty == "stats" {
            // at most one per workspace every 5 s: the first at once, the rest merged into one at the window's end
            if let Some(v) = stats.offer(v, std::time::Instant::now()) {
                send(&out, &v, Scope::Ws(ws_of(&v)));
            }
            continue;
        }
        let scope = if ty == "event" {
            let sid = v
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ws = match seen.get(&sid) {
                Some(w) => Some(w.clone()),
                None => {
                    let w = resolver.get().and_then(|f| f(&sid));
                    if let Some(w) = &w {
                        if seen.len() > 100_000 {
                            seen.clear();
                        }
                        seen.insert(sid, w.clone());
                    }
                    w
                }
            };
            if let Some(d) = v.get_mut("data") {
                *d = crate::api::slim::ev_out(d.take());
            }
            ws.map(Scope::Ws).unwrap_or(Scope::Nobody)
        } else {
            // Explicit workspace metadata is authoritative, including newly registered event
            // types. Known private types with missing/malformed metadata fail closed.
            match v.get("ws") {
                Some(Value::String(w)) if !w.is_empty() => Scope::Ws(w.clone()),
                Some(_) => Scope::Nobody,
                None if steve_modules::sse::requires_workspace(&ty) => Scope::Nobody,
                None => Scope::All,
            }
        };
        send(&out, &v, scope);
    }
}

fn send(out: &broadcast::Sender<Arc<Frame>>, v: &Value, scope: Scope) {
    let json = serde_json::to_string(v).unwrap_or_default();
    let _ = out.send(Arc::new(Frame {
        bytes: Bytes::from(format!("data: {json}\n\n")),
        scope,
    }));
}

fn ws_of(v: &Value) -> String {
    v.get("ws")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `{type:"stats"}` events, coalesced per workspace (stats.md §4.3): a quiet workspace's event goes out at
/// once; within [`StatsThrottle::WINDOW`] of the last one sent, the latest is held and sent when the window
/// ends. So a browser gets at most one every 5 seconds and never misses the last change.
#[derive(Debug, Default)]
struct StatsThrottle {
    last: HashMap<String, std::time::Instant>,
    held: HashMap<String, Value>,
}

impl StatsThrottle {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

    /// The event to send now, if any.
    fn offer(&mut self, v: Value, now: std::time::Instant) -> Option<Value> {
        let ws = ws_of(&v);
        match self.last.get(&ws) {
            Some(t) if now.duration_since(*t) < Self::WINDOW => {
                self.held.insert(ws, v);
                None
            }
            _ => {
                if self.last.len() > 10_000 {
                    self.last
                        .retain(|_, t| now.duration_since(*t) < Self::WINDOW);
                }
                self.last.insert(ws, now);
                Some(v)
            }
        }
    }

    /// When the earliest held event is due.
    fn deadline(&self) -> Option<std::time::Instant> {
        self.held
            .keys()
            .filter_map(|ws| self.last.get(ws))
            .map(|t| *t + Self::WINDOW)
            .min()
    }

    /// The held events whose window has ended.
    fn due(&mut self, now: std::time::Instant) -> Vec<Value> {
        let ready: Vec<String> = self
            .held
            .keys()
            .filter(|ws| {
                self.last
                    .get(*ws)
                    .is_none_or(|t| now.duration_since(*t) >= Self::WINDOW)
            })
            .cloned()
            .collect();
        ready
            .into_iter()
            .filter_map(|ws| {
                let v = self.held.remove(&ws)?;
                self.last.insert(ws, now);
                Some(v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn js(e: &HubEvent) -> String {
        serde_json::to_string(e).expect("json")
    }

    #[test]
    fn shapes_match_ts() {
        assert_eq!(
            js(&HubEvent::Item { id: "ST-1".into() }),
            r#"{"type":"item","id":"ST-1"}"#
        );
        assert_eq!(
            js(&HubEvent::Session {
                id: "s".into(),
                item_id: None
            }),
            r#"{"type":"session","id":"s"}"#
        );
        assert_eq!(
            js(&HubEvent::Session {
                id: "s".into(),
                item_id: Some(None)
            }),
            r#"{"type":"session","id":"s","item_id":null}"#
        );
        assert_eq!(
            js(&HubEvent::Decision {
                id: "d".into(),
                item_id: Some(Some("ST-2".into()))
            }),
            r#"{"type":"decision","id":"d","item_id":"ST-2"}"#
        );
        assert_eq!(
            js(&HubEvent::Event {
                session_id: "s".into(),
                seq: 3,
                kind: "user".into(),
                data: json!({"text":"hi","via":"me"})
            }),
            r#"{"type":"event","session_id":"s","seq":3,"kind":"user","data":{"text":"hi","via":"me"}}"#
        );
        assert_eq!(
            js(&HubEvent::Thinking {
                on: true,
                ws: Some("default".into()),
                phase: None
            }),
            r#"{"type":"steve.thinking","on":true,"ws":"default"}"#
        );
        assert_eq!(
            js(&HubEvent::Bot { id: "*".into() }),
            r#"{"type":"bot","id":"*"}"#
        );
        let back: HubEvent =
            serde_json::from_str(r#"{"type":"session","id":"s","item_id":null}"#).expect("parse");
        assert_eq!(
            back,
            HubEvent::Session {
                id: "s".into(),
                item_id: Some(None)
            }
        );
    }

    #[test]
    fn stats_are_coalesced_per_workspace() {
        let mut t = StatsThrottle::default();
        let at = std::time::Instant::now();
        let ev = |ws: &str, n: i64| json!({"type": "stats", "ws": ws, "n": n});
        let s = std::time::Duration::from_secs(1);
        assert_eq!(t.offer(ev("a", 1), at), Some(ev("a", 1)));
        assert_eq!(t.offer(ev("a", 2), at + s), None);
        assert_eq!(t.offer(ev("a", 3), at + 2 * s), None);
        // another workspace has its own window
        assert_eq!(t.offer(ev("b", 1), at + 2 * s), Some(ev("b", 1)));
        assert_eq!(t.deadline(), Some(at + 5 * s));
        assert!(t.due(at + 4 * s).is_empty());
        // the window's end sends the latest of the held ones, once
        assert_eq!(t.due(at + 5 * s), vec![ev("a", 3)]);
        assert_eq!(t.deadline(), None);
        assert_eq!(t.offer(ev("a", 4), at + 6 * s), None);
        assert_eq!(t.due(at + 10 * s), vec![ev("a", 4)]);
    }

    #[test]
    fn only_state_changes_bump_the_generation() {
        let bus = Bus::new();
        let g = bus.generation();
        bus.publish_value(
            json!({"type": "event", "session_id": "s", "seq": 1, "kind": "k", "data": {}}),
        );
        bus.publish_value(
            json!({"type": "steve.draft", "ws": "w1", "mid": "m", "thought": "", "text": "x"}),
        );
        bus.publish_value(json!({"type": "steve.tool", "ws": "w1"}));
        bus.publish_value(json!({"type": "stats", "ws": "w1"}));
        assert_eq!(bus.generation(), g);
        bus.publish_value(json!({"type": "session", "id": "s"}));
        bus.publish_value(json!({"type": "message", "id": "m"}));
        assert_eq!(bus.generation(), g + 2);
    }

    #[tokio::test]
    async fn stats_frames_stay_in_their_workspace() {
        let bus = Bus::new();
        let mut a = bus.subscribe();
        let g = bus.stats_generation();
        bus.publish_value(json!({"type": "stats", "ws": "w1", "day": "2026-11-01"}));
        bus.publish_value(json!({"type": "stats", "ws": "w1"}));
        assert_eq!(bus.stats_generation(), g + 2);
        let f = a.recv().await.expect("first");
        assert_eq!(f.scope, Scope::Ws("w1".into()));
        assert!(f.visible_to("w1") && !f.visible_to("w2"));
        // the second is held for the rest of the window
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), a.recv())
                .await
                .is_err()
        );
        let f = tokio::time::timeout(std::time::Duration::from_secs(4), a.recv())
            .await
            .expect("trailing")
            .expect("frame");
        assert_eq!(
            &f.bytes[..],
            &b"data: {\"type\":\"stats\",\"ws\":\"w1\"}\n\n"[..]
        );
    }

    #[tokio::test]
    async fn fan_out_in_order_with_scopes() {
        let bus = Bus::new();
        bus.set_session_workspace(|sid| (sid == "s1").then(|| "w1".to_string()));
        let mut a = bus.subscribe();
        bus.publish(HubEvent::Item { id: "1".into() });
        bus.publish(HubEvent::Event {
            session_id: "s1".into(),
            seq: 1,
            kind: "x".into(),
            data: json!({"_meta": 1, "a": 2}),
        });
        bus.publish(HubEvent::Event {
            session_id: "nope".into(),
            seq: 2,
            kind: "x".into(),
            data: json!({}),
        });
        bus.publish(HubEvent::Thinking {
            on: true,
            ws: Some("w2".into()),
            phase: None,
        });
        let f = a.recv().await.expect("1");
        assert_eq!(
            (&f.bytes[..], &f.scope),
            (
                &b"data: {\"type\":\"item\",\"id\":\"1\"}\n\n"[..],
                &Scope::All
            )
        );
        let f = a.recv().await.expect("2");
        assert_eq!(&f.bytes[..], &b"data: {\"type\":\"event\",\"session_id\":\"s1\",\"seq\":1,\"kind\":\"x\",\"data\":{\"a\":2}}\n\n"[..]);
        assert!(f.visible_to("w1") && !f.visible_to("w2"));
        assert_eq!(a.recv().await.expect("3").scope, Scope::Nobody);
        assert!(a.recv().await.expect("4").visible_to("w2"));
        /* a chat answer being written stays in its workspace */
        bus.publish_value(
            json!({"type": "steve.draft", "ws": "w1", "mid": "m", "thought": "", "text": "一"}),
        );
        let f = a.recv().await.expect("5");
        assert_eq!(f.scope, Scope::Ws("w1".into()));
    }
}

#[cfg(test)]
mod scope_regression {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn private_and_new_events_never_escape_their_workspace() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        for ty in [
            "steve.tool",
            "steve.draft",
            "steve.thinking",
            "plugin.future",
        ] {
            bus.publish_value(json!({"type":ty,"ws":"private-a","brief":"private"}));
            let f = rx.recv().await.unwrap();
            assert!(f.visible_to("private-a"));
            assert!(!f.visible_to("private-b"), "{ty}");
        }
        for ev in [
            json!({"type":"steve.tool"}),
            json!({"type":"plugin.future"}),
            json!({"type":"mail"}),
            json!({"type":"plugin.future","ws":null}),
            json!({"type":"steve.tool","ws":""}),
        ] {
            bus.publish_value(ev);
            assert_eq!(rx.recv().await.unwrap().scope, Scope::Nobody);
        }
    }
}
