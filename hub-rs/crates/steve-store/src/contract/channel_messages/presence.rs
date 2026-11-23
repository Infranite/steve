//! Stored conversation evidence used by the personal-account presence policy.

use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SelectStatement};
use serde_json::Value;

use crate::{Accounts, Contacts, Members, Read, Result};

use super::super::{channel_spaces, connections};

pub struct Owner {
    pub accounts: Vec<String>,
    parties: Vec<String>,
}

pub fn owner(r: &mut impl Read, ws: &str, space: &channel_spaces::rows::Space) -> Result<Owner> {
    let mut roots = Vec::new();
    for user in Members::new(r).owners(ws)? {
        if let Some(id) = Contacts::new(r).user_party(ws, &user)? {
            roots.push(id);
        }
    }
    if !space.via.is_empty()
        && channel_spaces::account_settings::get(r, ws, &space.via)?
            .is_some_and(|settings| settings.mode == channel_spaces::account_settings::Mode::Own)
        && let Some(connection) = connections::owner(r, ws, &space.via)?
        && let Some(id) = crate::accounts::owned_party(r, ws, &connection.account)?
    {
        roots.push(id);
    }
    let mut parties = Vec::new();
    for id in roots {
        parties.extend(Contacts::new(r).merged_party_ids(&id)?);
    }
    parties.sort_unstable();
    parties.dedup();
    let accounts = Accounts(r).ids_by_parties(&parties)?;
    Ok(Owner { accounts, parties })
}

impl Owner {
    fn is_sender(&self, r: &mut impl Read, alias: &'static str) -> Result<Expr> {
        let dialect = r.db().dialect();
        let parties = Expr::col((alias, "sender_party_id"));
        let accounts = Expr::col((alias, "sender_account_id"));
        Ok(parties
            .clone()
            .is_not_null()
            .and(parties.in_subquery(dialect.text_array(&self.parties)?))
            .or(accounts
                .clone()
                .is_not_null()
                .and(accounts.in_subquery(dialect.text_array(&self.accounts)?))))
    }
}

pub struct Line {
    pub id: String,
    pub at: i64,
    pub owner: bool,
    pub sender: String,
    pub thread: Option<String>,
    pub external: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
    pub account: Option<String>,
    pub text: String,
    pub answered: bool,
}

fn message_rows(r: &mut impl Read, owner: &Owner, space: &str) -> Result<SelectStatement> {
    let mine = owner.is_sender(r, "m")?;
    let account_ids = r.db().dialect().text_array(&owner.accounts)?;
    let answered = Query::select()
        .column(("o", "id"))
        .from_as("chan_messages", "o")
        .and_where(Expr::col(("o", "space_id")).equals(("m", "space_id")))
        .and_where(Expr::col(("o", "direction")).eq("out"))
        .and_where(Expr::col(("o", "reply_to")).equals(("m", "external_id")))
        .and_where(Expr::col(("o", "status")).is_in(["pending", "sent", "unknown", "recalled"]))
        .and_where(Expr::col(("o", "on_behalf")).eq("none"))
        .and_where(Expr::col(("o", "persona_id")).is_null())
        .and_where(Expr::col(("o", "sender_account_id")).in_subquery(account_ids))
        .take();
    Ok(Query::select()
        .columns([
            ("m", "id"),
            ("m", "created"),
            ("m", "sender_party_id"),
            ("m", "thread_key"),
            ("m", "external_id"),
            ("m", "reply_to"),
            ("m", "mentions"),
            ("m", "sender_account_id"),
            ("m", "text"),
        ])
        .expr(mine)
        .expr(Expr::exists(answered))
        .from_as("chan_messages", "m")
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .take())
}

fn line(row: &crate::contract::Row<'_>) -> Result<Line> {
    let account = row.get::<Option<String>>(7)?;
    let party = row.get::<Option<String>>(2)?;
    let mentions = row
        .get::<Option<String>>(6)?
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(&raw).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|mention| mention["account_id"].as_str().map(str::to_owned))
        .collect();
    Ok(Line {
        id: row.get(0)?,
        at: row.get(1)?,
        owner: row.get::<i64>(9)? != 0,
        sender: party.or_else(|| account.clone()).unwrap_or_default(),
        thread: row.get(3)?,
        external: row.get(4)?,
        reply_to: row.get(5)?,
        mentions,
        account,
        text: row.get::<Option<String>>(8)?.unwrap_or_default(),
        answered: row.get::<i64>(10)? != 0,
    })
}

pub fn get(r: &mut impl Read, owner: &Owner, space: &str, id: &str) -> Result<Option<Line>> {
    let mut rows = message_rows(r, owner, space)?;
    r.one(rows.and_where(Expr::col(("m", "id")).eq(id)), line)
}

pub fn around(
    r: &mut impl Read,
    owner: &Owner,
    space: &str,
    from: i64,
    to: i64,
    limit: u64,
) -> Result<Vec<Line>> {
    let tie = r.db().dialect().insertion_order(Some("m"));
    let mut rows = message_rows(r, owner, space)?;
    r.all(
        rows.and_where(Expr::col(("m", "created")).between(from, to))
            .order_by(("m", "created"), Order::Asc)
            .order_by_expr(tie, Order::Asc)
            .limit(limit),
        line,
    )
}

pub fn owner_texts(
    r: &mut impl Read,
    owner: &Owner,
    space: &str,
    from: i64,
    to: i64,
) -> Result<Vec<String>> {
    let sender = owner.is_sender(r, "m")?;
    r.all(
        Query::select()
            .column(("m", "text"))
            .from_as("chan_messages", "m")
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(Expr::col(("m", "created")).between(from, to))
            .and_where(sender),
        |row| Ok(row.get::<Option<String>>(0)?.unwrap_or_default()),
    )
}

pub fn merged_into(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("turn_of")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("direction").eq("in")),
        |row| row.get(0),
    )?
    .flatten())
}

pub struct Reply {
    pub space: String,
    pub source: String,
}

pub fn answered_message(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Reply>> {
    let row = r.one(
        Query::select()
            .columns(["space_id", "reply_to", "sender_account_id"])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("direction").eq("out"))
            .and_where(Expr::col("on_behalf").eq("none"))
            .and_where(Expr::col("persona_id").is_null()),
        |row| {
            Ok((
                row.get::<String>(0)?,
                row.get::<Option<String>>(1)?,
                row.get::<Option<String>>(2)?,
            ))
        },
    )?;
    let Some((space, Some(external), Some(account))) = row else {
        return Ok(None);
    };
    let Some(space) = channel_spaces::rows::get_in_workspace(r, ws, &space)? else {
        return Ok(None);
    };
    let owner = owner(r, ws, &space)?;
    if !owner.accounts.contains(&account) {
        return Ok(None);
    }
    let sender = owner.is_sender(r, "m")?;
    Ok(r.one(
        Query::select()
            .column(("m", "id"))
            .from_as("chan_messages", "m")
            .and_where(Expr::col(("m", "space_id")).eq(&space.id))
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(Expr::col(("m", "external_id")).eq(external))
            .and_where(sender.not())
            .limit(1),
        |row| row.get(0),
    )?
    .map(|source| Reply {
        space: space.id,
        source,
    }))
}

pub enum Sources<'a> {
    Before(i64),
    Messages(&'a [String]),
    All,
}

fn sources(
    r: &mut impl Read,
    owner: &Owner,
    space: &str,
    sources: &Sources<'_>,
) -> Result<SelectStatement> {
    let mut rows = Query::select();
    rows.column(("m", "id"))
        .from_as("chan_messages", "m")
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .and_where(owner.is_sender(r, "m")?.not());
    match sources {
        Sources::Before(at) => {
            rows.and_where(Expr::col(("m", "created")).lt(*at));
        }
        Sources::Messages(ids) => {
            rows.and_where(Expr::col(("m", "id")).in_subquery(r.db().dialect().text_array(ids)?));
        }
        Sources::All => {}
    }
    Ok(rows)
}

pub struct SupersededDraft {
    pub id: String,
    pub status: String,
    pub source: String,
}

pub fn drafts(
    r: &mut impl Read,
    owner: &Owner,
    space: &str,
    selected: &Sources<'_>,
) -> Result<Vec<SupersededDraft>> {
    let sources = sources(r, owner, space, selected)?;
    let accounts = r.db().dialect().text_array(&owner.accounts)?;
    r.all(
        Query::select()
            .columns([("o", "id"), ("o", "status"), ("m", "id")])
            .from_as("chan_messages", "o")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "space_id"))
                    .equals(("o", "space_id"))
                    .and(Expr::col(("m", "external_id")).equals(("o", "reply_to"))),
            )
            .and_where(Expr::col(("o", "space_id")).eq(space))
            .and_where(Expr::col(("o", "direction")).eq("out"))
            .and_where(Expr::col(("o", "status")).is_in(["draft", "failed"]))
            .and_where(Expr::col(("o", "on_behalf")).eq("none"))
            .and_where(Expr::col(("o", "persona_id")).is_null())
            .and_where(Expr::col(("o", "sender_account_id")).in_subquery(accounts))
            .and_where(Expr::col(("m", "id")).in_subquery(sources)),
        |row| {
            Ok(SupersededDraft {
                id: row.get(0)?,
                status: row.get(1)?,
                source: row.get(2)?,
            })
        },
    )
}

pub fn asks(
    r: &mut impl Read,
    ws: &str,
    owner: &Owner,
    space: &str,
    selected: &Sources<'_>,
) -> Result<Vec<(String, String)>> {
    let sources = sources(r, owner, space, selected)?;
    super::super::decisions::open_channel_asks(r, ws, sources)
}
