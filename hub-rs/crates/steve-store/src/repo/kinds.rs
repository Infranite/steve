//! Item categories (ST-34): every task item carries exactly one of six fixed tags of category
//! [`KIND_CAT`] — bug, feat, doc, refactor, ops, chore — and any number of other tags,
//! including optional multi-select [`DELIVERY_CAT`] tags for the handover's nature.
//! They are ordinary `tags` rows, so everything that shows an item's tags shows its category and a
//! TS Hub still reads them. [`ensure`] seeds them per workspace; renaming, recategorising, deleting
//! or merging them is refused, and every write of an item's tags goes through [`checked`].

use rusqlite::{Connection, params};

use super::{Cached, all, must, one};
use crate::{ChangeEvent, Result, StoreError, Tag, W, now, uid};

/// The tag category the six item categories live in, and only they.
pub const KIND_CAT: &str = "类型";

/// Optional, multi-select tags describing what an item hands over. They do not decide
/// whether a particular output belongs in Git or in the item's output directory.
pub const DELIVERY_CAT: &str = "交付";
pub const DELIVERIES: [(&str, &str); 6] = [
    ("方案", "设计稿、原型或实施方案"),
    ("代码", "Git 管理的代码、配置或脚本"),
    ("报告", "调研、审计、测试或分析结论"),
    ("内容", "手册、图片、演示稿等成品内容"),
    ("数据", "表格、数据集或导出结果"),
    ("操作", "部署、升级或配置变更等实际操作"),
];

/// Conservative hints for a new item's expected deliverables. More than one may apply.
pub(crate) fn expected_deliveries(title: &str, goal: &str) -> Vec<&'static str> {
    let text = format!("{title}\n{goal}");
    let mut labels = Vec::new();
    let rules: [(&str, &[&str]); 6] = [
        ("方案", &["设计", "方案", "原型", "线框"]),
        ("代码", &["实现", "开发", "修复", "改代码", "重构", "编程"]),
        (
            "报告",
            &["报告", "调研", "审计", "分析", "评估", "排查", "核查"],
        ),
        ("内容", &["文档", "手册", "图片", "海报", "幻灯片", "视频"]),
        ("数据", &["报表", "表格", "数据集", "数据导出"]),
        ("操作", &["部署", "升级", "发布", "安装", "迁移", "清理"]),
    ];
    for (name, words) in rules {
        if words.iter().any(|word| text.contains(word)) {
            labels.push(name);
        }
    }
    labels
}

/// Actual outputs can add deliverable tags without changing the item's purpose category.
pub(crate) fn output_deliveries(
    kind: &str,
    title: &str,
    location: Option<&str>,
    role: Option<&str>,
) -> Vec<&'static str> {
    if role == Some("record") || title == "交付正文" {
        return vec![];
    }
    let text = format!("{kind} {title}");
    let mut labels = Vec::new();
    if ["设计", "方案", "原型", "线框", "规范"]
        .iter()
        .any(|w| text.contains(w))
    {
        labels.push("方案");
    }
    if ["提交", "分支", "PR"].iter().any(|w| kind.contains(w))
        || location.is_some_and(|loc| {
            [".rs", ".ts", ".tsx", ".py", ".go", ".swift"]
                .iter()
                .any(|ext| loc.ends_with(ext))
        })
    {
        labels.push("代码");
    }
    if ["报告", "评审", "审计", "核验", "验收"]
        .iter()
        .any(|w| text.contains(w))
    {
        labels.push("报告");
    }
    if ["文档", "图片", "截图", "视频", "海报", "演示"]
        .iter()
        .any(|w| text.contains(w))
    {
        labels.push("内容");
    }
    if ["数据", "报表", "表格", "CSV"]
        .iter()
        .any(|w| text.contains(w))
    {
        labels.push("数据");
    }
    if ["部署", "升级", "运行版本", "安装", "发布"]
        .iter()
        .any(|w| text.contains(w))
    {
        labels.push("操作");
    }
    labels
}

pub(crate) fn add_delivery_labels(
    c: &Connection,
    ws: &str,
    item: &str,
    labels: &[&str],
) -> Result<()> {
    for label in labels {
        c.prepare_cached("INSERT OR IGNORE INTO item_tags (item_id, tag_id) SELECT ?, id FROM tags WHERE workspace_id = ? AND cat = ? AND name = ?")?
            .execute(params![item, ws, DELIVERY_CAT, label])?;
    }
    Ok(())
}

/// The item categories with what each is for (also the router's and the backfill's guidance).
pub const KINDS: [(&str, &str); 6] = [
    ("bug", "修复错误、故障或不符合预期的行为"),
    ("feat", "新增能力、交互或功能改进"),
    ("doc", "文档、方案、说明的编写与更新"),
    ("refactor", "不改变外部行为的代码重构"),
    ("ops", "部署、升级、运维与环境配置"),
    ("chore", "依赖维护、工具配置等杂项"),
];

/// What category `name` is for, in the request's language, for display. [`KINDS`] keeps the Chinese
/// text that is seeded into `tags.description` and given to the router; `None` for any other name.
pub fn description(name: &str) -> Option<String> {
    Some(match name {
        "bug" => steve_i18n::t!("store.kinds.desc.bug"),
        "feat" => steve_i18n::t!("store.kinds.desc.feat"),
        "doc" => steve_i18n::t!("store.kinds.desc.doc"),
        "refactor" => steve_i18n::t!("store.kinds.desc.refactor"),
        "ops" => steve_i18n::t!("store.kinds.desc.ops"),
        "chore" => steve_i18n::t!("store.kinds.desc.chore"),
        _ => return None,
    })
}

/// A category tag's stored description for display: the seeded Chinese text becomes [`description`];
/// a description someone edited is shown as written.
pub fn shown_description(name: &str, stored: &str) -> String {
    match KINDS.iter().find(|(k, _)| *k == name) {
        Some((_, seed)) if *seed == stored => description(name).unwrap_or_else(|| stored.into()),
        _ => stored.into(),
    }
}

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
    // Seed and infer only once. Delivery tags remain ordinary editable tags, so a person's later
    // choice to rename or remove one is not silently reversed by `ensure` on the next boot.
    let marker = "__delivery_tags_backfilled_v1";
    if !w
        .c
        .prepare_cached("SELECT 1 FROM settings WHERE workspace_id = ? AND key = ?")?
        .exists(params![ws, marker])?
    {
        for (name, desc) in DELIVERIES {
            match one(
                w.c,
                "SELECT * FROM tags WHERE workspace_id = ? AND name = ?",
                [ws, name],
                Tag::from_row,
            )? {
                Some(t) if t.cat == DELIVERY_CAT => {}
                Some(t) => {
                    w.c.run(
                        "UPDATE tags SET cat = ? WHERE id = ?",
                        [DELIVERY_CAT, &t.id],
                    )?;
                    w.publish(ChangeEvent::Tag { id: t.id });
                }
                None => {
                    let id = uid("tag");
                    w.c.run(
                    "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, 'Steve', ?)",
                    params![id, ws, name, DELIVERY_CAT, desc, now()],
                )?;
                    w.publish(ChangeEvent::Tag { id });
                }
            }
        }
        // One pass for pre-existing cards. A removed inferred tag stays removed afterwards.
        let items: Vec<(String, String, String)> = all(
            w.c,
            "SELECT i.id,i.title,COALESCE((SELECT text FROM item_goals WHERE item_id=i.id ORDER BY version DESC LIMIT 1),'') FROM items i WHERE i.workspace_id=? AND i.kind='task'",
            [ws],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        for (item, title, goal) in items {
            add_delivery_labels(w.c, ws, &item, &expected_deliveries(&title, &goal))?;
            let outputs: Vec<(String, String, Option<String>, Option<String>)> = all(
                w.c,
                "SELECT a.kind,a.title,a.location,r.role FROM artifacts a LEFT JOIN artifact_roles r ON r.artifact_id=a.id WHERE a.item_id=?",
                [&item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            for (kind, title, location, role) in outputs {
                add_delivery_labels(
                    w.c,
                    ws,
                    &item,
                    &output_deliveries(&kind, &title, location.as_deref(), role.as_deref()),
                )?;
            }
        }
        w.c.run(
            "INSERT INTO settings(workspace_id,key,value,updated) VALUES(?,?, 'true',?)",
            params![ws, marker, now()],
        )?;
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
        0 => Err(StoreError::Conflict(steve_i18n::t!(
            "store.kinds.missing",
            names = names()
        ))),
        _ => Err(StoreError::Conflict(steve_i18n::t!(
            "store.kinds.several",
            tags = kinds
                .iter()
                .map(|(_, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(&steve_i18n::t!("store.list_sep"))
        ))),
    }
}

/// `bug/feat/doc/refactor/ops/chore`
pub fn names() -> String {
    KINDS.map(|(k, _)| k).join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en<R>(f: impl FnOnce() -> R) -> R {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(steve_i18n::scope(steve_i18n::EN, async { f() }))
    }

    #[test]
    fn descriptions_match_the_seeds_in_zh_and_translate_in_en() {
        for (name, seed) in KINDS {
            assert_eq!(description(name).as_deref(), Some(seed));
            assert_eq!(shown_description(name, seed), seed);
            assert_ne!(en(|| shown_description(name, seed)), seed);
        }
        assert_eq!(
            en(|| shown_description("bug", "自己写的说明")),
            "自己写的说明"
        );
        assert_eq!(description("domain"), None);
        assert_eq!(
            en(|| description("bug")).as_deref(),
            Some("Fixing errors, failures or unexpected behavior")
        );
    }
}
