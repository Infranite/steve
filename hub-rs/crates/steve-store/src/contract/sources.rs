use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, Iden, OnConflict, Query};
use serde::{Deserialize, Serialize};

/// How a folder's mail is received (`source_settings.scopes[].mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeMode {
    /// Stored and turned into events.
    Receive,
    /// Stored, no events.
    Record,
    /// Not synced.
    Ignore,
}

impl ScopeMode {
    /// The default for a new folder: the inbox `receive`, sent mail `record`, the rest `ignore`.
    pub fn default_for(role: &str) -> Self {
        match role {
            "inbox" => Self::Receive,
            "sent" => Self::Record,
            _ => Self::Ignore,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Record => "record",
            Self::Ignore => "ignore",
        }
    }
}

/// One entry of `source_settings.scopes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEntry {
    pub ext_id: String,
    pub name: String,
    pub mode: ScopeMode,
}

/// `(paused, scopes, cursor)` of a connection's source settings.
pub type SourceRow = (bool, Vec<ScopeEntry>, Option<String>);

#[derive(Iden, Clone, Copy)]
enum SourceSettings {
    Table,
    ConnectionId,
    WorkspaceId,
    Paused,
    Scopes,
    Cursor,
    Updated,
}

/// The source settings of a connection, if the row exists.
pub fn source(c: &mut impl Read, conn: &str) -> Result<Option<SourceRow>> {
    use SourceSettings::*;
    c.one(
        Query::select()
            .columns([Paused, Scopes, Cursor])
            .from(Table)
            .and_where(Expr::col(ConnectionId).eq(conn)),
        |r| {
            let scopes: Option<String> = r.get(1)?;
            Ok((
                r.get::<i64>(0)? != 0,
                scopes
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                r.get(2)?,
            ))
        },
    )
}

/// Writes the scope list (creating the source settings row with its defaults when missing).
pub fn set_scopes(
    tx: &mut W<'_>,
    ws: &str,
    conn: &str,
    scopes: &[ScopeEntry],
    now: i64,
) -> Result<()> {
    use SourceSettings::*;
    let s = serde_json::to_string(scopes).unwrap_or_else(|_| "[]".into());
    tx.exec(
        Query::insert()
            .into_table(Table)
            .columns([ConnectionId, WorkspaceId, Scopes, Updated])
            .values_panic([Expr::val(conn), Expr::val(ws), Expr::val(s), Expr::val(now)])
            .on_conflict(
                OnConflict::columns([ConnectionId])
                    .update_columns([Scopes, Updated])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn set_cursor(tx: &mut W<'_>, conn: &str, cursor: Option<&str>) -> Result<()> {
    use SourceSettings::*;
    tx.exec(
        Query::update()
            .table(Table)
            .value(Cursor, cursor)
            .and_where(Expr::col(ConnectionId).eq(conn)),
    )?;
    Ok(())
}

pub fn remove(w: &mut W<'_>, conn: &str) -> Result<()> {
    use SourceSettings::*;
    w.exec(
        Query::delete()
            .from_table(Table)
            .and_where(Expr::col(ConnectionId).eq(conn)),
    )?;
    Ok(())
}
