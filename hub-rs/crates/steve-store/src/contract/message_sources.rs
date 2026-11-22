use std::collections::HashMap;

use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde_json::Value;

use crate::base::{Viewer, Visibility};
use crate::contract::{Json, Row};
use crate::{Read, Result};

/// Messages around a cursor, or the source rows of an already ordered set of IDs.
pub enum MessageSelection<'a> {
    After(i64, &'a str, usize),
    Through(i64, &'a str, usize),
    Ids(&'a [String]),
}

pub struct ChannelSources<'a, C>(pub &'a mut C);
pub struct MailSources<'a, C>(pub &'a mut C);
pub struct Connections<'a, C>(pub &'a mut C);

pub struct PollConnection {
    pub id: String,
    pub ws: String,
    pub adapter: String,
    pub caps: Vec<String>,
}

pub struct ConnectionInfo {
    pub id: String,
    pub account: String,
    pub adapter: String,
    pub status: String,
    pub last_sync: Option<i64>,
    pub caps: Vec<String>,
    pub created_by: String,
}

fn connection_info() -> SelectStatement {
    Query::select()
        .from("connections")
        .columns([
            "id",
            "account_id",
            "adapter",
            "status",
            "last_sync",
            "caps",
            "created_by",
        ])
        .take()
}

fn decode_connection(row: &Row<'_>) -> Result<ConnectionInfo> {
    Ok(ConnectionInfo {
        id: row.get(0)?,
        account: row.get(1)?,
        adapter: row.get(2)?,
        status: row.get(3)?,
        last_sync: row.get(4)?,
        caps: row.get::<Json<_>>(5)?.0,
        created_by: row.get(6)?,
    })
}

pub struct SpaceSource {
    pub name: String,
    pub kind: String,
    pub connection: Option<String>,
    pub members: i64,
    pub external: bool,
    pub role: String,
    pub muted: bool,
    pub attention: String,
    pub active: bool,
}

pub struct MailSource {
    pub subject: String,
    pub connection: Option<String>,
    pub members: i64,
    pub muted: bool,
    visibility: Visibility,
    author: String,
}

impl MailSource {
    pub fn visible_to(&self, viewer: &Viewer) -> bool {
        viewer.sees(&self.visibility, &self.author)
    }
}

pub struct ChannelMessage {
    pub id: String,
    pub ext: Option<String>,
    pub at: i64,
    pub party: Option<String>,
    pub account: Option<String>,
    pub out: bool,
    pub text: String,
    pub atts: Vec<Value>,
    pub mentions: Vec<Value>,
    pub thread: Option<String>,
    pub behalf: String,
    pub reply: Option<String>,
}

pub struct MailMessage {
    pub id: String,
    pub mid: Option<String>,
    pub at: i64,
    pub party: Option<String>,
    pub account: Option<String>,
    pub text: String,
    pub to: Vec<Value>,
    pub cc: Vec<Value>,
    pub name: String,
    pub addr: String,
    pub subject: String,
    pub bulk: bool,
    pub refs: Vec<String>,
    pub reply: Option<String>,
}

pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

fn optional_json<T: serde::de::DeserializeOwned + Default>(r: &Row<'_>, i: usize) -> Result<T> {
    Ok(r.get::<Option<Json<T>>>(i)?
        .map(|value| value.0)
        .unwrap_or_default())
}

fn within(table: &'static str, ws: &str, id: &str) -> Expr {
    Expr::col((table, "workspace_id"))
        .eq(ws)
        .and(Expr::col((table, "id")).eq(id))
}

fn source_messages(
    table: &'static str,
    parent: &'static str,
    timestamp: &'static str,
    ws: &str,
    id: &str,
    selection: MessageSelection<'_>,
) -> Option<SelectStatement> {
    let mut q = Query::select();
    q.from_as(table, "m")
        .and_where(Expr::col(("m", "workspace_id")).eq(ws))
        .and_where(Expr::col(("m", parent)).eq(id))
        .and_where(Expr::col(("m", "status")).is_in(["received", "sent"]));
    let upper = matches!(selection, MessageSelection::Through(..));
    match selection {
        MessageSelection::Ids(ids) if ids.is_empty() => return None,
        MessageSelection::Ids(ids) => {
            q.and_where(Expr::col(("m", "id")).is_in(ids.iter().map(String::as_str)));
        }
        MessageSelection::After(at, id, limit) | MessageSelection::Through(at, id, limit) => {
            let time = Expr::col(("m", timestamp));
            let key = Expr::col(("m", "id"));
            let condition = if upper {
                time.clone().lt(at).or(time.eq(at).and(key.lte(id)))
            } else {
                time.clone().gt(at).or(time.eq(at).and(key.gt(id)))
            };
            let order = if upper { Order::Desc } else { Order::Asc };
            q.and_where(condition)
                .order_by(("m", timestamp), order.clone())
                .order_by(("m", "id"), order)
                .limit(limit.min(i64::MAX as usize) as u64);
        }
    }
    Some(q)
}

fn message_locations(
    read: &mut impl Read,
    table: &'static str,
    parent: &'static str,
    timestamp: &'static str,
    ws: &str,
    conversation: &str,
    ids: &[String],
) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    for ids in ids.chunks(500) {
        out.extend(
            read.all(
                Query::select()
                    .columns(["id", timestamp])
                    .from(table)
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col(parent).eq(conversation))
                    .and_where(Expr::col("id").is_in(ids.iter().map(String::as_str))),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?,
        );
    }
    Ok(out)
}

fn excerpt(
    read: &mut impl Read,
    table: &'static str,
    parent: &'static str,
    text: &'static str,
    ws: &str,
    conversation: &str,
    id: &str,
) -> Result<Option<String>> {
    Ok(read
        .one(
            Query::select()
                .column(text)
                .from(table)
                .and_where(within(table, ws, id))
                .and_where(Expr::col(parent).eq(conversation)),
            |r| r.get::<Option<String>>(0),
        )?
        .flatten())
}

impl<C: Read> ChannelSources<'_, C> {
    pub fn message_locations(
        &mut self,
        ws: &str,
        space: &str,
        ids: &[String],
    ) -> Result<Vec<(String, i64)>> {
        message_locations(
            self.0,
            "chan_messages",
            "space_id",
            "created",
            ws,
            space,
            ids,
        )
    }

    pub fn excerpt(&mut self, ws: &str, space: &str, id: &str) -> Result<Option<String>> {
        excerpt(self.0, "chan_messages", "space_id", "text", ws, space, id)
    }

    pub fn space(&mut self, ws: &str, id: &str) -> Result<Option<SpaceSource>> {
        self.0.one(
            Query::select()
                .columns(
                    [
                        "name",
                        "kind",
                        "connection_id",
                        "member_count",
                        "has_external",
                    ]
                    .map(|field| ("spaces", field)),
                )
                .column(("space_settings", "duty"))
                .column(("space_settings", "muted"))
                .column(("space_prefs", "attention"))
                .column(("spaces", "status"))
                .from("spaces")
                .left_join(
                    "space_settings",
                    Expr::col(("space_settings", "space_id")).equals(("spaces", "id")),
                )
                .left_join(
                    "space_prefs",
                    Expr::col(("space_prefs", "space_id")).equals(("spaces", "id")),
                )
                .and_where(within("spaces", ws, id)),
            |r| {
                Ok(SpaceSource {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                    connection: r.get(2)?,
                    members: r.get(3)?,
                    external: r.get(4)?,
                    role: r.get::<Option<String>>(5)?.unwrap_or_default(),
                    muted: r.get::<Option<bool>>(6)?.unwrap_or(false),
                    attention: r
                        .get::<Option<String>>(7)?
                        .unwrap_or_else(|| "realtime".into()),
                    active: r.get::<String>(8)? == "active",
                })
            },
        )
    }

    pub fn messages(
        &mut self,
        ws: &str,
        space: &str,
        selection: MessageSelection<'_>,
    ) -> Result<Vec<ChannelMessage>> {
        let Some(mut q) =
            source_messages("chan_messages", "space_id", "created", ws, space, selection)
        else {
            return Ok(Vec::new());
        };
        let reply = Query::select()
            .column(("q", "id"))
            .from_as("chan_messages", "q")
            .and_where(Expr::col(("q", "space_id")).equals(("m", "space_id")))
            .and_where(Expr::col(("q", "external_id")).equals(("m", "reply_to")))
            .limit(1)
            .take();
        self.0.all(
            q.columns(
                [
                    "id",
                    "external_id",
                    "created",
                    "sender_party_id",
                    "sender_account_id",
                    "direction",
                    "text",
                    "attachments",
                    "mentions",
                    "thread_key",
                    "on_behalf",
                ]
                .map(|field| ("m", field)),
            )
            .expr(Expr::SubQuery(None, Box::new(reply.into()))),
            |r| {
                Ok(ChannelMessage {
                    id: r.get(0)?,
                    ext: r.get(1)?,
                    at: r.get(2)?,
                    party: r.get(3)?,
                    account: r.get(4)?,
                    out: r.get::<String>(5)? == "out",
                    text: r.get::<Option<String>>(6)?.unwrap_or_default(),
                    atts: optional_json(r, 7)?,
                    mentions: optional_json(r, 8)?,
                    thread: r.get(9)?,
                    behalf: r.get(10)?,
                    reply: r.get(11)?,
                })
            },
        )
    }
}

impl<C: Read> MailSources<'_, C> {
    pub fn message_locations(
        &mut self,
        ws: &str,
        thread: &str,
        ids: &[String],
    ) -> Result<Vec<(String, i64)>> {
        message_locations(
            self.0,
            "mail_messages",
            "thread_id",
            "date",
            ws,
            thread,
            ids,
        )
    }

    pub fn excerpt(&mut self, ws: &str, thread: &str, id: &str) -> Result<Option<String>> {
        excerpt(
            self.0,
            "mail_messages",
            "thread_id",
            "snippet",
            ws,
            thread,
            id,
        )
    }

    pub fn thread(&mut self, ws: &str, id: &str) -> Result<Option<MailSource>> {
        self.0.one(
            Query::select()
                .columns([
                    "subject",
                    "connection_id",
                    "participants",
                    "state",
                    "visibility",
                    "created_by",
                ])
                .from("mail_threads")
                .and_where(within("mail_threads", ws, id)),
            |r| {
                Ok(MailSource {
                    subject: r.get(0)?,
                    connection: r.get(1)?,
                    members: optional_json::<Vec<Value>>(r, 2)?.len() as i64,
                    muted: r.get::<String>(3)? == "muted",
                    visibility: Visibility::from_db(&r.get::<String>(4)?),
                    author: r.get(5)?,
                })
            },
        )
    }

    pub fn messages(
        &mut self,
        ws: &str,
        thread: &str,
        selection: MessageSelection<'_>,
    ) -> Result<Vec<MailMessage>> {
        let Some(mut q) =
            source_messages("mail_messages", "thread_id", "date", ws, thread, selection)
        else {
            return Ok(Vec::new());
        };
        let reply = Query::select()
            .column(("q", "id"))
            .from_as("mail_messages", "q")
            .and_where(Expr::col(("q", "workspace_id")).equals(("m", "workspace_id")))
            .and_where(Expr::col(("q", "thread_id")).equals(("m", "thread_id")))
            .and_where(Expr::col(("q", "message_id")).equals(("m", "in_reply_to")))
            .limit(1)
            .take();
        let text = Func::coalesce([
            Func::cust("nullif")
                .arg(Expr::col(("m", "body_new")))
                .arg(Expr::Constant("".into()))
                .into(),
            Func::cust("nullif")
                .arg(Expr::col(("m", "body_text")))
                .arg(Expr::Constant("".into()))
                .into(),
            Expr::col(("m", "snippet")),
        ]);
        self.0.all(
            q.columns(
                [
                    "id",
                    "message_id",
                    "date",
                    "from_party_id",
                    "from_account_id",
                ]
                .map(|field| ("m", field)),
            )
            .expr(text)
            .columns(
                [
                    "to_addrs",
                    "cc_addrs",
                    "from_name",
                    "from_addr",
                    "subject",
                    "bulk",
                    "refs",
                ]
                .map(|field| ("m", field)),
            )
            .expr(Expr::SubQuery(None, Box::new(reply.into()))),
            |r| {
                Ok(MailMessage {
                    id: r.get(0)?,
                    mid: r.get(1)?,
                    at: r.get(2)?,
                    party: r.get(3)?,
                    account: r.get(4)?,
                    text: r.get::<Option<String>>(5)?.unwrap_or_default(),
                    to: optional_json(r, 6)?,
                    cc: optional_json(r, 7)?,
                    name: r.get(8)?,
                    addr: r.get(9)?,
                    subject: r.get(10)?,
                    bulk: r.get(11)?,
                    refs: optional_json(r, 12)?,
                    reply: r.get(13)?,
                })
            },
        )
    }

    pub fn attachments(&mut self, ids: &[String]) -> Result<HashMap<String, Vec<Attachment>>> {
        let mut out = HashMap::<String, Vec<Attachment>>::new();
        for ids in ids.chunks(500) {
            let rows = self.0.all(
                Query::select()
                    .columns(["message_id", "id", "name", "mime", "size"])
                    .from("mail_attachments")
                    .and_where(Expr::col("message_id").is_in(ids.iter().map(String::as_str)))
                    .and_where(Expr::col("inline").eq(false))
                    .order_by("part", Order::Asc),
                |r| {
                    Ok((
                        r.get(0)?,
                        Attachment {
                            id: r.get(1)?,
                            name: r.get(2)?,
                            mime: r.get(3)?,
                            size: r.get(4)?,
                        },
                    ))
                },
            )?;
            for (message, attachment) in rows {
                out.entry(message).or_default().push(attachment);
            }
        }
        Ok(out)
    }
}

impl<C: Read> Connections<'_, C> {
    pub fn adapters(&mut self, ws: &str, ids: &[String]) -> Result<HashMap<String, String>> {
        let mut adapters = HashMap::new();
        for chunk in ids.chunks(500) {
            adapters.extend(
                self.0.all(
                    Query::select()
                        .columns(["id", "adapter"])
                        .from("connections")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("id").is_in(chunk.iter().map(String::as_str))),
                    |r| Ok((r.get::<String>(0)?, r.get::<String>(1)?)),
                )?,
            );
        }
        Ok(adapters)
    }

    pub fn info(&mut self, ws: &str, id: &str) -> Result<Option<ConnectionInfo>> {
        self.0.one(
            connection_info()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            decode_connection,
        )
    }

    pub fn for_adapter(&mut self, ws: &str, adapter: &str) -> Result<Vec<ConnectionInfo>> {
        self.0.all(
            connection_info()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("adapter").eq(adapter))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            decode_connection,
        )
    }

    pub fn adapter_in(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("adapter")
                .from("connections")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("workspace_id").eq(ws)),
            |row| row.get(0),
        )
    }

    pub fn poll_connections(&mut self) -> Result<Vec<PollConnection>> {
        self.0.all(
            Query::select()
                .from("connections")
                .columns(["id", "workspace_id", "adapter", "caps"])
                .and_where(Expr::col("status").is_in(["ok", "error"]))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |row| {
                Ok(PollConnection {
                    id: row.get(0)?,
                    ws: row.get(1)?,
                    adapter: row.get(2)?,
                    caps: serde_json::from_str(&row.get::<String>(3)?).unwrap_or_default(),
                })
            },
        )
    }

    pub fn display_names<'a>(
        &mut self,
        ws: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, String>> {
        use sea_query::JoinType;
        let ids: Vec<_> = ids.into_iter().collect();
        let mut out = HashMap::new();
        for ids in ids.chunks(500) {
            let rows = self.0.all(
                Query::select()
                    .columns([("c", "id"), ("a", "handle_display"), ("a", "handle")])
                    .from_as("connections", "c")
                    .join_as(
                        JoinType::InnerJoin,
                        "accounts",
                        "a",
                        Expr::col(("a", "id")).equals(("c", "account_id")),
                    )
                    .and_where(Expr::col(("c", "workspace_id")).eq(ws))
                    .and_where(Expr::col(("c", "id")).is_in(ids.iter().copied())),
                |row| {
                    Ok((
                        row.get::<String>(0)?,
                        row.get::<Option<String>>(1)?,
                        row.get::<String>(2)?,
                    ))
                },
            )?;
            out.extend(rows.into_iter().map(|(id, display, handle)| {
                (
                    id,
                    display
                        .filter(|display| !display.is_empty())
                        .unwrap_or(handle),
                )
            }));
        }
        Ok(out)
    }

    pub fn adapter(&mut self, id: Option<&str>) -> Result<String> {
        let Some(id) = id else {
            return Ok(String::new());
        };
        Ok(self
            .0
            .one(
                Query::select()
                    .column("adapter")
                    .from("connections")
                    .and_where(Expr::col("id").eq(id)),
                |r| r.get(0),
            )?
            .unwrap_or_default())
    }
}

impl Connections<'_, crate::W<'_>> {
    pub fn mark_synced(&mut self, ws: &str, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("connections")
                .value("last_sync", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("workspace_id").eq(ws)),
        )?;
        Ok(())
    }
}
