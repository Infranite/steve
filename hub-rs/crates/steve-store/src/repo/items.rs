//! Items with their goals, dependencies, timeline and artifacts.

use super::Cached;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Sets, all, file_refs, next_counter, one, revision, to_json};
use crate::types::{double, json_or};
use crate::{
    Artifact, FileRef, Goal, Item, Parent, PlanRound, PlanStep, Result, Snooze, Store,
    TimelineEntry, Workspace, now, uid,
};

impl Store {
    pub fn items(&self) -> Items<'_> {
        Items(self)
    }
    pub fn deps(&self) -> Deps<'_> {
        Deps(self)
    }
    pub fn timeline(&self) -> Timeline<'_> {
        Timeline(self)
    }
    pub fn artifacts(&self) -> Artifacts<'_> {
        Artifacts(self)
    }
}

/// `items.create` input. Defaults: kind `task`, status `run`, stage `定义`, source `["对话","chat"]`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewItem {
    pub title: String,
    pub goal: String,
    pub bot_id: String,
    pub project_id: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub stage: Option<String>,
    pub source: Option<(String, String)>,
    pub parent: Option<Parent>,
    /// Written in the same transaction; for a task item they must hold exactly one category
    /// ([`super::kinds::checked`]); empty or ambiguous task categories are refused.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The member who asked for it (`items.requested_by`): whose access its runs use. `None`: Steve,
    /// a flow, a schedule (the workspace's service principal).
    #[serde(default)]
    pub requested_by: Option<String>,
}

/// Patchable item fields. Deserializable so a stored decision `undo` (`{status, now, stage}`) can be applied.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct ItemPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<PlanStep>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub cwd: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub branch: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub accept_mode: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub bot_id: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub star: Option<bool>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub due: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub snooze: Option<Option<Snooze>>,
}

pub struct Items<'a>(&'a Store);

const ITEM: &str = "SELECT * FROM items WHERE id = ?";

impl Items<'_> {
    /// Direct descendants only, including archived/failed attempts. Parent links are scoped to
    /// the same workspace, including for old data that predates validation of those links.
    pub fn children(&self, parent: &str) -> Result<Vec<Item>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT i.* FROM items i JOIN items p ON p.id=? AND p.workspace_id=i.workspace_id
                 WHERE i.kind='task' AND json_extract(i.parent,'$.item')=p.id AND i.id<>p.id",
                [parent],
                Item::from_row,
            )
        })
    }

    /// Replayed after boot as well as on the lifecycle tick. The archive flag of the parent is
    /// irrelevant: closing it retired its unfinished descendants even if it was later archived.
    pub fn closed_parents(&self) -> Result<Vec<String>> {
        self.0.read_conn(|c| {
            all(c,
                "SELECT p.id FROM items p WHERE p.phase='closed' AND p.outcome IN ('completed','cancelled')
                 AND EXISTS(SELECT 1 FROM items i WHERE i.workspace_id=p.workspace_id
                     AND json_extract(i.parent,'$.item')=p.id AND i.id<>p.id)",
                [], |r| r.get(0))
        })
    }

    /// Task items (not chats), archived or not, most recently updated first.
    pub fn list(&self, ws: &str, archived: bool) -> Result<Vec<Item>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT * FROM items WHERE workspace_id = ? AND kind = 'task' AND archived = ? ORDER BY updated DESC",
                params![ws, archived as i64],
                Item::from_row,
            )
        })
    }

    /// Every task item of the workspace, archived included.
    pub fn all(&self, ws: &str) -> Result<Vec<Item>> {
        self.0.read_conn(|c| all(c, "SELECT * FROM items WHERE workspace_id = ? AND kind = 'task' ORDER BY updated DESC", [ws], Item::from_row))
    }

    pub fn get(&self, id: &str) -> Result<Option<Item>> {
        self.0.read_conn(|c| one(c, ITEM, [id], Item::from_row))
    }

    /// Task ids are `<item_prefix>-<n>` from the global `item:<prefix>` counter; chats get `chat_…` and no 0.
    /// Goal version 1 is written with it.
    pub fn create(&self, ws: &str, p: &NewItem) -> Result<Option<Item>> {
        self.0.write_conn(|w| {
            let kind = p.kind.as_deref().unwrap_or("task");
            let prefix = one(w.c, "SELECT * FROM workspaces WHERE id = ?", [ws], Workspace::from_row)?
                .map(|w| w.item_prefix)
                .unwrap_or_else(|| "ST".into());
            let task = kind == "task";
            /* checked before anything is written: a refused set leaves no item behind */
            let tags = if task {
                super::kinds::ensure(w, ws)?;
                super::kinds::checked(w.c, ws, &p.tags)?
            } else {
                p.tags.clone()
            };
            let transaction = w.tx()?;
            let tx = transaction.connection();
            let no = if task { next_counter(tx, &format!("item:{prefix}"))? } else { 0 };
            let id = if task { format!("{prefix}-{no}") } else { uid("chat") };
            let t = now();
            let source = match &p.source {
                Some(s) => to_json(s)?,
                None => r#"["对话","chat"]"#.to_string(),
            };
            let parent = p.parent.as_ref().map(to_json).transpose()?;
            tx.run(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, bot_id, project_id, source, parent, created, updated, requested_by) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    ws,
                    no,
                    kind,
                    p.title,
                    p.status.as_deref().unwrap_or("run"),
                    p.stage.as_deref().unwrap_or("定义"),
                    p.bot_id,
                    p.project_id,
                    source,
                    parent,
                    t,
                    t,
                    p.requested_by
                ],
            )?;
            super::put_item_tags(tx, &id, &tags)?;
            tx.run("INSERT INTO item_goals (item_id, version, text, created) VALUES (?, 1, ?, ?)", params![id, p.goal, t])?;
            if task {
                super::kinds::add_delivery_labels(
                    tx, ws, &id, &super::kinds::expected_deliveries(&p.title, &p.goal),
                )?;
                // stats: the item's first status
                status_mark(tx, ws, &id, t, None, p.status.as_deref().unwrap_or("run"))?;
            }
            let until = crate::diagnostics::capture_created(tx, ws, &id, t);
            transaction.commit()?;
            if let Some(until) = until {
                self.0.stage_diagnostic_until(until);
            }
            w.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: ws.into(),
                    id: id.clone().into(),
                },
            ));
            one(w.c, ITEM, [&id], Item::from_row)
        })
    }

    /// Sets the given fields and `updated`; an empty patch changes nothing and publishes nothing.
    pub fn patch(&self, id: &str, p: &ItemPatch) -> Result<Option<Item>> {
        self.0.write_conn(|w| {
            // stats: a status change leaves exactly one mark, in the same transaction as the change
            let transaction = w.tx()?;
            let tx = transaction.connection();
            let changed = patch_on(tx, id, p)?;
            transaction.commit()?;
            if changed {
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM items WHERE id = ?",
                    [id],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Item {
                            ws: ws.into(),
                            id: id.into(),
                        },
                    ));
                }
            }
            one(w.c, ITEM, [id], Item::from_row)
        })
    }

    /// Switches the item's design gate. Kept out of [`ItemPatch`] on purpose: no general item write
    /// (an agent's update, a module, a stored undo) can turn it, only the owner's own request. `None`
    /// when there is no such item; an unchanged value bumps and publishes nothing.
    pub fn set_design_gate(&self, id: &str, on: bool) -> Result<Option<Item>> {
        self.0.write_conn(|w| {
            let n = w.c.run(
                "UPDATE items SET design_gate = ?, updated = ? WHERE id = ? AND design_gate != ?",
                params![on as i64, now(), id, on as i64],
            )?;
            if n > 0 {
                if let Some(ws) = crate::repo::one(
                    w.c,
                    "SELECT workspace_id FROM items WHERE id = ?",
                    [id],
                    |r| r.get::<_, String>(0),
                )? {
                    w.publish(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Item {
                            ws: ws.into(),
                            id: id.into(),
                        },
                    ));
                }
            }
            one(w.c, ITEM, [id], Item::from_row)
        })
    }

    /// Deletes the item and what hangs off it; sessions and decisions are kept but unlinked.
    pub fn remove(&self, id: &str) -> Result<()> {
        use crate::contract::Q;
        use sea_query::{Expr, ExprTrait, Query};

        self.0.write(|w| {
            let item = || Expr::col("item_id").eq(id);
            w.exec(
                Query::update()
                    .table("sessions")
                    .value("item_id", None::<String>)
                    .value("no", 0_i64)
                    .and_where(item()),
            )?;
            let artifacts = Query::select()
                .column("id")
                .from("artifacts")
                .and_where(item())
                .take();
            w.exec(
                Query::delete()
                    .from_table("artifact_roles")
                    .and_where(Expr::col("artifact_id").in_subquery(artifacts)),
            )?;
            for table in [
                "item_tags",
                "item_goals",
                "timeline",
                "annotations",
                "artifacts",
                "thread_items",
            ] {
                w.exec(Query::delete().from_table(table).and_where(item()))?;
            }
            w.exec(
                Query::delete()
                    .from_table("item_deps")
                    .and_where(item().or(Expr::col("dep_id").eq(id))),
            )?;
            w.exec(
                Query::update()
                    .table("decisions")
                    .value("item_id", None::<String>)
                    .and_where(item()),
            )?;
            let ws: Option<String> = w.one(
                Query::select()
                    .column("workspace_id")
                    .from("items")
                    .and_where(Expr::col("id").eq(id)),
                |row| row.get(0),
            )?;
            w.exec(
                Query::delete()
                    .from_table("items")
                    .and_where(Expr::col("id").eq(id)),
            )?;
            if let Some(ws) = ws {
                w.emit(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Item {
                        ws: ws.clone().into(),
                        id: id.into(),
                    },
                ));
                if let Some(hook) = self.0.item_deleted_hook() {
                    hook(w, &ws, id).map_err(crate::StoreError::Hook)?;
                }
            }
            Ok(())
        })
    }

    /// Goal history, oldest first.
    pub fn goals(&self, id: &str) -> Result<Vec<Goal>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT version, text, created FROM item_goals WHERE item_id = ? ORDER BY version",
                [id],
                |r| {
                    Ok(Goal {
                        version: r.get(0)?,
                        text: r.get(1)?,
                        created: r.get(2)?,
                    })
                },
            )
        })
    }

    /// The current goal text ("" if none).
    pub fn goal(&self, id: &str) -> Result<String> {
        self.0.read_conn(|c| {
            Ok(one(
                c,
                "SELECT text FROM item_goals WHERE item_id = ? ORDER BY version DESC LIMIT 1",
                [id],
                |r| r.get(0),
            )?
            .unwrap_or_default())
        })
    }

    /// Earlier plans, oldest first.
    pub fn plan_rounds(&self, id: &str) -> Result<Vec<PlanRound>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT version, patch, created FROM revisions WHERE object_type = 'item_plan' AND object_id = ? ORDER BY version",
                [id],
                |r| {
                    let v: serde_json::Value = json_or(r.get(1)?, serde_json::Value::Null);
                    Ok(PlanRound {
                        round: r.get(0)?,
                        steps: serde_json::from_value(v["steps"].clone()).unwrap_or_default(),
                        end: v["end"].as_str().unwrap_or("replaced").to_string(),
                        created: r.get(2)?,
                    })
                },
            )
        })
    }

    /// Keeps a plan that is being replaced (`end`: `replaced` or `done`); returns its round number.
    pub fn add_plan_round(
        &self,
        ws: &str,
        id: &str,
        steps: &[PlanStep],
        end: &str,
        author: &str,
    ) -> Result<i64> {
        self.0.write_conn(|w| {
            let last: Option<i64> = one(
                w.c,
                "SELECT MAX(version) FROM revisions WHERE object_type = 'item_plan' AND object_id = ?",
                [id],
                |r| r.get(0),
            )?
            .flatten();
            let v = last.unwrap_or(0) + 1;
            revision(w.c, ws, "item_plan", id, v, &serde_json::json!({ "steps": steps, "end": end }), author)?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: ws.into(),
                    id: id.into(),
                },
            ));
            Ok(v)
        })
    }

    /// Appends a goal version; returns it.
    pub fn add_goal(&self, id: &str, text: &str) -> Result<i64> {
        self.0.write_conn(|w| {
            let last: Option<i64> = one(
                w.c,
                "SELECT version FROM item_goals WHERE item_id = ? ORDER BY version DESC LIMIT 1",
                [id],
                |r| r.get(0),
            )?;
            let v = last.unwrap_or(0) + 1;
            w.c.run(
                "INSERT INTO item_goals (item_id, version, text, created) VALUES (?, ?, ?, ?)",
                params![id, v, text, now()],
            )?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM items WHERE id = ?",
                [id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Item {
                        ws: ws.into(),
                        id: id.into(),
                    },
                ));
            }
            Ok(v)
        })
    }
}

pub struct Deps<'a>(&'a Store);

impl Deps<'_> {
    /// What `item` waits for, in the order they were added.
    pub fn of(&self, item: &str) -> Result<Vec<String>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT dep_id FROM item_deps WHERE item_id = ? ORDER BY created",
                [item],
                |r| r.get(0),
            )
        })
    }

    /// Items waiting for `item`.
    pub fn dependents(&self, item: &str) -> Result<Vec<String>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT item_id FROM item_deps WHERE dep_id = ?",
                [item],
                |r| r.get(0),
            )
        })
    }

    pub fn add(&self, item: &str, dep: &str) -> Result<()> {
        self.0.write(|tx| {
            let added = tx.conn.execute(
                "INSERT OR IGNORE INTO item_deps (item_id, dep_id, created) VALUES (?, ?, ?)",
                params![item, dep, now()],
            )?;
            if added > 0 {
                super::dependency_waits::rearm_in(tx, item)?;
            }
            if let Some(ws) = crate::repo::one(
                tx.conn,
                "SELECT workspace_id FROM items WHERE id = ?",
                [item],
                |r| r.get::<_, String>(0),
            )? {
                tx.emit(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Item {
                        ws: ws.into(),
                        id: item.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    pub fn remove(&self, item: &str, dep: &str) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "DELETE FROM item_deps WHERE item_id = ? AND dep_id = ?",
                [item, dep],
            )?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM items WHERE id = ?",
                [item],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Item {
                        ws: ws.into(),
                        id: item.into(),
                    },
                ));
            }
            Ok(())
        })
    }
}

pub struct Timeline<'a>(&'a Store);

impl Timeline<'_> {
    pub fn add(
        &self,
        item_id: &str,
        actor: &str,
        kind: &str,
        text: &str,
        session_id: Option<&str>,
        files: &[FileRef],
    ) -> Result<()> {
        self.0
            .write(|w| timeline_add(w, item_id, actor, kind, text, session_id, files))
    }

    pub fn list(&self, item_id: &str) -> Result<Vec<TimelineEntry>> {
        self.0.read_conn(|c| {
            all(c, "SELECT id, ts, actor, kind, text, session_id, files FROM timeline WHERE item_id = ? ORDER BY id", [item_id], |r| {
                Ok(TimelineEntry {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    actor: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    session_id: r.get(5)?,
                    files: json_or(r.get(6)?, Vec::new()),
                })
            })
        })
    }
}

pub struct Artifacts<'a>(&'a Store);

impl Artifacts<'_> {
    pub fn add(
        &self,
        item_id: &str,
        kind: &str,
        title: &str,
        location: Option<&str>,
    ) -> Result<()> {
        self.add_as(item_id, kind, title, location, None)
    }

    /// `role`: `record` for an output that documents the work (a self-check screenshot, a log) rather than
    /// being its result, `result` for one said to be the result; one the agent said nothing about gets no row
    /// (the page then judges it by its title, as it does outputs from before roles).
    pub fn add_as(
        &self,
        item_id: &str,
        kind: &str,
        title: &str,
        location: Option<&str>,
        role: Option<&str>,
    ) -> Result<()> {
        self.0
            .write(|w| artifact_add_in(w, item_id, kind, title, location, role))
    }

    /// `(artifact id, role)` of the item's outputs the agent said a role for.
    pub fn roles(&self, item_id: &str) -> Result<Vec<(String, String)>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT r.artifact_id, r.role FROM artifact_roles r JOIN artifacts a ON a.id = r.artifact_id WHERE a.item_id = ? ORDER BY a.created, a.rowid",
                [item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
    }

    pub fn list(&self, item_id: &str) -> Result<Vec<Artifact>> {
        self.0.read_conn(|c| {
            all(
                c,
                // Only an exact URL registered by this Hub can associate an artifact. Checking
                // both item and its current workspace also invalidates stale associations after
                // an item move. Old paths and external URLs remain ordinary legacy artifacts.
                "SELECT a.*, (SELECT d.id FROM deliveries d JOIN items i ON i.id=a.item_id AND i.workspace_id=d.workspace_id WHERE d.url=a.location AND d.item_id=a.item_id) AS delivery_id FROM artifacts a WHERE a.item_id = ? ORDER BY a.created, a.rowid",
                [item_id],
                |r| {
                    Ok(Artifact {
                        id: r.get("id")?,
                        item_id: r.get("item_id")?,
                        kind: r.get("kind")?,
                        title: r.get("title")?,
                        location: r.get("location")?,
                        delivery_id: r.get("delivery_id")?,
                        created: r.get("created")?,
                    })
                },
            )
        })
    }
}

/// Adds an artifact and its role inside the caller's transaction. Collaboration uses this with its
/// durable unit result and the child's closing intent, so all delivery evidence commits together.
pub fn artifact_add_in(
    w: &mut crate::W<'_>,
    item_id: &str,
    kind: &str,
    title: &str,
    location: Option<&str>,
    role: Option<&str>,
) -> Result<()> {
    use sea_query::Query;
    let id = uid("art");
    w.exec(
        Query::insert()
            .into_table("artifacts")
            .columns(["id", "item_id", "kind", "title", "location", "created"])
            .values_panic([
                id.as_str().into(),
                item_id.into(),
                kind.into(),
                title.into(),
                location.into(),
                now().into(),
            ]),
    )?;
    if let Some(role) = role.filter(|r| ["record", "result"].contains(r)) {
        w.exec(
            Query::insert()
                .into_table("artifact_roles")
                .columns(["artifact_id", "role"])
                .values_panic([id.as_str().into(), role.into()]),
        )?;
    }
    let workspace = crate::item_repo::item_workspace(w, item_id)?
        .ok_or(crate::StoreError::NotFound("没有这个事项"))?;
    super::kinds::add_delivery_labels(
        w.conn,
        &workspace,
        item_id,
        &super::kinds::output_deliveries(kind, title, location, role),
    )?;
    w.emit(steve_types::WorkEvent::Item {
        ws: workspace.into(),
        id: item_id.into(),
    });
    Ok(())
}

/// The `item_marks` row for a status change; the actor is unknown at this layer (`""`).
pub(super) fn status_mark(
    c: &rusqlite::Connection,
    ws: &str,
    item: &str,
    ts: i64,
    from: Option<&str>,
    to: &str,
) -> Result<()> {
    super::stats::mark_in(
        c,
        &crate::NewMark {
            workspace_id: ws.into(),
            item_id: item.into(),
            ts,
            kind: "status".into(),
            from_v: from.map(Into::into),
            to_v: Some(to.into()),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// [`Items::patch`] inside the caller's transaction (the event goes out after its commit).
pub fn item_patch_in(tx: &mut crate::W<'_>, id: &str, p: &ItemPatch) -> Result<Option<Item>> {
    if patch_on(tx.conn, id, p)? {
        if let Some(ws) = crate::repo::one(
            tx.conn,
            "SELECT workspace_id FROM items WHERE id = ?",
            [id],
            |r| r.get::<_, String>(0),
        )? {
            tx.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: ws.into(),
                    id: id.into(),
                },
            ));
        }
    }
    one(tx.conn, ITEM, [id], Item::from_row)
}

/// The patch's writes on a connection already in a transaction; `false` for an empty patch.
fn patch_on(c: &rusqlite::Connection, id: &str, p: &ItemPatch) -> Result<bool> {
    let plan = p.plan.as_ref().map(to_json).transpose()?;
    let snooze = match &p.snooze {
        None => None,
        Some(None) => Some(None),
        Some(Some(s)) => Some(Some(to_json(s)?)),
    };
    let mut s = Sets::default();
    s.val("title", p.title.clone())
        .val("status", p.status.clone())
        .val("stage", p.stage.clone())
        .val("now", p.now.clone())
        .val("plan", plan)
        .null("cwd", p.cwd.clone())
        .null("branch", p.branch.clone())
        .val("priority", p.priority.clone())
        .val("archived", p.archived)
        .null("accept_mode", p.accept_mode.clone())
        .null("bot_id", p.bot_id.clone())
        .null("project_id", p.project_id.clone())
        .val("star", p.star)
        .null("due", p.due.clone())
        .null("snooze", snooze);
    if s.is_empty() {
        return Ok(false);
    }
    // the lifecycle owns the terminal statuses and what a closed item is: nobody patches those
    if let Some(st) = p.status.as_deref() {
        if matches!(st, "done" | "drop" | "fail") {
            return Err(crate::StoreError::Conflict(steve_i18n::t!(
                "store.item.end_by_lifecycle",
                status = st
            )));
        }
        let phase: Option<String> = one(c, "SELECT phase FROM items WHERE id = ?", [id], |r| {
            r.get(0)
        })?;
        if phase.as_deref() == Some("closed") {
            return Err(crate::StoreError::Conflict(steve_i18n::t!(
                "store.item.closed"
            )));
        }
    }
    let before: Option<(String, String, String)> = match &p.status {
        Some(_) => one(
            c,
            "SELECT workspace_id, kind, status FROM items WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?,
        None => None,
    };
    let t = now();
    c.run(
        &format!("UPDATE items SET {}, updated = ? WHERE id = ?", s.sql()),
        rusqlite::params_from_iter(s.params([t.into(), id.to_string().into()])),
    )?;
    if let (Some((ws, kind, old)), Some(new)) = (before, p.status.as_deref())
        && kind == "task"
        && old != new
    {
        status_mark(c, &ws, id, t, Some(&old), new)?;
    }
    Ok(true)
}

/// [`Timeline::add`] (no attachments) inside the caller's transaction.
pub fn timeline_add_in(
    w: &mut crate::W<'_>,
    item_id: &str,
    actor: &str,
    kind: &str,
    text: &str,
    session_id: Option<&str>,
) -> Result<()> {
    timeline_add(w, item_id, actor, kind, text, session_id, &[])
}

fn timeline_add(
    w: &mut crate::W<'_>,
    item_id: &str,
    actor: &str,
    kind: &str,
    text: &str,
    session_id: Option<&str>,
    files: &[FileRef],
) -> Result<()> {
    crate::item_repo::store_timeline(
        w,
        item_id,
        actor,
        kind,
        text,
        session_id,
        file_refs(files)?.as_deref(),
        now(),
    )?;
    if let Some(ws) = crate::item_repo::item_workspace(w, item_id)? {
        w.emit(steve_types::WorkEvent::Item {
            ws: ws.into(),
            id: item_id.into(),
        });
    }
    Ok(())
}

/// What the core's lifecycle writes in one step: the phase and its fields, and the legacy projection
/// (`status`, `stage`, `now`) that follows from them. The only writer of these columns.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LifecycleWrite {
    pub phase: String,
    pub cycle: i64,
    pub revision: i64,
    pub outcome: Option<String>,
    pub acceptance: Option<String>,
    pub closing: Option<serde_json::Value>,
    pub closed_at: Option<i64>,
    pub closed_reason: Option<String>,
    /// Legacy projection; `None` leaves the column as it is.
    pub status: Option<String>,
    pub stage: Option<String>,
    pub now: Option<String>,
}

/// Checks the owner's precondition under the same write lock as the cycle commit.
pub fn require_item_revision_in(
    w: &mut crate::W<'_>,
    id: &str,
    cycle: i64,
    revision: i64,
) -> Result<()> {
    let matches: bool = w.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE id=? AND cycle=? AND revision=? AND phase<>'closed')",
        params![id, cycle, revision],
        |r| r.get(0),
    )?;
    if !matches {
        return Err(crate::StoreError::Conflict("item revision changed".into()));
    }
    Ok(())
}

/// Retires undispatched work with a finished cycle. Keep the person's queued text and files in
/// dispatch history, and leave already submitted work to its original receipt.
pub fn finish_item_dispatch_in(w: &mut crate::W<'_>, id: &str, cycle: i64) -> Result<()> {
    let sessions = all(
        w.conn,
        "SELECT id FROM sessions WHERE item_id=?",
        [id],
        |r| r.get::<_, String>(0),
    )?;
    let at = now();
    for session in sessions {
        w.conn.execute(
            "INSERT INTO session_dispatch (id,session_id,text,via,created,files,state,updated)
             SELECT id,session_id,text,via,created,files,'cancelled',? FROM queued
             WHERE session_id=? AND via NOT IN ('steve','resumed:steve')
             ON CONFLICT(id) DO UPDATE SET state='cancelled',updated=excluded.updated
             WHERE session_dispatch.state='claimed'",
            params![at, session],
        )?;
        super::sessions::discard_queue_in(w, &session)?;
        w.conn
            .execute("DELETE FROM session_resume WHERE session_id=?", [&session])?;
    }
    w.conn.execute(
        "DELETE FROM item_dependency_waits WHERE item_id=? AND cycle=? AND state='waiting'",
        params![id, cycle],
    )?;
    w.conn.execute(
        "UPDATE item_operations SET status='waived', required=0, updated=?
         WHERE item_id=? AND cycle=? AND status='pending'",
        params![at, id, cycle],
    )?;
    Ok(())
}

/// Writes one lifecycle step of `id` inside the caller's transaction (stats mark a status change as
/// [`Items::patch`] does). Returns the item as written.
pub fn lifecycle_write_in(
    w: &mut crate::W<'_>,
    id: &str,
    step: &LifecycleWrite,
) -> Result<Option<Item>> {
    use crate::contract::Q;
    use sea_query::{Expr, ExprTrait, Query};

    let before: Option<(String, String, String)> = w.one(
        Query::select()
            .columns(["workspace_id", "kind", "status"])
            .from("items")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let Some((ws, kind, old_status)) = before else {
        return Ok(None);
    };
    let closing = step.closing.as_ref().map(to_json).transpose()?;
    let at = now();
    let mut update = Query::update();
    update
        .table("items")
        .values([
            ("phase", step.phase.as_str().into()),
            ("cycle", step.cycle.into()),
            ("revision", step.revision.into()),
            ("outcome", step.outcome.as_deref().into()),
            ("acceptance", step.acceptance.as_deref().into()),
            ("closing", closing.into()),
            ("closed_at", step.closed_at.into()),
            ("closed_reason", step.closed_reason.as_deref().into()),
            ("updated", at.into()),
        ])
        .and_where(Expr::col("id").eq(id));
    for (column, value) in [
        ("status", &step.status),
        ("stage", &step.stage),
        ("now", &step.now),
    ] {
        if let Some(value) = value {
            update.value(column, value.as_str());
        }
    }
    w.exec(&update)?;
    if let Some(new_status) = step.status.as_deref()
        && kind == "task"
        && old_status != new_status
    {
        status_mark(w.conn, &ws, id, at, Some(&old_status), new_status)?;
    }
    w.emit(steve_types::DomainEvent::from(
        steve_types::WorkEvent::Item {
            ws: ws.into(),
            id: id.into(),
        },
    ));
    one(w.conn, ITEM, [id], Item::from_row)
}

/// Records a cycle that ended; `false` when this cycle was already recorded (a cycle closes once).
#[allow(clippy::too_many_arguments)]
pub fn item_cycle_add_in(
    w: &mut crate::W<'_>,
    item_id: &str,
    cycle: i64,
    revision: i64,
    outcome: &str,
    acceptance: Option<&str>,
    reason: Option<&str>,
    closed: i64,
) -> Result<bool> {
    use sea_query::{OnConflict, Query};

    let n = w.exec(
        Query::insert()
            .into_table("item_cycles")
            .columns([
                "item_id",
                "cycle",
                "revision",
                "outcome",
                "acceptance",
                "reason",
                "closed",
                "source",
            ])
            .values_panic([
                item_id.into(),
                cycle.into(),
                revision.into(),
                outcome.into(),
                acceptance.into(),
                reason.into(),
                closed.into(),
                "live".into(),
            ])
            .on_conflict(
                OnConflict::columns(["item_id", "cycle"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    Ok(n == 1)
}

/// One ended cycle of an item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemCycle {
    pub cycle: i64,
    pub revision: i64,
    pub outcome: String,
    pub acceptance: Option<String>,
    pub reason: Option<String>,
    pub closed: i64,
    pub source: String,
}

impl Items<'_> {
    /// The ended cycles of an item, oldest first.
    pub fn cycles(&self, id: &str) -> Result<Vec<ItemCycle>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT cycle, revision, outcome, acceptance, reason, closed, source FROM item_cycles WHERE item_id = ? ORDER BY cycle",
                [id],
                |r| {
                    Ok(ItemCycle {
                        cycle: r.get(0)?,
                        revision: r.get(1)?,
                        outcome: r.get(2)?,
                        acceptance: r.get(3)?,
                        reason: r.get(4)?,
                        closed: r.get(5)?,
                        source: r.get(6)?,
                    })
                },
            )
        })
    }

    /// Items of every workspace whose closing waits to be committed.
    pub fn closing_ids(&self) -> Result<Vec<String>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT id FROM items WHERE phase = 'closing' ORDER BY updated",
                [],
                |r| r.get(0),
            )
        })
    }
}

impl Items<'_> {
    /// The operations of one cycle of an item, oldest first.
    pub fn operations(&self, item_id: &str, cycle: i64) -> Result<Vec<crate::ItemOperation>> {
        self.0
            .read(|r| crate::operation_repo::for_cycle(r, item_id, cycle))
    }

    pub fn operation(&self, id: &str) -> Result<Option<crate::ItemOperation>> {
        self.0.read(|r| crate::operation_repo::get(r, id))
    }

    /// Every operation of `kind` in `status`, oldest first.
    pub fn operations_with(&self, kind: &str, status: &str) -> Result<Vec<crate::ItemOperation>> {
        self.0
            .read(|r| crate::operation_repo::with_status(r, kind, status))
    }
}
