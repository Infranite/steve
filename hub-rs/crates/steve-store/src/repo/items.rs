//! Items with their goals, dependencies, timeline and artifacts.

use super::Cached;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Sets, all, file_refs, next_counter, one, revision, to_json};
use crate::types::{double, json_or};
use crate::{
    Artifact, ChangeEvent, FileRef, Goal, Item, Parent, PlanRound, PlanStep, Result, Snooze, Store,
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
    /// ([`super::kinds::checked`]). The domain layer always gives them; empty writes none.
    #[serde(default)]
    pub tags: Vec<String>,
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
    /// Task items (not chats), archived or not, most recently updated first.
    pub fn list(&self, ws: &str, archived: bool) -> Result<Vec<Item>> {
        self.0.read(|c| {
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
        self.0.read(|c| all(c, "SELECT * FROM items WHERE workspace_id = ? AND kind = 'task' ORDER BY updated DESC", [ws], Item::from_row))
    }

    pub fn get(&self, id: &str) -> Result<Option<Item>> {
        self.0.read(|c| one(c, ITEM, [id], Item::from_row))
    }

    /// Task ids are `<item_prefix>-<n>` from the global `item:<prefix>` counter; chats get `chat_…` and no 0.
    /// Goal version 1 is written with it.
    pub fn create(&self, ws: &str, p: &NewItem) -> Result<Option<Item>> {
        self.0.write(|w| {
            let kind = p.kind.as_deref().unwrap_or("task");
            let prefix = one(w.c, "SELECT * FROM workspaces WHERE id = ?", [ws], Workspace::from_row)?
                .map(|w| w.item_prefix)
                .unwrap_or_else(|| "ST".into());
            let task = kind == "task";
            /* checked before anything is written: a refused set leaves no item behind */
            let tags = if task && !p.tags.is_empty() {
                super::kinds::ensure(w, ws)?;
                super::kinds::checked(w.c, ws, &p.tags)?
            } else {
                p.tags.clone()
            };
            let no = if task { next_counter(w.c, &format!("item:{prefix}"))? } else { 0 };
            let id = if task { format!("{prefix}-{no}") } else { uid("chat") };
            let t = now();
            let source = match &p.source {
                Some(s) => to_json(s)?,
                None => r#"["对话","chat"]"#.to_string(),
            };
            let parent = p.parent.as_ref().map(to_json).transpose()?;
            w.c.run(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, bot_id, project_id, source, parent, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                    t
                ],
            )?;
            super::put_item_tags(w.c, &id, &tags)?;
            w.c.run("INSERT INTO item_goals (item_id, version, text, created) VALUES (?, 1, ?, ?)", params![id, p.goal, t])?;
            if task {
                // stats: the item's first status (modules/stats.md §2.5)
                status_mark(w.c, ws, &id, t, None, p.status.as_deref().unwrap_or("run"))?;
            }
            w.publish(ChangeEvent::Item { id: id.clone() });
            one(w.c, ITEM, [&id], Item::from_row)
        })
    }

    /// Sets the given fields and `updated`; an empty patch changes nothing and publishes nothing.
    pub fn patch(&self, id: &str, p: &ItemPatch) -> Result<Option<Item>> {
        self.0.write(|w| {
            // stats: a status change leaves exactly one mark, in the same transaction as the change
            let tx = w.c.unchecked_transaction()?;
            let changed = patch_on(&tx, id, p)?;
            tx.commit()?;
            if changed {
                w.publish(ChangeEvent::Item { id: id.into() });
            }
            one(w.c, ITEM, [id], Item::from_row)
        })
    }

    /// Deletes the item and what hangs off it; sessions and decisions are kept but unlinked.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
            tx.run(
                "UPDATE sessions SET item_id = NULL, no = 0 WHERE item_id = ?",
                [id],
            )?;
            tx.run(
                "DELETE FROM artifact_roles WHERE artifact_id IN (SELECT id FROM artifacts WHERE item_id = ?)",
                [id],
            )?;
            for t in [
                "item_tags",
                "item_goals",
                "timeline",
                "annotations",
                "artifacts",
                "thread_items",
            ] {
                tx.run(&format!("DELETE FROM {t} WHERE item_id = ?"), [id])?;
            }
            tx.run(
                "DELETE FROM item_deps WHERE item_id = ? OR dep_id = ?",
                [id, id],
            )?;
            tx.run(
                "UPDATE decisions SET item_id = NULL WHERE item_id = ?",
                [id],
            )?;
            let ws: Option<String> = {
                use rusqlite::OptionalExtension;
                tx.query_row("SELECT workspace_id FROM items WHERE id = ?", [id], |r| {
                    r.get(0)
                })
                .optional()?
            };
            tx.run("DELETE FROM items WHERE id = ?", [id])?;
            let mut extra = Vec::new();
            if let (Some(hook), Some(ws)) = (self.0.item_deleted_hook(), ws) {
                let t = crate::Tx::wrap(&tx);
                hook(&t, &ws, id).map_err(crate::StoreError::Hook)?;
                extra = t.take_events();
            }
            tx.commit()?;
            w.publish(ChangeEvent::Item { id: id.into() });
            for e in extra {
                w.publish(e);
            }
            Ok(())
        })
    }

    /// Goal history, oldest first.
    pub fn goals(&self, id: &str) -> Result<Vec<Goal>> {
        self.0.read(|c| {
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
        self.0.read(|c| {
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
        self.0.read(|c| {
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
        self.0.write(|w| {
            let last: Option<i64> = one(
                w.c,
                "SELECT MAX(version) FROM revisions WHERE object_type = 'item_plan' AND object_id = ?",
                [id],
                |r| r.get(0),
            )?
            .flatten();
            let v = last.unwrap_or(0) + 1;
            revision(w.c, ws, "item_plan", id, v, &serde_json::json!({ "steps": steps, "end": end }), author)?;
            w.publish(ChangeEvent::Item { id: id.into() });
            Ok(v)
        })
    }

    /// Appends a goal version; returns it.
    pub fn add_goal(&self, id: &str, text: &str) -> Result<i64> {
        self.0.write(|w| {
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
            w.publish(ChangeEvent::Item { id: id.into() });
            Ok(v)
        })
    }
}

pub struct Deps<'a>(&'a Store);

impl Deps<'_> {
    /// What `item` waits for, in the order they were added.
    pub fn of(&self, item: &str) -> Result<Vec<String>> {
        self.0.read(|c| {
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
        self.0.read(|c| {
            all(
                c,
                "SELECT item_id FROM item_deps WHERE dep_id = ?",
                [item],
                |r| r.get(0),
            )
        })
    }

    pub fn add(&self, item: &str, dep: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "INSERT OR IGNORE INTO item_deps (item_id, dep_id, created) VALUES (?, ?, ?)",
                params![item, dep, now()],
            )?;
            w.publish(ChangeEvent::Item { id: item.into() });
            Ok(())
        })
    }

    pub fn remove(&self, item: &str, dep: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "DELETE FROM item_deps WHERE item_id = ? AND dep_id = ?",
                [item, dep],
            )?;
            w.publish(ChangeEvent::Item { id: item.into() });
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
        self.0.write(|w| {
            timeline_on(w.c, item_id, actor, kind, text, session_id, files)?;
            w.publish(ChangeEvent::Item { id: item_id.into() });
            Ok(())
        })
    }

    pub fn list(&self, item_id: &str) -> Result<Vec<TimelineEntry>> {
        self.0.read(|c| {
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
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
            let id = uid("art");
            tx.run(
                "INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES (?, ?, ?, ?, ?, ?)",
                params![id, item_id, kind, title, location, now()],
            )?;
            if let Some(role) = role.filter(|r| ["record", "result"].contains(r)) {
                tx.run(
                    "INSERT INTO artifact_roles (artifact_id, role) VALUES (?, ?)",
                    [&id, role],
                )?;
            }
            tx.commit()?;
            w.publish(ChangeEvent::Item { id: item_id.into() });
            Ok(())
        })
    }

    /// `(artifact id, role)` of the item's outputs the agent said a role for.
    pub fn roles(&self, item_id: &str) -> Result<Vec<(String, String)>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT r.artifact_id, r.role FROM artifact_roles r JOIN artifacts a ON a.id = r.artifact_id WHERE a.item_id = ? ORDER BY a.created, a.rowid",
                [item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
    }

    pub fn list(&self, item_id: &str) -> Result<Vec<Artifact>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM artifacts WHERE item_id = ? ORDER BY created, rowid",
                [item_id],
                |r| {
                    Ok(Artifact {
                        id: r.get("id")?,
                        item_id: r.get("item_id")?,
                        kind: r.get("kind")?,
                        title: r.get("title")?,
                        location: r.get("location")?,
                        created: r.get("created")?,
                    })
                },
            )
        })
    }
}

/// The `item_marks` row for a status change; the actor is unknown at this layer (`""`).
fn status_mark(
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
pub fn item_patch_in(tx: &crate::Tx, id: &str, p: &ItemPatch) -> Result<Option<Item>> {
    if patch_on(tx, id, p)? {
        tx.publish(ChangeEvent::Item { id: id.into() });
    }
    one(tx, ITEM, [id], Item::from_row)
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
            return Err(crate::StoreError::Conflict(format!(
                "事项的结束只能由生命周期提交（{st}）"
            )));
        }
        let phase: Option<String> = one(c, "SELECT phase FROM items WHERE id = ?", [id], |r| {
            r.get(0)
        })?;
        if phase.as_deref() == Some("closed") {
            return Err(crate::StoreError::Conflict(
                "这个事项已经结束了，需要时先新建周期".into(),
            ));
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
    tx: &crate::Tx,
    item_id: &str,
    actor: &str,
    kind: &str,
    text: &str,
    session_id: Option<&str>,
) -> Result<()> {
    timeline_on(tx, item_id, actor, kind, text, session_id, &[])?;
    tx.publish(ChangeEvent::Item { id: item_id.into() });
    Ok(())
}

fn timeline_on(
    c: &rusqlite::Connection,
    item_id: &str,
    actor: &str,
    kind: &str,
    text: &str,
    session_id: Option<&str>,
    files: &[FileRef],
) -> Result<()> {
    // V4: a session's injected values never reach the timeline (agent text, tool output)
    let text = match session_id {
        Some(sid) => steve_vault::redact::scrub_text(sid, text),
        None => std::borrow::Cow::Borrowed(text),
    };
    c.prepare_cached("INSERT INTO timeline (item_id, ts, actor, kind, text, session_id, files) VALUES (?, ?, ?, ?, ?, ?, ?)")?
        .execute(params![item_id, now(), actor, kind, text.as_ref(), session_id, file_refs(files)?])?;
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

/// Writes one lifecycle step of `id` inside the caller's transaction (stats mark a status change as
/// [`Items::patch`] does). Returns the item as written.
pub fn lifecycle_write_in(tx: &crate::Tx, id: &str, w: &LifecycleWrite) -> Result<Option<Item>> {
    let before: Option<(String, String, String)> = one(
        tx,
        "SELECT workspace_id, kind, status FROM items WHERE id = ?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let Some((ws, kind, old)) = before else {
        return Ok(None);
    };
    let closing = w.closing.as_ref().map(to_json).transpose()?;
    let t = now();
    tx.run(
        "UPDATE items SET phase = ?, cycle = ?, revision = ?, outcome = ?, acceptance = ?, closing = ?,
            closed_at = ?, closed_reason = ?, status = coalesce(?, status), stage = coalesce(?, stage),
            now = coalesce(?, now), updated = ? WHERE id = ?",
        params![
            w.phase,
            w.cycle,
            w.revision,
            w.outcome,
            w.acceptance,
            closing,
            w.closed_at,
            w.closed_reason,
            w.status,
            w.stage,
            w.now,
            t,
            id
        ],
    )?;
    if let Some(new) = w.status.as_deref()
        && kind == "task"
        && old != new
    {
        status_mark(tx, &ws, id, t, Some(&old), new)?;
    }
    tx.publish(ChangeEvent::Item { id: id.into() });
    one(tx, ITEM, [id], Item::from_row)
}

/// Records a cycle that ended; `false` when this cycle was already recorded (a cycle closes once).
#[allow(clippy::too_many_arguments)]
pub fn item_cycle_add_in(
    tx: &crate::Tx,
    item_id: &str,
    cycle: i64,
    revision: i64,
    outcome: &str,
    acceptance: Option<&str>,
    reason: Option<&str>,
    closed: i64,
) -> Result<bool> {
    let n = tx.prepare_cached(
        "INSERT OR IGNORE INTO item_cycles (item_id, cycle, revision, outcome, acceptance, reason, closed, source)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'live')",
    )?
    .execute(params![item_id, cycle, revision, outcome, acceptance, reason, closed])?;
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
        self.0.read(|c| {
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
        self.0.read(|c| {
            all(
                c,
                "SELECT id FROM items WHERE phase = 'closing' ORDER BY updated",
                [],
                |r| r.get(0),
            )
        })
    }
}

/// One operation of an item's cycle (see migration 027).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemOperation {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub revision: i64,
    pub kind: String,
    pub effect_id: Option<String>,
    pub required: bool,
    /// `pending` | `running` | `succeeded` | `failed` | `unknown` | `waived`
    pub status: String,
    pub label: String,
    pub evidence: serde_json::Value,
    pub created: i64,
    pub updated: i64,
}

fn op_row(r: &rusqlite::Row) -> rusqlite::Result<ItemOperation> {
    Ok(ItemOperation {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        revision: r.get("revision")?,
        kind: r.get("kind")?,
        effect_id: r.get("effect_id")?,
        required: r.get::<_, i64>("required")? != 0,
        status: r.get("status")?,
        label: r.get("label")?,
        evidence: json_or(
            r.get("evidence")?,
            serde_json::Value::Object(Default::default()),
        ),
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

/// A new operation; with an `effect_id` already recorded for (item, kind) the existing one is returned
/// instead (a repeated report of the same effect).
#[allow(clippy::too_many_arguments)]
pub fn operation_add_in(
    tx: &crate::Tx,
    item_id: &str,
    cycle: i64,
    revision: i64,
    kind: &str,
    effect_id: Option<&str>,
    required: bool,
    status: &str,
    label: &str,
    evidence: &serde_json::Value,
) -> Result<(ItemOperation, bool)> {
    if let Some(e) = effect_id
        && let Some(op) = one(
            tx,
            "SELECT * FROM item_operations WHERE item_id = ? AND kind = ? AND effect_id = ?",
            params![item_id, kind, e],
            op_row,
        )?
    {
        return Ok((op, false));
    }
    let id = uid("op");
    let t = now();
    tx.run(
        "INSERT INTO item_operations (id, item_id, cycle, revision, kind, effect_id, required, status, label, evidence, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![id, item_id, cycle, revision, kind, effect_id, required as i64, status, label, to_json(evidence)?, t, t],
    )?;
    tx.publish(ChangeEvent::Item { id: item_id.into() });
    let op = one(
        tx,
        "SELECT * FROM item_operations WHERE id = ?",
        [&id],
        op_row,
    )?
    .ok_or(crate::StoreError::NotFound("没有这个操作"))?;
    Ok((op, true))
}

/// Sets an operation's status (and merges `evidence` into what it had); `false` when there is no such
/// operation or it already ended (a terminal status is never overwritten).
pub fn operation_set_in(
    tx: &crate::Tx,
    id: &str,
    status: &str,
    evidence: &serde_json::Value,
) -> Result<bool> {
    let Some(op) = one(
        tx,
        "SELECT * FROM item_operations WHERE id = ?",
        [id],
        op_row,
    )?
    else {
        return Ok(false);
    };
    if matches!(op.status.as_str(), "succeeded" | "failed" | "waived") {
        return Ok(false);
    }
    let mut ev = op.evidence.clone();
    if let (Some(o), Some(x)) = (ev.as_object_mut(), evidence.as_object()) {
        for (k, v) in x {
            o.insert(k.clone(), v.clone());
        }
    }
    tx.run(
        "UPDATE item_operations SET status = ?, evidence = ?, updated = ? WHERE id = ?",
        params![status, to_json(&ev)?, now(), id],
    )?;
    tx.publish(ChangeEvent::Item { id: op.item_id });
    Ok(true)
}

impl Items<'_> {
    /// The operations of one cycle of an item, oldest first.
    pub fn operations(&self, item_id: &str, cycle: i64) -> Result<Vec<ItemOperation>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_operations WHERE item_id = ? AND cycle = ? ORDER BY created, id",
                params![item_id, cycle],
                op_row,
            )
        })
    }
    /// One operation by id.
    pub fn operation(&self, id: &str) -> Result<Option<ItemOperation>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM item_operations WHERE id = ?",
                [id],
                op_row,
            )
        })
    }
    /// Every operation of `kind` in `status`, any item, oldest first.
    pub fn operations_with(&self, kind: &str, status: &str) -> Result<Vec<ItemOperation>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_operations WHERE kind = ? AND status = ? ORDER BY created, id",
                params![kind, status],
                op_row,
            )
        })
    }
    /// Every operation of an item, any cycle, oldest first.
    pub fn operations_all(&self, item_id: &str) -> Result<Vec<ItemOperation>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_operations WHERE item_id = ? ORDER BY created, id",
                [item_id],
                op_row,
            )
        })
    }
}
