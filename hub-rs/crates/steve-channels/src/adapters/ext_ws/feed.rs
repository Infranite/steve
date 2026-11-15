//! [`ExtWsHook`]: what runs for each `ext_ws` connection that receives (§5, §9). It dials the
//! channel service, says hello, checks who is signed in, subscribes to the changes feed from the
//! saved cursor and stores each batch before it acknowledges it; it repairs gaps by reading
//! history, and redials with a jittered backoff when the socket goes.
//!
//! - Saved before acked: the cursor is written after every message of its batch went through the
//!   inbound pipeline, then the batch is acked (§5.3). A crash in between gets the batch again,
//!   and the pipeline drops what it already stored by its delivery id (`source_id`).
//! - Only recorded, never acted on: messages before the subscription's watermark, gap repair and
//!   resume repair (backfill), and the owner's own messages (`is_mine`) (§5.6). Messages written
//!   before the connection was turned on, or arriving late, are the pipeline's own watermark.
//! - A remote never affects the Hub: every wait is bounded, the run ends with its stop token, and
//!   a remote crash only costs liveness (§9.3).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::AdapterError;
use steve_modules::ApiError;
use steve_modules::accounts::connections::{Health, ProviderError};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::provider::{
    self, ExtWs, GAP_PAGE, GAP_PAGES, OpenFail, call_err, full_jitter, open_err, same_identity,
};
use super::session::{CallError, Note, OpenError, Session};
use super::wire::{self, Chat, Reason, SessionStatus, Status, WireMessage};
use crate::Channels;
use crate::channel::{InboundMessage, SpaceEvent};
use crate::lifecycle::{ConnRun, ConnState, ConnectionHook, default_fingerprint};

/// The `chan_cursors` container of the feed's cursor.
pub const CURSOR: &str = "changes";
/// Chats one gap item repairs at most.
const GAP_CHATS: usize = 200;

/// The feed of every receiving `ext_ws` connection.
pub struct ExtWsHook {
    channels: Channels,
    ext: Arc<ExtWs>,
}

impl ExtWsHook {
    /// `ext`: the same object registered as the channel side's views (its sessions are shared).
    pub fn new(channels: Channels, ext: Arc<ExtWs>) -> Self {
        Self { channels, ext }
    }
}

/// How one session ended.
enum End {
    /// The run was stopped.
    Stopped,
    /// The credential or the identity is refused: re-authorization, no redial (§9.2).
    Auth(ProviderError),
    /// No common version, a required feature missing, a refused URL: wait the maximum (§3.4).
    Fatal(ProviderError),
    /// The dial failed.
    Failed(ProviderError),
    /// It was up, and went.
    Lost(String),
}

/// The saved resume point: the cursor of the last stored batch (none: start at now) and when it
/// was saved (the start of the repair when it is lost, §5.7).
#[derive(Debug, Clone, PartialEq)]
struct Saved {
    cursor: Option<String>,
    at: i64,
}

#[async_trait]
impl ConnectionHook for ExtWsHook {
    /// The credential, the pair, and where to dial: a new URL or pin restarts the feed.
    fn fingerprint(&self, s: &ConnState) -> String {
        format!(
            "{}\n{}\n{}",
            default_fingerprint(s),
            s.config
                .get(provider::URL)
                .and_then(Value::as_str)
                .unwrap_or_default(),
            s.config
                .get(provider::PIN)
                .and_then(Value::as_str)
                .unwrap_or_default()
        )
    }

    async fn run(&self, run: ConnRun) -> Result<(), AdapterError> {
        let t = self.ext.timing;
        let conn = run.state.id.clone();
        let mut backoff = t.backoff_min;
        let mut failed = 0u32;
        loop {
            if run.stop.is_cancelled() {
                return Ok(());
            }
            let started = Instant::now();
            let end = self.once(&run).await;
            self.ext.set_live(&conn, None);
            let wait = match end {
                End::Stopped => return Ok(()),
                End::Auth(e) => {
                    tracing::warn!(conn, "ext_ws: re-authorization needed: {e}");
                    return Err(e.into());
                }
                End::Fatal(e) => {
                    tracing::warn!(conn, "ext_ws: cannot use the channel service: {e}");
                    run.cx.report(Health::Failed(e.to_string())).await;
                    t.backoff_max
                }
                End::Failed(e) => {
                    failed += 1;
                    tracing::info!(conn, failed, "ext_ws: dial failed: {e}");
                    // an error after 3 failed dials in a row (§9.2)
                    if failed >= 3 {
                        run.cx.report(Health::Failed(e.to_string())).await;
                    }
                    let w = full_jitter(backoff);
                    backoff = (backoff * 2).min(t.backoff_max);
                    w
                }
                End::Lost(why) => {
                    tracing::info!(conn, "ext_ws: session ended: {why}");
                    failed = 0;
                    if started.elapsed() >= t.stable {
                        backoff = t.backoff_min;
                    }
                    let w = full_jitter(backoff);
                    backoff = (backoff * 2).min(t.backoff_max);
                    w
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = run.stop.cancelled() => return Ok(()),
            }
        }
    }
}

impl ExtWsHook {
    /// One session: open, check, follow the feed until it ends.
    async fn once(&self, run: &ConnRun) -> End {
        let (s, notes, me) = match self.ext.open(&run.cx).await {
            Ok(x) => x,
            Err(OpenFail::Provider(e @ ProviderError::Auth(_))) => return End::Auth(e),
            Err(OpenFail::Provider(e @ ProviderError::Transient(_))) => return End::Failed(e),
            Err(OpenFail::Provider(e)) => return End::Fatal(e),
            Err(OpenFail::Open(e)) => {
                let fatal = matches!(
                    e,
                    OpenError::Version(_) | OpenError::Missing(_) | OpenError::Refused(_)
                );
                let auth = matches!(e, OpenError::Auth(_));
                let e = open_err(e);
                return if auth {
                    End::Auth(e)
                } else if fatal {
                    End::Fatal(e)
                } else {
                    End::Failed(e)
                };
            }
        };
        if let Err(e) = same_identity(run.cx.config(), &me) {
            s.close(1000, "account changed");
            return End::Auth(e);
        }
        self.ext.set_live(&run.state.id, Some(s.clone()));
        let known = provider::known_systems(&run.cx);
        let mut feed = Feed {
            hook: self,
            run,
            realm: me.realm.clone(),
            s,
            sub: None,
            last_acked: 0,
            watermark: 0,
            known,
            seen_chats: HashSet::new(),
        };
        let end = feed.follow(notes).await;
        if let End::Stopped = end {
            feed.s.close(1000, "stopping");
        }
        end
    }
}

struct Feed<'a> {
    hook: &'a ExtWsHook,
    run: &'a ConnRun,
    s: Arc<Session>,
    realm: String,
    /// The subscription batches are taken from; `None` while the remote session is not usable.
    sub: Option<String>,
    last_acked: u64,
    watermark: i64,
    known: HashSet<String>,
    /// Chats whose name was looked up this session.
    seen_chats: HashSet<String>,
}

/// A protocol violation or a lost socket: the session ends.
type Step = Result<(), End>;

impl Feed<'_> {
    fn conn(&self) -> &str {
        &self.run.state.id
    }

    fn now(&self) -> i64 {
        self.hook.channels.now()
    }

    async fn follow(&mut self, mut notes: mpsc::Receiver<Note>) -> End {
        if let Err(e) = self.on_status(self.s.status(), true).await {
            return e;
        }
        loop {
            let note = tokio::select! {
                _ = self.run.stop.cancelled() => return End::Stopped,
                _ = self.s.closed() => return End::Lost(self.s.close_reason().unwrap_or_default()),
                n = notes.recv() => n,
            };
            let Some(note) = note else {
                return End::Lost(self.s.close_reason().unwrap_or_default());
            };
            let r = match note.method.as_str() {
                "changes.batch" => self.batch(&note.params).await,
                "session.status" => match Status::parse(&note.params, None) {
                    Ok(st) => self.on_status(st, false).await,
                    Err(e) => self.violation(&e),
                },
                "push.status" => {
                    let st = note.params.get("status").and_then(Value::as_str);
                    if st != Some("ok") {
                        tracing::warn!(
                            conn = self.conn(),
                            status = st.unwrap_or("?"),
                            "ext_ws: the channel service's upstream is not ok; gaps are expected"
                        );
                    }
                    Ok(())
                }
                other => {
                    tracing::debug!(
                        conn = self.conn(),
                        method = other,
                        "ext_ws: unknown notification ignored"
                    );
                    Ok(())
                }
            };
            if let Err(end) = r {
                if self.run.stop.is_cancelled() {
                    return End::Stopped;
                }
                return end;
            }
        }
    }

    fn violation(&self, why: &str) -> Step {
        tracing::warn!(conn = self.conn(), "ext_ws: protocol violation: {why}");
        self.s.close(1008, "protocol violation");
        Err(End::Lost(format!("protocol violation: {why}")))
    }

    fn lost(&self, e: &CallError) -> End {
        match e {
            CallError::Rpc(r)
                if matches!(r.reason, Reason::Unauthorized | Reason::AccountChanged) =>
            {
                self.s.close(1000, "re-authorization needed");
                End::Auth(call_err(e.clone()))
            }
            e => {
                self.s.close(1000, "request failed");
                End::Lost(e.to_string())
            }
        }
    }

    /// A session status from hello or a notification (§9.2): usable → (re)check the identity
    /// and subscribe; expired or locked → shown, nothing taken from the feed until it is back.
    async fn on_status(&mut self, st: Status, first: bool) -> Step {
        let was = self.s.status().status;
        self.s.set_status(st.clone());
        let cx = &self.run.cx;
        match st.status {
            SessionStatus::Ok => {
                if first || was != SessionStatus::Ok || self.sub.is_none() {
                    if !first {
                        let me = provider::me(&self.s).await.map_err(|e| self.lost(&e))?;
                        if let Err(e) = same_identity(cx.config(), &me) {
                            self.s.close(1000, "account changed");
                            return Err(End::Auth(e));
                        }
                    }
                    self.subscribe().await?;
                    cx.report(Health::Ok).await;
                }
                Ok(())
            }
            SessionStatus::Expired | SessionStatus::Locked => {
                self.sub = None;
                let mut msg = if st.status == SessionStatus::Expired {
                    steve_i18n::t!("channels.ext_ws.session_expired")
                } else {
                    steve_i18n::t!("channels.ext_ws.session_locked")
                };
                if let Some(r) = st.reason.as_deref().filter(|r| !r.trim().is_empty()) {
                    msg = format!("{msg}（{r}）");
                }
                // shown on the connection; the socket stays and the remote brings it back
                cx.report(Health::Failed(msg)).await;
                Ok(())
            }
        }
    }

    fn saved(&self) -> Option<Saved> {
        let raw = self.run.queries.cursor(CURSOR).ok().flatten()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        Some(Saved {
            cursor: v
                .get("cursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string),
            at: v.get("at").and_then(Value::as_i64).unwrap_or(0),
        })
    }

    fn save(&self, s: &Saved) -> Result<(), ApiError> {
        self.run
            .queries
            .save_cursor(CURSOR, &json!({"cursor": s.cursor, "at": s.at}).to_string())
    }

    async fn subscribe_from(&self, from: Value) -> Result<Value, CallError> {
        self.s
            .call(
                "changes.subscribe",
                json!({"from": from}),
                self.s.timing().call,
            )
            .await
    }

    /// Resumes after the saved cursor, or starts at now (§5.1, §5.7). A lost resume point is a gap
    /// of every chat from when it was saved to the new watermark, read back as backfill.
    async fn subscribe(&mut self) -> Step {
        let saved = self.saved();
        let mut repair_from: Option<i64> = None;
        let r = match saved.as_ref().and_then(|s| s.cursor.clone()) {
            Some(c) => match self.subscribe_from(json!({"cursor": c})).await {
                Err(CallError::Rpc(e))
                    if matches!(e.reason, Reason::UnknownCursor | Reason::CursorTooOld) =>
                {
                    tracing::warn!(
                        conn = self.conn(),
                        "ext_ws: the saved cursor is not a resume point ({}); starting at now",
                        e.reason.name()
                    );
                    repair_from = saved.as_ref().map(|s| s.at);
                    self.subscribe_from(json!("now")).await
                }
                r => r,
            },
            None => {
                // subscribed before, never got a batch: what came in meanwhile is a gap
                repair_from = saved.as_ref().map(|s| s.at).filter(|at| *at > 0);
                self.subscribe_from(json!("now")).await
            }
        };
        let v = r.map_err(|e| self.lost(&e))?;
        let Ok(sub) = wire::id(&v, "subscription") else {
            return self.violation("a subscription without an id");
        };
        let now = self.now();
        let watermark = wire::time(&v, "watermark_ms", now).unwrap_or(now);
        let next_seq = v.get("next_seq").and_then(Value::as_u64).unwrap_or(1);
        self.sub = Some(sub);
        self.last_acked = next_seq.saturating_sub(1);
        self.watermark = watermark;
        let resumed = v.get("resumed").and_then(Value::as_bool).unwrap_or(false);
        if !resumed {
            // the next start's repair begins here until a batch gives a cursor
            if let Err(e) = self.save(&Saved {
                cursor: None,
                at: watermark,
            }) {
                tracing::warn!(conn = self.conn(), "ext_ws: resume point not saved: {e}");
            }
            if let Some(from) = repair_from.filter(|f| *f < watermark) {
                self.repair(None, from, watermark).await?;
            }
        }
        Ok(())
    }

    /// One batch (§5.3): redelivery → re-ack; else bodies fetched, every message through the
    /// pipeline, gaps repaired, the cursor saved, then the ack.
    async fn batch(&mut self, p: &Value) -> Step {
        let Some(sub) = self.sub.clone() else {
            // not subscribed (the remote session is not usable): taken again after resume
            return Ok(());
        };
        if p.get("subscription").and_then(Value::as_str) != Some(sub.as_str()) {
            return Ok(());
        }
        let Some(seq) = p.get("seq").and_then(Value::as_u64) else {
            return self.violation("a batch without a seq");
        };
        let Some(cursor) = p
            .get("cursor")
            .and_then(Value::as_str)
            .filter(|c| wire::opaque_ok(c, 1024))
            .map(str::to_string)
        else {
            return self.violation("a batch without a valid cursor");
        };
        let items = p.get("items").and_then(Value::as_array);
        let items: &[Value] = items.map_or(&[], |v| v.as_slice());
        if items.len() > self.s.limits().max_batch_items {
            return self.violation("a batch over max_batch_items");
        }
        if seq <= self.last_acked {
            return self.ack(&sub, seq).await;
        }
        let now = self.now();
        let max_text = self.s.limits().max_text_bytes;
        let mut metas: Vec<WireMessage> = Vec::new();
        let mut bodies: Vec<Option<WireMessage>> = Vec::new();
        let mut gaps: Vec<(Option<Vec<String>>, i64, i64)> = Vec::new();
        let mut chats: Vec<(String, String)> = Vec::new();
        for it in items {
            match it.get("type").and_then(Value::as_str) {
                Some("message") => match WireMessage::meta(it, now, true) {
                    Ok(m) => {
                        let inline = it
                            .get("message")
                            .filter(|_| self.s.has("changes.inline_body"))
                            .and_then(|b| WireMessage::parse(b, now, max_text).ok())
                            .filter(|b| b.chat_id == m.chat_id && b.source_id == m.source_id);
                        metas.push(m);
                        bodies.push(inline);
                    }
                    Err(e) => tracing::warn!(conn = self.conn(), "ext_ws: item dropped: {e}"),
                },
                Some("gap") => match gap(it, now) {
                    Ok(g) => gaps.push(g),
                    Err(e) => tracing::warn!(conn = self.conn(), "ext_ws: gap dropped: {e}"),
                },
                Some("participants") => {
                    self.participants(it).await;
                }
                Some("event") => {
                    self.connector_event(it).await?;
                }
                Some("chat") => {
                    if let (Ok(c), Some(ch)) = (
                        wire::id(it, "chat_id"),
                        it.get("change").and_then(Value::as_str),
                    ) {
                        chats.push((c, ch.to_string()));
                    }
                }
                // message_recalled, message_edited and unknown types: skipped, still acked
                _ => {}
            }
        }
        self.fetch_bodies(&metas, &mut bodies).await?;
        for (meta, body) in metas.into_iter().zip(bodies) {
            let mut m = match body {
                Some(mut b) => {
                    // the item is what the feed says about delivery and authorship
                    b.is_mine = meta.is_mine;
                    b.source_id = meta.source_id.clone();
                    b
                }
                None => {
                    let mut m = meta;
                    m.raw = json!({"body_unavailable": true});
                    m
                }
            };
            if m.created_ms < self.watermark {
                tracing::info!(
                    conn = self.conn(),
                    "ext_ws: a message from before the watermark, only recorded"
                );
                m.raw = json!({"before_watermark": true, "raw": m.raw});
                let msg = provider::inbound(&m, &self.realm, &self.known, "ws").backfill();
                self.deliver(msg).await?;
            } else {
                let msg = provider::inbound(&m, &self.realm, &self.known, "ws");
                self.deliver(msg).await?;
            }
        }
        for (list, from, to) in gaps {
            self.repair(list, from, to).await?;
        }
        for (chat, change) in chats {
            self.chat_change(&chat, &change).await;
        }
        // stored, then acked (§5.3 step 6)
        let saved = Saved {
            cursor: Some(cursor),
            at: self.now(),
        };
        if let Err(e) = self.save(&saved) {
            tracing::warn!(
                conn = self.conn(),
                "ext_ws: cursor not saved, not acked: {e}"
            );
            self.s.close(1011, "cursor not saved");
            return Err(End::Lost(format!("cursor not saved: {e}")));
        }
        self.ack(&sub, seq).await
    }

    async fn ack(&mut self, sub: &str, seq: u64) -> Step {
        match self
            .s
            .call(
                "changes.ack",
                json!({"subscription": sub, "seq": seq}),
                self.s.timing().call,
            )
            .await
        {
            Ok(_) => {
                self.last_acked = self.last_acked.max(seq);
                Ok(())
            }
            Err(CallError::Rpc(e)) => {
                tracing::warn!(conn = self.conn(), seq, "ext_ws: ack refused: {e}");
                Ok(())
            }
            Err(e) => Err(self.lost(&e)),
        }
    }

    /// `messages.get` for every item without an inlined body, in chunks; a body that cannot be
    /// found stays `None` (stored as metadata only).
    async fn fetch_bodies(
        &self,
        metas: &[WireMessage],
        bodies: &mut [Option<WireMessage>],
    ) -> Step {
        let missing: Vec<usize> = (0..metas.len()).filter(|i| bodies[*i].is_none()).collect();
        let now = self.now();
        let max_text = self.s.limits().max_text_bytes;
        for chunk in missing.chunks(self.s.limits().max_ids_per_call.max(1)) {
            let items: Vec<Value> = chunk
                .iter()
                .map(|i| {
                    let m = &metas[*i];
                    let mut o = json!({"chat_id": m.chat_id, "source_id": m.source_id});
                    if let Some(id) = &m.id {
                        o["id"] = id.clone().into();
                    }
                    o
                })
                .collect();
            let v = ExtWs::read(
                &self.s,
                "messages.get",
                json!({"items": items}),
                self.s.timing().call,
            )
            .await;
            let v = match v {
                Ok(v) => v,
                Err(e @ (CallError::Closed(_) | CallError::Timeout)) => return Err(self.lost(&e)),
                Err(e) => {
                    tracing::warn!(conn = self.conn(), "ext_ws: bodies not fetched: {e}");
                    continue;
                }
            };
            let got = v.get("messages").and_then(Value::as_array);
            for (k, i) in chunk.iter().enumerate() {
                let body = got
                    .and_then(|a| a.get(k))
                    .filter(|b| !b.is_null())
                    .and_then(|b| match WireMessage::parse(b, now, max_text) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            tracing::warn!(conn = self.conn(), "ext_ws: a body dropped: {e}");
                            None
                        }
                    })
                    .filter(|b| b.chat_id == metas[*i].chat_id);
                bodies[*i] = body;
            }
        }
        Ok(())
    }

    /// One message through the pipeline (its dedupe drops what is already stored). A message the
    /// pipeline refuses is dropped and logged; a store failure ends the session, unacked.
    async fn deliver(&mut self, mut msg: InboundMessage) -> Step {
        if self.s.hello().connector.is_some() {
            for a in &mut msg.attachments {
                if let Some(key) = a["key"].as_str() {
                    a["reference_id"] = crate::connector::reference_id(
                        self.conn(),
                        &msg.space_ext_id,
                        msg.source_id.as_deref().unwrap_or(&msg.external_id),
                        key,
                    )
                    .into();
                }
            }
            // Rich bodies are represented by text + checked metadata/references, not opaque blobs.
            msg.raw = json!({"connector": true});
        }
        if msg.space_name.is_none() && !self.seen_chats.contains(&msg.space_ext_id) {
            self.seen_chats.insert(msg.space_ext_id.clone());
            if self.unknown_chat(&msg.space_ext_id)
                && let Some(c) = self.chat(&msg.space_ext_id).await
            {
                msg.space_name = c.name;
                msg.space_kind = msg.space_kind.or(c.kind);
            }
        }
        if self.run.queries.known(&msg).unwrap_or(false) {
            return Ok(());
        }
        match self.run.inbound.message(msg).await {
            Ok(_) => Ok(()),
            Err(ApiError::Internal(e)) => {
                tracing::warn!(conn = self.conn(), "ext_ws: message not stored: {e}");
                self.s.close(1011, "message not stored");
                Err(End::Lost(format!("message not stored: {e}")))
            }
            Err(e) if self.run.inbound.is_stopped() => {
                tracing::debug!(conn = self.conn(), "ext_ws: stopped: {e}");
                Err(End::Stopped)
            }
            Err(e) => {
                tracing::warn!(conn = self.conn(), "ext_ws: message dropped: {e}");
                Ok(())
            }
        }
    }

    fn unknown_chat(&self, chat: &str) -> bool {
        let (ws, conn) = (self.run.state.ws.as_str(), self.conn());
        self.hook
            .channels
            .store()
            .reader(|c| crate::spaces::find(c, ws, conn, chat))
            .map(|s| s.is_none())
            .unwrap_or(false)
    }

    /// Participant IDs and aliases use the same identity normalization as message senders.
    async fn participants(&self, item: &Value) {
        if self.s.hello().connector.is_none() {
            return;
        }
        let (Ok(chat), Some(members)) = (
            wire::id(item, "conversation_id"),
            item["members"].as_array().filter(|m| m.len() <= 256),
        ) else {
            return;
        };
        let members = members
            .iter()
            .filter_map(|v| {
                let wire = wire::WireSender::parse(v).ok()?;
                let sender = provider::sender(&wire, &self.realm, &self.known);
                Some(crate::channel::MemberInfo {
                    handle: sender.handle,
                    aliases: sender.aliases,
                    name: sender.display_name.unwrap_or_default(),
                    role: if sender.is_bot {
                        "bot".into()
                    } else {
                        v["role"]
                            .as_str()
                            .filter(|r| matches!(*r, "owner" | "admin" | "member"))
                            .unwrap_or("member")
                            .into()
                    },
                    external: sender.external,
                })
            })
            .collect::<Vec<_>>();
        let event = if item["change"] == "removed" {
            SpaceEvent::MembersRemoved {
                space_ext_id: chat,
                has_external: None,
                members: members.into_iter().map(|m| m.handle).collect(),
            }
        } else {
            SpaceEvent::MembersAdded {
                space_ext_id: chat,
                has_external: None,
                members,
            }
        };
        if let Err(e) = self.run.inbound.space_event(event).await {
            tracing::warn!(
                conn = self.conn(),
                "connector participants not applied: {e}"
            );
        }
    }

    /// The remote describes platform objects, never Hub IDs or visibility. EventSink binds
    /// privacy to this connection's owner and persists the stable event ID before we ack.
    async fn connector_event(&self, item: &Value) -> Step {
        let Some(manifest) = &self.s.hello().connector else {
            return Ok(());
        };
        let Some(kind) = item["event_type"]
            .as_str()
            .filter(|k| manifest.events.iter().any(|e| e == k))
        else {
            return Ok(());
        };
        let (Ok(id), Ok(occurred)) = (
            wire::id(item, "event_id"),
            wire::time(item, "occurred_ms", self.now()),
        ) else {
            tracing::warn!(
                conn = self.conn(),
                "connector event has invalid identity/time"
            );
            return Ok(());
        };
        let summary = item["summary"]
            .as_str()
            .unwrap_or(kind)
            .chars()
            .take(1000)
            .collect::<String>();
        let mut event = steve_flow::RawEvent::new(
            "connector",
            kind,
            format!("event:{}", wire::idempotency_key(&id)),
            occurred,
            &summary,
        );
        event.thread_key = wire::opt_id(item, "topic_id").ok().flatten();
        event.scope_ext_id = wire::opt_id(item, "conversation_id").ok().flatten();
        // Only checked reference metadata accompanies events. No eager remote object/download.
        event.payload = json!({"connector": manifest.id, "event_id": id,
            "conversation_id": event.scope_ext_id, "topic_id": event.thread_key});
        if occurred < self.watermark {
            event.record_only = Some("before connector watermark".into());
        }
        match self.run.inbound.connector_event(event).await {
            Ok(_) => Ok(()),
            Err(ApiError::Internal(e)) => {
                self.s.close(1011, "event not stored");
                Err(End::Lost(format!("event not stored: {e}")))
            }
            Err(_) if self.run.inbound.is_stopped() => Err(End::Stopped),
            Err(e) => {
                tracing::warn!(conn = self.conn(), "connector event dropped: {e}");
                Ok(())
            }
        }
    }

    /// `chats.get`, best effort.
    async fn chat(&self, chat: &str) -> Option<Chat> {
        let v = ExtWs::read(
            &self.s,
            "chats.get",
            json!({"chat_id": chat}),
            self.s.timing().call,
        )
        .await
        .ok()?;
        Chat::parse(&v).ok().filter(|c| c.chat_id == chat)
    }

    /// A `chat` item (§5.2): joined, left, renamed; members changes are not followed.
    async fn chat_change(&self, chat: &str, change: &str) {
        let ev = match change {
            "joined" => {
                let c = self.chat(chat).await;
                SpaceEvent::Joined {
                    space_ext_id: chat.to_string(),
                    name: c.as_ref().and_then(|c| c.name.clone()),
                    has_external: c.map(|c| c.has_external),
                }
            }
            "left" => SpaceEvent::Left {
                space_ext_id: chat.to_string(),
                archived: false,
            },
            "renamed" => {
                let c = self.chat(chat).await;
                SpaceEvent::Updated {
                    space_ext_id: chat.to_string(),
                    name: c.as_ref().and_then(|c| c.name.clone()),
                    has_external: c.map(|c| c.has_external),
                }
            }
            _ => return,
        };
        if let Err(e) = self.run.inbound.space_event(ev).await {
            tracing::warn!(conn = self.conn(), "ext_ws: chat change not applied: {e}");
        }
    }

    /// The chats this connection sees (for a gap of every chat).
    fn my_chats(&self) -> Vec<String> {
        let (ws, conn) = (self.run.state.ws.clone(), self.conn().to_string());
        self.hook
            .channels
            .store()
            .reader(|c| -> Result<Vec<String>, ApiError> {
                let mut st = c.prepare(
                    "SELECT DISTINCT s.external_id FROM spaces s
                       JOIN space_identities i ON i.space_id = s.id AND i.left_at IS NULL
                      WHERE i.connection_id = ?1 AND s.workspace_id = ?2 AND s.status = 'active'
                      ORDER BY s.updated DESC LIMIT ?3",
                )?;
                let rows = st
                    .query_map(params![conn, ws, GAP_CHATS as i64], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(rows)
            })
            .unwrap_or_default()
    }

    /// Reads `[from, to)` of the listed chats (`None`: every chat of the connection) back as
    /// backfill, at most 10 pages of 50 per chat (§5.5); what could not be read is noted.
    async fn repair(&mut self, chats: Option<Vec<String>>, from: i64, to: i64) -> Step {
        // Connector history is pull-only. A gap is observable, not permission to import history.
        if self.s.hello().connector.is_some() {
            tracing::warn!(
                conn = self.conn(),
                from,
                to,
                "connector gap: history not backfilled"
            );
            return Ok(());
        }
        if from >= to {
            return Ok(());
        }
        let chats = match chats {
            Some(c) => c.into_iter().take(GAP_CHATS).collect(),
            None => self.my_chats(),
        };
        let now = self.now();
        for chat in chats {
            let r = provider::read_range(
                &self.s,
                &chat,
                from,
                to,
                GAP_PAGE * GAP_PAGES,
                GAP_PAGE,
                GAP_PAGES,
                now,
            )
            .await;
            let complete = match r {
                Ok((msgs, complete)) => {
                    for m in msgs {
                        let msg = provider::inbound(&m, &self.realm, &self.known, "history");
                        self.deliver(msg.backfill()).await?;
                    }
                    complete
                }
                Err(e @ (CallError::Closed(_) | CallError::Timeout)) => {
                    self.note_missing(&chat, from, to);
                    return Err(self.lost(&e));
                }
                Err(e) => {
                    tracing::warn!(conn = self.conn(), chat, "ext_ws: gap not read: {e}");
                    false
                }
            };
            if !complete {
                self.note_missing(&chat, from, to);
            }
        }
        Ok(())
    }

    /// One line in the next report: messages of the chat in `[from, to)` may be missing (§5.5).
    fn note_missing(&self, chat: &str, from: i64, to: i64) {
        let (ws, conn) = (self.run.state.ws.as_str(), self.conn());
        let now = self.now();
        let fmt = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default()
        };
        let r = self
            .hook
            .channels
            .store()
            .transaction(|tx| -> Result<(), ApiError> {
                let label = crate::spaces::find(tx, ws, conn, chat)?
                    .map(|s| crate::respond::space_label(&s))
                    .unwrap_or_else(|| chat.to_string());
                let text = format!(
                    "「{label}」在 {} 到 {} 之间的消息可能不全（渠道服务没能补齐）",
                    fmt(from),
                    fmt(to)
                );
                steve_flow::digest::add(tx, ws, &text, None, None, now)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                Ok(())
            });
        tracing::warn!(conn, chat, from, to, "ext_ws: messages may be missing");
        if let Err(e) = r {
            tracing::warn!(conn, "ext_ws: missing messages not noted: {e}");
        }
    }
}

/// A `gap` item: its chats (`None`: every chat), from, to.
fn gap(it: &Value, now: i64) -> Result<(Option<Vec<String>>, i64, i64), String> {
    let chats = it
        .get("chats")
        .and_then(Value::as_array)
        .ok_or("a gap without chats")?;
    let list = if chats.iter().any(|c| c.as_str() == Some("*")) {
        None
    } else {
        Some(
            chats
                .iter()
                .filter_map(Value::as_str)
                .filter(|c| wire::opaque_ok(c, 256))
                .map(str::to_string)
                .collect(),
        )
    };
    let from = it
        .get("from_ms")
        .and_then(Value::as_i64)
        .ok_or("a gap without from_ms")?;
    let to = it
        .get("to_ms")
        .and_then(Value::as_i64)
        .ok_or("a gap without to_ms")?;
    if from < 0 || to < from || to > now + wire::MAX_AHEAD_MS {
        return Err("a gap with a bad range".into());
    }
    Ok((list, from, to))
}
