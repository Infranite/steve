//! Projects, tags, devices, links.

use super::Cached;
use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Sets, all, kinds, must, one, revision, to_json};
use crate::types::double;
use crate::{
    ChangeEvent, Device, Link, Project, Release, Result, Store, StoreError, Tag, now, uid,
};

impl Store {
    pub fn projects(&self) -> Projects<'_> {
        Projects(self)
    }
    pub fn tags(&self) -> Tags<'_> {
        Tags(self)
    }
    pub fn devices(&self) -> Devices<'_> {
        Devices(self)
    }
    pub fn links(&self) -> Links<'_> {
        Links(self)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    pub device_id: String,
    pub path: String,
    pub kind: String,
    pub worktree: bool,
    pub default_bot: Option<String>,
    pub release: Option<Release>,
}

/// Fields of a project that may change; nullable columns use `Some(None)` for "set to null".
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProjectPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_bot: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub release: Option<Option<Release>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

pub struct Projects<'a>(&'a Store);

const PROJECT: &str = "SELECT * FROM projects WHERE id = ?";

impl Projects<'_> {
    /// Archived or not, ordered by name.
    pub fn list(&self, ws: &str, archived: bool) -> Result<Vec<Project>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM projects WHERE workspace_id = ? AND archived = ? ORDER BY name",
                params![ws, archived as i64],
                Project::from_row,
            )
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Project>> {
        self.0.read(|c| one(c, PROJECT, [id], Project::from_row))
    }

    pub fn by_path(&self, ws: &str, path: &str) -> Result<Option<Project>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM projects WHERE workspace_id = ? AND path = ?",
                [ws, path],
                Project::from_row,
            )
        })
    }

    pub fn create(&self, ws: &str, p: &NewProject) -> Result<Project> {
        self.0.write(|w| {
            let (id, t) = (uid("prj"), now());
            let release = p.release.as_ref().map(to_json).transpose()?;
            w.c.run(
                "INSERT INTO projects (id, workspace_id, name, description, device_id, path, kind, worktree, default_bot, release, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, ws, p.name, p.description, p.device_id, p.path, p.kind, p.worktree as i64, p.default_bot, release, t, t],
            )?;
            w.publish(ChangeEvent::Project { id: id.clone() });
            must(one(w.c, PROJECT, [&id], Project::from_row)?)
        })
    }

    /// Fails with `没有这个项目` if the id is unknown. Always records a revision with the patch.
    pub fn patch(&self, id: &str, p: &ProjectPatch, author: &str) -> Result<Project> {
        self.0.write(|w| {
            let cur = one(w.c, PROJECT, [id], Project::from_row)?
                .ok_or(StoreError::NotFound("没有这个项目"))?;
            let release = match &p.release {
                None => None,
                Some(None) => Some(None),
                Some(Some(r)) => Some(Some(to_json(r)?)),
            };
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("description", p.description.clone())
                .val("device_id", p.device_id.clone())
                .val("path", p.path.clone())
                .val("kind", p.kind.clone())
                .val("worktree", p.worktree)
                .null("default_bot", p.default_bot.clone())
                .null("release", release)
                .val("archived", p.archived);
            if !s.is_empty() {
                w.c.run(
                    &format!("UPDATE projects SET {}, updated = ? WHERE id = ?", s.sql()),
                    rusqlite::params_from_iter(s.params([now().into(), id.to_string().into()])),
                )?;
            }
            revision(w.c, &cur.workspace_id, "project", id, 0, p, author)?;
            w.publish(ChangeEvent::Project { id: id.into() });
            must(one(w.c, PROJECT, [id], Project::from_row)?)
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewTag {
    pub name: String,
    pub cat: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TagPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub struct Tags<'a>(&'a Store);

const TAG: &str = "SELECT * FROM tags WHERE id = ?";
const TAG_BY_NAME: &str = "SELECT * FROM tags WHERE workspace_id = ? AND name = ?";
/// An item's tag ids, its category first, the rest as they were added.
pub(crate) const ITEM_TAGS: &str = "SELECT it.tag_id FROM item_tags it LEFT JOIN tags t ON t.id = it.tag_id WHERE it.item_id = ? ORDER BY coalesce(t.cat = '类型', 0) DESC, it.rowid";

fn not_a_kind() -> StoreError {
    StoreError::Conflict(format!(
        "“{}”只放 {} 六个分类标签；新标签请选领域、主题等类别",
        kinds::KIND_CAT,
        kinds::names()
    ))
}

/// Writes an item's tags as given (already [`kinds::checked`]).
pub(crate) fn put_item_tags(c: &rusqlite::Connection, item: &str, ids: &[String]) -> Result<()> {
    c.run("DELETE FROM item_tags WHERE item_id = ?", [item])?;
    for t in ids {
        c.run(
            "INSERT OR IGNORE INTO item_tags (item_id, tag_id) VALUES (?, ?)",
            [item, t],
        )?;
    }
    Ok(())
}

impl Tags<'_> {
    pub fn list(&self, ws: &str) -> Result<Vec<Tag>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM tags WHERE workspace_id = ? ORDER BY cat, name",
                [ws],
                Tag::from_row,
            )
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Tag>> {
        self.0.read(|c| one(c, TAG, [id], Tag::from_row))
    }

    pub fn by_name(&self, ws: &str, name: &str) -> Result<Option<Tag>> {
        self.0
            .read(|c| one(c, TAG_BY_NAME, [ws, name], Tag::from_row))
    }

    /// The six item categories of `ws` (seeded on first use), in [`kinds::KINDS`] order.
    pub fn kinds(&self, ws: &str) -> Result<Vec<Tag>> {
        self.0.write(|w| kinds::ensure(w, ws))
    }

    /// Returns the existing tag of that name unchanged (and without an event) if there is one.
    /// [`kinds::KIND_CAT`] holds only the six categories.
    pub fn create(&self, ws: &str, p: &NewTag, by: &str) -> Result<Tag> {
        self.0.write(|w| {
            kinds::ensure(w, ws)?;
            if let Some(ex) = one(w.c, TAG_BY_NAME, [ws, &p.name], Tag::from_row)? {
                return Ok(ex);
            }
            if p.cat == kinds::KIND_CAT {
                return Err(not_a_kind());
            }
            let id = uid("tag");
            w.c.run(
                "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![id, ws, p.name, p.cat, p.description, by, now()],
            )?;
            w.publish(ChangeEvent::Tag { id: id.clone() });
            must(one(w.c, TAG, [&id], Tag::from_row)?)
        })
    }

    /// A category keeps its name and category (its description may change); no other tag joins them.
    pub fn patch(&self, id: &str, p: &TagPatch) -> Result<Option<Tag>> {
        self.0.write(|w| {
            if let Some(t) = one(w.c, TAG, [id], Tag::from_row)? {
                let kind = t.cat == kinds::KIND_CAT;
                if kind && p.name.as_ref().is_some_and(|n| *n != t.name) {
                    return Err(StoreError::Conflict("分类标签不能改名".into()));
                }
                if kind && p.cat.as_ref().is_some_and(|c| c != kinds::KIND_CAT) {
                    return Err(StoreError::Conflict("分类标签不能改类别".into()));
                }
                if !kind && p.cat.as_deref() == Some(kinds::KIND_CAT) {
                    return Err(not_a_kind());
                }
                if !kind && p.name.as_deref().is_some_and(kinds::is_kind) {
                    return Err(StoreError::Conflict(format!(
                        "{} 是分类标签的名字",
                        p.name.as_deref().unwrap_or_default()
                    )));
                }
            }
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("cat", p.cat.clone())
                .val("description", p.description.clone());
            if !s.is_empty() {
                w.c.run(
                    &format!("UPDATE tags SET {} WHERE id = ?", s.sql()),
                    rusqlite::params_from_iter(s.params([id.to_string().into()])),
                )?;
            }
            w.publish(ChangeEvent::Tag { id: id.into() });
            one(w.c, TAG, [id], Tag::from_row)
        })
    }

    /// A category is never removed: items would be left without one.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            if kinds::is_kind_tag(w.c, id)? {
                return Err(StoreError::Conflict("分类标签不能删除".into()));
            }
            let tx = w.tx()?;
            tx.run("DELETE FROM item_tags WHERE tag_id = ?", [id])?;
            // migration 4: tags on contacts, teams, … reference the tag too
            tx.run("DELETE FROM entity_tags WHERE tag_id = ?", [id])?;
            tx.run("DELETE FROM tags WHERE id = ?", [id])?;
            tx.commit()?;
            w.publish(ChangeEvent::Tag { id: id.into() });
            Ok(())
        })
    }

    /// Moves every use of `from` (items and, since migration 4, other objects) onto `into`, then deletes `from`.
    /// Categories take no part: merging could leave an item with none or with two.
    pub fn merge(&self, from: &str, into: &str) -> Result<()> {
        self.0.write(|w| {
            if kinds::is_kind_tag(w.c, from)? || kinds::is_kind_tag(w.c, into)? {
                return Err(StoreError::Conflict("分类标签不能合并".into()));
            }
            let tx = w.tx()?;
            tx.run("INSERT OR IGNORE INTO item_tags (item_id, tag_id) SELECT item_id, ? FROM item_tags WHERE tag_id = ?", [into, from])?;
            tx.run("DELETE FROM item_tags WHERE tag_id = ?", [from])?;
            tx.run("INSERT OR IGNORE INTO entity_tags (workspace_id, tag_id, object_type, object_id, created) SELECT workspace_id, ?, object_type, object_id, created FROM entity_tags WHERE tag_id = ?", [into, from])?;
            tx.run("DELETE FROM entity_tags WHERE tag_id = ?", [from])?;
            tx.run("DELETE FROM tags WHERE id = ?", [from])?;
            tx.commit()?;
            w.publish(ChangeEvent::Tag { id: into.into() });
            Ok(())
        })
    }

    /// Items per tag (0 for unused tags) in the workspace.
    pub fn uses(&self, ws: &str) -> Result<BTreeMap<String, i64>> {
        self.0.read(|c| {
            let rows: Vec<(String, i64)> = all(
                c,
                "SELECT t.id, count(it.item_id) n FROM tags t LEFT JOIN item_tags it ON it.tag_id = t.id WHERE t.workspace_id = ? GROUP BY t.id",
                [ws],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(rows.into_iter().collect())
        })
    }

    /// The item's tag ids, its category first.
    pub fn of_item(&self, item: &str) -> Result<Vec<String>> {
        self.0.read(|c| all(c, ITEM_TAGS, [item], |r| r.get(0)))
    }

    /// Replaces the item's tags; publishes `item`. A task item's set must hold exactly one category
    /// ([`kinds::checked`]: `Conflict` otherwise, nothing written); ids of other workspaces are dropped.
    /// Returns the ids kept, the category first.
    pub fn set_item(&self, item: &str, ids: &[String]) -> Result<Vec<String>> {
        self.0.write(|w| {
            let (ws, kind): (String, String) =
                w.c.row(
                    "SELECT workspace_id, kind FROM items WHERE id = ?",
                    [item],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or(StoreError::NotFound("没有这个事项"))?;
            let ids = if kind == "task" {
                kinds::ensure(w, &ws)?;
                kinds::checked(w.c, &ws, ids)?
            } else {
                ids.to_vec()
            };
            let tx = w.tx()?;
            put_item_tags(&tx, item, &ids)?;
            tx.commit()?;
            w.publish(ChangeEvent::Item { id: item.into() });
            Ok(ids)
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub roles: Vec<String>,
    pub os: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DevicePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<i64>,
}

/// The Hub registers itself; paired nodes come later.
pub struct Devices<'a>(&'a Store);

impl Devices<'_> {
    pub fn list(&self) -> Result<Vec<Device>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM devices ORDER BY created",
                [],
                Device::from_row,
            )
        })
    }

    /// Inserts, or refreshes os/version/seen (name and roles of a known device stay). No event.
    pub fn upsert(&self, d: &DeviceInfo) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO devices (id, name, roles, os, version, seen, created) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET os = excluded.os, version = excluded.version, seen = excluded.seen",
                params![d.id, d.name, to_json(&d.roles)?, d.os, d.version, now(), now()],
            )?;
            Ok(())
        })
    }

    pub fn patch(&self, id: &str, p: &DevicePatch) -> Result<()> {
        self.0.write(|w| {
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("status", p.status.clone())
                .val("max_sessions", p.max_sessions);
            if !s.is_empty() {
                w.c.run(
                    &format!("UPDATE devices SET {} WHERE id = ?", s.sql()),
                    rusqlite::params_from_iter(s.params([id.to_string().into()])),
                )?;
            }
            w.publish(ChangeEvent::Device { id: id.into() });
            Ok(())
        })
    }
}

/// Semantic many-to-many relations with a source.
pub struct Links<'a>(&'a Store);

impl Links<'_> {
    /// `subject`/`object` are `(type, id)`. Returns whether a new link was inserted. No event.
    pub fn add(
        &self,
        ws: &str,
        subject: (&str, &str),
        predicate: &str,
        object: (&str, &str),
        source: &str,
        props: &Value,
    ) -> Result<bool> {
        self.0.write(|w| {
            let n = w.c.run(
                "INSERT OR IGNORE INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![ws, subject.0, subject.1, predicate, object.0, object.1, to_json(props)?, source, now()],
            )?;
            Ok(n > 0)
        })
    }

    pub fn from(&self, ws: &str, kind: &str, id: &str) -> Result<Vec<Link>> {
        self.0.read(|c| {
            all(c, "SELECT * FROM links WHERE workspace_id = ? AND subject_type = ? AND subject_id = ?", [ws, kind, id], Link::from_row)
        })
    }
}
