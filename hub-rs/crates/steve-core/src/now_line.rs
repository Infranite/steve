//! The `now` lines the Hub's own work judgement writes ([`crate::work`], the quota wait): stored in zh-CN like
//! every other stored text, shown in the reader's language. Each line is a `core.now.*` key whose zh-CN
//! template is the stored text: the producer fills it under [`steve_i18n::stored`], and [`shown`] reads a
//! stored line back against the same templates and fills the reader's. A line that is none of them (an agent's
//! own `steve_update`, a delivery summary) is shown as it was written.

use std::borrow::Cow;

use steve_i18n::ZH_CN;
use steve_i18n::template::{fill, read};

/// Whole lines, the more specific first where one template also matches another's text.
const LINES: &[&str] = &[
    "core.now.item_paused",
    "core.now.deps_done",
    "core.now.deps_wait",
    "core.now.turn_running",
    "core.now.node_wait",
    "core.now.input_queued",
    "core.now.waiting_user",
    "core.now.quota_wait",
    "core.now.workflow_halted",
    "core.now.workflow_paused",
    "core.now.workflow_replan",
    "core.now.workflow_running",
    "core.now.workflow_user",
    "core.now.workflow_cannot_start",
    "core.now.workflow_queued",
    "core.now.workflow_failed_held",
    "core.now.workflow_failed",
];

/// One work unit in a list of them (`names`, `held`), the more specific first.
const ENTRIES: &[&str] = &[
    "core.now.unit_out_of_attempts",
    "core.now.unit_failed_why",
    "core.now.unit_failed",
    "core.now.unit_cancelled",
    "core.now.unit_frozen_pending",
    "core.now.optional_failed",
];

const MORE: &str = "core.now.names_more";
const SEP: &str = "core.now.list_sep";

fn template(locale: &str, key: &str) -> String {
    rust_i18n::t!(key, locale = locale).into_owned()
}

/// `stored` read against the zh-CN templates of `keys` and filled in `locale`'s.
fn line(keys: &[&str], stored: &str, locale: &str) -> Option<String> {
    keys.iter().find_map(|k| {
        let vals = read(&template(ZH_CN, k), stored)?;
        let vals: Vec<_> = vals
            .into_iter()
            .map(|(n, v)| {
                let v = match n.as_str() {
                    "names" | "held" => list(&v, locale, true),
                    "deps" => list(&v, locale, false),
                    _ => v,
                };
                (n, v)
            })
            .collect();
        Some(fill(&template(locale, k), &vals))
    })
}

/// A "、" list (of units: each one read against [`ENTRIES`]), with its "… 等 N 项".
fn list(v: &str, locale: &str, units: bool) -> String {
    let (names, count) = match read(&template(ZH_CN, MORE), v).as_deref() {
        Some([(_, names), (_, count)]) => (names.clone(), Some(count.clone())),
        _ => (v.to_owned(), None),
    };
    let zh_sep = template(ZH_CN, SEP);
    let joined = names
        .split(zh_sep.as_str())
        .map(|x| {
            units
                .then(|| line(ENTRIES, x, locale))
                .flatten()
                .unwrap_or_else(|| x.to_owned())
        })
        .collect::<Vec<_>>()
        .join(&template(locale, SEP));
    match count {
        Some(count) => fill(
            &template(locale, MORE),
            &[("names".into(), joined), ("count".into(), count)],
        ),
        None => joined,
    }
}

/// A stored `now` as the current request's language shows it: one of the work judgement's lines in that
/// language, anything else as it was written. zh-CN gets the stored text unchanged.
pub fn shown(stored: &str) -> Cow<'_, str> {
    let locale = steve_i18n::current();
    if locale == ZH_CN || stored.is_empty() {
        return Cow::Borrowed(stored);
    }
    line(LINES, stored, locale).map_or(Cow::Borrowed(stored), Cow::Owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en(s: &str) -> String {
        steve_i18n::with(steve_i18n::EN, || shown(s).into_owned())
    }

    #[test]
    fn every_key_has_both_languages() {
        for k in LINES.iter().chain(ENTRIES).chain([&MORE, &SEP]) {
            let (zh, en) = (template(ZH_CN, k), template(steve_i18n::EN, k));
            assert_ne!(zh, *k, "{k} missing in zh-CN");
            assert_ne!(en, *k, "{k} missing in en");
            assert_ne!(zh, en, "{k} not translated");
        }
    }

    #[test]
    fn fixed_lines_read_in_english() {
        assert_eq!(en("协作已暂停"), "Collaboration paused");
        assert_eq!(
            en("协作已暂停，等你决定：等测试环境"),
            "Collaboration paused, waiting for your decision: 等测试环境"
        );
        assert_eq!(en("事项已暂停"), "Item paused");
        assert_eq!(en("回合执行中"), "Turn running");
        assert_eq!(
            en("等待前置事项完成：ST-1、ST-2"),
            "Waiting for prerequisites: ST-1, ST-2"
        );
        assert_eq!(
            en("模型配额用完，14:05 后继续"),
            "Model quota used up, continues at 14:05"
        );
    }

    #[test]
    fn unit_lists_read_entry_by_entry() {
        assert_eq!(
            en("协作工作未通过：构建 尝试次数已用完、评审 未通过：缺测试；受阻：集成"),
            "Collaboration work failed: 构建 is out of attempts, 评审 failed: 缺测试; blocked: 集成"
        );
        assert_eq!(
            en("协作排队中：a、b（交付待冻结）、c 等 5 项"),
            "Collaboration queued: a, b (delivery not frozen yet), c (5 in all)"
        );
        assert_eq!(
            en("协作工作未通过：可选工作未通过"),
            "Collaboration work failed: Optional work failed"
        );
    }

    #[test]
    fn the_node_wait_prefix_is_its_template() {
        let line = steve_i18n::stored(|| steve_i18n::t!("core.now.node_wait", why = "x"));
        assert_eq!(line, format!("{}x", crate::work::NODE_WAIT));
    }

    #[test]
    fn other_text_and_chinese_stay_as_written() {
        assert_eq!(en("正在改 README"), "正在改 README");
        assert_eq!(en(""), "");
        let s = "协作工作未通过：构建 尝试次数已用完";
        assert_eq!(steve_i18n::with(ZH_CN, || shown(s).into_owned()), s);
    }
}
