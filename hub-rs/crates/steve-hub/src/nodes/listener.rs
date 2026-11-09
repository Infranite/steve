//! `GET /node/ws`: a Node's link. The upgrade is size-limited, the signed handshake decides who it
//! is, and from then on the link carries the ports both ways: the Hub's calls go through the
//! Node's [`steve_node_wire::RemoteNode`], the Node's through [`super::guard`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt, future};
use rusqlite::OptionalExtension;
use serde_json::Value;
use steve_node_api::*;
use steve_node_wire::envelope::{Frame, FrameSink, FrameStream, MAX_FRAME};
use steve_node_wire::handshake::{self, Admission, Admitted, Hello};
use steve_node_wire::{Conn, ConnOptions, Handler, ServeHub, close};
use steve_store::StoreError;

use super::guard::{GuardedFiles, GuardedSink, GuardedTools, LinkCtx};
use super::{NodeService, Peer, now};
use crate::wire::node_runtime::{catch_up_streams, read_snapshot};

pub async fn upgrade(
    State(svc): State<Arc<NodeService>>,
    Peer(peer): Peer,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !svc.ingress.allows(peer, &headers) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    // links not yet through the handshake are bounded (M5): many half-sent hellos cost memory
    // and the store's blocking pool, and a Node that is turned away dials again shortly
    let Ok(unproven) = svc.handshakes.clone().try_acquire_owned() else {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            let (sink, stream) = split(socket);
            serve_after(svc, sink, stream, Some(unproven)).await;
        })
}

/// axum's socket as a [`Frame`] link.
pub fn split(ws: WebSocket) -> (FrameSink, FrameStream) {
    let (tx, rx) = ws.split();
    let tx = tx.sink_map_err(std::io::Error::other).with(|f: Frame| {
        future::ready(Ok::<_, std::io::Error>(match f {
            Frame::Text(t) => Message::Text(t.into()),
            Frame::Close(code, reason) => Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            })),
        }))
    });
    let rx = rx.filter_map(|m| {
        future::ready(match m {
            Ok(Message::Text(t)) => Some(Ok(Frame::Text(t.to_string()))),
            Ok(Message::Close(c)) => Some(Ok(match c {
                Some(c) => Frame::Close(c.code, c.reason.to_string()),
                None => Frame::Close(1005, String::new()),
            })),
            Ok(Message::Binary(_)) => Some(Err(std::io::Error::other("binary frame"))),
            Ok(_) => None,
            Err(e) => Some(Err(std::io::Error::other(e))),
        })
    });
    (Box::pin(tx), Box::pin(rx))
}

/// The store's side of a handshake, for one connection.
struct Admit {
    svc: Arc<NodeService>,
    /// The incarnation before this link (a change: the Node restarted).
    previous: Mutex<Option<Option<i64>>>,
}

#[async_trait]
impl Admission for Admit {
    async fn key(&self, node: &NodeId) -> Result<Option<[u8; 32]>, String> {
        let (store, node) = (self.svc.store.clone(), node.to_string());
        let row = tokio::task::spawn_blocking(move || {
            store.reader(|c| steve_store::nodes::get(c, &node))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        Ok(row
            .filter(|r| matches!(r.state.as_str(), "active" | "paused"))
            .and_then(|r| r.pubkey.try_into().ok()))
    }

    async fn admit(&self, hello: &Hello) -> Result<Option<Admitted>, String> {
        let (store, node, inc) = (
            self.svc.store.clone(),
            hello.node_id.to_string(),
            hello.incarnation as i64,
        );
        let link = tokio::task::spawn_blocking(move || {
            store.transaction::<_, StoreError>(|tx| {
                let l = steve_store::nodes::begin_link(tx, &node, inc, now())?;
                if let Some(l) = &l {
                    steve_store::nodes::audit(
                        tx,
                        Some(&node),
                        None,
                        "link",
                        Some(&format!("generation {} incarnation {inc}", l.generation)),
                        now(),
                    )?;
                }
                Ok(l)
            })
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let Some(link) = link else { return Ok(None) };
        *self.previous.lock().unwrap() = Some(link.previous_incarnation);
        Ok(Some(Admitted {
            generation: link.generation as u64,
            paused: link.paused,
        }))
    }
}

/// Requests of the Node: refused `stale` once this link is not the current one, then the guarded
/// ports.
struct LinkHandler {
    ctx: LinkCtx,
    ports: ServeHub,
}

#[async_trait]
impl Handler for LinkHandler {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        self.ctx.live()?;
        self.ports.handle(method, p).await
    }
}

/// Runs one link from the handshake to its close.
pub async fn serve(svc: Arc<NodeService>, sink: FrameSink, stream: FrameStream) {
    serve_after(svc, sink, stream, None).await
}

/// `unproven`: this link's place among those still in the handshake, given back once it is done.
async fn serve_after(
    svc: Arc<NodeService>,
    mut sink: FrameSink,
    mut stream: FrameStream,
    unproven: Option<tokio::sync::OwnedSemaphorePermit>,
) {
    let admit = Admit {
        svc: svc.clone(),
        previous: Mutex::new(None),
    };
    let hub = svc.identity.get().map(|k| k.as_ref());
    // the pins of this Hub's certificate (STEVE_NODE_TLS_PIN, the next one's too while changing it)
    let pins: Vec<String> = svc
        .pin
        .iter()
        .flat_map(|p| p.split(','))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let accepted =
        handshake::accept_as(&mut sink, &mut stream, &svc.hub_id, &admit, hub, &pins).await;
    drop(unproven);
    let (hello, admitted) = match accepted {
        Ok(x) => x,
        Err(e) => {
            tracing::info!("node link refused: {e}");
            return;
        }
    };
    let node = hello.node_id.clone();
    let generation = admitted.generation;
    let ctx = LinkCtx {
        node: node.clone(),
        generation,
        store: svc.store.clone(),
        registry: svc.registry.clone(),
        fleet: svc.fleet.clone(),
    };
    let handler = Arc::new(LinkHandler {
        ports: ServeHub {
            sink: Arc::new(GuardedSink(ctx.clone())),
            tools: Arc::new(GuardedTools(ctx.clone())),
            files: Some(Arc::new(GuardedFiles(ctx.clone()))),
            // only what was given to this Node's own commands (vault uses, N0e-E7)
            secrets: svc.secrets.get().map(|s| s.for_node(node.clone())),
        },
        ctx,
    });
    let conn = Conn::spawn(
        sink,
        stream,
        generation,
        handler,
        ConnOptions {
            heartbeat: Duration::from_secs(10),
            dead_after: Duration::from_secs(30),
            side: "h",
            // what the Node said it speaks: requests newer than it are refused, not sent
            peer: hello.proto,
        },
    );
    let previous = admit.previous.lock().unwrap().take().flatten();
    let restarted = previous.is_some_and(|p| p != hello.incarnation as i64);
    let remote = svc.fleet.remote(&node);
    let Some(paused) = install_link(&svc, &node, conn.clone(), restarted).await else {
        return;
    };
    svc.fleet.linked(&node, hello.proto);
    if paused != admitted.paused {
        // paused or resumed since the welcome went out
        conn.notify("paused", serde_json::json!({ "paused": paused }));
    }
    tracing::info!(node = %node, generation, restarted, paused, "node linked");
    if remote.is_reconciling() {
        match svc.fleet.runtime_for(node.as_str()) {
            Some(rt) => tokio::spawn(reconcile_again(rt, remote.clone(), conn.clone())),
            None => tokio::spawn(catch_up(svc.store.clone(), remote.clone(), conn.clone())),
        };
    }
    let (code, reason) = conn.closed().await;
    tracing::info!(node = %node, generation, code, reason, "node link closed");
    svc.registry.remove(node.as_str(), generation);
    remote.detach(generation);
}

/// Installs an admitted link, under [`NodeService::links`]: the store must still say this
/// generation, active or paused (a revoke, a pause or a newer link may have landed since the
/// handshake), and whether it is paused is read there, not from the welcome. `Some(paused)`: the
/// link is the Node's now. `None`: it was closed (`denied` if the Node may no longer connect,
/// `fenced` if a newer link was admitted).
#[doc(hidden)]
pub async fn install_link(
    svc: &NodeService,
    node: &NodeId,
    conn: Arc<Conn>,
    restarted: bool,
) -> Option<bool> {
    let generation = conn.generation();
    let _g = svc.links.lock().await;
    let (store, id) = (svc.store.clone(), node.to_string());
    let state = tokio::task::spawn_blocking(move || {
        store.reader(|c| steve_store::nodes::link_state(c, &id))
    })
    .await;
    let paused = match state {
        Ok(Ok(Some((g, st)))) if g as u64 == generation && st == "active" => false,
        Ok(Ok(Some((g, st)))) if g as u64 == generation && st == "paused" => true,
        Ok(Ok(Some((g, st))))
            if g as u64 > generation && matches!(st.as_str(), "active" | "paused") =>
        {
            conn.close(
                Some(("fenced", serde_json::json!({"gen": g}))),
                close::FENCED,
                "fenced",
            );
            return None;
        }
        Ok(Ok(_)) => {
            conn.close(None, close::DENIED, "revoked");
            return None;
        }
        Ok(Err(e)) => {
            tracing::warn!(node = %node, "node link: {e}");
            conn.close(None, close::INTERNAL, "internal");
            return None;
        }
        Err(e) => {
            tracing::warn!(node = %node, "node link: {e}");
            conn.close(None, close::INTERNAL, "internal");
            return None;
        }
    };
    if !svc
        .registry
        .install(node.as_str(), generation, conn.clone())
    {
        return None;
    }
    // two parties with one key take the link from each other (M4): the Node takes no new work
    // until its owner looks, rotates the key or revokes it
    let paused = if !paused && svc.registry.taken_over_often(node.as_str()) {
        suspect_takeover(svc, node).await || paused
    } else {
        paused
    };
    if !svc.fleet.remote(node).attach(conn.clone(), restarted) {
        // cannot happen while both are installed under the lock; never leave the two disagreeing
        svc.registry.remove(node.as_str(), generation);
        conn.close(None, close::FENCED, "fenced");
        return None;
    }
    // a coordinator runs for every Node with sessions bound to it (started at boot or on bind)
    if let Some(rt) = svc.fleet.runtime_for(node.as_str()) {
        rt.set_paused(paused);
    }
    Some(paused)
}

/// Pauses `node` and tells its owner; `true` once it is paused.
async fn suspect_takeover(svc: &NodeService, node: &NodeId) -> bool {
    let (store, id) = (svc.store.clone(), node.to_string());
    let r = tokio::task::spawn_blocking(move || {
        let name = store.transaction::<_, StoreError>(|tx| {
            let name: Option<String> = tx
                .query_row(
                    "UPDATE nodes SET state='paused' WHERE node_id=? AND state='active' RETURNING name",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            if name.is_some() {
                steve_store::nodes::audit(
                    tx,
                    Some(&id),
                    None,
                    "takeover_suspected",
                    Some(&format!(
                        "its link was taken over {} times within {}s",
                        super::registry::TAKEOVERS,
                        super::registry::TAKEOVER_WINDOW.as_secs()
                    )),
                    now(),
                )?;
            }
            Ok(name)
        })?;
        if let Some(name) = &name {
            let db = steve_core::repo::Db::new(store);
            let text = steve_i18n::t!("hub.node.takeover_suspected", name = name);
            if let Err(e) = steve_core::notify::notify(&db, steve_store::WS, "system", "fail", &text, None)
            {
                tracing::error!("takeover notice: {e}");
            }
        }
        Ok::<_, StoreError>(name.is_some())
    })
    .await;
    match r {
        Ok(Ok(paused)) => {
            tracing::warn!(node = %node, "node link taken over repeatedly: paused, its owner told");
            paused
        }
        Ok(Err(e)) => {
            tracing::error!(node = %node, "pausing a node taken over: {e}");
            false
        }
        Err(e) => {
            tracing::error!(node = %node, "pausing a node taken over: {e}");
            false
        }
    }
}

/// After a Node restart, with its coordinator running: the coordinator reconciles with the Node
/// over this link (its ledger, sessions and waiting permissions) before new work goes to it.
/// Retried with backoff while this link lives; a link that is gone or replaced clears nothing.
async fn reconcile_again(
    rt: Arc<crate::wire::node_runtime::NodeRuntime>,
    remote: Arc<steve_node_wire::RemoteNode>,
    conn: Arc<Conn>,
) {
    let mut backoff = Duration::from_millis(200);
    loop {
        if conn.is_closed() || remote.generation() != Some(conn.generation()) {
            return;
        }
        match rt.reconcile_again().await {
            Ok(_) => break,
            Err(e) => {
                tracing::warn!("node reconcile after restart: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = conn.closed() => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
    remote.set_ready(conn.generation());
}

/// The catch-up after a Node restart when no coordinator runs for it (nothing is bound to it, or
/// a fleet without the Hub's domain): read the Node's whole snapshot once over this link before new
/// work goes to it, and compare its stream heads with the Hub's inbox (the coordinator's own
/// [`catch_up_streams`]: what cannot be reconciled is faulted, what the Hub holds and the Node did
/// not hear acknowledged is acknowledged). With no coordinator to take them, events the Hub lacks
/// stay in the Node's journal until one starts and replays them. A failure is retried with backoff
/// while this link lives (a redial of the same incarnation is no longer a restart, so giving up
/// would leave the Node refusing new work for good); a link that is gone or replaced clears
/// nothing.
async fn catch_up(
    store: steve_store::Store,
    remote: Arc<steve_node_wire::RemoteNode>,
    conn: Arc<Conn>,
) {
    let mut backoff = Duration::from_millis(200);
    loop {
        if conn.is_closed() || remote.generation() != Some(conn.generation()) {
            return;
        }
        let done = match read_snapshot(&*remote, 100).await {
            Ok(s) => catch_up_streams(&store, &*remote, &s.heads, None).await,
            Err(e) => Err(e),
        };
        match done {
            Ok(_) => break,
            Err(e) => {
                tracing::warn!("node catch-up: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = conn.closed() => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
    remote.set_ready(conn.generation());
}
