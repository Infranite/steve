//! Create outbound messages and copies that need a separate delivery identity.

use sea_query::{Expr, ExprTrait, Order, Query};
use serde_json::Value;

use crate::contract::Q;
use crate::{Read, Result, StoreError, W};

use steve_store_sqlite::digest::{self, Source};

pub struct Approval {
    pub space: String,
    pub status: String,
    pub decision: Option<String>,
}

pub fn approval(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Approval>> {
    r.one(
        Query::select()
            .columns(["space_id", "status", "decision_id"])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("direction").eq("out")),
        |row| {
            Ok(Approval {
                space: row.get(0)?,
                status: row.get(1)?,
                decision: row.get(2)?,
            })
        },
    )
}

pub fn decision_id(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("decision_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

pub struct NewMessage<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub space: &'a str,
    pub thread: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub account: Option<&'a str>,
    pub persona: Option<&'a str>,
    pub on_behalf: &'a str,
    pub text: Option<&'a str>,
    pub status: &'a str,
    pub created: i64,
}

pub fn insert(w: &mut W<'_>, n: &NewMessage<'_>) -> Result<()> {
    let mask = match n.account {
        Some(account) => super::super::channel_spaces::account_identity_mask(w, n.space, account)?,
        None => 0,
    };
    insert_with_provenance(w, n, mask, None, None, None, None)
}

pub fn set_burst_id(w: &mut W<'_>, id: &str, burst_id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("burst_id", burst_id)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn burst_id_of(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("burst_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

fn insert_with_provenance(
    w: &mut W<'_>,
    n: &NewMessage<'_>,
    seen_by: i64,
    basis_id: Option<&str>,
    composed: Option<&str>,
    answer_to: Option<&str>,
    burst_id: Option<&str>,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("chan_messages")
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "thread_key",
                "reply_to",
                "direction",
                "sender_account_id",
                "persona_id",
                "on_behalf",
                "text",
                "status",
                "created",
                "seen_by",
                "basis_id",
                "composed",
                "answer_to",
                "burst_id",
            ])
            .values_panic([
                n.id.into(),
                n.ws.into(),
                n.space.into(),
                n.thread.into(),
                n.reply_to.into(),
                "out".into(),
                n.account.into(),
                n.persona.into(),
                n.on_behalf.into(),
                n.text.into(),
                n.status.into(),
                n.created.into(),
                seen_by.into(),
                basis_id.into(),
                composed.into(),
                answer_to.into(),
                burst_id.into(),
            ]),
    )?;
    digest::sync_message(w.conn, Source::ChanMessage, n.id, None)?;
    Ok(())
}

struct RecoverySource {
    ws: String,
    space: String,
    thread: Option<String>,
    reply_to: Option<String>,
    account: Option<String>,
    persona: Option<String>,
    on_behalf: String,
    text: Option<String>,
    seen_by: i64,
    basis_id: Option<String>,
    composed: Option<String>,
    answer_to: Option<String>,
    burst_id: Option<String>,
}

/// A rewritten payload must have a fresh id. Copy its basis and composer provenance with it.
pub fn copy_for_recovery(w: &mut W<'_>, id: &str, next: &str, now: i64) -> Result<()> {
    let source = w
        .one(
            Query::select()
                .columns([
                    "workspace_id",
                    "space_id",
                    "thread_key",
                    "reply_to",
                    "sender_account_id",
                    "persona_id",
                    "on_behalf",
                    "text",
                    "seen_by",
                    "basis_id",
                    "composed",
                    "answer_to",
                    "burst_id",
                ])
                .from("chan_messages")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("direction").eq("out")),
            |r| {
                Ok(RecoverySource {
                    ws: r.get(0)?,
                    space: r.get(1)?,
                    thread: r.get(2)?,
                    reply_to: r.get(3)?,
                    account: r.get(4)?,
                    persona: r.get(5)?,
                    on_behalf: r.get(6)?,
                    text: r.get(7)?,
                    seen_by: r.get(8)?,
                    basis_id: r.get(9)?,
                    composed: r.get(10)?,
                    answer_to: r.get(11)?,
                    burst_id: r.get(12)?,
                })
            },
        )?
        .ok_or_else(|| StoreError::Rejected("recovery draft is missing".into()))?;
    insert_with_provenance(
        w,
        &NewMessage {
            id: next,
            ws: &source.ws,
            space: &source.space,
            thread: source.thread.as_deref(),
            reply_to: source.reply_to.as_deref(),
            account: source.account.as_deref(),
            persona: source.persona.as_deref(),
            on_behalf: &source.on_behalf,
            text: source.text.as_deref(),
            status: "draft",
            created: now,
        },
        source.seen_by,
        source.basis_id.as_deref(),
        source.composed.as_deref(),
        source.answer_to.as_deref(),
        source.burst_id.as_deref(),
    )?;
    super::incoming_reactions::copy_reply_source(w, id, next)
}

/// One outbound `chan_messages` row as the sender needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutRow {
    pub id: String,
    pub ws: String,
    pub space_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub thread_key: Option<String>,
    pub sender_account_id: Option<String>,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub sent: Option<i64>,
}

pub fn send_row(r: &mut impl Read, id: &str) -> Result<Option<OutRow>> {
    r.one(
        Query::select()
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "text",
                "reply_to",
                "thread_key",
                "sender_account_id",
                "persona_id",
                "on_behalf",
                "status",
                "decision_id",
                "sent",
            ])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("direction").eq("out")),
        |row| {
            Ok(OutRow {
                id: row.get(0)?,
                ws: row.get(1)?,
                space_id: row.get(2)?,
                text: row.get::<Option<String>>(3)?.unwrap_or_default(),
                reply_to: row.get(4)?,
                thread_key: row.get(5)?,
                sender_account_id: row.get(6)?,
                persona_id: row.get(7)?,
                on_behalf: row.get(8)?,
                status: row.get(9)?,
                decision_id: row.get(10)?,
                sent: row.get(11)?,
            })
        },
    )
}

pub fn link_decision(w: &mut W<'_>, id: &str, decision: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("decision_id", decision)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn latest_answer(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("answer_to").eq(message))
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("created", Order::Desc)
            .order_by("rowid", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}

pub fn draft_source(r: &mut impl Read, row: &OutRow) -> Result<Option<String>> {
    let answer = Query::select()
        .column("answer_to")
        .from("chan_messages")
        .and_where(Expr::col("id").eq(row.id.as_str()))
        .take();
    let reply_src = Query::select()
        .column("source_message_id")
        .from("chan_reply_sources")
        .and_where(Expr::col("out_message_id").eq(row.id.as_str()))
        .take();
    let delivery = Query::select()
        .column("source_message_id")
        .from("output_deliveries")
        .and_where(Expr::col("message_id").eq(row.id.as_str()))
        .take();
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("workspace_id").eq(row.ws.as_str()))
            .and_where(Expr::col("space_id").eq(row.space_id.as_str()))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("purged").is_null())
            .and_where(
                Expr::col("id")
                    .in_subquery(answer)
                    .or(Expr::col("id").in_subquery(reply_src))
                    .or(Expr::col("id").in_subquery(delivery))
                    .or(Expr::col("external_id").eq(row.reply_to.as_deref())),
            )
            .order_by("created", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}

pub fn store_composer_result(w: &mut W<'_>, id: &str, result: &Value) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("composed", result.to_string())
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// Editing a draft invalidates the basis of its previously generated text.
pub fn edit_draft(w: &mut W<'_>, id: &str, text: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", text)
            .value("basis_id", Option::<String>::None)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").eq("draft")),
    )?;
    digest::sync_message(w.conn, Source::ChanMessage, id, None)?;
    Ok(())
}

pub fn update_draft_text(w: &mut W<'_>, id: &str, text: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", text)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").eq("draft")),
    )?;
    Ok(())
}

pub fn composer_result(r: &mut impl Read, id: &str) -> Result<Value> {
    let raw: Option<String> = r
        .one(
            Query::select()
                .column("composed")
                .from("chan_messages")
                .and_where(Expr::col("id").eq(id)),
            |row| row.get(0),
        )?
        .flatten();
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

pub fn pending_disclosure(r: &mut impl Read, verdict: &str, limit: u64) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("m", "id"))
            .from_as("chan_messages", "m")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "reply_basis",
                "b",
                Expr::col(("b", "id")).equals(("m", "basis_id")),
            )
            .and_where(Expr::col(("m", "direction")).eq("out"))
            .and_where(Expr::col(("m", "status")).eq("draft"))
            .and_where(Expr::col(("b", "verdict")).eq(verdict))
            .order_by(("m", "created"), Order::Asc)
            .limit(limit),
        |row| row.get(0),
    )
}

pub fn sent_connection(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("sent_via")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?
    .flatten())
}

/// An open outbound draft decision replying to an @-mention or DM, evaluated for working-hour
/// escalation (#213).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAddressedDraft {
    pub decision_id: String,
    pub ws: String,
    pub space_id: String,
    pub message_id: String,
    pub created: i64,
    pub is_dm: bool,
    pub is_mention: bool,
    pub result_json: Option<String>,
}

/// Lists open `chan_message` approval drafts whose inbound source is a DM or @-mention of the
/// speaking account (not yet marked escalated).
pub fn open_addressed_drafts(r: &mut impl Read) -> Result<Vec<OpenAddressedDraft>> {
    use sea_query::{Alias, JoinType};
    let source_subquery = Expr::cust(
        "(SELECT t.id FROM (
            SELECT source_message_id AS id FROM chan_reply_sources WHERE out_message_id = m.id
            UNION ALL SELECT m.answer_to AS id WHERE m.answer_to IS NOT NULL
            UNION ALL SELECT parent.id FROM chan_messages parent
              WHERE parent.space_id = m.space_id AND parent.external_id = m.reply_to
            UNION ALL SELECT source_message_id FROM output_deliveries WHERE message_id = m.id
          ) matches
          CROSS JOIN chan_messages t ON t.id = matches.id
          WHERE t.workspace_id = d.workspace_id AND t.space_id = m.space_id AND t.direction = 'in'
          ORDER BY t.created DESC, t.id LIMIT 1)",
    );
    let is_mention = Expr::cust(
        "EXISTS (SELECT 1 FROM ext_events e WHERE e.message_id = src.id AND e.type = 'im.mention')
         OR (src.mentions IS NOT NULL AND c.account_id IS NOT NULL AND instr(src.mentions, c.account_id) > 0)",
    );
    let mut q = Query::select();
    q.column((Alias::new("d"), Alias::new("id")))
        .column((Alias::new("d"), Alias::new("workspace_id")))
        .column((Alias::new("m"), Alias::new("space_id")))
        .column((Alias::new("m"), Alias::new("id")))
        .column((Alias::new("m"), Alias::new("created")))
        .expr(Expr::cust("s.kind = 'dm'"))
        .expr(is_mention.clone())
        .column((Alias::new("d"), Alias::new("result")))
        .from_as("decisions", "d")
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "m",
            Expr::cust("m.id = d.subject_id AND m.workspace_id = d.workspace_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::cust("s.id = m.space_id AND s.workspace_id = d.workspace_id"),
        )
        .join_as(
            JoinType::LeftJoin,
            "connections",
            "c",
            Expr::cust("c.id = s.connection_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "src",
            Expr::col((Alias::new("src"), Alias::new("id"))).eq(source_subquery),
        )
        .and_where(Expr::col((Alias::new("d"), Alias::new("status"))).eq("open"))
        .and_where(Expr::col((Alias::new("d"), Alias::new("subject_type"))).eq("chan_message"))
        .and_where(Expr::col((Alias::new("d"), Alias::new("kind"))).eq("approve"))
        .and_where(Expr::col((Alias::new("m"), Alias::new("direction"))).eq("out"))
        .and_where(Expr::col((Alias::new("m"), Alias::new("status"))).eq("draft"))
        .and_where(
            Expr::col((Alias::new("m"), Alias::new("decision_id")))
                .equals((Alias::new("d"), Alias::new("id"))),
        )
        .and_where(Expr::cust("s.kind = 'dm'").or(is_mention))
        .order_by((Alias::new("d"), Alias::new("created")), Order::Asc);
    r.all(&q, |row| {
        Ok(OpenAddressedDraft {
            decision_id: row.get(0)?,
            ws: row.get(1)?,
            space_id: row.get(2)?,
            message_id: row.get(3)?,
            created: row.get(4)?,
            is_dm: row.get::<i64>(5)? != 0,
            is_mention: row.get::<i64>(6)? != 0,
            result_json: row.get(7)?,
        })
    })
}

/// Deterministic structural facts used to group similar ignored messages (#213).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCategoryFact {
    pub space_kind: String,
    pub sender_is_bot: bool,
    pub is_mention: bool,
}

/// One resolved channel decision in a space within the ignore-suggestion lookback window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredMessageFact {
    pub decision_id: String,
    pub kind: String,
    pub resolved: i64,
    pub result_json: Option<String>,
    pub category: MessageCategoryFact,
}

/// Reads the deterministic category facts of an inbound message (or the inbound source of an
/// outbound draft) for ignore-pattern grouping.
pub fn message_category_fact(
    r: &mut impl Read,
    ws: &str,
    message_id: &str,
) -> Result<Option<MessageCategoryFact>> {
    use sea_query::{Alias, JoinType};
    let source_expr = Expr::cust(
        "CASE m.direction
           WHEN 'in' THEN m.id
           WHEN 'out' THEN COALESCE(
             (SELECT t.id FROM (
                SELECT source_message_id AS id FROM chan_reply_sources WHERE out_message_id = m.id
                UNION ALL SELECT m.answer_to AS id WHERE m.answer_to IS NOT NULL
                UNION ALL SELECT parent.id FROM chan_messages parent
                  WHERE parent.space_id = m.space_id AND parent.external_id = m.reply_to
                UNION ALL SELECT source_message_id FROM output_deliveries WHERE message_id = m.id
              ) matches
              CROSS JOIN chan_messages t ON t.id = matches.id
              WHERE t.workspace_id = m.workspace_id AND t.space_id = m.space_id AND t.direction = 'in'
              ORDER BY t.created DESC, t.id LIMIT 1),
             m.id)
         END",
    );
    let sender_is_bot = Expr::cust(
        "EXISTS (
           SELECT 1 FROM accounts a
             LEFT JOIN parties p ON p.id = a.party_id
            WHERE a.id = src.sender_account_id
              AND (a.kind IN ('bot', 'app_bot') OR p.type = 'bot' OR coalesce(p.contact_kind, '') = 'bot')
         ) OR EXISTS (
           SELECT 1 FROM parties p
            WHERE p.id = src.sender_party_id
              AND (p.type = 'bot' OR coalesce(p.contact_kind, '') = 'bot')
         )",
    );
    let is_mention = Expr::cust(
        "EXISTS (SELECT 1 FROM ext_events e WHERE e.message_id = src.id AND e.type = 'im.mention')
         OR (src.mentions IS NOT NULL AND src.mentions <> '' AND src.mentions <> '[]')",
    );
    let mut q = Query::select();
    q.column((Alias::new("s"), Alias::new("kind")))
        .expr(sender_is_bot)
        .expr(is_mention)
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::cust("s.id = m.space_id AND s.workspace_id = m.workspace_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "src",
            Expr::col((Alias::new("src"), Alias::new("id"))).eq(source_expr),
        )
        .and_where(Expr::col((Alias::new("m"), Alias::new("id"))).eq(message_id))
        .and_where(Expr::col((Alias::new("m"), Alias::new("workspace_id"))).eq(ws));
    r.one(&q, |row| {
        Ok(MessageCategoryFact {
            space_kind: row.get(0)?,
            sender_is_bot: row.get::<i64>(1)? != 0,
            is_mention: row.get::<i64>(2)? != 0,
        })
    })
}

/// Maximum recent resolved channel decisions scanned per space when counting ignores within the
/// rolling rule-suggestion window.
const RECENT_RESOLVED_SCAN_LIMIT: u64 = 200;

/// Lists recent resolved `chan_message` decisions in `space` together with the deterministic
/// category facts of their underlying inbound messages.
pub fn recent_resolved_in_space(
    r: &mut impl Read,
    ws: &str,
    space: &str,
) -> Result<Vec<IgnoredMessageFact>> {
    use sea_query::{Alias, JoinType};
    let source_expr = Expr::cust(
        "CASE m.direction
           WHEN 'in' THEN m.id
           WHEN 'out' THEN COALESCE(
             (SELECT t.id FROM (
                SELECT source_message_id AS id FROM chan_reply_sources WHERE out_message_id = m.id
                UNION ALL SELECT m.answer_to AS id WHERE m.answer_to IS NOT NULL
                UNION ALL SELECT parent.id FROM chan_messages parent
                  WHERE parent.space_id = m.space_id AND parent.external_id = m.reply_to
                UNION ALL SELECT source_message_id FROM output_deliveries WHERE message_id = m.id
              ) matches
              CROSS JOIN chan_messages t ON t.id = matches.id
              WHERE t.workspace_id = m.workspace_id AND t.space_id = m.space_id AND t.direction = 'in'
              ORDER BY t.created DESC, t.id LIMIT 1),
             m.id)
         END",
    );
    let sender_is_bot = Expr::cust(
        "EXISTS (
           SELECT 1 FROM accounts a
             LEFT JOIN parties p ON p.id = a.party_id
            WHERE a.id = src.sender_account_id
              AND (a.kind IN ('bot', 'app_bot') OR p.type = 'bot' OR coalesce(p.contact_kind, '') = 'bot')
         ) OR EXISTS (
           SELECT 1 FROM parties p
            WHERE p.id = src.sender_party_id
              AND (p.type = 'bot' OR coalesce(p.contact_kind, '') = 'bot')
         )",
    );
    let is_mention = Expr::cust(
        "EXISTS (SELECT 1 FROM ext_events e WHERE e.message_id = src.id AND e.type = 'im.mention')
         OR (src.mentions IS NOT NULL AND src.mentions <> '' AND src.mentions <> '[]')",
    );
    let mut q = Query::select();
    q.column((Alias::new("d"), Alias::new("id")))
        .column((Alias::new("d"), Alias::new("kind")))
        .column((Alias::new("d"), Alias::new("resolved")))
        .column((Alias::new("d"), Alias::new("result")))
        .column((Alias::new("s"), Alias::new("kind")))
        .expr(sender_is_bot)
        .expr(is_mention)
        .from_as("decisions", "d")
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "m",
            Expr::cust("m.id = d.subject_id AND m.workspace_id = d.workspace_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::cust("s.id = m.space_id AND s.workspace_id = d.workspace_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "src",
            Expr::col((Alias::new("src"), Alias::new("id"))).eq(source_expr),
        )
        .and_where(Expr::col((Alias::new("d"), Alias::new("workspace_id"))).eq(ws))
        .and_where(Expr::col((Alias::new("d"), Alias::new("subject_type"))).eq("chan_message"))
        .and_where(Expr::col((Alias::new("d"), Alias::new("status"))).eq("resolved"))
        .and_where(Expr::col((Alias::new("d"), Alias::new("kind"))).is_in(["approve", "ask"]))
        .and_where(Expr::col((Alias::new("d"), Alias::new("resolved"))).is_not_null())
        .and_where(Expr::col((Alias::new("m"), Alias::new("space_id"))).eq(space))
        .order_by((Alias::new("d"), Alias::new("resolved")), Order::Desc)
        .limit(RECENT_RESOLVED_SCAN_LIMIT);
    r.all(&q, |row| {
        Ok(IgnoredMessageFact {
            decision_id: row.get(0)?,
            kind: row.get(1)?,
            resolved: row.get(2)?,
            result_json: row.get(3)?,
            category: MessageCategoryFact {
                space_kind: row.get(4)?,
                sender_is_bot: row.get::<i64>(5)? != 0,
                is_mention: row.get::<i64>(6)? != 0,
            },
        })
    })
}

/// Resolved timestamps and result payloads of prior `space` rule-suggestion proposals for `space`.
pub fn resolved_space_suggestions(
    r: &mut impl Read,
    ws: &str,
    space: &str,
) -> Result<Vec<(i64, Option<String>)>> {
    r.all(
        Query::select()
            .columns(["resolved", "result"])
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("space"))
            .and_where(Expr::col("subject_id").eq(space))
            .and_where(Expr::col("kind").eq("propose"))
            .and_where(Expr::col("status").eq("resolved"))
            .and_where(Expr::col("resolved").is_not_null())
            .order_by("resolved", Order::Desc)
            .limit(20),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}
