//! Projects, tags, devices, links.

use super::Cached;
use std::collections::BTreeMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Sets, all, must, one, revision, to_json};
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

    /// Returns the existing tag of that name unchanged (and without an event) if there is one.
    pub fn create(&self, ws: &str, p: &NewTag, by: &str) -> Result<Tag> {
        self.0.write(|w| {
            if let Some(ex) = one(w.c, TAG_BY_NAME, [ws, &p.name], Tag::from_row)? {
                return Ok(ex);
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

    pub fn patch(&self, id: &str, p: &TagPatch) -> Result<Option<Tag>> {
        self.0.write(|w| {
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

    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
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
    pub fn merge(&self, from: &str, into: &str) -> Result<()> {
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
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

    pub fn of_item(&self, item: &str) -> Result<Vec<String>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT tag_id FROM item_tags WHERE item_id = ?",
                [item],
                |r| r.get(0),
            )
        })
    }

    /// Replaces the item's tags; publishes `item`.
    pub fn set_item(&self, item: &str, ids: &[String]) -> Result<()> {
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
            tx.run("DELETE FROM item_tags WHERE item_id = ?", [item])?;
            for t in ids {
                tx.run(
                    "INSERT OR IGNORE INTO item_tags (item_id, tag_id) VALUES (?, ?)",
                    [item, t],
                )?;
            }
            tx.commit()?;
            w.publish(ChangeEvent::Item { id: item.into() });
            Ok(())
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
