//! Evidence kept with a proposed outbound channel message.

use sea_query::{Expr, ExprTrait, JoinType, Query, SelectStatement};
use serde_json::{Value, json};

use crate::contract::{Json, Q};
use crate::{Read, Result, W, uid};

pub struct BasisRow {
    pub id: String,
    pub message_id: String,
    pub entries: Vec<Value>,
    pub rewrites: Vec<Value>,
    pub verdict: String,
    pub answer_model: Option<String>,
    pub check_model: Option<String>,
    pub created: i64,
}

fn by_message(message: &str) -> SelectStatement {
    Query::select()
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "reply_basis",
            "b",
            Expr::col(("b", "id")).eq(Expr::col(("m", "basis_id"))),
        )
        .and_where(Expr::col(("m", "id")).eq(message))
        .take()
}

pub fn verdict(r: &mut impl Read, message: &str) -> Result<Option<String>> {
    r.one(by_message(message).column(("b", "verdict")), |row| {
        row.get(0)
    })
}

pub fn of_message(
    r: &mut impl Read,
    message: &str,
    workspace: Option<&str>,
) -> Result<Option<BasisRow>> {
    let mut query = by_message(message);
    query.columns([
        ("b", "id"),
        ("b", "message_id"),
        ("b", "entries"),
        ("b", "rewrites"),
        ("b", "verdict"),
        ("b", "answer_model"),
        ("b", "check_model"),
        ("b", "created"),
    ]);
    if let Some(ws) = workspace {
        query.and_where(Expr::col(("m", "workspace_id")).eq(ws));
    }
    r.one(&query, |row| {
        Ok(BasisRow {
            id: row.get(0)?,
            message_id: row.get(1)?,
            entries: row.get::<Json<_>>(2)?.0,
            rewrites: row.get::<Json<_>>(3)?.0,
            verdict: row.get(4)?,
            answer_model: row.get(5)?,
            check_model: row.get(6)?,
            created: row.get(7)?,
        })
    })
}

pub struct Basis<'a> {
    pub entries: Vec<Value>,
    pub rewrites: Vec<Value>,
    pub verdict: &'a str,
    pub check_model: Option<&'a str>,
}

pub fn insert(
    w: &mut W<'_>,
    ws: &str,
    message: &str,
    answer_model: Option<&str>,
    basis: Basis<'_>,
    at: i64,
) -> Result<String> {
    let id = uid("bs");
    w.exec(
        Query::insert()
            .into_table("reply_basis")
            .columns([
                "id",
                "workspace_id",
                "message_id",
                "entries",
                "rewrites",
                "verdict",
                "answer_model",
                "check_model",
                "created",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                message.into(),
                serde_json::to_string(&basis.entries)?.into(),
                serde_json::to_string(&basis.rewrites)?.into(),
                basis.verdict.into(),
                answer_model.into(),
                basis.check_model.into(),
                at.into(),
            ]),
    )?;
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("basis_id", id.as_str())
            .and_where(Expr::col("id").eq(message)),
    )?;
    Ok(id)
}

pub fn settle_check(w: &mut W<'_>, id: &str, basis: Basis<'_>) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("reply_basis")
            .values([
                ("verdict", basis.verdict.into()),
                ("entries", serde_json::to_string(&basis.entries)?.into()),
                ("rewrites", serde_json::to_string(&basis.rewrites)?.into()),
                ("check_model", basis.check_model.into()),
            ])
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("verdict").eq("checking")),
    )? > 0)
}

pub fn stop_check(w: &mut W<'_>, message: &str, why: &str) -> Result<bool> {
    let checking = w.one(
        Query::select()
            .columns([("b", "id"), ("b", "entries")])
            .from_as("chan_messages", "m")
            .join_as(
                JoinType::InnerJoin,
                "reply_basis",
                "b",
                Expr::col(("b", "id")).eq(Expr::col(("m", "basis_id"))),
            )
            .and_where(Expr::col(("m", "id")).eq(message))
            .and_where(Expr::col(("b", "verdict")).eq("checking")),
        |row| Ok((row.get::<String>(0)?, row.get::<Json<Vec<Value>>>(1)?.0)),
    )?;
    let Some((id, mut entries)) = checking else {
        return Ok(false);
    };
    entries.push(json!({"source_type": "span", "text": "", "reason": why}));
    w.exec(
        Query::update()
            .table("reply_basis")
            .value("verdict", "unchecked")
            .value("entries", serde_json::to_string(&entries)?)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(true)
}
