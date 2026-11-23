//! Realtime output evidence recorded with a Node stream's apply watermark.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Query};
use steve_node_api::{AttemptId, Role, SessionId};

use crate::{Read, Result, W};

use super::Q;

const ATTEMPTS: &str = "realtime_output_attempts";
const SAMPLES: &str = "realtime_output_samples";
const BLOCKS: &str = "realtime_output_blocks";

#[derive(Clone, Copy)]
pub struct Position<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
    pub sequence: i64,
    pub event: &'a str,
}

#[derive(Clone, Copy)]
pub enum SampleKind {
    Activity,
    Text,
    Gap,
    End,
}

impl SampleKind {
    fn code(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Text => "text",
            Self::Gap => "gap",
            Self::End => "end",
        }
    }
}

pub struct Sample<'a> {
    pub position: Position<'a>,
    pub attempt: &'a AttemptId,
    pub at: i64,
    pub source_at: i64,
    pub received_at: i64,
    pub kind: SampleKind,
    pub units: i64,
}

pub struct Block<'a> {
    pub attempt: &'a AttemptId,
    pub message: &'a str,
    pub block: &'a str,
}

impl Block<'_> {
    fn matches(&self) -> Expr {
        Expr::col("attempt_id")
            .eq(self.attempt.as_str())
            .and(Expr::col("message").eq(self.message))
            .and(Expr::col("block").eq(self.block))
    }
}

pub struct Output<'a, C>(pub &'a mut C);

impl<C: Read> Output<'_, C> {
    pub fn last_sample_at(&mut self, attempt: &AttemptId) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("at")))
                    .from(SAMPLES)
                    .and_where(Expr::col("attempt_id").eq(attempt.as_str())),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }

    fn seen(&mut self, p: Position<'_>) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("event_id")
                    .from(SAMPLES)
                    .and_where(Expr::col("node_id").eq(p.node))
                    .and_where(
                        Expr::col("event_id").eq(p.event).or(Expr::col("stream")
                            .eq(p.stream)
                            .and(Expr::col("epoch").eq(p.epoch))
                            .and(Expr::col("seq").eq(p.sequence))),
                    )
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn has_text(&mut self, attempt: &AttemptId) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("attempt_id")
                    .from(SAMPLES)
                    .and_where(Expr::col("attempt_id").eq(attempt.as_str()))
                    .and_where(Expr::col("units").gt(0_i64))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn block_checkpoint(&mut self, block: &Block<'_>) -> Result<Option<(Role, i64)>> {
        self.0.one(
            Query::select()
                .columns(["role", "utf8_end"])
                .from(BLOCKS)
                .and_where(block.matches()),
            |r| {
                let role: String = r.get(0)?;
                let role = match role.as_str() {
                    "assistant" => Role::Assistant,
                    "thought" => Role::Thought,
                    "user" => Role::User,
                    _ => {
                        return Err(crate::StoreError::Rejected(
                            "invalid output block role".into(),
                        ));
                    }
                };
                Ok((role, r.get(1)?))
            },
        )
    }
}

struct Attribution {
    workspace: String,
    project: Option<String>,
    bot: Option<String>,
    created: i64,
}

impl Output<'_, W<'_>> {
    /// Admit a live, previously unseen output position, creating its attempt baseline on first use.
    pub fn admit(
        &mut self,
        position: Position<'_>,
        session: &SessionId,
        attempt: &AttemptId,
        started_at: Option<i64>,
        received_at: i64,
    ) -> Result<Option<i64>> {
        let row: Option<(String, String, i64, Option<i64>)> = self.0.one(
            Query::select()
                .columns(["session_id", "node_id", "started", "ended"])
                .from(ATTEMPTS)
                .and_where(Expr::col("attempt_id").eq(attempt.as_str())),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let started = if let Some((sid, node, started, ended)) = row {
            if sid != session.as_str() || node != position.node || ended.is_some() {
                return Ok(None);
            }
            started
        } else {
            let attribution = self.0.one(
                Query::select()
                    .columns([
                        ("s", "workspace_id"),
                        ("i", "project_id"),
                        ("s", "bot_id"),
                        ("a", "created"),
                    ])
                    .from_as("runtime_attempts", "a")
                    .join_as(
                        JoinType::InnerJoin,
                        "runtime_turns",
                        "t",
                        Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
                    )
                    .join_as(
                        JoinType::InnerJoin,
                        "sessions",
                        "s",
                        Expr::col(("s", "id")).eq(Expr::col(("t", "session_id"))),
                    )
                    .join_as(
                        JoinType::LeftJoin,
                        "items",
                        "i",
                        Expr::col(("i", "id")).eq(Expr::col(("s", "item_id"))).and(
                            Expr::col(("i", "workspace_id")).eq(Expr::col(("s", "workspace_id"))),
                        ),
                    )
                    .and_where(Expr::col(("a", "attempt_id")).eq(attempt.as_str()))
                    .and_where(Expr::col(("s", "id")).eq(session.as_str()))
                    .and_where(Expr::col(("a", "node_id")).eq(position.node))
                    .and_where(Expr::col(("s", "readonly")).eq(false)),
                |r| {
                    Ok(Attribution {
                        workspace: r.get(0)?,
                        project: r.get(1)?,
                        bot: r.get(2)?,
                        created: r.get(3)?,
                    })
                },
            )?;
            let Some(a) = attribution else {
                return Ok(None);
            };
            let started = started_at.unwrap_or_else(|| std::cmp::min(a.created, received_at));
            self.0.exec(
                Query::insert()
                    .into_table(ATTEMPTS)
                    .columns([
                        "attempt_id",
                        "session_id",
                        "workspace_id",
                        "project_id",
                        "bot_id",
                        "node_id",
                        "started",
                        "baseline",
                    ])
                    .values_panic([
                        attempt.as_str().into(),
                        session.as_str().into(),
                        a.workspace.into(),
                        a.project.into(),
                        a.bot.into(),
                        position.node.into(),
                        started.into(),
                        started_at.is_some().into(),
                    ]),
            )?;
            started
        };
        Ok((!self.seen(position)?).then_some(started))
    }

    pub fn record(&mut self, sample: Sample<'_>) -> Result<()> {
        self.insert(sample, false)
    }

    /// Mark a faulted stream position once, if this attempt has a live output interval.
    pub fn record_gap(&mut self, sample: Sample<'_>) -> Result<()> {
        let live = self
            .0
            .one(
                Query::select()
                    .column("attempt_id")
                    .from(ATTEMPTS)
                    .and_where(Expr::col("attempt_id").eq(sample.attempt.as_str()))
                    .and_where(Expr::col("ended").is_null()),
                |_| Ok(()),
            )?
            .is_some();
        if live {
            self.insert(sample, true)?;
        }
        Ok(())
    }

    fn insert(&mut self, sample: Sample<'_>, ignore_duplicate: bool) -> Result<()> {
        let mut insert = Query::insert();
        let p = sample.position;
        insert
            .into_table(SAMPLES)
            .columns([
                "node_id",
                "stream",
                "epoch",
                "seq",
                "event_id",
                "attempt_id",
                "at",
                "source_at",
                "received_at",
                "kind",
                "units",
            ])
            .values_panic([
                p.node.into(),
                p.stream.into(),
                p.epoch.into(),
                p.sequence.into(),
                p.event.into(),
                sample.attempt.as_str().into(),
                sample.at.into(),
                sample.source_at.into(),
                sample.received_at.into(),
                sample.kind.code().into(),
                sample.units.into(),
            ]);
        if ignore_duplicate {
            insert.on_conflict(OnConflict::new().do_nothing().to_owned());
        }
        self.0.exec(&insert)?;
        Ok(())
    }

    pub fn start_block(&mut self, block: &Block<'_>, role: Role) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(BLOCKS)
                .columns(["attempt_id", "message", "block", "role"])
                .values_panic([
                    block.attempt.as_str().into(),
                    block.message.into(),
                    block.block.into(),
                    match role {
                        Role::Assistant => "assistant",
                        Role::Thought => "thought",
                        Role::User => "user",
                    }
                    .into(),
                ])
                .on_conflict(
                    OnConflict::columns(["attempt_id", "message", "block"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn advance_block(&mut self, block: &Block<'_>, next: i64) -> Result<()> {
        let end = self
            .0
            .dialect()
            .greatest(Expr::col("utf8_end"), Expr::val(next));
        self.0.exec(
            Query::update()
                .table(BLOCKS)
                .value("utf8_end", end)
                .and_where(block.matches()),
        )?;
        Ok(())
    }

    pub fn end(&mut self, attempt: &AttemptId, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(ATTEMPTS)
                .value("ended", at)
                .and_where(Expr::col("attempt_id").eq(attempt.as_str()))
                .and_where(Expr::col("ended").is_null()),
        )?;
        Ok(())
    }
}
