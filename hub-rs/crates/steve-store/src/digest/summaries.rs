use sea_query::{Expr, ExprTrait, Iden, Order, Query};
use serde::Serialize;
use serde_json::Value;

use crate::{R, Read, Result, W};

#[derive(Iden, Clone, Copy)]
enum ConvSummaries {
    Table,
    WorkspaceId,
    Conv,
    Version,
    Text,
    Rules,
    PacketId,
    Writer,
    Created,
}
use ConvSummaries as S;

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub version: i64,
    pub text: String,
    pub rules: Value,
    pub packet_id: Option<String>,
    pub writer: String,
    pub created: i64,
}

pub struct NewSummary<'a> {
    pub text: Option<&'a str>,
    pub rules: Option<&'a Value>,
    pub packet: Option<&'a str>,
    pub writer: &'a str,
    pub now: i64,
}

pub struct Summaries<'a, C>(&'a mut C);

impl R<'_> {
    pub fn summaries(&mut self) -> Summaries<'_, Self> {
        Summaries(self)
    }
}

impl W<'_> {
    pub fn summaries(&mut self) -> Summaries<'_, Self> {
        Summaries(self)
    }
}

fn conversation(ws: &str, conv: &str) -> Expr {
    Expr::col(S::WorkspaceId)
        .eq(ws)
        .and(Expr::col(S::Conv).eq(conv))
}

impl<C: Read> Summaries<'_, C> {
    pub fn latest(&mut self, ws: &str, conv: &str) -> Result<Option<Summary>> {
        self.0.one(
            Query::select()
                .columns([
                    S::Version,
                    S::Text,
                    S::Rules,
                    S::PacketId,
                    S::Writer,
                    S::Created,
                ])
                .from(S::Table)
                .and_where(conversation(ws, conv))
                .order_by(S::Version, Order::Desc)
                .limit(1),
            |row| {
                let rules: String = row.get(2)?;
                Ok(Summary {
                    version: row.get(0)?,
                    text: row.get(1)?,
                    rules: serde_json::from_str(&rules).unwrap_or(Value::Null),
                    packet_id: row.get(3)?,
                    writer: row.get(4)?,
                    created: row.get(5)?,
                })
            },
        )
    }
}

impl Summaries<'_, W<'_>> {
    /// Writes one version, keeping each part that the caller did not replace.
    pub fn append(&mut self, ws: &str, conv: &str, new: NewSummary<'_>) -> Result<i64> {
        let old = self.latest(ws, conv)?;
        let version = old.as_ref().map_or(1, |old| old.version + 1);
        let text = new
            .text
            .or_else(|| old.as_ref().map(|old| old.text.as_str()))
            .unwrap_or_default();
        let rules = new
            .rules
            .or_else(|| old.as_ref().map(|old| &old.rules))
            .filter(|rules| !rules.is_null())
            .map(Value::to_string)
            .unwrap_or_default();
        self.0.exec(
            Query::insert()
                .into_table(S::Table)
                .columns([
                    S::WorkspaceId,
                    S::Conv,
                    S::Version,
                    S::Text,
                    S::Rules,
                    S::PacketId,
                    S::Writer,
                    S::Created,
                ])
                .values_panic([
                    ws.into(),
                    conv.into(),
                    version.into(),
                    text.into(),
                    rules.into(),
                    new.packet.into(),
                    new.writer.into(),
                    new.now.into(),
                ]),
        )?;
        super::emit(self.0, ws, conv);
        Ok(version)
    }

    /// Removes a version only while it is still the newest.
    pub fn remove_latest(&mut self, ws: &str, conv: &str, version: i64) -> Result<bool> {
        if self.latest(ws, conv)?.map(|row| row.version) != Some(version) {
            return Ok(false);
        }
        self.0.exec(
            Query::delete()
                .from_table(S::Table)
                .and_where(conversation(ws, conv))
                .and_where(Expr::col(S::Version).eq(version)),
        )?;
        super::emit(self.0, ws, conv);
        Ok(true)
    }
}
