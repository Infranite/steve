use sea_query::{Expr, ExprTrait, Func, Iden, OnConflict, Order, Query};

use crate::{Read, Result, W};

pub struct FlowSources<'a, C>(pub &'a mut C);

#[derive(Iden)]
enum SourceColumn {
    ConnectionId,
    WorkspaceId,
    Paused,
    TriageBot,
    Types,
    Scopes,
    Cursor,
    Revision,
    Updated,
}
use SourceColumn::*;

pub struct SourceConfig<'a> {
    pub connection: &'a str,
    pub ws: &'a str,
    pub paused: bool,
    pub triage_bot: Option<&'a str>,
    pub types: &'a serde_json::Value,
    pub scopes: &'a serde_json::Value,
    pub cursor: Option<&'a str>,
    pub now: i64,
}

pub struct SourceConfigRow {
    pub paused: bool,
    pub triage_bot: Option<String>,
    pub types: serde_json::Value,
    pub scopes: serde_json::Value,
    pub cursor: Option<String>,
    pub revision: i64,
}

impl<C: Read> FlowSources<'_, C> {
    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<SourceConfigRow>> {
        self.0.one(
            Query::select()
                .from("source_settings")
                .columns([Paused, TriageBot, Types, Scopes, Cursor, Revision])
                .and_where(Expr::col(WorkspaceId).eq(ws))
                .and_where(Expr::col(ConnectionId).eq(id)),
            |r| {
                Ok(SourceConfigRow {
                    paused: r.get(0)?,
                    triage_bot: r.get(1)?,
                    types: r.get::<super::Json<_>>(2)?.0,
                    scopes: r.get::<super::Json<_>>(3)?.0,
                    cursor: r.get(4)?,
                    revision: r.get(5)?,
                })
            },
        )
    }

    pub fn revision(&mut self, ws: &str, ids: &[String]) -> Result<i64> {
        let mut total = 0;
        for batch in ids.chunks(500) {
            total += self
                .0
                .one(
                    Query::select()
                        .from("source_settings")
                        .expr(Func::sum(Expr::col(Revision)))
                        .and_where(Expr::col(WorkspaceId).eq(ws))
                        .and_where(Expr::col(ConnectionId).is_in(batch.iter().map(String::as_str))),
                    |r| r.get::<Option<i64>>(0),
                )?
                .flatten()
                .unwrap_or(0);
        }
        Ok(total)
    }

    pub fn cursor(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(Cursor)
                    .from("source_settings")
                    .and_where(Expr::col(WorkspaceId).eq(ws))
                    .and_where(Expr::col(ConnectionId).eq(id)),
                |row| row.get::<Option<String>>(0),
            )?
            .flatten())
    }

    pub fn triage(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(TriageBot)
                    .from("source_settings")
                    .and_where(Expr::col(WorkspaceId).eq(ws))
                    .and_where(Expr::col(ConnectionId).eq(id)),
                |row| row.get::<Option<String>>(0),
            )?
            .flatten())
    }

    pub fn assigned(&mut self, ws: &str, bot: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column(ConnectionId)
                .from("source_settings")
                .and_where(Expr::col(WorkspaceId).eq(ws))
                .and_where(Expr::col(TriageBot).eq(bot))
                .order_by(ConnectionId, Order::Asc),
            |row| row.get(0),
        )
    }
}

impl FlowSources<'_, W<'_>> {
    pub fn save(&mut self, config: &SourceConfig<'_>) -> Result<()> {
        let changed = self.0.exec(
            Query::insert()
                .into_table("source_settings")
                .columns([
                    ConnectionId,
                    WorkspaceId,
                    Paused,
                    TriageBot,
                    Types,
                    Scopes,
                    Cursor,
                    Revision,
                    Updated,
                ])
                .values_panic([
                    config.connection.into(),
                    config.ws.into(),
                    config.paused.into(),
                    config.triage_bot.into(),
                    config.types.to_string().into(),
                    config.scopes.to_string().into(),
                    config.cursor.into(),
                    1_i64.into(),
                    config.now.into(),
                ])
                .on_conflict(
                    OnConflict::column(ConnectionId)
                        .update_columns([Paused, TriageBot, Types, Scopes, Cursor, Updated])
                        .value(Revision, Expr::col(Revision).add(1_i64))
                        .action_and_where(Expr::col(WorkspaceId).eq(config.ws))
                        .to_owned(),
                ),
        )?;
        if changed == 0 {
            return Err(crate::StoreError::Rejected(
                "Source settings belong to another workspace".into(),
            ));
        }
        Ok(())
    }

    /// The worker's cursor is separate from the edit revision.
    pub fn save_cursor(&mut self, ws: &str, id: &str, cursor: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("source_settings")
                .columns([ConnectionId, WorkspaceId, Cursor, Updated])
                .values_panic([id.into(), ws.into(), cursor.into(), now.into()])
                .on_conflict(
                    OnConflict::column(ConnectionId)
                        .update_column(Cursor)
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn clear_triage(&mut self, ws: &str, id: &str, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("source_settings")
                .value(TriageBot, Option::<String>::None)
                .value(Revision, Expr::col(Revision).add(1_i64))
                .value(Updated, at)
                .and_where(Expr::col(WorkspaceId).eq(ws))
                .and_where(Expr::col(ConnectionId).eq(id)),
        )?;
        Ok(())
    }
}
