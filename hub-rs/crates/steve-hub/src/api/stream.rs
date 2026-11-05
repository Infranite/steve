//! `GET /api/stream`: one SSE stream for everything the UI shows; clients refetch what changed.
//! Frames come from the bus already serialized and scoped (see [`crate::bus::Frame`]); this only filters by
//! workspace and adds Hono's ping (`event: ping\ndata: \n\n` every 25 s).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use futures::stream::{self, Stream};
use tokio::sync::broadcast::{
    Receiver,
    error::{RecvError, TryRecvError},
};
use tokio::time::{Instant, Interval, interval_at};

use super::{AppState, Ctx};
use crate::bus::{Frame, HubEvent};

pub const PING_EVERY: Duration = Duration::from_secs(25);

pub fn ping_frame() -> Bytes {
    Bytes::from_static(b"event: ping\ndata: \n\n")
}

struct Sub {
    ws: String,
    rx: Receiver<Arc<Frame>>,
    ping: Interval,
}

impl Sub {
    /// The frame plus whatever else is already waiting, as one chunk: under load this turns thousands of
    /// small writes per second into a few larger ones. Frame boundaries (`\n\n`) are unchanged.
    fn coalesce(&mut self, first: Arc<Frame>) -> Bytes {
        let mut buf: Option<Vec<u8>> = None;
        // Bound work as well as bytes: a busy unrelated workspace must not starve ping/resync.
        for _ in 0..1024 {
            match self.rx.try_recv() {
                Ok(f) => {
                    if !f.visible_to(&self.ws) {
                        continue;
                    }
                    let b = buf.get_or_insert_with(|| first.bytes.to_vec());
                    b.extend_from_slice(&f.bytes);
                    if b.len() >= 256 * 1024 {
                        break;
                    }
                }
                Err(TryRecvError::Lagged(n)) => {
                    tracing::warn!("sse subscriber lagged by {n} events while coalescing");
                    buf.get_or_insert_with(|| first.bytes.to_vec())
                        .extend_from_slice(&self.resync_frame());
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        match buf {
            Some(b) => Bytes::from(b),
            None => first.bytes.clone(),
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
            tokio::select! {
                _ = self.ping.tick() => return Some((Ok(ping_frame()), self)),
                got = self.rx.recv() => match got {
                    Ok(f) if f.visible_to(&self.ws) => return Some((Ok(self.coalesce(f)), self)),
                    Ok(_) => {}
                    // fell behind the bus: ask the web to refetch everything
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("sse subscriber lagged by {n} events");
                        return Some((Ok(self.resync_frame()), self));
                    }
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    }
}

/// The SSE byte stream for one subscriber in workspace `ws`.
pub fn events(
    st: &AppState,
    ws: String,
) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + use<> {
    let sub = Sub {
        rx: st.bus.subscribe(),
        ws,
        ping: interval_at(Instant::now() + PING_EVERY, PING_EVERY),
    };
    stream::unfold(sub, Sub::next)
}

pub async fn stream(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> Response {
    let mut res = Response::new(Body::from_stream(events(&st, c.ws)));
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
        };
        tx.send(frame(Scope::All)).unwrap();
        let first = sub.rx.recv().await.unwrap();
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
        };
        tx.send(Arc::new(Frame {
            bytes: Bytes::from_static(b"data: secret\n\n"),
            scope: Scope::Ws("b".into()),
            id: None,
        }))
        .unwrap();
        assert_eq!(sub.coalesce(first), Bytes::from_static(b"data: own\n\n"));
    }
}
