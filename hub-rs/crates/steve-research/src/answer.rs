//! Turning the model's raw answer into what the thread shows: the `OFFER:` line lifted into an [`Offer`],
//! `path:line` citations collected, and the end mark of a run that did not finish.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Mark appended when the user stopped the run.
pub const MARK_STOPPED: &str = "（已停下）";
/// Mark appended when a budget (time, turns, tools) ended the run.
pub const MARK_BUDGET: &str = "（没读完就停了）";
/// A tool check stopped the run; this does not prove that protected data was read.
pub const MARK_VIOLATION: &str = "（本次查询在工具检查时停止，尚未完成。）";

/// "Want me to change it?" — a suggested item (`meta.offer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub title: String,
    pub goal: String,
}

/// A `path:line[-line]` reference in the answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    /// Whether the run actually read that file (a cite to an unread file is suspicious).
    pub read: bool,
}

static OFFER_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*OFFER:\s*(\{.*\})\s*$").expect("static regex"));
static BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`:(\d+)(?:[-–](\d+))?`").expect("static regex"));
static CITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s(\[`'“\x22])((?:[\w.\-]+/)*[\w.\-]*\.[A-Za-z0-9]+):(\d+)(?:[-–](\d+))?")
        .expect("static regex")
});

/// The answer without its `OFFER:` line, and the offer if it parsed.
pub fn split_offer(raw: &str) -> (String, Option<Offer>) {
    let mut offer = None;
    for c in OFFER_LINE.captures_iter(raw) {
        if let Ok(o) = serde_json::from_str::<Offer>(&c[1])
            && !o.title.trim().is_empty()
        {
            offer = Some(o);
        }
    }
    let text = OFFER_LINE.replace_all(raw, "");
    (text.trim_end().to_string(), offer)
}

/// Every distinct `path:line` in `text`; `files_read` are the paths the run read (relative to its cwd).
/// A bare `` `:line` `` after a full reference belongs to the last path cited.
pub fn citations(text: &str, files_read: &[String]) -> Vec<Citation> {
    let mut out: Vec<Citation> = Vec::new();
    let mut found: Vec<(usize, Option<String>, u32, Option<u32>)> = Vec::new();
    for c in CITE.captures_iter(text) {
        if let (Some(m), Ok(line)) = (c.get(1), c[2].parse::<u32>()) {
            let end = c.get(3).and_then(|m| m.as_str().parse().ok());
            found.push((
                m.start(),
                Some(c[1].trim_start_matches("./").to_string()),
                line,
                end,
            ));
        }
    }
    for c in BARE.captures_iter(text) {
        if let (Some(m), Ok(line)) = (c.get(0), c[1].parse::<u32>()) {
            found.push((
                m.start(),
                None,
                line,
                c.get(2).and_then(|m| m.as_str().parse().ok()),
            ));
        }
    }
    found.sort_by_key(|f| f.0);
    let mut last: Option<String> = None;
    for (_, path, line, end) in found {
        let path = match path {
            Some(p) => {
                last = Some(p.clone());
                p
            }
            None => match &last {
                Some(p) => p.clone(),
                None => continue,
            },
        };
        let read = files_read.iter().any(|f| {
            f == &path || f.ends_with(&format!("/{path}")) || path.ends_with(&format!("/{f}"))
        });
        let cite = Citation {
            path,
            line,
            end,
            read,
        };
        if !out.contains(&cite) {
            out.push(cite);
        }
    }
    out
}

/// `text` followed by `mark` on its own paragraph (or just the mark when nothing was written).
pub fn with_mark(text: &str, mark: &str) -> String {
    let t = text.trim_end();
    if t.is_empty() {
        mark.to_string()
    } else {
        format!("{t}\n\n{mark}")
    }
}

/// The offer attached to a run a budget cut short: carry the question into an item to go deeper.
pub fn deeper_offer(question: &str) -> Offer {
    let q: String = question.trim().chars().take(40).collect();
    Offer {
        title: format!("继续深入：{q}"),
        goal: question.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_line_is_lifted() {
        let raw = "结论在这。\n\nOFFER: {\"title\":\"改按钮\",\"goal\":\"把按钮改成蓝色\"}\n";
        let (t, o) = split_offer(raw);
        assert_eq!(t, "结论在这。");
        assert_eq!(o.unwrap().title, "改按钮");
        let (t, o) = split_offer("没有提议");
        assert_eq!(t, "没有提议");
        assert!(o.is_none());
    }

    #[test]
    fn cites() {
        let read = vec!["src/retrieve.rs".to_string()];
        let c = citations(
            "`:5` 在开头不算。检索在 `src/retrieve.rs:42` 里，打分见 src/retrieve.rs:50-61，入口 lib.rs:3（`:7-9`）。时间 10:30 不算。",
            &read,
        );
        assert_eq!(c.len(), 4, "{c:?}");
        assert_eq!(
            (c[3].path.as_str(), c[3].line, c[3].end),
            ("lib.rs", 7, Some(9))
        );
        assert_eq!(c[0].path, "src/retrieve.rs");
        assert_eq!(c[0].line, 42);
        assert!(c[0].read);
        assert_eq!(c[1].end, Some(61));
        assert_eq!(c[2].path, "lib.rs");
        assert!(!c[2].read);
    }

    #[test]
    fn marks() {
        assert_eq!(with_mark("半句", MARK_STOPPED), "半句\n\n（已停下）");
        assert_eq!(with_mark("", MARK_BUDGET), MARK_BUDGET);
    }
}
