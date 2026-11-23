use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query};

use crate::contract::WorkspaceSettings;
use crate::{Read, Result};

pub struct ItemReads<'a, C>(pub &'a mut C);
pub struct TimelineRows<'a, C>(pub &'a mut C);
pub struct TimezoneSettings<'a, C>(pub &'a mut C);

pub struct ItemEventFields {
    pub title: String,
    pub status: String,
    pub bot_id: Option<String>,
    pub project_id: Option<String>,
    pub source: serde_json::Value,
}

/// The item state needed when linking contacts or adding a timeline entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemHead {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub archived: bool,
}

impl<C: Read> ItemReads<'_, C> {
    pub fn active_count_for_party(&mut self, ws: &str, party: &str) -> Result<i64> {
        let items = Query::select()
            .from_as("links", "l")
            .column(("l", "subject_id"))
            .distinct()
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id"))
                    .equals(("l", "subject_id"))
                    .and(Expr::col(("i", "workspace_id")).equals(("l", "workspace_id"))),
            )
            .and_where(Expr::col(("l", "workspace_id")).eq(ws))
            .and_where(Expr::col(("l", "subject_type")).eq("item"))
            .and_where(Expr::col(("l", "object_type")).eq("party"))
            .and_where(Expr::col(("l", "object_id")).eq(party))
            .and_where(Expr::col(("i", "archived")).eq(0_i64))
            .take();
        Ok(self
            .0
            .one(
                Query::select()
                    .from_subquery(items, "active_items")
                    .expr(Func::count(Expr::col("subject_id"))),
                |row| row.get(0),
            )?
            .unwrap_or_default())
    }

    pub fn head(&mut self, id: &str) -> Result<Option<ItemHead>> {
        self.0.one(
            Query::select()
                .from("items")
                .columns(["id", "workspace_id", "title", "archived"])
                .and_where(Expr::col("id").eq(id)),
            |row| {
                Ok(ItemHead {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    title: row.get(2)?,
                    archived: row.get(3)?,
                })
            },
        )
    }

    pub fn heads(
        &mut self,
        ws: &str,
        ids: &[&str],
    ) -> Result<std::collections::HashMap<String, ItemHead>> {
        if ids.is_empty() {
            return Ok(Default::default());
        }
        Ok(self
            .0
            .all(
                Query::select()
                    .from("items")
                    .columns(["id", "workspace_id", "title", "archived"])
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").is_in(ids.iter().copied())),
                |row| {
                    Ok(ItemHead {
                        id: row.get(0)?,
                        workspace_id: row.get(1)?,
                        title: row.get(2)?,
                        archived: row.get(3)?,
                    })
                },
            )?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect())
    }

    pub fn unfinished(&mut self, ws: &str, ids: &[String]) -> Result<bool> {
        for batch in ids.chunks(500) {
            if self
                .0
                .one(
                    Query::select()
                        .column("id")
                        .from("items")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("archived").eq(0_i64))
                        .and_where(Expr::col("status").is_not_in(["done", "drop"]))
                        .and_where(Expr::col("id").is_in(batch.iter().map(String::as_str)))
                        .limit(1),
                    |_| Ok(()),
                )?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn existing_ids<'a>(
        &mut self,
        ws: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<std::collections::HashSet<String>> {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut existing = std::collections::HashSet::new();
        for batch in ids.chunks(500) {
            existing.extend(
                self.0.all(
                    Query::select()
                        .column("id")
                        .from("items")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("id").is_in(batch.iter().copied())),
                    |r| r.get::<String>(0),
                )?,
            );
        }
        Ok(existing)
    }

    pub fn open_ids<'a>(
        &mut self,
        ws: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<std::collections::HashSet<String>> {
        use sea_query::Cond;
        let phase = Expr::col("phase");
        Ok(self
            .0
            .all(
                Query::select()
                    .column("id")
                    .from("items")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("archived").eq(0_i64))
                    .cond_where(
                        Cond::any()
                            .add(phase.clone().is_null())
                            .add(phase.ne("closed")),
                    )
                    .and_where(Expr::col("id").is_in(ids)),
                |row| row.get(0),
            )?
            .into_iter()
            .collect())
    }

    pub fn title(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("title")
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }
}

impl TimelineRows<'_, crate::W<'_>> {
    pub fn add(&mut self, ws: &str, item: &str, actor: &str, kind: &str, text: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("timeline")
                .columns(["item_id", "ts", "actor", "kind", "text"])
                .values_panic([
                    item.into(),
                    crate::now().into(),
                    actor.into(),
                    kind.into(),
                    text.into(),
                ]),
        )?;
        self.0.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Item {
                ws: ws.into(),
                id: item.into(),
            },
        ));
        Ok(())
    }
}

impl<C: Read> TimezoneSettings<'_, C> {
    pub fn workspace(&mut self, ws: &str) -> Result<Option<String>> {
        Ok(WorkspaceSettings(self.0)
            .get(ws, "tz")?
            .and_then(|value| serde_json::from_str(&value).ok()))
    }

    pub fn person(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("timezone")
                    .from("notification_policies")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("user_id").eq(user)),
                |r| r.get::<Option<String>>(0),
            )?
            .flatten())
    }
}

impl<C: Read> ItemReads<'_, C> {
    pub fn status(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("status")
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn project_name(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("name")
                .from("projects")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn in_project(&mut self, ws: &str, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("id")
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("project_id").eq(id)),
            |r| r.get(0),
        )
    }
}

impl<C: Read> ItemReads<'_, C> {
    pub fn event_fields(&mut self, ws: &str, id: &str) -> Result<Option<ItemEventFields>> {
        self.0.one(
            Query::select()
                .columns(["title", "status", "bot_id", "project_id", "source"])
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |row| {
                Ok(ItemEventFields {
                    title: row.get(0)?,
                    status: row.get(1)?,
                    bot_id: row.get(2)?,
                    project_id: row.get(3)?,
                    source: row
                        .get::<Option<super::Json<_>>>(4)?
                        .map_or(serde_json::Value::Null, |json| json.0),
                })
            },
        )
    }

    pub fn changed_since(
        &mut self,
        ws: &str,
        since: i64,
        limit: u64,
    ) -> Result<Vec<(String, String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "title", "now"])
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("updated").gt(since))
                .and_where(Expr::col("archived").eq(0_i64))
                .order_by("updated", Order::Desc)
                .limit(limit),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }
}

pub struct NotificationPolicies<'a, C>(pub &'a mut C);

pub struct QuietPolicy {
    pub enabled: bool,
    pub mode: String,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl<C: Read> NotificationPolicies<'_, C> {
    pub fn quiet(&mut self, ws: &str, user: &str) -> Result<Option<QuietPolicy>> {
        self.0.one(
            Query::select()
                .columns(["enabled", "mode", "quiet_start", "quiet_end"])
                .from("notification_policies")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user)),
            |row| {
                Ok(QuietPolicy {
                    enabled: row.get(0)?,
                    mode: row.get(1)?,
                    start: row.get(2)?,
                    end: row.get(3)?,
                })
            },
        )
    }

    pub fn updated_between(
        &mut self,
        after: i64,
        through: i64,
    ) -> Result<Vec<(String, String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["workspace_id", "user_id", "updated"])
                .from("notification_policies")
                .and_where(Expr::col("updated").gt(after))
                .and_where(Expr::col("updated").lte(through)),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }
}

pub struct AttentionReads<'a, C>(pub &'a mut C);
pub struct FeedReads<'a, C>(pub &'a mut C);

impl<C: Read> AttentionReads<'_, C> {
    /// Open pending cards, excluding memory confirmations and waits on snoozed or archived items.
    pub fn pending_count(&mut self, ws: &str, user: &str, now: i64) -> Result<usize> {
        if crate::Members::new(self.0).role(ws, user)?.is_none() {
            return Err(crate::StoreError::NotFound("workspace member"));
        }
        let dialect = self.0.db().dialect();
        let item = Expr::col(("items", "id"));
        let snooze = Expr::col(("items", "snooze"));
        let until = sea_query::Func::coalesce([
            dialect.json_field(snooze.clone(), "until"),
            Expr::value(0_i64),
        ]);
        let label = sea_query::Func::coalesce([
            dialect.json_field(snooze.clone(), "label"),
            Expr::value(""),
        ]);
        let snoozed = dialect
            .json_valid(snooze)
            .and(until.gt(now))
            .and(label.ne(""));
        let memory_confirm = Expr::col(("decisions", "kind"))
            .eq("confirm")
            .and(Expr::col(("decisions", "subject_type")).eq("mem"));
        let count = self
            .0
            .one(
                Query::select()
                    .expr(Expr::col(("decisions", "id")).count())
                    .from("decisions")
                    .left_join(
                        "items",
                        item.clone().equals(("decisions", "item_id")).and(
                            Expr::col(("items", "workspace_id"))
                                .equals(("decisions", "workspace_id")),
                        ),
                    )
                    .and_where(Expr::col(("decisions", "workspace_id")).eq(ws))
                    .and_where(Expr::col(("decisions", "status")).eq("open"))
                    .and_where(memory_confirm.not())
                    .and_where(
                        item.is_null().or(Expr::col(("items", "archived"))
                            .eq(0_i64)
                            .and(snoozed.not())),
                    ),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or_default();
        Ok(count as usize)
    }
}

impl<C: Read> FeedReads<'_, C> {
    pub fn unread_since(
        &mut self,
        ws: &str,
        user: &str,
        since: i64,
        exclude: &str,
    ) -> Result<usize> {
        let read = Query::select()
            .column("entry_id")
            .from("feed_reads")
            .and_where(Expr::col("entry_id").equals(("feed_entries", "id")))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").eq(user))
            .take();
        let count = self
            .0
            .one(
                Query::select()
                    .expr(Expr::col("id").count())
                    .from("feed_entries")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("recipient_id").eq(user))
                    .and_where(Expr::col("published_at").gte(since))
                    .and_where(Expr::col("kind").ne(exclude))
                    .and_where(Expr::exists(read).not()),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or_default();
        Ok(count as usize)
    }
}

pub struct ItemStatus {
    pub id: String,
    pub title: String,
    pub status: String,
}

fn requested(ws: &str, user: &str) -> sea_query::SelectStatement {
    Query::select()
        .from("items")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("requested_by").eq(user))
        .take()
}

impl<C: Read> ItemReads<'_, C> {
    pub fn running_count(&mut self, ws: &str, user: &str) -> Result<usize> {
        Ok(self
            .0
            .one(
                requested(ws, user)
                    .expr(Expr::col("id").count())
                    .and_where(Expr::col("status").eq("run"))
                    .and_where(Expr::col("archived").eq(0_i64)),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or_default() as usize)
    }

    pub fn finished_since(&mut self, ws: &str, user: &str, since: i64) -> Result<usize> {
        Ok(self
            .0
            .one(
                requested(ws, user)
                    .expr(Expr::col("id").count())
                    .and_where(Expr::col("status").eq("done"))
                    .and_where(
                        sea_query::Func::coalesce([Expr::col("closed_at"), Expr::col("updated")])
                            .gte(since),
                    ),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or_default() as usize)
    }

    pub fn active_by_status(
        &mut self,
        ws: &str,
        user: &str,
        states: &[&str],
        limit: u64,
    ) -> Result<Vec<ItemStatus>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        self.0.all(
            requested(ws, user)
                .columns(["id", "title", "status"])
                .and_where(Expr::col("status").is_in(states.iter().copied()))
                .and_where(Expr::col("archived").eq(0_i64))
                .order_by("updated", sea_query::Order::Desc)
                .limit(limit),
            |r| {
                Ok(ItemStatus {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    status: r.get(2)?,
                })
            },
        )
    }
}

impl<C: Read> ItemReads<'_, C> {
    pub fn project_id(&mut self, ws: &str, item: &str) -> Result<Option<Option<String>>> {
        self.0.one(
            Query::select()
                .from("items")
                .column("project_id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(item)),
            |row| row.get(0),
        )
    }
}
