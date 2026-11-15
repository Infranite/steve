//! Conversation intake/storage and the tool contract for an ordinary digest Bot. Extraction and
//! write validation are deterministic; the configured Bot judges meaning through its harness.
//! No model is called by this crate, and output delivery remains separate.
//!
//! - [`conv`]: a conversation, source-neutral (`space:<id>` — a group, DM or topic; `mail_thread:<id>`;
//!   connectors add more types), and the messages Steve stored for it, read the same way for every source.
//! - [`profile`]: each conversation's kind (专项群 / 团队群 / 信息归类群 / 私聊 / 邮件线程 / PR …), its role (the
//!   space's duty) and its digest settings — window size, quiet time, maximum wait, depth, daily budget, priority,
//!   notification level — each either set by you or following the kind's default.
//! - [`window`]: the per-conversation cursor and the window cut (size, quiet, maximum wait), the tick that cuts
//!   windows for every conversation and queues them as packets.
//! - [`facts`] and [`normalize`]: what a window says for certain — conversation, message ids and time span,
//!   senders and @ parties, reply chains and topics, your participation, references (items, links, documents,
//!   attachments, cards, forwarded and quoted messages) — and its text with position numbers.
//! - [`candidates`]: what a window may belong to, found by deterministic signals only (same reply chain or topic,
//!   referenced items, the same people close in time, full-text hits on matters and memories).
//! - [`alerts`]: alert-type conversations aggregated by rule (category, count, recovered, handler); only the
//!   anomalies are queued.
//! - [`chunk`]: a big window cut by token budget at reply-chain, topic and time-gap boundaries, with overlap.
//! - [`queue`]: the durable packet queue — batching (every 30 minutes or N packets), leases, retry and progress
//!   per chunk.
//! - [`matter`] (事), [`todo`] (我的待办), [`memindex`] (the conversation, people, matter and message range of a
//!   memory), [`changes`] (every digest write, undoable).
//! - [`bot`] and [`tools`]: ordinary Bot rules, leased chunk reads, validated writes and persisted reports.
//!   The Hub binds its ordinary schedule; this crate never starts a harness or calls a model.
//! - [`api`]: the routes of the conversation page, the knowledge page's matters and 待处理's 我的待办.

steve_i18n::init!();

pub mod alerts;
pub mod api;
pub mod bot;
pub mod candidates;
pub mod changes;
pub mod chunk;
pub mod conv;
pub mod facts;
pub mod job;
pub mod matter;
pub mod memindex;
pub mod normalize;
pub mod profile;
pub mod queue;
pub mod summary;
pub mod todo;
pub mod tools;
pub mod window;

pub use api::routes;
pub use conv::ConvRef;

use steve_store::ChangeEvent;

/// `{type:"digest", id, ws}` after the commit: a conversation profile, matter, todo or packet changed.
pub(crate) fn event(tx: &steve_store::Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Digest {
        id: id.into(),
        ws: ws.into(),
    });
}

/// Unix ms as `MM-DD HH:MM` in the Hub's local time (packet text, tool results).
pub(crate) fn short_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|d| d.format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// The local day `YYYY-MM-DD` of Unix ms.
pub(crate) fn day_of(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// The first `n` characters of `s`, with `…` when cut.
pub(crate) fn cut(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}
