//! Item categories (ST-34): every task item carries exactly one of six fixed tags of category
//! [`KIND_CAT`] — bug, feat, doc, refactor, ops, chore — and any number of other (domain) tags.
//! They are ordinary `tags` rows, so everything that shows an item's tags shows its category and a
//! TS Hub still reads them. [`ensure`] seeds them per workspace; renaming, recategorising, deleting
//! or merging them is refused, and every write of an item's tags goes through [`checked`].

use rusqlite::{Connection, params};

use super::{Cached, all, must, one};
use crate::{ChangeEvent, Result, StoreError, Tag, W, now, uid};

/// The tag category the six item categories live in, and only they.
pub const KIND_CAT: &str = "类型";

/// The item categories with what each is for (also the router's and the backfill's guidance).
pub const KINDS: [(&str, &str); 6] = [
    ("bug", "修复错误、故障或不符合预期的行为"),
    ("feat", "新增能力、交互或功能改进"),
    ("doc", "文档、方案、说明的编写与更新"),
    ("refactor", "不改变外部行为的代码重构"),
    ("ops", "部署、升级、运维与环境配置"),
    ("chore", "依赖维护、工具配置等杂项"),
];

pub fn is_kind(name: &str) -> bool {
    KINDS.iter().any(|(k, _)| *k == name)
}

/// Where a domain tag goes when it was filed under [`KIND_CAT`] before the categories existed.
const MOVED_CAT: &str = "主题";

/// Seeds the six categories in `ws` and returns them in [`KINDS`] order. Idempotent: an existing tag of
/// that name becomes the category (keeping its id, uses and description); any other tag under
/// [`KIND_CAT`] moves to [`MOVED_CAT`], so the category is never ambiguous.
pub(crate) fn ensure(w: &W, ws: &str) -> Result<Vec<Tag>> {
    let mut out = vec![];
    for (name, desc) in KINDS {
        let ex = one(
            w.c,
            "SELECT * FROM tags WHERE workspace_id = ? AND name = ?",
            [ws, name],
            Tag::from_row,
        )?;
        match ex {
            Some(t) if t.cat == KIND_CAT => out.push(t),
            Some(t) => {
                w.c.run("UPDATE tags SET cat = ? WHERE id = ?", [KIND_CAT, &t.id])?;
                w.publish(ChangeEvent::Tag { id: t.id.clone() });
                out.push(Tag {
                    cat: KIND_CAT.into(),
                    ..t
                });
            }
            None => {
                let id = uid("tag");
                w.c.run(
                    "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, 'Steve', ?)",
                    params![id, ws, name, KIND_CAT, desc, now()],
                )?;
                w.publish(ChangeEvent::Tag { id: id.clone() });
                out.push(must(one(
                    w.c,
                    "SELECT * FROM tags WHERE id = ?",
                    [&id],
                    Tag::from_row,
                )?)?);
            }
        }
    }
    let strays: Vec<String> = all(
        w.c,
        "SELECT id FROM tags WHERE workspace_id = ? AND cat = ? AND name NOT IN ('bug', 'feat', 'doc', 'refactor', 'ops', 'chore')",
        [ws, KIND_CAT],
        |r| r.get(0),
    )?;
    for id in strays {
        w.c.run("UPDATE tags SET cat = ? WHERE id = ?", [MOVED_CAT, &id])?;
        w.publish(ChangeEvent::Tag { id });
    }
    Ok(out)
}

/// Whether `id` is one of the categories.
pub(crate) fn is_kind_tag(c: &Connection, id: &str) -> Result<bool> {
    Ok(
        c.prepare_cached("SELECT 1 FROM tags WHERE id = ? AND cat = ?")?
            .exists([id, KIND_CAT])?,
    )
}

/// An item's tag ids made valid for `ws`: ids that are not tags of `ws` dropped, duplicates once, the
/// category first. Refused (`Conflict`, user-facing) unless exactly one of them is a category.
pub(crate) fn checked(c: &Connection, ws: &str, ids: &[String]) -> Result<Vec<String>> {
    let (mut kinds, mut rest): (Vec<(String, String)>, Vec<String>) = (vec![], vec![]);
    for id in ids {
        if kinds.iter().any(|(k, _)| k == id) || rest.contains(id) {
            continue;
        }
        let t = one(
            c,
            "SELECT * FROM tags WHERE id = ? AND workspace_id = ?",
            [id, ws],
            Tag::from_row,
        )?;
        match t {
            Some(t) if t.cat == KIND_CAT => kinds.push((t.id, t.name)),
            Some(t) => rest.push(t.id),
            None => {}
        }
    }
    match kinds.len() {
        1 => Ok(kinds.into_iter().map(|(id, _)| id).chain(rest).collect()),
        0 => Err(StoreError::Conflict(format!(
            "事项必须有一个分类标签（{} 选一个）",
            names()
        ))),
        _ => Err(StoreError::Conflict(format!(
            "事项只能有一个分类标签，现在有 {}；换分类请直接选新的",
            kinds
                .iter()
                .map(|(_, n)| n.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ))),
    }
}

/// `bug/feat/doc/refactor/ops/chore`
pub fn names() -> String {
    KINDS.map(|(k, _)| k).join("/")
}
