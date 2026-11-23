use sea_query::{Expr, ExprTrait, Iden, OnConflict, Query};
use serde::Serialize;

use crate::{R, Read, Result, StoreError, W};
use steve_store_sqlite::digest;

#[derive(Iden, Clone, Copy)]
enum DigestCursors {
    Table,
    WorkspaceId,
    Conv,
    At,
    MsgId,
    Windows,
    Day,
    Spent,
    Updated,
    Since,
}
use DigestCursors as C;

#[derive(Iden, Clone, Copy)]
enum DigestReceipts {
    Table,
    WorkspaceId,
    Conv,
    MessageId,
    Seq,
    Created,
}
use DigestReceipts as D;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Cursor {
    pub at: i64,
    pub msg_id: String,
    pub windows: i64,
    pub day: String,
    pub spent: i64,
}

pub struct Windows<'a, C>(&'a mut C);

impl R<'_> {
    pub fn digest_windows(&mut self) -> Windows<'_, Self> {
        Windows(self)
    }
}

impl W<'_> {
    pub fn digest_windows(&mut self) -> Windows<'_, Self> {
        Windows(self)
    }
}

impl<T: Read> Windows<'_, T> {
    pub fn new(handle: &mut T) -> Windows<'_, T> {
        Windows(handle)
    }

    pub fn pending_ids(&mut self, ws: &str, conv: &str, limit: usize) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("message_id")
                .from("digest_pending")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("conv").eq(conv))
                .order_by("at", sea_query::Order::Asc)
                .order_by("message_id", sea_query::Order::Asc)
                .limit(limit.min(i64::MAX as usize) as u64),
            |row| row.get(0),
        )
    }

    pub fn pending_conversations(&mut self, ws: &str) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["conv", "newest"])
                .from("digest_pending_convs")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// The count and oldest/newest timestamps of a conversation's unconsumed messages.
    pub fn pending_stats(&mut self, ws: &str, conv: &str) -> Result<(i64, i64, i64)> {
        Ok(self
            .0
            .one(
                Query::select()
                    .columns(["messages", "oldest", "newest"])
                    .from("digest_pending_convs")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("conv").eq(conv)),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
            .unwrap_or_default())
    }

    pub fn cursor(&mut self, ws: &str, conv: &str) -> Result<Option<Cursor>> {
        self.0.one(
            Query::select()
                .columns([C::At, C::MsgId, C::Windows, C::Day, C::Spent])
                .from(C::Table)
                .and_where(Expr::col(C::WorkspaceId).eq(ws))
                .and_where(Expr::col(C::Conv).eq(conv)),
            |row| {
                Ok(Cursor {
                    at: row.get(0)?,
                    msg_id: row.get(1)?,
                    windows: row.get(2)?,
                    day: row.get(3)?,
                    spent: row.get(4)?,
                })
            },
        )
    }
}

impl Windows<'_, W<'_>> {
    /// Preserves the first cutoff so the pending projection can be audited later.
    pub fn save_cursor(
        &mut self,
        ws: &str,
        conv: &str,
        cursor: &Cursor,
        now: i64,
        initial_since: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(C::Table)
                .columns([
                    C::WorkspaceId,
                    C::Conv,
                    C::At,
                    C::MsgId,
                    C::Windows,
                    C::Day,
                    C::Spent,
                    C::Updated,
                    C::Since,
                ])
                .values_panic([
                    ws.into(),
                    conv.into(),
                    cursor.at.into(),
                    cursor.msg_id.as_str().into(),
                    cursor.windows.into(),
                    cursor.day.as_str().into(),
                    cursor.spent.into(),
                    now.into(),
                    initial_since.into(),
                ])
                .on_conflict(
                    OnConflict::columns([C::WorkspaceId, C::Conv])
                        .update_columns([C::At, C::MsgId, C::Windows, C::Day, C::Spent, C::Updated])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    /// Excludes historical messages on the first cut, without acknowledging them.
    pub fn exclude_before(&mut self, ws: &str, conv: &str, floor: i64) -> Result<()> {
        digest::remove_before(&self.0.legacy(), ws, conv, floor).map_err(StoreError::from)
    }

    /// Records the receipts and updates the pending projection once for this conversation.
    pub fn acknowledge(
        &mut self,
        ws: &str,
        conv: &str,
        ids: impl IntoIterator<Item = impl AsRef<str>>,
        seq: i64,
        now: i64,
    ) -> Result<()> {
        let ids = ids.into_iter().collect::<Vec<_>>();
        for id in &ids {
            self.0.exec(
                Query::insert()
                    .into_table(D::Table)
                    .columns([D::WorkspaceId, D::Conv, D::MessageId, D::Seq, D::Created])
                    .values_panic([
                        ws.into(),
                        conv.into(),
                        id.as_ref().into(),
                        seq.into(),
                        now.into(),
                    ]),
            )?;
        }
        digest::dequeue_on_receipt(&self.0.legacy(), ws, conv, ids).map_err(StoreError::from)
    }
}
