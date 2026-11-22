use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result, W};

pub struct SignalRows<'a, C>(pub &'a mut C);

#[derive(serde::Serialize)]
pub struct SignalEvent {
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub connection_id: Option<String>,
    pub external_id: Option<String>,
    pub thread_key: Option<String>,
    pub group_key: Option<String>,
    pub scope_ext_id: Option<String>,
    pub space_id: Option<String>,
    pub message_id: Option<String>,
    pub actor_account_id: Option<String>,
    pub actor_party_id: Option<String>,
    pub occurred: i64,
    pub received: i64,
    pub summary: String,
    pub payload: serde_json::Value,
}

pub struct NewCorrection<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub event: &'a str,
    pub scope: &'a str,
    pub action: &'a str,
    pub flow: Option<&'a str>,
    pub change: Option<&'a str>,
    pub memory: Option<&'a str>,
    pub item: Option<&'a str>,
    pub actor: &'a str,
    pub at: i64,
}

pub struct SignalSource {
    pub id: String,
    pub source: String,
    pub type_key: String,
    pub occurred: i64,
    pub summary: String,
    pub payload: Option<serde_json::Value>,
    pub purged: bool,
    pub actor: Option<String>,
}

pub struct SignalTriage {
    pub outcome: String,
    pub label: String,
    pub reason: String,
    pub handled_by: Option<String>,
    pub flow: Option<String>,
    pub run: Option<String>,
    pub item: Option<String>,
    pub decision: Option<String>,
}

impl<C: Read> SignalRows<'_, C> {
    pub fn thread_events(
        &mut self,
        ws: &str,
        thread: &str,
        exclude: &str,
    ) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["id", "received"])
                .from("ext_events")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("thread_key").eq(thread))
                .and_where(Expr::col("id").ne(exclude)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn counts_by_kind(
        &mut self,
        ws: &str,
        connections: &[String],
        since: i64,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut counts = std::collections::HashMap::new();
        for batch in connections.chunks(500) {
            for (kind, count) in self.0.all(
                Query::select()
                    .from("ext_events")
                    .column("type")
                    .expr(sea_query::Func::count(Expr::col("id")))
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("received").gte(since))
                    .and_where(Expr::col("connection_id").is_in(batch.iter().map(String::as_str)))
                    .group_by_col("type"),
                |r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?)),
            )? {
                *counts.entry(kind).or_default() += count;
            }
        }
        Ok(counts)
    }

    pub fn event(&mut self, id: &str) -> Result<Option<SignalEvent>> {
        self.0.one(
            Query::select()
                .from("ext_events")
                .columns([
                    "id",
                    "source",
                    "type",
                    "connection_id",
                    "external_id",
                    "thread_key",
                    "group_key",
                    "scope_ext_id",
                    "space_id",
                    "message_id",
                    "actor_account_id",
                    "actor_party_id",
                    "occurred",
                    "received",
                    "summary",
                    "payload",
                ])
                .and_where(Expr::col("id").eq(id)),
            |r| {
                Ok(SignalEvent {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    kind: r.get(2)?,
                    connection_id: r.get(3)?,
                    external_id: r.get(4)?,
                    thread_key: r.get(5)?,
                    group_key: r.get(6)?,
                    scope_ext_id: r.get(7)?,
                    space_id: r.get(8)?,
                    message_id: r.get(9)?,
                    actor_account_id: r.get(10)?,
                    actor_party_id: r.get(11)?,
                    occurred: r.get(12)?,
                    received: r.get(13)?,
                    summary: r.get(14)?,
                    payload: r
                        .get::<Option<super::Json<_>>>(15)?
                        .map_or(serde_json::Value::Null, |v| v.0),
                })
            },
        )
    }

    pub fn recent_kinds(
        &mut self,
        ws: &str,
        source: &str,
        limit: u64,
    ) -> Result<Vec<(String, String)>> {
        let tie = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .columns(["id", "type"])
                .from("ext_events")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("source").eq(source))
                .order_by("received", Order::Desc)
                .order_by_expr(tie, Order::Desc)
                .limit(limit),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn current_outcome(&mut self, id: &str) -> Result<Option<(String, String, String)>> {
        self.0.one(
            Query::select()
                .columns(["workspace_id", "outcome", "outcome_label"])
                .from("ext_events")
                .and_where(Expr::col("id").eq(id)),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    pub fn source(&mut self, ws: &str, id: &str) -> Result<Option<SignalSource>> {
        self.0.one(
            Query::select()
                .columns([
                    "id",
                    "source",
                    "type",
                    "occurred",
                    "summary",
                    "payload",
                    "payload_purged",
                    "actor_party_id",
                ])
                .from("ext_events")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |row| {
                Ok(SignalSource {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    type_key: row.get(2)?,
                    occurred: row.get(3)?,
                    summary: row.get(4)?,
                    payload: row.get::<Option<super::Json<_>>>(5)?.map(|json| json.0),
                    purged: row.get::<Option<i64>>(6)?.is_some(),
                    actor: row.get(7)?,
                })
            },
        )
    }

    pub fn existing_ids<'a>(
        &mut self,
        ws: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<std::collections::HashSet<String>> {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut existing = std::collections::HashSet::new();
        for chunk in ids.chunks(500) {
            existing.extend(
                self.0.all(
                    Query::select()
                        .column("id")
                        .from("ext_events")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("id").is_in(chunk.iter().copied())),
                    |row| row.get::<String>(0),
                )?,
            );
        }
        Ok(existing)
    }

    pub fn with_payload_before(&mut self, ws: &str, cut: i64, limit: u64) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("id")
                .from("ext_events")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("received").lt(cut))
                .and_where(Expr::col("payload_purged").is_null())
                .order_by_columns([("received", Order::Asc), ("id", Order::Asc)])
                .limit(limit),
            |row| row.get(0),
        )
    }
}

impl SignalRows<'_, W<'_>> {
    pub fn correct(&mut self, correction: &NewCorrection<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("signal_corrections")
                .columns([
                    "id",
                    "workspace_id",
                    "event_id",
                    "scope",
                    "action",
                    "flow_id",
                    "changeset_id",
                    "mem_id",
                    "work_item_id",
                    "created_by",
                    "created",
                ])
                .values_panic([
                    correction.id.into(),
                    correction.ws.into(),
                    correction.event.into(),
                    correction.scope.into(),
                    correction.action.into(),
                    correction.flow.into(),
                    correction.change.into(),
                    correction.memory.into(),
                    correction.item.into(),
                    correction.actor.into(),
                    correction.at.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn mark_item(&mut self, ws: &str, id: &str, item: &str, label: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("ext_events")
                .value("outcome", "item")
                .value("outcome_label", label)
                .value("work_item_id", item)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn settle(&mut self, ws: &str, id: &str, state: &SignalTriage) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("ext_events")
                .value("outcome", state.outcome.as_str())
                .value("outcome_label", state.label.as_str())
                .value("reason", state.reason.as_str())
                .value("handled_by", state.handled_by.as_deref())
                .value("flow_id", state.flow.as_deref())
                .value("run_id", state.run.as_deref())
                .value("work_item_id", state.item.as_deref())
                .value("decision_id", state.decision.as_deref())
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn purge_payloads(&mut self, ids: &[String], now: i64) -> Result<u64> {
        self.0.exec(
            Query::update()
                .table("ext_events")
                .value("payload", Option::<String>::None)
                .value("payload_purged", now)
                .and_where(Expr::col("id").is_in(ids.iter().map(String::as_str))),
        )
    }

    pub fn delete_before(&mut self, cut: i64, limit: u64) -> Result<u64> {
        let batch = Query::select()
            .column("id")
            .from("ext_events")
            .and_where(Expr::col("received").lt(cut))
            .order_by_columns([("received", Order::Asc), ("id", Order::Asc)])
            .limit(limit)
            .take();
        self.0.exec(
            Query::delete()
                .from_table("ext_events")
                .and_where(Expr::col("id").in_subquery(batch)),
        )
    }
}

#[derive(Default)]
pub struct SignalFilter<'a> {
    pub since: i64,
    pub source: Option<&'a str>,
    pub outcome: Option<&'a str>,
    pub text: Option<&'a str>,
    pub matching_sources: &'a [&'a str],
}

pub struct SignalRow {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub summary: String,
    pub thread: Option<String>,
    pub group: Option<String>,
    pub connection: Option<String>,
    pub party: Option<String>,
    pub actor_account: Option<String>,
    pub outcome: String,
    pub label: String,
    pub reason: String,
    pub handled_by: Option<String>,
    pub flow: Option<String>,
    pub item: Option<String>,
    pub decision: Option<String>,
    pub raw_available: bool,
    pub occurred: i64,
    pub received: i64,
}

fn signal(row: &super::Row<'_>) -> Result<SignalRow> {
    Ok(SignalRow {
        id: row.get(0)?,
        source: row.get(1)?,
        kind: row.get(2)?,
        summary: row.get(3)?,
        thread: row.get(4)?,
        group: row.get(5)?,
        connection: row.get(6)?,
        party: row.get(7)?,
        actor_account: row.get(8)?,
        outcome: row.get(9)?,
        label: row.get(10)?,
        reason: row.get(11)?,
        handled_by: row.get(12)?,
        flow: row.get(13)?,
        item: row.get(14)?,
        decision: row.get(15)?,
        raw_available: row.get(16)?,
        occurred: row.get(17)?,
        received: row.get(18)?,
    })
}

fn with_projection(q: &mut sea_query::SelectStatement) -> &mut sea_query::SelectStatement {
    q.columns([
        "id",
        "source",
        "type",
        "summary",
        "thread_key",
        "group_key",
        "connection_id",
        "actor_party_id",
        "actor_account_id",
        "outcome",
        "outcome_label",
        "reason",
        "handled_by",
        "flow_id",
        "work_item_id",
        "decision_id",
    ])
    .expr(Expr::col("payload").is_not_null())
    .columns(["occurred", "received"])
}

fn visible(viewer: &crate::base::Viewer) -> sea_query::SelectStatement {
    let mut q = Query::select();
    q.from("ext_events")
        .and_where(Expr::col("workspace_id").eq(viewer.ws.as_str()));
    if viewer.is_service() {
        q.and_where(Expr::col("visibility").eq("workspace"));
    }
    q
}

impl<C: Read> SignalRows<'_, C> {
    pub fn get(&mut self, viewer: &crate::base::Viewer, id: &str) -> Result<Option<SignalRow>> {
        self.0.one(
            with_projection(&mut visible(viewer)).and_where(Expr::col("id").eq(id)),
            signal,
        )
    }

    pub fn payload(
        &mut self,
        viewer: &crate::base::Viewer,
        id: &str,
    ) -> Result<Option<Option<serde_json::Value>>> {
        self.0.one(
            visible(viewer)
                .column("payload")
                .and_where(Expr::col("id").eq(id)),
            |row| Ok(row.get::<Option<super::Json<_>>>(0)?.map(|json| json.0)),
        )
    }

    pub fn list(
        &mut self,
        viewer: &crate::base::Viewer,
        filter: &SignalFilter<'_>,
        limit: u64,
    ) -> Result<(Vec<(String, i64)>, Vec<SignalRow>)> {
        use sea_query::{Cond, Func};

        let dialect = self.0.db().dialect();
        let mut q = visible(viewer);
        q.and_where(Expr::col("received").gte(filter.since));
        if let Some(source) = filter.source {
            q.and_where(Expr::col("source").eq(source));
        }
        if let Some(text) = filter.text {
            let matched = ["summary", "group_key", "outcome_label"].into_iter().fold(
                Cond::any(),
                |condition, col| {
                    condition.add(dialect.contains(
                        Func::cust("lower").arg(Expr::col(col)).into(),
                        Expr::value(text),
                    ))
                },
            );
            let matched = if filter.matching_sources.is_empty() {
                matched
            } else {
                matched.add(Expr::col("source").is_in(filter.matching_sources.iter().copied()))
            };
            q.cond_where(matched);
        }
        let counts = self.0.all(
            q.clone()
                .column("outcome")
                .expr(Func::count(Expr::col("id")))
                .group_by_col("outcome"),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Some(outcome) = filter.outcome {
            q.and_where(Expr::col("outcome").eq(outcome));
        }
        let rows = self.0.all(
            with_projection(&mut q)
                .order_by("received", Order::Desc)
                .order_by_expr(dialect.insertion_order(None), Order::Desc)
                .limit(limit),
            signal,
        )?;
        Ok((counts, rows))
    }
}

pub struct NewSignal<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub connection: Option<&'a str>,
    pub source: &'a str,
    pub kind: &'a str,
    pub external_id: &'a str,
    pub dedupe: &'a str,
    pub thread: Option<&'a str>,
    pub group: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub space: Option<&'a str>,
    pub message: Option<&'a str>,
    pub actor_account: Option<&'a str>,
    pub actor_party: Option<&'a str>,
    pub occurred: i64,
    pub received: i64,
    pub summary: &'a str,
    pub payload: &'a serde_json::Value,
    pub personal: bool,
}

impl<C: Read> SignalRows<'_, C> {
    pub fn duplicate(&mut self, ws: &str, dedupe: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("id")
                .from("ext_events")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("dedupe_key").eq(dedupe)),
            |row| row.get(0),
        )
    }
}

impl SignalRows<'_, W<'_>> {
    pub fn insert(&mut self, row: &NewSignal<'_>) -> Result<()> {
        let payload = (!row.payload.is_null()).then(|| row.payload.to_string());
        let visibility = if row.personal { "private" } else { "workspace" };
        self.0.exec(
            Query::insert()
                .into_table("ext_events")
                .columns([
                    "id",
                    "workspace_id",
                    "connection_id",
                    "source",
                    "type",
                    "external_id",
                    "dedupe_key",
                    "thread_key",
                    "group_key",
                    "scope_ext_id",
                    "space_id",
                    "message_id",
                    "actor_account_id",
                    "actor_party_id",
                    "occurred",
                    "received",
                    "summary",
                    "payload",
                    "visibility",
                ])
                .values_panic([
                    row.id.into(),
                    row.ws.into(),
                    row.connection.into(),
                    row.source.into(),
                    row.kind.into(),
                    row.external_id.into(),
                    row.dedupe.into(),
                    row.thread.into(),
                    row.group.into(),
                    row.scope.into(),
                    row.space.into(),
                    row.message.into(),
                    row.actor_account.into(),
                    row.actor_party.into(),
                    row.occurred.into(),
                    row.received.into(),
                    row.summary.into(),
                    payload.into(),
                    visibility.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn recorded(&mut self, id: &str, label: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("ext_events")
                .value("outcome", "recorded")
                .value("outcome_label", label)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}
