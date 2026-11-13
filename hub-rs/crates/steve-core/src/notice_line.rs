//! Notification texts the Hub writes itself (disk warnings, YOLO, archiving, releases that did not go
//! out): stored in zh-CN like every other stored text, shown in the reader's language. Each is a
//! `core.notice.*` key whose zh-CN template is the stored text: the producer fills it under
//! [`steve_i18n::stored`], and [`shown`] reads a stored text back against the same templates. Anything
//! else (a release's own report, an error) is shown as it was written.

/// The notices, the more specific first where one template also matches another's text.
const KEYS: &[&str] = &[
    "core.notice.disk_low",
    "core.notice.disk_floor",
    "core.notice.yolo_ended",
    "core.notice.full_ended",
    "core.notice.yolo_paused",
    "core.notice.yolo_item_off",
    "core.notice.full_on",
    "core.notice.not_released_then",
    "core.notice.not_released",
];

/// A stored notice in the current request's language; `None`: show it as stored (zh-CN, or not one of
/// ours).
pub fn shown(stored: &str) -> Option<String> {
    const ARCHIVED: &[&str] = &["core.notice.archived_more", "core.notice.archived"];
    // the archived items are ids in a "、" list
    if let Some(s) = steve_i18n::reword!(stored, ARCHIVED) {
        return Some(s.replace('、', ", "));
    }
    steve_i18n::reword!(stored, KEYS)
}

/// The one notice of an archiving run: `ids` (the first three named), finished `days` ago.
pub fn archived(ids: &[String], days: &str) -> String {
    let named = ids[..ids.len().min(3)].join("、");
    steve_i18n::stored(|| {
        if ids.len() > 3 {
            steve_i18n::t!(
                "core.notice.archived_more",
                n = ids.len(),
                days = days,
                ids = named
            )
        } else {
            steve_i18n::t!(
                "core.notice.archived",
                n = ids.len(),
                days = days,
                ids = named
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_i18n::{EN, ZH_CN, with};

    fn en(s: &str) -> Option<String> {
        with(EN, || shown(s))
    }

    #[test]
    fn stored_notices_are_the_old_text() {
        let ids: Vec<String> = ["ST-1", "ST-2", "ST-3", "ST-4"].map(String::from).into();
        // what the producers wrote before the keys, byte for byte
        assert_eq!(
            with(EN, || archived(&ids, "30")),
            "自动归档了 4 件完成满 30 天的事项：ST-1、ST-2、ST-3 等"
        );
        assert_eq!(
            archived(&ids[..2], "7"),
            "自动归档了 2 件完成满 7 天的事项：ST-1、ST-2"
        );
        assert_eq!(
            with(EN, || steve_i18n::stored(|| steve_i18n::t!(
                "core.notice.full_on"
            ))),
            crate::yolo::FULL_WARNING
        );
    }

    #[test]
    fn notices_read_in_english() {
        assert_eq!(
            en("磁盘只剩 12 GB（提醒线 40 GB）：Steve 已回收已完成和闲置事项的构建缓存。低于 10 GB 时新事项将暂时不建工作区，可以在“设置 · 数据 · 工作区与磁盘”查看占用。").as_deref(),
            Some("Only 12 GB of disk left (warning line 40 GB): Steve reclaimed the build caches of finished and idle items. Below 10 GB new items won't get a workspace for now; see what takes the space in Settings · Data · Workspaces and disk.")
        );
        assert_eq!(
            en("自动归档了 4 件完成满 30 天的事项：ST-1、ST-2、ST-3 等").as_deref(),
            Some("Archived 4 items finished 30 days ago or more: ST-1, ST-2, ST-3 and more")
        );
        assert_eq!(
            en("YOLO 已暂停：连续出错").as_deref(),
            Some("YOLO paused: 连续出错")
        );
        assert!(
            en(crate::yolo::FULL_WARNING)
                .unwrap()
                .starts_with("Full access: ")
        );
        assert_eq!(en("没有发布：冲突"), None);
    }

    #[test]
    fn chinese_readers_get_the_stored_text() {
        assert_eq!(with(ZH_CN, || shown("YOLO 已暂停：连续出错")), None);
    }
}
