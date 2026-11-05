//! `GET /api/stream`: one SSE stream for everything the UI shows; clients refetch what changed.
//! Frames come from the bus already serialized and scoped (see [`crate::bus::Frame`]); this only filters by
//! workspace and adds Hono's ping (`event: ping\ndata: \n\n` every 25 s).
//!
//! Resuming is opt-in. A client that sends a cursor (`?after=<epoch>:<n>`, from `/api/state`'s
//! `x-steve-cursor` or the last `id:` it saw; or the `Last-Event-ID` header, the query wins) gets:
//! - first, every kept frame after the cursor that its workspace may see, each preceded by
//!   `id: <epoch>:<n>`, then one `{"type":"stream.gap"}` (no id): transcript chunks and drafts from the gap are
//!   not replayed, the per-session events API has them;
//! - or, when the cursor can't be continued (another epoch: the Hub restarted; ahead of the log; older than
//!   the retained window), one `id: <epoch>:<head>` + `{"type":"stream.reset"}`: refetch everything;
//! - then live frames, with an `id:` line when they are kept in the log; frames already replayed are skipped.
//!
//! Falling behind the bus replays from the last frame written (or resets) instead of the `workspace` frame.
//! Pings never carry an id. Without a cursor the bytes are exactly as before: no `id:` lines, no new types.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use futures::stream::{self, Stream};
use serde::Deserialize;
use tokio::sync::broadcast::{
    Receiver,
    error::{RecvError, TryRecvError},
};
use tokio::time::{Instant, Interval, interval_at};

use super::{AppState, Ctx};
use crate::bus::{Bus, Cursor, Frame, HubEvent, Replay};

pub const PING_EVERY: Duration = Duration::from_secs(25);

/// How many bytes one written chunk gathers before it goes out.
const CHUNK: usize = 256 * 1024;

pub fn ping_frame() -> Bytes {
    Bytes::from_static(b"event: ping\ndata: \n\n")
}

/// Where a resumable subscriber is in the change log.
struct Resume {
    bus: Bus,
    /// every kept frame up to this id has been written or was not visible
    last: u64,
}

struct Sub {
    ws: String,
    rx: Receiver<Arc<Frame>>,
    ping: Interval,
    /// `None`: a legacy subscriber (no cursor), served exactly as before
    resume: Option<Resume>,
    /// chunks to write before anything else (a replay)
    pending: VecDeque<Bytes>,
}

fn with_id(epoch: &str, id: u64, bytes: &[u8]) -> Vec<u8> {
    let mut b = format!("id: {epoch}:{id}\n").into_bytes();
    b.extend_from_slice(bytes);
    b
}

fn gap_frame() -> &'static [u8] {
    b"data: {\"type\":\"stream.gap\"}\n\n"
}

fn reset_frame(epoch: &str, head: u64) -> Vec<u8> {
    with_id(epoch, head, b"data: {\"type\":\"stream.reset\"}\n\n")
}

impl Sub {
    /// A subscriber that continues after `after` (as sent on the wire). Subscribes before reading the log, so
    /// a frame sent in between arrives on the broadcast too and is dropped there as already replayed.
    fn resumed(bus: &Bus, ws: String, after: &str) -> Sub {
        Sub::resumed_on(bus, ws, bus.subscribe(), after)
    }

    /// [`Sub::resumed`] on a receiver subscribed before this call.
    fn resumed_on(bus: &Bus, ws: String, rx: Receiver<Arc<Frame>>, after: &str) -> Sub {
        let replay = bus.replay_from(after);
        let mut sub = Sub {
            ws,
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: Some(Resume {
                bus: bus.clone(),
                last: 0,
            }),
            pending: VecDeque::new(),
        };
        sub.write_replay(replay);
        sub
    }

    /// Queues what a replay found and moves the cursor to its head.
    fn write_replay(&mut self, replay: Replay) {
        let Some(r) = self.resume.as_mut() else {
            return;
        };
        let epoch = r.bus.epoch().to_string();
        match replay {
            Replay::Frames { frames, head } => {
                let mut buf: Vec<u8> = Vec::new();
                for f in frames {
                    let Some(id) = f.id else { continue };
                    if id <= r.last || !f.visible_to(&self.ws) {
                        continue;
                    }
                    buf.extend_from_slice(&with_id(&epoch, id, &f.bytes));
                    if buf.len() >= CHUNK {
                        self.pending
                            .push_back(Bytes::from(std::mem::take(&mut buf)));
                    }
                }
                buf.extend_from_slice(gap_frame());
                self.pending.push_back(Bytes::from(buf));
                r.last = r.last.max(head);
            }
            Replay::Reset { head } => {
                self.pending
                    .push_back(Bytes::from(reset_frame(&epoch, head)));
                r.last = head;
            }
        }
    }

    /// Fell behind the bus: a resumable subscriber continues from the log, a legacy one is told to refetch.
    fn lagged(&mut self) -> Option<Bytes> {
        match &self.resume {
            Some(r) => {
                let replay = r.bus.replay(&Cursor {
                    epoch: r.bus.epoch().to_string(),
                    n: r.last,
                });
                self.write_replay(replay);
                None
            }
            None => Some(self.resync_frame()),
        }
    }

    /// The bytes `f` goes out as on this stream, or `None` when it is not for this subscriber or was
    /// already written.
    fn take(&mut self, f: &Frame) -> Option<Bytes> {
        let visible = f.visible_to(&self.ws);
        match (self.resume.as_mut(), f.id) {
            (Some(r), Some(id)) => {
                if id <= r.last {
                    return None;
                }
                r.last = id;
                visible.then(|| Bytes::from(with_id(r.bus.epoch(), id, &f.bytes)))
            }
            _ => visible.then(|| f.bytes.clone()),
        }
    }

    /// The frame plus whatever else is already waiting, as one chunk: under load this turns thousands of
    /// small writes per second into a few larger ones. Frame boundaries (`\n\n`) are unchanged.
    fn coalesce(&mut self, first: Bytes) -> Bytes {
        let mut buf: Option<Vec<u8>> = None;
        // Bound work as well as bytes: a busy unrelated workspace must not starve ping/resync.
        for _ in 0..1024 {
            match self.rx.try_recv() {
                Ok(f) => {
                    let Some(more) = self.take(&f) else { continue };
                    let b = buf.get_or_insert_with(|| first.to_vec());
                    b.extend_from_slice(&more);
                    if b.len() >= CHUNK {
                        break;
                    }
                }
                Err(TryRecvError::Lagged(n)) => {
                    tracing::warn!("sse subscriber lagged by {n} events while coalescing");
                    if let Some(more) = self.lagged() {
                        buf.get_or_insert_with(|| first.to_vec())
                            .extend_from_slice(&more);
                    }
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        match buf {
            Some(b) => Bytes::from(b),
            None => first,
        }
    }

    fn resync_frame(&self) -> Bytes {
        let json = serde_json::to_string(&HubEvent::Workspace {
            id: self.ws.clone(),
        })
        .expect("workspace event serializes");
        Bytes::from(format!("data: {json}\n\n"))
    }

    async fn next(mut self) -> Option<(Result<Bytes, Infallible>, Sub)> {
        loop {
            if let Some(b) = self.pending.pop_front() {
                return Some((Ok(b), self));
            }
            tokio::select! {
                _ = self.ping.tick() => return Some((Ok(ping_frame()), self)),
                got = self.rx.recv() => match got {
                    Ok(f) => {
                        if let Some(b) = self.take(&f) {
                            return Some((Ok(self.coalesce(b)), self));
                        }
                    }
                    // fell behind the bus: replay from the log, or ask the web to refetch everything
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("sse subscriber lagged by {n} events");
                        if let Some(b) = self.lagged() {
                            return Some((Ok(b), self));
                        }
                    }
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    }
}

/// The SSE byte stream for one subscriber in workspace `ws`; `after` is the cursor to resume from (`None`:
/// the legacy stream).
pub fn events(
    st: &AppState,
    ws: String,
    after: Option<String>,
) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + use<> {
    let sub = match after {
        Some(a) => Sub::resumed(&st.bus, ws, &a),
        None => Sub {
            rx: st.bus.subscribe(),
            ws,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: VecDeque::new(),
        },
    };
    stream::unfold(sub, Sub::next)
}

#[derive(Debug, Default, Deserialize)]
pub struct StreamQuery {
    after: Option<String>,
}

/// The cursor a request resumes from: `?after=` wins over `Last-Event-ID`; empty means none.
fn cursor_of(q: &StreamQuery, h: &HeaderMap) -> Option<String> {
    let header = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    q.after
        .clone()
        .filter(|a| !a.trim().is_empty())
        .or(header.filter(|a| !a.trim().is_empty()))
}

pub async fn stream(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let after = cursor_of(&q, &headers);
    let mut res = Response::new(Body::from_stream(events(&st, c.ws, after)));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    h.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("keep-alive"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Scope;

    #[tokio::test]
    async fn coalescing_lag_emits_workspace_resync_and_keeps_surviving_frames() {
        let (tx, rx) = tokio::sync::broadcast::channel(2);
        let frame = |scope| {
            Arc::new(Frame {
                bytes: Bytes::from_static(b"data: {}\n\n"),
                scope,
                id: None,
            })
        };
        let mut sub = Sub {
            ws: "private-a".into(),
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: VecDeque::new(),
        };
        tx.send(frame(Scope::All)).unwrap();
        let first = sub.rx.recv().await.unwrap().bytes.clone();
        for _ in 0..4 {
            tx.send(frame(Scope::Ws("private-a".into()))).unwrap();
        }
        let out = sub.coalesce(first);
        let out = std::str::from_utf8(&out).unwrap();
        assert!(out.contains("\"type\":\"workspace\""));
        assert!(out.contains("private-a"));
        assert!(sub.rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn coalescing_does_not_include_other_workspaces() {
        let (tx, rx) = tokio::sync::broadcast::channel(4);
        let first = Arc::new(Frame {
            bytes: Bytes::from_static(b"data: own\n\n"),
            scope: Scope::Ws("a".into()),
            id: None,
        });
        let mut sub = Sub {
            ws: "a".into(),
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: VecDeque::new(),
        };
        tx.send(Arc::new(Frame {
            bytes: Bytes::from_static(b"data: secret\n\n"),
            scope: Scope::Ws("b".into()),
            id: None,
        }))
        .unwrap();
        assert_eq!(
            sub.coalesce(first.bytes.clone()),
            Bytes::from_static(b"data: own\n\n")
        );
    }

    mod resume {
        use super::super::*;
        use serde_json::json;

        async fn sent(rx: &mut Receiver<Arc<Frame>>, n: usize) -> Vec<Arc<Frame>> {
            let mut out = vec![];
            for _ in 0..n {
                out.push(
                    tokio::time::timeout(Duration::from_secs(10), rx.recv())
                        .await
                        .expect("in time")
                        .expect("frame"),
                );
            }
            out
        }

        /// What `next` would write, without waiting for pings: the queued replay, then everything waiting.
        fn drain(sub: &mut Sub) -> String {
            let mut out: Vec<u8> = vec![];
            loop {
                if let Some(b) = sub.pending.pop_front() {
                    out.extend_from_slice(&b);
                    continue;
                }
                match sub.rx.try_recv() {
                    Ok(f) => {
                        if let Some(b) = sub.take(&f) {
                            out.extend_from_slice(&sub.coalesce(b));
                        }
                    }
                    Err(TryRecvError::Lagged(_)) => {
                        if let Some(b) = sub.lagged() {
                            out.extend_from_slice(&b);
                        }
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8(out).expect("utf8")
        }

        fn item(bus: &Bus, id: &str) {
            bus.publish(HubEvent::Item { id: id.into() });
        }

        fn in_ws(bus: &Bus, id: &str, ws: &str) {
            bus.publish_value(json!({"type": "project", "id": id, "ws": ws}));
        }

        #[tokio::test]
        async fn legacy_bytes_are_unchanged() {
            let bus = Bus::new();
            let mut sub = Sub {
                ws: "a".into(),
                rx: bus.subscribe(),
                ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
                resume: None,
                pending: VecDeque::new(),
            };
            let mut watch = bus.subscribe();
            item(&bus, "ST-1");
            in_ws(&bus, "p", "b");
            sent(&mut watch, 2).await;
            assert_eq!(
                drain(&mut sub),
                "data: {\"type\":\"item\",\"id\":\"ST-1\"}\n\n"
            );
        }

        #[tokio::test]
        async fn resume_replays_only_later_visible_frames_then_a_gap() {
            let bus = Bus::new();
            let mut watch = bus.subscribe();
            item(&bus, "ST-1");
            sent(&mut watch, 1).await;
            let c = bus.cursor();
            item(&bus, "ST-2");
            in_ws(&bus, "secret", "b");
            in_ws(&bus, "mine", "a");
            sent(&mut watch, 3).await;
            let e = bus.epoch().to_string();
            let mut sub = Sub::resumed(&bus, "a".into(), &c.to_string());
            let out = drain(&mut sub);
            assert_eq!(
                out,
                format!(
                    "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-2\"}}\n\n\
                     id: {e}:{}\ndata: {{\"type\":\"project\",\"id\":\"mine\",\"ws\":\"a\"}}\n\n\
                     data: {{\"type\":\"stream.gap\"}}\n\n",
                    c.n + 1,
                    c.n + 3
                )
            );
            // live frames carry their id; frames without one go out as they are
            item(&bus, "ST-3");
            bus.publish_value(json!({"type": "steve.draft", "ws": "a", "mid": "m", "text": "x"}));
            sent(&mut watch, 2).await;
            assert_eq!(
                drain(&mut sub),
                format!(
                    "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-3\"}}\n\n\
                     data: {{\"type\":\"steve.draft\",\"ws\":\"a\",\"mid\":\"m\",\"text\":\"x\"}}\n\n",
                    c.n + 4
                )
            );
        }

        #[tokio::test]
        async fn a_frame_sent_between_subscribe_and_replay_arrives_once() {
            let bus = Bus::new();
            let c = bus.cursor();
            let rx = bus.subscribe();
            let mut watch = bus.subscribe();
            item(&bus, "ST-1");
            sent(&mut watch, 1).await;
            // ST-1 is now both waiting on `rx` and in the log the replay reads
            let mut sub = Sub::resumed_on(&bus, "a".into(), rx, &c.to_string());
            let out = drain(&mut sub);
            assert_eq!(out.matches("ST-1").count(), 1, "{out}");
            assert!(
                out.ends_with("data: {\"type\":\"stream.gap\"}\n\n"),
                "{out}"
            );
        }

        #[tokio::test]
        async fn a_stale_epoch_or_evicted_cursor_resets_once_at_the_head() {
            let bus = Bus::with_window(2, usize::MAX);
            let mut watch = bus.subscribe();
            for i in 0..4 {
                item(&bus, &format!("ST-{i}"));
            }
            sent(&mut watch, 4).await;
            let e = bus.epoch().to_string();
            let head = bus.cursor().n;
            for after in [
                format!("ep_other:{head}"),
                format!("{e}:1"),
                format!("{e}:{}", head + 5),
                "garbage".to_string(),
            ] {
                let mut sub = Sub::resumed(&bus, "a".into(), &after);
                assert_eq!(
                    drain(&mut sub),
                    format!("id: {e}:{head}\ndata: {{\"type\":\"stream.reset\"}}\n\n"),
                    "{after}"
                );
                if after == "garbage" {
                    // later frames continue after the head, no second reset
                    item(&bus, "ST-9");
                    sent(&mut watch, 1).await;
                    let out = drain(&mut sub);
                    assert_eq!(
                        out,
                        format!(
                            "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-9\"}}\n\n",
                            head + 1
                        )
                    );
                }
            }
        }

        #[tokio::test]
        async fn lag_replays_from_the_log_instead_of_a_workspace_frame() {
            let bus = Bus::new();
            let mut watch = bus.subscribe();
            let c = bus.cursor();
            for i in 0..5 {
                item(&bus, &format!("ST-{i}"));
            }
            let frames = sent(&mut watch, 5).await;
            // a receiver that holds two frames: feeding it five makes it lag
            let (tx, rx) = tokio::sync::broadcast::channel(2);
            for f in frames {
                tx.send(f).unwrap();
            }
            let mut sub = Sub {
                ws: "a".into(),
                rx,
                ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
                resume: Some(Resume {
                    bus: bus.clone(),
                    last: c.n,
                }),
                pending: VecDeque::new(),
            };
            let out = drain(&mut sub);
            assert!(!out.contains("\"type\":\"workspace\""), "{out}");
            for i in 0..5 {
                assert_eq!(out.matches(&format!("\"ST-{i}\"")).count(), 1, "{out}");
            }
            assert_eq!(out.matches("id: ").count(), 5, "{out}");
            assert!(
                out.ends_with("data: {\"type\":\"stream.gap\"}\n\n"),
                "{out}"
            );
        }

        #[tokio::test]
        async fn pings_carry_no_id_and_do_not_move_the_cursor() {
            let bus = Bus::new();
            let mut sub = Sub::resumed(&bus, "a".into(), &bus.cursor().to_string());
            sub.ping = interval_at(Instant::now(), PING_EVERY);
            assert_eq!(drain(&mut sub), "data: {\"type\":\"stream.gap\"}\n\n");
            let last = sub.resume.as_ref().map(|r| r.last);
            let (b, sub) = sub.next().await.expect("ping");
            let b = b.unwrap();
            assert_eq!(b, ping_frame());
            assert!(!std::str::from_utf8(&b).unwrap().contains("id:"));
            assert_eq!(sub.resume.as_ref().map(|r| r.last), last);
        }

        #[test]
        fn the_query_cursor_wins_over_last_event_id() {
            let mut h = HeaderMap::new();
            h.insert("last-event-id", "ep_x:3".parse().unwrap());
            let q = |a: Option<&str>| StreamQuery {
                after: a.map(str::to_string),
            };
            assert_eq!(cursor_of(&q(Some("ep_y:4")), &h).as_deref(), Some("ep_y:4"));
            assert_eq!(cursor_of(&q(None), &h).as_deref(), Some("ep_x:3"));
            assert_eq!(cursor_of(&q(Some("")), &h).as_deref(), Some("ep_x:3"));
            assert_eq!(cursor_of(&q(None), &HeaderMap::new()), None);
        }
    }
}
