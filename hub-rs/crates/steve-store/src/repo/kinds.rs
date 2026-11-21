//! Item categories (ST-34): every task item carries exactly one of six fixed tags of category
//! [`KIND_CAT`] — bug, feat, doc, refactor, ops, chore — and any number of other tags,
//! including optional multi-select [`DELIVERY_CAT`] tags for the handover's nature.
//! They are ordinary `tags` rows, so everything that shows an item's tags shows its category and a
//! TS Hub still reads them. [`ensure`] seeds them per workspace; renaming, recategorising, deleting
//! or merging them is refused, and every write of an item's tags goes through [`checked`].

use rusqlite::{Connection, params};

use super::{Cached, must, one};
use crate::{Result, StoreError, Tag, W, now, uid};

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

/// The item categories with what each is for, including the router's guidance.
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

/// Seeds the six categories in `ws` and returns them in [`KINDS`] order. Existing category
/// tags keep their ids and descriptions. A reserved name in another category is refused before
/// any seeding; an operator must explicitly rename that tag first. Delivery tags are seeded
/// only when the workspace is created, never by this recurring category check.
pub(crate) fn ensure(w: &W, ws: &str) -> Result<Vec<Tag>> {
    let mut pending = Vec::with_capacity(KINDS.len());
    for (name, desc) in KINDS {
        let existing = one(
            w.c,
            "SELECT * FROM tags WHERE workspace_id = ? AND name = ?",
            [ws, name],
            Tag::from_row,
        )?;
        if let Some(tag) = existing.as_ref().filter(|tag| tag.cat != KIND_CAT) {
            return Err(StoreError::Conflict(steve_i18n::t!(
                "store.kinds.seed_conflict",
                id = format!("{:?}", tag.id),
                name = format!("{:?}", tag.name),
                cat = format!("{:?}", tag.cat)
            )));
        }
        pending.push((name, desc, existing));
    }
    let mut out = vec![];
    for (name, desc, existing) in pending {
        match existing {
            Some(tag) => out.push(tag),
            None => {
                let id = uid("tag");
                w.c.run(
                    "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, 'Steve', ?)",
                    params![id, ws, name, KIND_CAT, desc, now()],
                )?;
                w.publish(steve_types::DomainEvent::from(
                    steve_types::ConfigEvent::Tag {
                        ws: ws.into(),
                        id: id.clone().into(),
                    },
                ));
                out.push(must(one(
                    w.c,
                    "SELECT * FROM tags WHERE id = ?",
                    [&id],
                    Tag::from_row,
                )?)?);
            }
        }
    }
    Ok(out)
}

/// Initial optional delivery tags for a newly inserted workspace. Never call on an existing
/// workspace: deleting or renaming these editable tags must not make them reappear.
pub(crate) fn seed_deliveries(w: &W, ws: &str) -> Result<()> {
    // Preserve any existing names without recategorizing user tags.
    for (name, desc) in DELIVERIES {
        if w.c
            .prepare_cached("SELECT 1 FROM tags WHERE workspace_id = ? AND name = ?")?
            .exists([ws, name])?
        {
            continue;
        }
        let id = uid("tag");
        w.c.run(
            "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, 'Steve', ?)",
            params![id, ws, name, DELIVERY_CAT, desc, now()],
        )?;
        w.publish(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Tag {
                ws: ws.into(),
                id: id.into(),
            },
        ));
    }
    Ok(())
}

/// Whether `id` is one of the six reserved categories.
pub(crate) fn is_kind_tag(c: &Connection, id: &str) -> Result<bool> {
    Ok(one(
        c,
        "SELECT name FROM tags WHERE id = ? AND cat = ?",
        [id, KIND_CAT],
        |r| r.get::<_, String>(0),
    )?
    .is_some_and(|name| is_kind(&name)))
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
            Some(t) if t.cat == KIND_CAT && is_kind(&t.name) => kinds.push((t.id, t.name)),
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
