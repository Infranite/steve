//! Durable reading entries, independent from delivery receipts and chat.

use crate::{Result, StoreError, W, uid};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Platform-neutral content. Rich cards/HTML belong to the connector adapter; all adapters can
/// receive the lossless Markdown fallback, including actions as labelled links. No model rewrites
/// or content masking happen here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub fields: Vec<(String, String)>,
    #[serde(default)]
    pub actions: Vec<(String, String)>,
    #[serde(default)]
    pub references: Vec<(String, String)>,
}

impl Content {
    pub fn markdown(&self) -> String {
        let mut parts = Vec::new();
        if !self.title.is_empty() {
            parts.push(format!("# {}", self.title));
        }
        if !self.markdown.is_empty() {
            parts.push(self.markdown.clone());
        }
        parts.extend(self.fields.iter().map(|(k, v)| format!("{k}: {v}")));
        parts.extend(
            self.actions
                .iter()
                .chain(&self.references)
                .map(|(label, url)| format!("{label}: {url}")),
        );
        parts.join("\n\n")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub kind: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub unread: bool,
}
impl Filter {
    pub fn validate(&self) -> Result<()> {
        if self.kind.as_deref().is_some_and(|x| {
            !matches!(
                x,
                "report" | "completion" | "starred" | "reminder" | "briefing"
            )
        }) || self
            .source
            .as_deref()
            .is_some_and(|x| !matches!(x, "flow" | "notification"))
        {
            return Err(StoreError::Conflict("Invalid feed filter".into()));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub r#type: String,
    pub id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub event_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub seq: i64,
    pub kind: String,
    pub title: String,
    pub excerpt: String,
    pub body: Option<Content>,
    pub published_at: i64,
    pub read_at: Option<i64>,
    pub purged: bool,
    pub source: Source,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next_cursor: Option<String>,
    pub snapshot: String,
    pub unread_count: i64,
}
pub(crate) fn member_on(c: &Connection, ws: &str, user: &str) -> Result<()> {
    if !c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=?)",
        params![ws, user],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(StoreError::NotFound("workspace membership"));
    }
    Ok(())
}
const SELECT: &str = "SELECT e.id,e.seq,e.kind,e.content,e.published_at,r.read_at,e.source_type,e.source_id,e.run_id,e.node_id,CASE WHEN e.source_type='notification' THEN json_extract(e.source_key,'$[1]') END,n.body,n.state,e.item_id FROM feed_entries e LEFT JOIN personal_notifications n ON n.id=e.notification_id LEFT JOIN feed_reads r ON r.workspace_id=e.workspace_id AND r.user_id=?2 AND r.entry_id=e.id";
const SCOPE: &str = "e.workspace_id=?1 AND (e.audience='workspace' OR e.recipient_id=?2) AND EXISTS(SELECT 1 FROM workspace_members m WHERE m.workspace_id=?1 AND m.user_id=?2)";
fn entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let raw: Option<String> = r.get(3)?;
    let ntf: Option<String> = r.get(11)?;
    let state: Option<String> = r.get(12)?;
    let item: Option<String> = r.get(13)?;
    let content = if let Some(raw) = raw {
        Some(serde_json::from_str::<Content>(&raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?)
    } else if state.as_deref() == Some("purged") {
        None
    } else {
        ntf.map(|markdown| Content {
            title: markdown.lines().next().unwrap_or("").to_string(),
            markdown,
            // This is only a navigation reference: the item API still checks its own visibility.
            references: item
                .into_iter()
                .map(|id| {
                    let path: String = id
                        .bytes()
                        .map(|b| {
                            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                                (b as char).to_string()
                            } else {
                                format!("%{b:02X}")
                            }
                        })
                        .collect();
                    (id, format!("#/items/{path}"))
                })
                .collect(),
            ..Default::default()
        })
    };
    let title = content
        .as_ref()
        .map(|c| c.title.clone())
        .unwrap_or_default();
    let excerpt = content
        .as_ref()
        .map(|c| c.markdown.chars().take(240).collect())
        .unwrap_or_default();
    Ok(Entry {
        id: r.get(0)?,
        seq: r.get(1)?,
        kind: r.get(2)?,
        purged: content.is_none(),
        body: content,
        title,
        excerpt,
        published_at: r.get(4)?,
        read_at: r.get(5)?,
        source: Source {
            r#type: r.get(6)?,
            id: r.get(7)?,
            run_id: r.get(8)?,
            node_id: r.get(9)?,
            event_id: r.get(10)?,
        },
    })
}
pub(crate) fn detail_on(c: &Connection, ws: &str, user: &str, id: &str) -> Result<Entry> {
    member_on(c, ws, user)?;
    c.query_row(
        &format!("{SELECT} WHERE {SCOPE} AND e.id=?3"),
        params![ws, user, id],
        entry,
    )
    .optional()?
    .ok_or(StoreError::NotFound("feed entry"))
}
fn window(
    tx: &mut W<'_>,
    ws: &str,
    user: &str,
    f: &Filter,
    upper: i64,
    before: Option<i64>,
    now: i64,
) -> Result<String> {
    // Polling an unchanged page reuses its capability rather than accumulating a token per read.
    if let Some(token)=tx.conn.query_row("SELECT token FROM feed_windows WHERE workspace_id=? AND user_id=? AND kind IS ? AND source IS ? AND unread=? AND upper_seq=? AND before_seq IS ? AND expires>? LIMIT 1",params![ws,user,f.kind,f.source,f.unread,upper,before,now+3_600_000],|r|r.get::<_,String>(0)).optional()? { return Ok(token); }
    let token = format!("{}{}", uid("fw"), uid(""));
    tx.conn.execute("INSERT INTO feed_windows(token,workspace_id,user_id,kind,source,unread,upper_seq,before_seq,expires) VALUES(?,?,?,?,?,?,?,?,?)",params![token,ws,user,f.kind,f.source,f.unread,upper,before,now+86_400_000])?;
    Ok(token)
}
fn resolve(
    c: &Connection,
    ws: &str,
    user: &str,
    f: &Filter,
    token: &str,
    cursor: bool,
    now: i64,
) -> Result<(i64, Option<i64>)> {
    let row:Option<(Option<String>,Option<String>,bool,i64,Option<i64>)> = c.query_row("SELECT kind,source,unread,upper_seq,before_seq FROM feed_windows WHERE token=? AND workspace_id=? AND user_id=? AND expires>?",params![token,ws,user,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    match row {
        Some((kind, source, unread, upper, before))
            if kind == f.kind
                && source == f.source
                && unread == f.unread
                && before.is_some() == cursor =>
        {
            Ok((upper, before))
        }
        _ => Err(StoreError::Conflict(
            "Invalid or expired feed cursor/snapshot for this scope and filter".into(),
        )),
    }
}
const FILTER: &str = "(?3 IS NULL OR e.kind=?3) AND (?4 IS NULL OR e.source_type=?4)";
pub fn list(
    tx: &mut W<'_>,
    ws: &str,
    user: &str,
    f: &Filter,
    cursor: Option<&str>,
    limit: usize,
    now: i64,
) -> Result<Page> {
    member_on(tx.conn, ws, user)?;
    f.validate()?;
    if !(1..=100).contains(&limit) {
        return Err(StoreError::Conflict("Feed limit must be 1..100".into()));
    }
    let (upper, before) = if let Some(token) = cursor {
        resolve(tx.conn, ws, user, f, token, true, now)?
    } else {
        (
            tx.conn.query_row(
                &format!("SELECT coalesce(max(e.seq),0) FROM feed_entries e WHERE {SCOPE}"),
                params![ws, user],
                |r| r.get(0),
            )?,
            None,
        )
    };
    let mut st=tx.conn.prepare(&format!("{SELECT} WHERE {SCOPE} AND {FILTER} AND (?5=0 OR r.read_at IS NULL) AND e.seq<=?6 AND (?7 IS NULL OR e.seq<?7) ORDER BY e.seq DESC LIMIT ?8"))?;
    let mut entries = st
        .query_map(
            params![
                ws,
                user,
                f.kind,
                f.source,
                f.unread,
                upper,
                before,
                (limit + 1) as i64
            ],
            entry,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = entries.len() > limit;
    entries.truncate(limit);
    let next_cursor = if more {
        Some(window(
            tx,
            ws,
            user,
            f,
            upper,
            entries.last().map(|e| e.seq),
            now,
        )?)
    } else {
        None
    };
    for e in &mut entries {
        e.body = None;
    }
    let unread_count=tx.conn.query_row(&format!("SELECT count(*) FROM feed_entries e LEFT JOIN feed_reads r ON r.workspace_id=e.workspace_id AND r.user_id=?2 AND r.entry_id=e.id WHERE {SCOPE} AND {FILTER} AND r.read_at IS NULL"),params![ws,user,f.kind,f.source],|r|r.get(0))?;
    Ok(Page {
        entries,
        next_cursor,
        snapshot: window(tx, ws, user, f, upper, None, now)?,
        unread_count,
    })
}
pub fn read_ids(tx: &mut W<'_>, ws: &str, user: &str, ids: &[String], now: i64) -> Result<usize> {
    member_on(tx.conn, ws, user)?;
    if ids.is_empty() || ids.len() > 100 {
        return Err(StoreError::Conflict("Select 1..100 feed entries".into()));
    }
    // Validate the entire set before writing, including when the caller nests this transaction.
    for id in ids {
        detail_on(tx.conn, ws, user, id)?;
    }
    let mut changed = 0;
    for id in ids {
        changed+=tx.conn.execute("INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![ws,user,id,now])?;
    }
    Ok(changed)
}
pub fn read_snapshot(
    tx: &mut W<'_>,
    ws: &str,
    user: &str,
    f: &Filter,
    token: &str,
    now: i64,
) -> Result<usize> {
    member_on(tx.conn, ws, user)?;
    f.validate()?;
    let (upper, _) = resolve(tx.conn, ws, user, f, token, false, now)?;
    Ok(tx.conn.execute(&format!("INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at) SELECT ?1,?2,e.id,?7 FROM feed_entries e LEFT JOIN feed_reads r ON r.workspace_id=e.workspace_id AND r.user_id=?2 AND r.entry_id=e.id WHERE {SCOPE} AND {FILTER} AND (?5=0 OR r.read_at IS NULL) AND e.seq<=?6 ON CONFLICT DO NOTHING"),params![ws,user,f.kind,f.source,f.unread,upper,now])?)
}
/// Stable business identity includes the graph node, not only the run or content hash.
fn report_key(run: &str, node: &str) -> String {
    serde_json::to_string(&(run, node)).expect("string tuple")
}
impl<C: crate::Read> crate::FeedReads<'_, C> {
    pub fn report_result(&mut self, ws: &str, run: &str, node: &str) -> Result<Option<Value>> {
        use sea_query::{Expr, ExprTrait, Query};
        self.0.one(
            Query::select()
                .column("result")
                .from("feed_entries")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("source_type").eq("flow"))
                .and_where(Expr::col("source_key").eq(report_key(run, node))),
            |row| Ok(row.get::<crate::contract::Json<Value>>(0)?.0),
        )
    }
}

/// A report's identity and payload, persisted when its run publishes.
pub struct RunReport<'a> {
    pub ws: &'a str,
    pub flow: &'a str,
    pub run: &'a str,
    pub node: &'a str,
    pub content: &'a Content,
    pub result: &'a Value,
    pub now: i64,
}

impl crate::W<'_> {
    /// The caller acknowledges digest notes only when this inserts the report.
    pub fn publish_report(&mut self, report: &RunReport<'_>) -> Result<(Value, bool)> {
        use sea_query::Query;
        if let Some(existing) =
            crate::FeedReads(self).report_result(report.ws, report.run, report.node)?
        {
            return Ok((existing, false));
        }
        let id = uid("feed");
        let mut result = report.result.clone();
        result["feed_entry_id"] = Value::String(id.clone());
        self.exec(
            Query::insert()
                .into_table("feed_entries")
                .columns([
                    "id",
                    "workspace_id",
                    "audience",
                    "kind",
                    "source_type",
                    "source_key",
                    "source_id",
                    "run_id",
                    "node_id",
                    "content",
                    "result",
                    "published_at",
                ])
                .values_panic([
                    id.into(),
                    report.ws.into(),
                    "workspace".into(),
                    "report".into(),
                    "flow".into(),
                    report_key(report.run, report.node).into(),
                    report.flow.into(),
                    report.run.into(),
                    report.node.into(),
                    serde_json::to_string(report.content)?.into(),
                    result.to_string().into(),
                    report.now.into(),
                ]),
        )?;
        Ok((result, true))
    }
}

impl crate::FeedReads<'_, crate::W<'_>> {
    /// Groups the notification lookup and its feed read receipt in one write.
    pub fn mark_notification(&mut self, ws: &str, user: &str, id: &str, now: i64) -> Result<bool> {
        use crate::contract::Q;
        use sea_query::{Expr, ExprTrait, OnConflict, Query};
        let entry = self.0.one(
            Query::select()
                .column("id")
                .from("feed_entries")
                .and_where(Expr::col("notification_id").eq(id))
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("recipient_id").eq(user)),
            |row| row.get::<String>(0),
        )?;
        let Some(entry) = entry else { return Ok(false) };
        self.0.exec(
            Query::insert()
                .into_table("feed_reads")
                .columns(["workspace_id", "user_id", "entry_id", "read_at"])
                .values_panic([ws.into(), user.into(), entry.into(), now.into()])
                .on_conflict(OnConflict::new().do_nothing().to_owned()),
        )?;
        Ok(true)
    }
}

pub fn member(r: &mut impl crate::Read, ws: &str, user: &str) -> Result<()> {
    member_on(r.connection(), ws, user)
}

pub fn detail(r: &mut impl crate::Read, ws: &str, user: &str, id: &str) -> Result<Entry> {
    detail_on(r.connection(), ws, user, id)
}
