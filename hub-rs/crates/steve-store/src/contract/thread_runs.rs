//! The durable state of a main-thread answer or research run.

use sea_query::{Expr, ExprTrait, Query};

use crate::contract::Row;
use crate::{NewThreadRun, Read, Result, RunPatch, StoreError, ThreadRun, W, now, uid};

const COLUMNS: [&str; 19] = [
    "id",
    "workspace_id",
    "message_id",
    "reply_id",
    "prev_run",
    "user_id",
    "project_id",
    "model",
    "session",
    "harness",
    "status",
    "token_hash",
    "tools",
    "turns",
    "first_text_ms",
    "error",
    "created",
    "started",
    "ended",
];

fn decode(r: &Row<'_>) -> Result<ThreadRun> {
    Ok(ThreadRun {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        message_id: r.get(2)?,
        reply_id: r.get(3)?,
        prev_run: r.get(4)?,
        user_id: r.get(5)?,
        project_id: r.get(6)?,
        model: r.get(7)?,
        session: r.get(8)?,
        harness: r.get(9)?,
        status: r.get(10)?,
        token_hash: r.get(11)?,
        tools: r.get(12)?,
        turns: r.get(13)?,
        first_text_ms: r.get(14)?,
        error: r.get(15)?,
        created: r.get(16)?,
        started: r.get(17)?,
        ended: r.get(18)?,
    })
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<ThreadRun>> {
    r.one(
        Query::select()
            .columns(COLUMNS)
            .from("thread_runs")
            .and_where(Expr::col("id").eq(id)),
        decode,
    )
}

pub fn add(w: &mut W<'_>, n: &NewThreadRun) -> Result<ThreadRun> {
    let id = uid("tr");
    w.exec(
        Query::insert()
            .into_table("thread_runs")
            .columns([
                "id",
                "workspace_id",
                "message_id",
                "reply_id",
                "prev_run",
                "user_id",
                "project_id",
                "status",
                "created",
            ])
            .values_panic([
                id.as_str().into(),
                n.workspace_id.as_str().into(),
                n.message_id.as_str().into(),
                n.reply_id.as_deref().into(),
                n.prev_run.as_deref().into(),
                n.user_id.as_deref().into(),
                n.project_id.as_deref().into(),
                "queued".into(),
                now().into(),
            ]),
    )?;
    get(w, &id)?.ok_or_else(|| StoreError::Storage("inserted thread run missing".into()))
}

pub fn patch(w: &mut W<'_>, id: &str, p: &RunPatch) -> Result<Option<ThreadRun>> {
    let values = [
        ("status", p.status.as_deref().map(Expr::val)),
        ("reply_id", p.reply_id.as_deref().map(Expr::val)),
        ("model", p.model.as_deref().map(Expr::val)),
        ("session", p.session.as_deref().map(Expr::val)),
        ("harness", p.harness.as_deref().map(Expr::val)),
        (
            "token_hash",
            p.token_hash
                .as_ref()
                .map(|token| Expr::val(token.as_deref())),
        ),
        ("tools", p.tools.map(Expr::val)),
        ("turns", p.turns.map(Expr::val)),
        ("first_text_ms", p.first_text_ms.map(Expr::val)),
        ("error", p.error.as_deref().map(Expr::val)),
        ("started", p.started.map(Expr::val)),
        ("ended", p.ended.map(Expr::val)),
    ];
    if values.iter().any(|(_, value)| value.is_some()) {
        w.exec(
            Query::update()
                .table("thread_runs")
                .values(
                    values
                        .into_iter()
                        .filter_map(|(key, value)| value.map(|value| (key, value))),
                )
                .and_where(Expr::col("id").eq(id)),
        )?;
    }
    get(w, id)
}
