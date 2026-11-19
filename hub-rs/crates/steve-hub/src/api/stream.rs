//! `GET /api/stream`: one SSE stream for everything the UI shows; clients refetch what changed.
//! Typed deliveries are encoded here after visibility filtering; the stream also adds Hono's ping
//! (`event: ping\ndata: \n\n` every 25 s).
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
//! Falling behind the bus replays from the last frame written (or resets). A subscriber without a cursor
//! receives `stream.reset` without an id and refetches everything. Its ordinary live frames still have no
//! `id:` lines. Pings never carry an id.

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
use crate::bus::{Bus, Cursor, Delivery, Replay, Viewer};
use crate::presence::Here;
use steve_types::{BusEvent, Envelope, LiveEvent, Scoped, StreamControl};

/// The single JSON encoding used by SSE and the plugin boundary. No plugin declares a need for
/// full transcript payloads: both public exits use the existing SSE slimming policy, while
/// internal typed taps keep the original opaque data. Keep the transform on the API side.
pub(crate) fn encode_event(event: &Envelope<BusEvent>) -> Bytes {
    let json = if matches!(
        &event.event,
        BusEvent::Live(LiveEvent::Work(steve_types::WorkLiveEvent::Event { .. }))
    ) {
        let mut slim = event.clone();
        if let BusEvent::Live(LiveEvent::Work(steve_types::WorkLiveEvent::Event { data, .. })) =
            &mut slim.event
        {
            let value = super::slim::ev_out(serde_json::to_value(&*data).expect("transcript JSON"));
            *data =
                steve_types::RawJson::from_str(&serde_json::to_string(&value).expect("slim JSON"))
                    .expect("validated JSON");
        }
        serde_json::to_string(&slim).expect("egress event")
    } else {
        serde_json::to_string(event).expect("egress event")
    };
    Bytes::from(json)
}

/// One SSE frame, encoded once at the stream boundary and shared across readers and replay.
#[derive(Debug)]
pub struct Frame {
    pub bytes: Bytes,
    pub scope: crate::bus::Scope,
    pub id: Option<u64>,
}

impl From<&Delivery> for Frame {
    fn from(event: &Delivery) -> Self {
        let bytes = event.encoded.get_or_init(|| {
            let json = encode_event(&event.envelope);
            let mut bytes = Vec::with_capacity(json.len() + 8);
            bytes.extend_from_slice(b"data: ");
            bytes.extend_from_slice(&json);
            bytes.extend_from_slice(b"\n\n");
            Bytes::from(bytes)
        });
        Self {
            bytes: bytes.clone(),
            scope: event.envelope.scope(),
            id: event.id,
        }
    }
}

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
    /// who the stream is for: its workspace, and who is signed in
    ws: Viewer,
    rx: Receiver<Arc<Delivery>>,
    ping: Interval,
    /// `None`: a legacy subscriber (no cursor), served exactly as before
    resume: Option<Resume>,
    /// what to write before anything else (a replay)
    pending: Pending,
    /// the signed-in client this stream keeps online; the stream ends when it is signed out
    here: Option<Here>,
}

/// Resolves once the stream's client was signed out; never for a stream without one.
async fn kicked(here: &mut Option<Here>) {
    match here {
        Some(h) => h.kicked().await,
        None => std::future::pending().await,
    }
}

/// One queued piece of a replay.
enum Part {
    /// a replayed frame, written as `id: <epoch>:<id>` + its bytes when its chunk is built
    Frame(Arc<Delivery>, u64),
    /// bytes written as they are (the trailing gap, a reset)
    Bytes(Bytes),
}

/// A replay waiting to be written. It holds the frames the log handed out (shared `Arc`s), not copies of
/// their bytes: chunks of about [`CHUNK`] bytes are built one at a time as the stream is polled, so a resume
/// from the start of a full window costs one chunk of memory per subscriber, not the whole window.
#[derive(Default)]
struct Pending {
    parts: VecDeque<Part>,
    /// the bus epoch the `id:` lines carry (constant for a bus)
    epoch: Arc<str>,
}

impl Pending {
    fn push(&mut self, p: Part) {
        self.parts.push_back(p);
    }

    /// The next chunk: queued parts gathered until it reaches [`CHUNK`] bytes; `None` when nothing waits.
    fn pop_front(&mut self) -> Option<Bytes> {
        let first = self.parts.pop_front()?;
        let mut buf: Vec<u8> = Vec::new();
        let mut part = Some(first);
        while let Some(p) = part.take() {
            match p {
                Part::Frame(f, id) => {
                    buf.extend_from_slice(format!("id: {}:{id}\n", self.epoch).as_bytes());
                    buf.extend_from_slice(&Frame::from(f.as_ref()).bytes);
                }
                Part::Bytes(b) => buf.extend_from_slice(&b),
            }
            if buf.len() >= CHUNK {
                break;
            }
            part = self.parts.pop_front();
        }
        Some(Bytes::from(buf))
    }
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
    fn resumed(bus: &Bus, ws: Viewer, after: &str) -> Sub {
        Sub::resumed_on(bus, ws, bus.subscribe(), after)
    }

    /// [`Sub::resumed`] on a receiver subscribed before this call.
    fn resumed_on(bus: &Bus, ws: Viewer, rx: Receiver<Arc<Delivery>>, after: &str) -> Sub {
        let replay = bus.replay_from(after);
        let mut sub = Sub {
            ws,
            here: None,
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: Some(Resume {
                bus: bus.clone(),
                last: 0,
            }),
            pending: Pending::default(),
        };
        sub.write_replay(replay);
        sub
    }

    /// Queues what a replay found and moves the cursor to its head.
    fn write_replay(&mut self, replay: Replay) {
        let Some(r) = self.resume.as_mut() else {
            return;
        };
        if *self.pending.epoch != *r.bus.epoch() {
            self.pending.epoch = r.bus.epoch().into();
        }
        match replay {
            Replay::Frames { frames, head } => {
                // Keep typed Arcs; encode only a chunk at a time in `Pending::pop_front`.
                for f in frames {
                    let Some(id) = f.id else { continue };
                    if id <= r.last || !f.visible(&self.ws) {
                        continue;
                    }
                    self.pending.push(Part::Frame(f, id));
                }
                self.pending
                    .push(Part::Bytes(Bytes::from_static(gap_frame())));
                r.last = r.last.max(head);
            }
            Replay::Reset { head } => {
                self.pending
                    .push(Part::Bytes(Bytes::from(reset_frame(r.bus.epoch(), head))));
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
    fn take(&mut self, f: &Delivery) -> Option<Bytes> {
        let visible = f.visible(&self.ws);
        match (self.resume.as_mut(), f.id) {
            (Some(r), Some(id)) => {
                if id <= r.last {
                    return None;
                }
                r.last = id;
                visible.then(|| Bytes::from(with_id(r.bus.epoch(), id, &Frame::from(f).bytes)))
            }
            _ => visible.then(|| Frame::from(f).bytes),
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
        let json = serde_json::to_string(&StreamControl::Reset).expect("stream control serializes");
        Bytes::from(format!("data: {json}\n\n"))
    }

    async fn next(mut self) -> Option<(Result<Bytes, Infallible>, Sub)> {
        loop {
            if let Some(b) = self.pending.pop_front() {
                return Some((Ok(b), self));
            }
            tokio::select! {
                // signed out: the stream ends, and the page's reconnect is refused
                _ = kicked(&mut self.here) => return None,
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
    ws: Viewer,
    here: Option<Here>,
    after: Option<String>,
) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + use<> {
    let mut sub = match after {
        Some(a) => Sub::resumed(&st.bus, ws, &a),
        None => Sub {
            rx: st.bus.subscribe(),
            ws,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: Pending::default(),
            here: None,
        },
    };
    sub.here = here;
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
    let owner = c.user.get("role").and_then(serde_json::Value::as_str) == Some("owner");
    // a signed-in client is online while this stream is open (G6)
    let store = st.modules.as_ref().map(|m| &m.store);
    let here = c
        .client
        .as_ref()
        .map(|cl| st.presence.enter(store, &cl.id, &c.me));
    let viewer = Viewer {
        ws: c.ws,
        user: c.me,
        owner,
    };
    let mut res = Response::new(Body::from_stream(events(&st, viewer, here, after)));
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

    fn delivery(scope: Scope) -> Arc<Delivery> {
        let event: BusEvent = match scope {
            Scope::Ws(ws) => steve_types::ConfigEvent::Project { id: "p".into(), ws }.into(),
            Scope::Hub => steve_types::DevicesEvent::Device { id: "d".into() }.into(),
            Scope::User(user) => steve_types::DevicesEvent::Node {
                id: "n".into(),
                user,
            }
            .into(),
        };
        Arc::new(Delivery {
            envelope: Arc::new(Envelope {
                event,
                rev: steve_types::Rev(1),
            }),
            id: None,
            encoded: Default::default(),
        })
    }

    #[tokio::test]
    async fn visible_connections_and_replay_share_one_lazy_encoding() {
        let events: [BusEvent; 2] = [
            steve_types::WorkEvent::Item {
                id: "item".into(),
                ws: "a".into(),
            }
            .into(),
            steve_types::WorkLiveEvent::Event {
                session_id: "s".into(),
                seq: steve_types::Seq(1),
                kind: "tool".into(),
                data: steve_types::RawJson::from_str(
                    &serde_json::json!({"rawOutput": "x".repeat(5000)}).to_string(),
                )
                .unwrap(),
                ws: "a".into(),
            }
            .into(),
        ];
        for event in events {
            let bus = Bus::new();
            let start = bus.cursor();
            let subscriber = || Sub {
                ws: "a".into(),
                here: None,
                rx: bus.subscribe(),
                ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
                resume: None,
                pending: Pending::default(),
            };
            let (first, second) = (subscriber(), subscriber());
            let mut watch = bus.subscribe();
            bus.publish(event);
            let delivery = tokio::time::timeout(Duration::from_secs(5), watch.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                delivery.encoded.get().is_none(),
                "publication must not encode"
            );
            let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(first.next(), second.next())
            })
            .await
            .unwrap();
            let first = first.unwrap().0.unwrap();
            let second = second.unwrap().0.unwrap();
            // Both real connections receive the same allocation, rather than separately encoded copies.
            assert_eq!(first.as_ptr(), second.as_ptr());
            assert_eq!(first.as_ptr(), delivery.encoded.get().unwrap().as_ptr());
            if delivery.id.is_some() {
                let Replay::Frames { frames, .. } = bus.replay(&start) else {
                    panic!("retained event must be replayable");
                };
                assert_eq!(frames.len(), 1);
                assert!(Arc::ptr_eq(&frames[0], &delivery));
                assert_eq!(
                    Frame::from(frames[0].as_ref()).bytes.as_ptr(),
                    first.as_ptr()
                );
            }
        }
    }

    #[tokio::test]
    async fn coalescing_lag_emits_reset_and_keeps_surviving_frames() {
        let (tx, rx) = tokio::sync::broadcast::channel(2);
        let mut sub = Sub {
            ws: "private-a".into(),
            here: None,
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: Pending::default(),
        };
        tx.send(delivery(Scope::Hub)).unwrap();
        let first = Frame::from(sub.rx.recv().await.unwrap().as_ref()).bytes;
        for _ in 0..4 {
            tx.send(delivery(Scope::Ws("private-a".into()))).unwrap();
        }
        let out = sub.coalesce(first);
        let out = std::str::from_utf8(&out).unwrap();
        assert!(out.contains("\"type\":\"stream.reset\""));
        assert!(sub.rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn coalescing_does_not_include_other_workspaces() {
        let (tx, rx) = tokio::sync::broadcast::channel(4);
        let first = Frame::from(delivery(Scope::Ws("a".into())).as_ref());
        let mut sub = Sub {
            ws: "a".into(),
            here: None,
            rx,
            ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
            resume: None,
            pending: Pending::default(),
        };
        tx.send(delivery(Scope::Ws("b".into()))).unwrap();
        assert_eq!(sub.coalesce(first.bytes.clone()), first.bytes);
    }

    mod resume {
        use super::super::*;

        async fn sent(rx: &mut Receiver<Arc<Delivery>>, n: usize) -> Vec<Arc<Delivery>> {
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
            bus.publish(steve_types::WorkEvent::Item {
                id: id.into(),
                ws: "a".into(),
            });
        }
        fn in_ws(bus: &Bus, id: &str, ws: &str) {
            bus.publish(steve_types::ConfigEvent::Project {
                id: id.into(),
                ws: ws.into(),
            });
        }

        #[tokio::test]
        async fn cursorless_frames_keep_scoped_payload_without_cursor_lines() {
            let bus = Bus::new();
            let mut sub = Sub {
                ws: "a".into(),
                here: None,
                rx: bus.subscribe(),
                ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
                resume: None,
                pending: Pending::default(),
            };
            let mut watch = bus.subscribe();
            item(&bus, "ST-1");
            in_ws(&bus, "p", "b");
            sent(&mut watch, 2).await;
            assert_eq!(
                drain(&mut sub),
                "data: {\"type\":\"item\",\"id\":\"ST-1\",\"ws\":\"a\",\"rev\":1}\n\n"
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
                    "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-2\",\"ws\":\"a\",\"rev\":2}}\n\n\
                     id: {e}:{}\ndata: {{\"type\":\"project\",\"id\":\"mine\",\"ws\":\"a\",\"rev\":4}}\n\n\
                     data: {{\"type\":\"stream.gap\"}}\n\n",
                    c.n + 1,
                    c.n + 3
                )
            );
            // live frames carry their id; frames without one go out as they are
            item(&bus, "ST-3");
            bus.publish(steve_types::WorkLiveEvent::SteveDraft {
                ws: "a".into(),
                mid: "m".into(),
                thought: "".into(),
                text: "x".into(),
            });
            sent(&mut watch, 2).await;
            assert_eq!(
                drain(&mut sub),
                format!(
                    "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-3\",\"ws\":\"a\",\"rev\":5}}\n\n\
                     data: {{\"type\":\"steve.draft\",\"ws\":\"a\",\"mid\":\"m\",\"thought\":\"\",\"text\":\"x\",\"rev\":5}}\n\n",
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
                            "id: {e}:{}\ndata: {{\"type\":\"item\",\"id\":\"ST-9\",\"ws\":\"a\",\"rev\":5}}\n\n",
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
                here: None,
                rx,
                ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
                resume: Some(Resume {
                    bus: bus.clone(),
                    last: c.n,
                }),
                pending: Pending::default(),
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
        async fn a_large_replay_is_queued_as_shared_frames_and_written_chunk_by_chunk() {
            let bus = Bus::with_window(64, usize::MAX);
            let mut watch = bus.subscribe();
            let c = bus.cursor();
            let big = "x".repeat(100 * 1024);
            for i in 0..40 {
                in_ws(&bus, &format!("{i}-{big}"), "a");
            }
            let frames = sent(&mut watch, 40).await;
            let mut sub = Sub::resumed(&bus, "a".into(), &c.to_string());
            // resuming copies no frame bytes: the queue holds the bus's own Arcs
            let queued: Vec<&Arc<Delivery>> = sub
                .pending
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Frame(f, _) => Some(f),
                    Part::Bytes(_) => None,
                })
                .collect();
            assert_eq!(queued.len(), 40);
            for (q, f) in queued.iter().zip(&frames) {
                assert!(Arc::ptr_eq(q, f));
            }
            // chunks are built one at a time, each about CHUNK bytes
            let e = bus.epoch().to_string();
            let mut out: Vec<u8> = vec![];
            let mut chunks = 0;
            while let Some(b) = sub.pending.pop_front() {
                assert!(b.len() < CHUNK + 110 * 1024, "{}", b.len());
                out.extend_from_slice(&b);
                chunks += 1;
            }
            assert!(chunks > 10, "{chunks}");
            let mut want: Vec<u8> = vec![];
            for f in &frames {
                want.extend_from_slice(&with_id(&e, f.id.unwrap(), &Frame::from(f.as_ref()).bytes));
            }
            want.extend_from_slice(gap_frame());
            assert!(out == want, "replayed bytes differ");
        }

        #[tokio::test]
        async fn replay_byte_limit_matches_encoded_utf8_and_escaped_frames_exactly() {
            let id = "quoted\"\n😀";
            let expected = format!(
                "data: {{\"type\":\"item\",\"id\":{},\"ws\":\"a\",\"rev\":1}}\n\n",
                serde_json::to_string(id).unwrap()
            );
            for (budget, kept) in [(expected.len() * 2, 2), (expected.len() * 2 - 1, 1)] {
                let bus = Bus::with_window(10, budget);
                let mut watch = bus.subscribe();
                let start = bus.cursor();
                item(&bus, id);
                item(&bus, id);
                let deliveries = sent(&mut watch, 2).await;
                assert_eq!(
                    Frame::from(deliveries[0].as_ref()).bytes.as_ref(),
                    expected.as_bytes()
                );
                match bus.replay(&Cursor {
                    epoch: bus.epoch().into(),
                    n: 1,
                }) {
                    Replay::Frames { frames, .. } => assert_eq!(frames.len(), 1),
                    _ => panic!("the newest frame must remain"),
                }
                assert_eq!(
                    matches!(bus.replay(&start), Replay::Frames { .. }),
                    kept == 2
                );
            }
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

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new().route("/stream", get(stream::stream))
}
