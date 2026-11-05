//! Item categories (ST-34): Steve gives every new item one, and fills in the items made before.
//!
//! The store refuses any task item tag set without exactly one category (`steve_store::kinds`).
//! Here the categories are chosen: the router names one with the item; when nothing named one
//! (a flow, a mail rule, a channel bot), [`guess`] reads the title and goal. [`Core::backfill_kinds`]
//! gives the items that have none (or more than one) theirs, by the model with [`guess`] behind it,
//! and lists the ones it is unsure of for the user ([`Core::kinds_review`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::kinds::KINDS;

use crate::gateway::{first_json, jstr};
use crate::repo::{Db, Tag};
use crate::util::js_head;
use crate::{Core, Result, Tier};

/// The settings key of the items whose filled-in category the user should look at.
pub const REVIEW_KEY: &str = "kinds_review";

/// A model's answer under this counts as unsure.
const SURE: f64 = 0.7;

/// Items per classification call.
const BATCH: usize = 20;

/// Why the rules picked a category, as stored in the review list (shown through [`why_shown`]).
const WHY_KEYWORDS: &str = "按标题和目标里的关键词";
const WHY_NO_CLUE: &str = "没有明显线索";

/// A stored reason as the UI shows it: the rules' fixed reasons in the request's language, a model's own
/// words as they are.
fn why_shown(why: String) -> String {
    match why.as_str() {
        WHY_KEYWORDS => steve_i18n::t!("core.kinds.why_keywords"),
        WHY_NO_CLUE => steve_i18n::t!("core.kinds.why_no_clue"),
        _ => why,
    }
}

fn reviews_shown(list: Vec<Review>) -> Vec<Review> {
    list.into_iter()
        .map(|r| Review {
            why: why_shown(r.why),
            ..r
        })
        .collect()
}

/// What [`guess`] read from a title and goal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guess {
    pub kind: &'static str,
    /// a clear winner; otherwise it is a default worth a second look
    pub sure: bool,
}

/// Words that point to a category, strongest category first (it wins a tie).
const WORDS: [(&str, &[&str]); 6] = [
    (
        "bug",
        &[
            "修复",
            "修正",
            "修一下",
            "bug",
            "报错",
            "出错",
            "错误",
            "故障",
            "失败",
            "崩溃",
            "异常",
            "无法",
            "不能用",
            "不生效",
            "没生效",
            "没反应",
            "不显示",
            "丢失",
            "卡住",
            "卡死",
            "闪退",
            "白屏",
            "乱码",
            "回归",
            "fix",
            "不对",
            "不正确",
            "有问题",
        ],
    ),
    (
        "ops",
        &[
            "发布",
            "部署",
            "上线",
            "运维",
            "重启",
            "备份",
            "证书",
            "域名",
            "服务器",
            "launchd",
            "systemd",
            "deploy",
            "回滚",
            "监控",
            "磁盘",
            "环境变量",
            "升级 steve",
            "升级版本",
            "配置环境",
        ],
    ),
    (
        "doc",
        &[
            "文档",
            "方案",
            "说明",
            "readme",
            "手册",
            "报告",
            "总结",
            "调研",
            "讲一下",
            "讲讲",
            "解释",
            "介绍一下",
            "梳理",
            "写一份",
            "出一份",
        ],
    ),
    (
        "refactor",
        &[
            "重构",
            "refactor",
            "拆分模块",
            "解耦",
            "抽取",
            "整理代码",
            "清理代码",
            "简化代码",
            "代码结构",
        ],
    ),
    (
        "chore",
        &[
            "依赖",
            "lint",
            "格式化",
            "工具配置",
            "杂项",
            "版本号",
            "bump",
            "gitignore",
            "清理日志",
            "清理缓存",
            "升级依赖",
        ],
    ),
    (
        "feat",
        &[
            "新增",
            "增加",
            "添加",
            "支持",
            "实现",
            "功能",
            "优化",
            "改进",
            "允许",
            "能够",
            "接入",
            "引入",
            "页面",
            "交互",
            "界面",
            "体验",
            "设计并实现",
        ],
    ),
];

/// The category a title and goal read as: each word found counts its length, twice in the title.
/// Nothing found reads as `feat` (most work adds or improves something), unsure.
pub fn guess(title: &str, goal: &str) -> Guess {
    let (t, g) = (title.to_lowercase(), goal.to_lowercase());
    let mut scores: Vec<(&'static str, usize)> = WORDS
        .iter()
        .map(|(k, ws)| {
            let s = ws
                .iter()
                .map(|w| {
                    let n = w.chars().count();
                    t.matches(w).count() * n * 2 + g.matches(w).count() * n
                })
                .sum();
            (*k, s)
        })
        .collect();
    /* stable: on a tie the earlier (stronger) category stays first */
    scores.sort_by_key(|s| std::cmp::Reverse(s.1));
    let (top, second) = (scores[0], scores[1]);
    if top.1 == 0 {
        return Guess {
            kind: "feat",
            sure: false,
        };
    }
    Guess {
        kind: top.0,
        sure: top.1 >= 4 && top.1 >= second.1 * 2,
    }
}

/// A new item's tags made whole: tags of other workspaces dropped, duplicates once, exactly one
/// category, first. The first category given wins; with none, [`guess`] picks it (returned).
pub(crate) fn settle(
    db: &Db,
    ws: &str,
    ids: &[String],
    title: &str,
    goal: &str,
) -> Result<(Vec<String>, Option<Guess>)> {
    let kinds = db.kinds(ws)?;
    let mut kind: Option<String> = None;
    let mut rest: Vec<String> = vec![];
    for id in ids {
        if kinds.iter().any(|k| &k.id == id) {
            kind.get_or_insert_with(|| id.clone());
        } else if !rest.contains(id) && db.tag(id)?.is_some_and(|t| t.workspace_id == ws) {
            rest.push(id.clone());
        }
    }
    let (kind, guessed) = match kind {
        Some(k) => (k, None),
        None => {
            let g = guess(title, goal);
            (id_of(&kinds, g.kind), Some(g))
        }
    };
    Ok((std::iter::once(kind).chain(rest).collect(), guessed))
}

fn id_of(kinds: &[Tag], name: &str) -> String {
    kinds
        .iter()
        .find(|k| k.name == name)
        .map(|k| k.id.clone())
        .unwrap_or_default()
}

/// The category lines for a prompt: `  bug: 修复错误…`.
pub fn prompt_lines() -> Vec<String> {
    KINDS.iter().map(|(k, d)| format!("  {k}: {d}")).collect()
}

/// One item the backfill gave a category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filled {
    pub id: String,
    pub title: String,
    pub kind: String,
    /// `model` or `rule`
    pub by: String,
    pub confidence: f64,
    pub uncertain: bool,
    pub why: String,
    /// the categories it had when it had more than one
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub had: Vec<String>,
}

/// What one backfill run did (or, `dry`, would do).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Backfill {
    /// task items looked at, archived and finished ones included
    pub checked: usize,
    /// already with exactly one category: left as they are
    pub kept: usize,
    pub filled: Vec<Filled>,
    pub dry: bool,
}

/// An item whose filled-in category waits for the user's look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub kind: String,
    pub why: String,
}

/// The classification prompt for a batch of `(id, title, goal)`.
fn classify_prompt(items: &[(String, String, String)]) -> String {
    let mut head = vec![
        "Classify work items of a personal assistant (Steve). Each item gets exactly ONE category:"
            .to_string(),
    ];
    head.extend(prompt_lines());
    head.push("Pick by the item's main acceptance goal: a fix of wrong behaviour is bug even if it adds a little; a plan, proposal or explanation without code is doc; deploying, releasing, upgrading or configuring a running system is ops; only use chore for upkeep like dependencies or tool config, never as a default.".into());
    head.push("Reply with ONLY JSON: {\"items\":[{\"id\":\"<id>\",\"kind\":\"bug|feat|doc|refactor|ops|chore\",\"confidence\":0.0-1.0,\"why\":\"<= 20 chars, Chinese\"}]}".into());
    let lines: Vec<String> = items
        .iter()
        .map(|(id, t, g)| format!("  {id}: {} — {}", t, js_head(&g.replace('\n', " "), 300)))
        .collect();
    crate::gateway::with_static(
        &head.join("\n"),
        &format!("Items (id: title — goal):\n{}", lines.join("\n")),
    )
}

/// The model's `{id: (kind, confidence, why)}`; anything unusable is left out.
fn parse_classified(raw: &str) -> Vec<(String, String, f64, String)> {
    let Ok(Some(v)) = first_json(raw) else {
        return vec![];
    };
    v.get("items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let id = x.get("id").map(jstr)?;
                    let kind = x.get("kind").map(jstr)?;
                    steve_store::kinds::is_kind(&kind).then(|| {
                        let c = x
                            .get("confidence")
                            .and_then(Value::as_f64)
                            .filter(|c| c.is_finite())
                            .map(|c| c.clamp(0.0, 1.0))
                            .unwrap_or(0.5);
                        let why = x.get("why").map(jstr).unwrap_or_default();
                        (id, kind, c, js_head(&why, 40).to_string())
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Core {
    /// Gives every task item of `ws` without exactly one category its category: the model reads
    /// title and goal ([`guess`] when it fails or skips one); an item with several keeps the one the
    /// model picks among them, else the first. Items that already have one are not touched, so a
    /// second run finds nothing to do. Unsure picks are listed in [`REVIEW_KEY`]. `dry` writes nothing.
    pub async fn backfill_kinds(&self, ws: &str, dry: bool) -> Result<Backfill> {
        let db = self.store();
        let kinds = db.kinds(ws)?;
        let name_of = |id: &str| kinds.iter().find(|k| k.id == id).map(|k| k.name.clone());
        let mut out = Backfill {
            dry,
            ..Default::default()
        };
        /* (id, title, goal, categories it has) */
        let mut todo: Vec<(String, String, String, Vec<String>)> = vec![];
        for i in db.items_all(ws)? {
            out.checked += 1;
            let has: Vec<String> = db
                .tags_of_item(&i.id)?
                .iter()
                .filter_map(|t| name_of(t))
                .collect();
            if has.len() == 1 {
                out.kept += 1;
                continue;
            }
            todo.push((i.id.clone(), i.title.clone(), db.goal(&i.id)?, has));
        }
        let mut picked: Vec<(String, String, f64, String)> = vec![];
        for chunk in todo.chunks(BATCH) {
            let batch: Vec<(String, String, String)> = chunk
                .iter()
                .map(|(id, t, g, _)| (id.clone(), t.clone(), g.clone()))
                .collect();
            let prompt = classify_prompt(&batch);
            let call = self
                .gw()
                .print(&prompt, Tier::Fast, Duration::from_secs(120));
            match crate::stats::tagged("route", ws, call).await {
                Ok(raw) => picked.extend(parse_classified(&raw)),
                Err(e) => tracing::warn!("kinds backfill: model failed, rules decide: {e}"),
            }
        }
        for (id, title, goal, had) in todo {
            let by_model = picked.iter().find(|p| p.0 == id).filter(|p| {
                /* with several, the pick must be one of them */
                had.len() < 2 || had.contains(&p.1)
            });
            let f = match by_model {
                Some((_, kind, c, why)) => Filled {
                    id: id.clone(),
                    title,
                    kind: kind.clone(),
                    by: "model".into(),
                    confidence: *c,
                    uncertain: *c < SURE,
                    why: why.clone(),
                    had: if had.len() > 1 { had } else { vec![] },
                },
                None => {
                    let g = guess(&title, &goal);
                    let (kind, sure) = match had.first() {
                        Some(k) if had.len() > 1 => (
                            if had.iter().any(|h| h == g.kind) {
                                g.kind.to_string()
                            } else {
                                k.clone()
                            },
                            false,
                        ),
                        _ => (g.kind.to_string(), g.sure),
                    };
                    Filled {
                        id: id.clone(),
                        title,
                        kind,
                        by: "rule".into(),
                        confidence: if sure { 0.7 } else { 0.4 },
                        uncertain: !sure,
                        why: if sure {
                            WHY_KEYWORDS.into()
                        } else {
                            WHY_NO_CLUE.into()
                        },
                        had: if had.len() > 1 { had } else { vec![] },
                    }
                }
            };
            if !dry && !self.fill_kind(ws, &kinds, &f)? {
                continue;
            }
            out.filled.push(f);
        }
        if !dry {
            let unsure: Vec<Review> = out
                .filled
                .iter()
                .filter(|f| f.uncertain)
                .map(|f| Review {
                    id: f.id.clone(),
                    kind: f.kind.clone(),
                    why: f.why.clone(),
                })
                .collect();
            if !unsure.is_empty() {
                let mut list = self.review_list(ws)?;
                for r in unsure {
                    list.retain(|x| x.id != r.id);
                    list.push(r);
                }
                self.save_review(ws, &list)?;
            }
        }
        for f in &mut out.filled {
            f.why = why_shown(std::mem::take(&mut f.why));
        }
        Ok(out)
    }

    /// Writes one filled-in category; `false` when the item got exactly one meanwhile (left alone).
    fn fill_kind(&self, ws: &str, kinds: &[Tag], f: &Filled) -> Result<bool> {
        let db = self.store();
        let now = db.tags_of_item(&f.id)?;
        let is_kind = |t: &String| kinds.iter().any(|k| &k.id == t);
        if now.iter().filter(|t| is_kind(t)).count() == 1 {
            return Ok(false);
        }
        let ids: Vec<String> = std::iter::once(id_of(kinds, &f.kind))
            .chain(now.into_iter().filter(|t| !is_kind(t)))
            .collect();
        db.set_item_tags(&f.id, &ids)?;
        let note = if f.uncertain {
            "，拿不准，请你确认"
        } else {
            ""
        };
        db.timeline_add(
            &f.id,
            "steve",
            "sys",
            &format!("Steve 补齐分类：{}{note}", f.kind),
            None,
        )?;
        db.audit(
            "steve",
            "item.kind.fill",
            &f.id,
            &json!({"kind": f.kind, "by": f.by, "confidence": f.confidence, "uncertain": f.uncertain, "had": f.had}),
            ws,
        )?;
        Ok(true)
    }

    /// [`Core::backfill_kinds`] for every workspace, once after the Hub starts: the items made before
    /// the categories (and any a crash left without one) get theirs.
    pub async fn backfill_kinds_all(self) {
        let ids = match self.store().workspace_ids() {
            Ok(ids) => ids,
            Err(e) => return tracing::error!("kinds backfill {e}"),
        };
        for ws in ids {
            match self.backfill_kinds(&ws, false).await {
                Ok(r) if !r.filled.is_empty() => tracing::info!(
                    "kinds backfill {ws}: {} filled, {} unsure",
                    r.filled.len(),
                    r.filled.iter().filter(|f| f.uncertain).count()
                ),
                Ok(_) => {}
                Err(e) => tracing::error!("kinds backfill {ws}: {e}"),
            }
        }
    }

    fn review_list(&self, ws: &str) -> Result<Vec<Review>> {
        Ok(self
            .store()
            .store()
            .settings()
            .get::<Vec<Review>>(ws, REVIEW_KEY, vec![])?)
    }

    fn save_review(&self, ws: &str, list: &[Review]) -> Result<()> {
        let mut p = serde_json::Map::new();
        p.insert(REVIEW_KEY.into(), json!(list));
        self.store().settings_set(ws, &p, "steve")
    }

    /// The filled-in categories still to look at: an item whose category changed since (the user
    /// picked another) or that is gone has been looked at.
    pub fn kinds_review(&self, ws: &str) -> Result<Vec<Review>> {
        Ok(reviews_shown(self.reviews_open(ws)?))
    }

    /// [`Core::kinds_review`] as stored.
    fn reviews_open(&self, ws: &str) -> Result<Vec<Review>> {
        let db = self.store();
        let kinds = db.kinds(ws)?;
        let mut out = vec![];
        for r in self.review_list(ws)? {
            if db.item(&r.id)?.is_none_or(|i| i.workspace_id != ws) {
                continue;
            }
            let id = id_of(&kinds, &r.kind);
            if db.tags_of_item(&r.id)?.contains(&id) {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// The user looked at these items' categories (kept or changed): they leave the list.
    pub fn kinds_reviewed(&self, ws: &str, ids: &[String]) -> Result<Vec<Review>> {
        let left: Vec<Review> = self
            .reviews_open(ws)?
            .into_iter()
            .filter(|r| !ids.contains(&r.id))
            .collect();
        if left.len() != self.review_list(ws)?.len() {
            self.save_review(ws, &left)?;
        }
        Ok(reviews_shown(left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_reads_the_main_goal() {
        let g = |t: &str, goal: &str| guess(t, goal).kind;
        assert_eq!(g("修复消息无法排队", "思考时发送的消息丢了"), "bug");
        assert_eq!(
            g("新增排队消息编辑能力", "排队中的消息可以撤回修改"),
            "feat"
        );
        assert_eq!(g("设计交付队列方案", "先出方案，不改代码"), "doc");
        assert_eq!(g("重构标签模块", "不改变外部行为"), "refactor");
        assert_eq!(g("发布 Hub 新版本", "部署到生产并检查健康"), "ops");
        assert_eq!(g("升级依赖", "把 npm 依赖升到最新"), "chore");
        assert_eq!(g("优化事项页面信息密度与布局", ""), "feat");
    }

    #[test]
    fn nothing_to_go_on_is_an_unsure_feat_never_chore() {
        let x = guess("ST-9", "看看");
        assert_eq!(
            x,
            Guess {
                kind: "feat",
                sure: false
            }
        );
        assert!(guess("修复登录失败", "").sure);
    }

    #[test]
    fn the_model_answer_is_checked() {
        let r = parse_classified(
            r#"{"items":[{"id":"ST-1","kind":"bug","confidence":0.9,"why":"修复"},{"id":"ST-2","kind":"misc"},{"id":"ST-3","kind":"doc","confidence":7}]}"#,
        );
        assert_eq!(
            r,
            vec![
                ("ST-1".into(), "bug".into(), 0.9, "修复".into()),
                ("ST-3".into(), "doc".into(), 1.0, String::new())
            ]
        );
        assert!(parse_classified("no json").is_empty());
    }
}
