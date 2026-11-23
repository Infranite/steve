//! Authorization and retry state of outbound channel messages.

use sea_query::{Expr, ExprTrait, Func, OnConflict, Query};

use crate::contract::Q;
use crate::{Read, Result, W};

/// What lets a message go out without a draft (“授权（规则或决定）”). The outbound verifies it
/// against the store again right before the adapter is called; anything else becomes a draft.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Auth {
    /// Nothing: a draft for your approval.
    #[default]
    None,
    /// A live member explicitly requested a bot send, bound to a durable output intent.
    /// Never valid on personal accounts or on legacy messages without that intent.
    Requester { user: String },
    /// A bot account's response rule that speaks (`reply`; `create_item`'s “收到，已记为…”;
    /// `handoff`'s “我请…来看。”) for the inbound message it answers.
    Rule { source: String, act: Act },
    /// Your own trigger words in your own message (only on your own account).
    Trigger { source: String },
    /// A live `msg.send` grant of the flow, for the flow version the run is running (a grant is
    /// bound to one version: an edit of the flow ends it).
    Grant {
        flow_id: String,
        run_id: String,
        #[serde(default)]
        flow_version: i64,
    },
    /// The flow engine's approval for this run: `gate` = you approved exactly this step (its fixed
    /// text was on the card), else an `approve` node earlier on the path.
    Step {
        run_id: String,
        node_id: String,
        gate: bool,
    },
    /// You approved this message (a resolved `approve` `yes` or `fail` `retry` decision about it).
    Decision { id: String },
}

impl Auth {
    /// For the audit log.
    pub fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Requester { .. } => "requester",
            Self::Rule { .. } => "rule",
            Self::Trigger { .. } => "trigger",
            Self::Grant { .. } => "grant",
            Self::Step { .. } => "step",
            Self::Decision { .. } => "decision",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemMode {
    Start,
    Propose,
}

impl ItemMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Propose => "propose",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Act {
    Reply,
    Draft,
    Record,
    CreateItem(ItemMode),
    Handoff,
}

impl Act {
    pub fn code(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Draft => "draft",
            Self::Record => "record",
            Self::CreateItem(_) => "create_item",
            Self::Handoff => "handoff",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendState {
    pub auth: Auth,
    pub first_at: Option<i64>,
    pub attempts: i64,
}

pub fn keep(w: &mut W<'_>, message: &str, auth: &Auth) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("chan_send_state")
            .columns(["message_id", "auth"])
            .values_panic([message.into(), serde_json::to_string(auth)?.into()])
            .on_conflict(OnConflict::column("message_id").do_nothing().to_owned()),
    )?;
    Ok(())
}

/// Records the first attempt and keeps the same text and mentions on retries.
pub fn attempt(
    w: &mut W<'_>,
    message: &str,
    auth: &Auth,
    text: &str,
    at: &[String],
    now: i64,
) -> Result<(String, Vec<String>)> {
    keep(w, message, auth)?;
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value("attempts", Expr::col("attempts").add(1_i64))
            .value("last_attempt_at", now)
            .and_where(Expr::col("message_id").eq(message)),
    )?;
    let kept = w.one(
        Query::select()
            .columns(["text", "at"])
            .from("chan_send_state")
            .and_where(Expr::col("message_id").eq(message)),
        |row| Ok((row.get::<Option<String>>(0)?, row.get::<Option<String>>(1)?)),
    )?;
    let mut update = Query::update();
    update
        .table("chan_send_state")
        .value("attempts", Expr::col("attempts").add(1_i64))
        .and_where(Expr::col("message_id").eq(message));
    let outgoing = match kept {
        Some((Some(s), at)) => (
            s,
            at.and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
        ),
        _ => {
            update
                .value("text", text)
                .value("at", serde_json::to_string(at)?)
                .value(
                    "first_at",
                    Func::coalesce([Expr::col("first_at"), Expr::val(now)]),
                );
            (text.to_owned(), at.to_vec())
        }
    };
    w.exec(&update)?;
    Ok(outgoing)
}

pub fn delivery_error(w: &mut W<'_>, message: &str, error: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value("last_error", error)
            .and_where(Expr::col("message_id").eq(message)),
    )?;
    Ok(())
}

pub fn error(w: &mut W<'_>, message: &str, error: &str) -> Result<()> {
    delivery_error(w, message, error)?;
    w.exec(
        Query::update()
            .table("chan_send_state")
            .value("last_error", error)
            .and_where(Expr::col("message_id").eq(message)),
    )?;
    Ok(())
}

pub fn get(r: &mut impl Read, message: &str) -> Result<Option<SendState>> {
    Ok(r.one(
        Query::select()
            .columns(["auth", "first_at", "attempts"])
            .from("chan_send_state")
            .and_where(Expr::col("message_id").eq(message)),
        |row| Ok((row.get::<String>(0)?, row.get(1)?, row.get(2)?)),
    )?
    .and_then(|(auth, first_at, attempts)| {
        Some(SendState {
            auth: serde_json::from_str(&auth).ok()?,
            first_at,
            attempts,
        })
    }))
}

pub(crate) fn attempted(r: &mut impl Read, message: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("message_id")
            .from("chan_send_state")
            .and_where(Expr::col("message_id").eq(message))
            .and_where(Expr::col("first_at").is_not_null()),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn forget(w: &mut W<'_>, message: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("chan_send_state")
            .and_where(Expr::col("message_id").eq(message)),
    )?;
    Ok(())
}

pub fn last_error(r: &mut impl Read, message: &str) -> Result<Option<String>> {
    let error: Option<String> = r
        .one(
            Query::select()
                .column("last_error")
                .from("chan_send_state")
                .and_where(Expr::col("message_id").eq(message)),
            |row| row.get(0),
        )?
        .flatten();
    Ok(error.filter(|error| !error.is_empty()))
}
