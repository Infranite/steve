//! Node event ingest on the storage contract: where each of a Node's streams stands
//! (`node_streams`), the session events its events become (`events`), what a running attempt did
//! so far (`runtime_attempt_facts`, `runtime_attempts.active_at`), the running cost of a harness
//! process (`runtime_cost_marks`) and the Node's view of a session (`runtime_sessions`).
//!
//! Reads are on `Repo<C: Read>` (a snapshot or the write itself), writes only on `Repo<W>`.

use std::marker::PhantomData;

use sea_query::{Expr, ExprTrait, Func, OnConflict, Query};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::contract::{Json, Q};
use crate::{R, Read, Result, W};

pub(crate) mod schema {
    use sea_query::Iden;

    #[derive(Iden, Clone, Copy)]
    pub enum NodeStreams {
        Table,
        NodeId,
        Stream,
        Epoch,
        Applied,
        SessionId,
        Fault,
        FaultEvent,
        FaultReason,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum Events {
        Table,
        Seq,
        SessionId,
        Ts,
        Kind,
        Data,
        Format,
        SourceNode,
        SourceStream,
        SourceEpoch,
        SourceSeq,
        SourceAttempt,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum EventPayloads {
        Table,
        SessionId,
        Hash,
        Data,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum EventPayloadRefs {
        Table,
        EventSeq,
        Path,
        SessionId,
        Hash,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum Sessions {
        Table,
        Id,
        WorkspaceId,
        FirstToolAt,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum RuntimeSessions {
        Table,
        SessionId,
        NodeId,
        NodeRevision,
        NodeResume,
        NodeViews,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum RuntimeAttempts {
        Table,
        AttemptId,
        State,
        ActiveAt,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum RuntimeAttemptFacts {
        Table,
        AttemptId,
        Tools,
        Permissions,
        Model,
        Cost,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum RuntimeCostMarks {
        Table,
        NodeId,
        Stream,
        Epoch,
        MeterEpoch,
        Cost,
    }
}
use schema::*;

/// A typed repository over a read snapshot or the write transaction.
pub struct Repo<'a, C, K>(&'a mut C, PhantomData<K>);

pub enum StreamsK {}
pub enum EventsK {}
pub enum AttemptsK {}
pub enum CostMarksK {}
pub enum RuntimeK {}

pub type Streams<'a, C> = Repo<'a, C, StreamsK>;
pub type SessionEvents<'a, C> = Repo<'a, C, EventsK>;
pub type Attempts<'a, C> = Repo<'a, C, AttemptsK>;
pub type CostMarks<'a, C> = Repo<'a, C, CostMarksK>;
pub type RuntimeViews<'a, C> = Repo<'a, C, RuntimeK>;

/// The ingest context's repositories, on `r` and on `w`.
pub trait Ingest: Read + Sized {
    fn streams(&mut self) -> Streams<'_, Self> {
        Repo(self, PhantomData)
    }
    fn events(&mut self) -> SessionEvents<'_, Self> {
        Repo(self, PhantomData)
    }
    fn attempts(&mut self) -> Attempts<'_, Self> {
        Repo(self, PhantomData)
    }
    fn cost_marks(&mut self) -> CostMarks<'_, Self> {
        Repo(self, PhantomData)
    }
    fn runtime(&mut self) -> RuntimeViews<'_, Self> {
        Repo(self, PhantomData)
    }
}
impl Ingest for R<'_> {}
impl Ingest for W<'_> {}

// ── streams ──

/// One of a Node's event streams, in one epoch.
#[derive(Debug, Clone, Copy)]
pub struct StreamKey<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
}

/// Where a stream's apply stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamState {
    /// The last position applied (its events are applied in order, each once).
    pub applied: i64,
    /// Nothing more of it is applied ([`Streams::fault`]).
    pub faulted: bool,
}

/// Why a stream's apply stopped for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// One of its events could not be applied.
    Failed,
    /// The Node dropped events the Hub does not hold.
    Gap,
    /// The Hub holds more than the Node produced (its journal went back).
    Ahead,
}

impl Fault {
    fn name(self) -> &'static str {
        match self {
            Fault::Failed => "failed",
            Fault::Gap => "gap",
            Fault::Ahead => "ahead",
        }
    }
}

/// What a fault is recorded with.
#[derive(Debug, Clone, Copy, Default)]
pub struct FaultNote<'a> {
    /// The session whose events the stream carries.
    pub session: Option<&'a str>,
    /// The event that could not be applied.
    pub event: Option<&'a str>,
    pub reason: Option<&'a str>,
}

fn stream_is(k: StreamKey<'_>) -> Expr {
    Expr::col(NodeStreams::NodeId)
        .eq(k.node)
        .and(Expr::col(NodeStreams::Stream).eq(k.stream))
        .and(Expr::col(NodeStreams::Epoch).eq(k.epoch))
}

impl<C: Read> Streams<'_, C> {
    pub fn state(&mut self, k: StreamKey<'_>) -> Result<Option<StreamState>> {
        self.0.one(
            Query::select()
                .column(NodeStreams::Applied)
                .expr(Expr::col(NodeStreams::Fault).is_not_null())
                .from(NodeStreams::Table)
                .and_where(stream_is(k)),
            |r| {
                Ok(StreamState {
                    applied: r.get(0)?,
                    faulted: r.get(1)?,
                })
            },
        )
    }
}

impl Streams<'_, W<'_>> {
    /// The stream's state, its row made first if the Hub never heard of it (`session`: the session
    /// whose events it carries, if any).
    pub fn open(&mut self, k: StreamKey<'_>, session: Option<&str>) -> Result<StreamState> {
        self.0.exec(
            Query::insert()
                .into_table(NodeStreams::Table)
                .columns([
                    NodeStreams::NodeId,
                    NodeStreams::Stream,
                    NodeStreams::Epoch,
                    NodeStreams::SessionId,
                ])
                .values_panic([
                    k.node.into(),
                    k.stream.into(),
                    k.epoch.into(),
                    session.into(),
                ])
                .on_conflict(
                    OnConflict::columns([
                        NodeStreams::NodeId,
                        NodeStreams::Stream,
                        NodeStreams::Epoch,
                    ])
                    .do_nothing()
                    .to_owned(),
                ),
        )?;
        self.state(k)?
            .ok_or_else(|| crate::StoreError::Rejected("node stream row not made".into()))
    }

    /// The stream's events up to `seq` were applied in this transaction, in order.
    pub fn applied(&mut self, k: StreamKey<'_>, seq: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(NodeStreams::Table)
                .value(NodeStreams::Applied, seq)
                .and_where(stream_is(k))
                .and_where(Expr::col(NodeStreams::Applied).lt(seq)),
        )?;
        Ok(())
    }

    /// Marks the stream faulted: none of its events is applied any more. The first fault is kept.
    /// True: the stream was not faulted before.
    pub fn fault(&mut self, k: StreamKey<'_>, fault: Fault, note: FaultNote<'_>) -> Result<bool> {
        self.open(k, note.session)?;
        Ok(self.0.exec(
            Query::update()
                .table(NodeStreams::Table)
                .value(NodeStreams::Fault, fault.name())
                .value(
                    NodeStreams::SessionId,
                    Func::coalesce([Expr::col(NodeStreams::SessionId), Expr::val(note.session)]),
                )
                .value(NodeStreams::FaultEvent, note.event)
                .value(NodeStreams::FaultReason, note.reason)
                .and_where(stream_is(k))
                .and_where(Expr::col(NodeStreams::Fault).is_null()),
        )? > 0)
    }
}

// ── session events ──

/// Where a session event came from on a Node: its stream position is its identity.
#[derive(Debug, Clone, Copy)]
pub struct EventSource<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
    pub seq: i64,
    /// The attempt the event is of, if any.
    pub attempt: Option<&'a str>,
}

/// The format session events are written in: always Steve's own terms.
const FORMAT: &str = "steve.v1";

impl<C: Read> SessionEvents<'_, C> {
    pub fn latest_protocol_hash(&mut self, session: &str) -> Result<Option<String>> {
        let data = Expr::col(Events::Data);
        let dialect = self.0.db().dialect();
        self.0.one(
            Query::select()
                .expr(dialect.json_field(data.clone(), "steve_protocol_hash"))
                .from(Events::Table)
                .and_where(Expr::col(Events::SessionId).eq(session))
                .and_where(
                    Expr::cust_with_expr("json_type(?, '$.steve_protocol_hash')", data).eq("text"),
                )
                .order_by(Events::Seq, sea_query::Order::Desc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn latest_timestamp(&mut self, session: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column(Events::Ts)
                .from(Events::Table)
                .and_where(Expr::col(Events::SessionId).eq(session))
                .order_by(Events::Seq, sea_query::Order::Desc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn has_since(&mut self, session: &str, ts: i64) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column(Events::Seq)
                    .from(Events::Table)
                    .and_where(Expr::col(Events::SessionId).eq(session))
                    .and_where(Expr::col(Events::Ts).gte(ts))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn latest_input_seq(&mut self, session: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column(Events::Seq)
                .from(Events::Table)
                .and_where(Expr::col(Events::SessionId).eq(session))
                .and_where(Expr::col(Events::Kind).eq("user"))
                .order_by(Events::Seq, sea_query::Order::Desc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn latest_payload_after(
        &mut self,
        session: &str,
        kind: &str,
        seq: i64,
    ) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column(Events::Data)
                .from(Events::Table)
                .and_where(Expr::col(Events::SessionId).eq(session))
                .and_where(Expr::col(Events::Kind).eq(kind))
                .and_where(Expr::col(Events::Seq).gt(seq))
                .order_by(Events::Seq, sea_query::Order::Desc)
                .limit(1),
            |row| row.get(0),
        )
    }
}

impl SessionEvents<'_, W<'_>> {
    /// Appends an event written by the Hub, with no Node stream position to deduplicate.
    pub fn add_local(
        &mut self,
        session: &str,
        kind: &str,
        data: serde_json::Value,
        at: i64,
    ) -> Result<i64> {
        self.add(session, kind, data, None, at)?
            .ok_or_else(|| crate::StoreError::Rejected("session event not added".into()))
    }

    /// Appends a session event (`at`: its time) and returns its `seq`; it is published after the
    /// commit, carrying `data` as given (large tool content is kept apart, see
    /// `event_payload`). With a source, the same Node event again is a no-op (`None`).
    pub fn add(
        &mut self,
        session: &str,
        kind: &str,
        data: serde_json::Value,
        source: Option<EventSource<'_>>,
        at: i64,
    ) -> Result<Option<i64>> {
        let (stored, payloads) = crate::event_payload::split(kind, &data)?;
        let stored = match stored {
            std::borrow::Cow::Owned(v) => Some(v),
            std::borrow::Cow::Borrowed(_) => None,
        };
        let published = serde_json::to_string(&data)?;
        let s = source;
        let seq: Option<i64> = self.0.one(
            Query::insert()
                .into_table(Events::Table)
                .columns([
                    Events::SessionId,
                    Events::Ts,
                    Events::Kind,
                    Events::Data,
                    Events::Format,
                    Events::SourceNode,
                    Events::SourceStream,
                    Events::SourceEpoch,
                    Events::SourceSeq,
                    Events::SourceAttempt,
                ])
                .values_panic([
                    session.into(),
                    at.into(),
                    kind.into(),
                    stored.unwrap_or(data).into(),
                    FORMAT.into(),
                    s.map(|s| s.node).into(),
                    s.map(|s| s.stream).into(),
                    s.map(|s| s.epoch).into(),
                    s.map(|s| s.seq).into(),
                    s.and_then(|s| s.attempt).into(),
                ])
                // the partial unique index of Node events (migration 034)
                .on_conflict(
                    OnConflict::columns([
                        Events::SourceNode,
                        Events::SourceStream,
                        Events::SourceEpoch,
                        Events::SourceSeq,
                    ])
                    .target_and_where(Expr::col(Events::SourceNode).is_not_null())
                    .do_nothing()
                    .to_owned(),
                )
                .returning_col(Events::Seq),
            |r| r.get(0),
        )?;
        let Some(seq) = seq else { return Ok(None) };
        crate::event_payload::save(self.0.db(), session, seq, &payloads)?;
        // when the session made its first tool call (migration 092): how long a fresh session
        // takes to start working
        if kind == crate::TOOL_CALL_EVENT {
            self.0.exec(
                Query::update()
                    .table(Sessions::Table)
                    .value(Sessions::FirstToolAt, at)
                    .and_where(Expr::col(Sessions::Id).eq(session))
                    .and_where(Expr::col(Sessions::FirstToolAt).is_null()),
            )?;
        }
        let ws: Option<String> = self.0.one(
            Query::select()
                .column(Sessions::WorkspaceId)
                .from(Sessions::Table)
                .and_where(Expr::col(Sessions::Id).eq(session)),
            |r| r.get(0),
        )?;
        if let Some(ws) = ws {
            self.0.emit(steve_types::LiveEvent::from(
                steve_types::WorkLiveEvent::Event {
                    ws: ws.into(),
                    session_id: session.into(),
                    seq: steve_types::Seq(seq),
                    kind: kind.into(),
                    data: steve_types::RawJson::from_str(&published).expect("serialized JSON"),
                },
            ));
        }
        Ok(Some(seq))
    }
}

// ── attempts ──

/// What a running attempt did so far (`tools`: the caller's own record of its tools, kept as JSON).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeptFacts<T> {
    pub tools: T,
    pub permissions: i64,
    pub model: Option<String>,
    pub cost: Option<f64>,
}

fn facts_of(attempt: &str) -> sea_query::SelectStatement {
    Query::select()
        .columns([
            RuntimeAttemptFacts::Tools,
            RuntimeAttemptFacts::Permissions,
            RuntimeAttemptFacts::Model,
            RuntimeAttemptFacts::Cost,
        ])
        .from(RuntimeAttemptFacts::Table)
        .and_where(Expr::col(RuntimeAttemptFacts::AttemptId).eq(attempt))
        .to_owned()
}

impl<C: Read> Attempts<'_, C> {
    /// What `attempt` did so far, if anything was kept.
    pub fn facts<T: DeserializeOwned>(&mut self, attempt: &str) -> Result<Option<KeptFacts<T>>> {
        self.0.one(&facts_of(attempt), |r| {
            Ok(KeptFacts {
                tools: r.get::<Json<T>>(0)?.0,
                permissions: r.get(1)?,
                model: r.get(2)?,
                cost: r.get(3)?,
            })
        })
    }
}

impl Attempts<'_, W<'_>> {
    /// Keeps what `attempt` did so far, while it is not over (a late event of an attempt that
    /// ended keeps nothing).
    pub fn keep_facts<T: Serialize>(&mut self, attempt: &str, f: &KeptFacts<T>) -> Result<()> {
        let open = self.0.one(
            Query::select()
                .column(RuntimeAttempts::AttemptId)
                .from(RuntimeAttempts::Table)
                .and_where(Expr::col(RuntimeAttempts::AttemptId).eq(attempt))
                .and_where(Expr::col(RuntimeAttempts::State).ne("finished")),
            |_| Ok(()),
        )?;
        if open.is_none() {
            return Ok(());
        }
        self.0.exec(
            Query::insert()
                .into_table(RuntimeAttemptFacts::Table)
                .columns([
                    RuntimeAttemptFacts::AttemptId,
                    RuntimeAttemptFacts::Tools,
                    RuntimeAttemptFacts::Permissions,
                    RuntimeAttemptFacts::Model,
                    RuntimeAttemptFacts::Cost,
                ])
                .values_panic([
                    attempt.into(),
                    serde_json::to_value(&f.tools)?.into(),
                    f.permissions.into(),
                    f.model.as_deref().into(),
                    f.cost.into(),
                ])
                .on_conflict(
                    OnConflict::column(RuntimeAttemptFacts::AttemptId)
                        .update_columns([
                            RuntimeAttemptFacts::Tools,
                            RuntimeAttemptFacts::Permissions,
                            RuntimeAttemptFacts::Model,
                            RuntimeAttemptFacts::Cost,
                        ])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    /// What `attempt` did, once it is over: what was kept goes with it.
    pub fn take_facts<T: DeserializeOwned>(
        &mut self,
        attempt: &str,
    ) -> Result<Option<KeptFacts<T>>> {
        let facts = self.facts(attempt)?;
        self.0.exec(
            Query::delete()
                .from_table(RuntimeAttemptFacts::Table)
                .and_where(Expr::col(RuntimeAttemptFacts::AttemptId).eq(attempt)),
        )?;
        Ok(facts)
    }

    /// The agent did something in a running attempt, received `at`: a workflow's evidence that
    /// it runs.
    pub fn touch(&mut self, attempt: &str, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(RuntimeAttempts::Table)
                .value(RuntimeAttempts::ActiveAt, at)
                .and_where(Expr::col(RuntimeAttempts::AttemptId).eq(attempt))
                .and_where(Expr::col(RuntimeAttempts::State).eq("running")),
        )?;
        Ok(())
    }
}

// ── running cost of a harness process ──

/// One harness process's meter (its meter epoch) on a stream.
#[derive(Debug, Clone, Copy)]
pub struct MeterKey<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
    pub meter_epoch: &'a str,
}

fn meter_is(k: MeterKey<'_>) -> Expr {
    Expr::col(RuntimeCostMarks::NodeId)
        .eq(k.node)
        .and(Expr::col(RuntimeCostMarks::Stream).eq(k.stream))
        .and(Expr::col(RuntimeCostMarks::Epoch).eq(k.epoch))
        .and(Expr::col(RuntimeCostMarks::MeterEpoch).eq(k.meter_epoch))
}

impl<C: Read> CostMarks<'_, C> {
    /// The highest running total the meter reported so far.
    pub fn mark(&mut self, k: MeterKey<'_>) -> Result<Option<f64>> {
        self.0.one(
            Query::select()
                .column(RuntimeCostMarks::Cost)
                .from(RuntimeCostMarks::Table)
                .and_where(meter_is(k)),
            |r| r.get(0),
        )
    }
}

impl CostMarks<'_, W<'_>> {
    /// Keeps the meter's highest running total.
    pub fn raise(&mut self, k: MeterKey<'_>, cost: f64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(RuntimeCostMarks::Table)
                .columns([
                    RuntimeCostMarks::NodeId,
                    RuntimeCostMarks::Stream,
                    RuntimeCostMarks::Epoch,
                    RuntimeCostMarks::MeterEpoch,
                    RuntimeCostMarks::Cost,
                ])
                .values_panic([
                    k.node.into(),
                    k.stream.into(),
                    k.epoch.into(),
                    k.meter_epoch.into(),
                    cost.into(),
                ])
                .on_conflict(
                    OnConflict::columns([
                        RuntimeCostMarks::NodeId,
                        RuntimeCostMarks::Stream,
                        RuntimeCostMarks::Epoch,
                        RuntimeCostMarks::MeterEpoch,
                    ])
                    .value(
                        RuntimeCostMarks::Cost,
                        Func::greatest([
                            Expr::col((RuntimeCostMarks::Table, RuntimeCostMarks::Cost)),
                            Expr::col(("excluded", RuntimeCostMarks::Cost)),
                        ]),
                    )
                    .to_owned(),
                ),
        )?;
        Ok(())
    }
}

// ── the Node's view of a session ──

impl<C: Read> RuntimeViews<'_, C> {
    /// Whether `session` runs on `node`.
    pub fn on_node(&mut self, session: &str, node: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(RuntimeSessions::SessionId)
                    .from(RuntimeSessions::Table)
                    .and_where(Expr::col(RuntimeSessions::SessionId).eq(session))
                    .and_where(Expr::col(RuntimeSessions::NodeId).eq(node)),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl RuntimeViews<'_, W<'_>> {
    /// The Node showed a view of `session` (its revision, and its resume handle if it gave one):
    /// kept as the last one seen and added to the views since the last open.
    pub fn seen_view(
        &mut self,
        session: &str,
        node: &str,
        revision: &str,
        resume: Option<&str>,
    ) -> Result<()> {
        let views = self
            .0
            .dialect()
            .json_push(Expr::col(RuntimeSessions::NodeViews), Expr::val(revision));
        self.0.exec(
            Query::update()
                .table(RuntimeSessions::Table)
                .value(RuntimeSessions::NodeRevision, revision)
                .value(
                    RuntimeSessions::NodeResume,
                    Func::coalesce([Expr::val(resume), Expr::col(RuntimeSessions::NodeResume)]),
                )
                .value(RuntimeSessions::NodeViews, views)
                .and_where(Expr::col(RuntimeSessions::SessionId).eq(session))
                .and_where(Expr::col(RuntimeSessions::NodeId).eq(node)),
        )?;
        Ok(())
    }
}
