//! Turning the model's raw answer into what the thread shows: the `OFFER:` line lifted into an
//! [`Offer`], `path:line` citations collected, and the end mark of a run that did not finish.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub const MARK_STOPPED: &str = "（已停下）";
pub const MARK_BUDGET: &str = "（没读完就停了）";
pub const MARK_VIOLATION: &str = "（本次查询在工具检查时停止，尚未完成。）";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub title: String,
    pub goal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
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

pub fn with_mark(text: &str, mark: &str) -> String {
    let t = text.trim_end();
    if t.is_empty() {
        mark.to_string()
    } else {
        format!("{t}\n\n{mark}")
    }
}

pub fn deeper_offer(question: &str) -> Offer {
    let q: String = question.trim().chars().take(40).collect();
    Offer {
        title: format!("继续深入：{q}"),
        goal: question.trim().to_string(),
    }
}
