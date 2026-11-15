//! Rich content as text plus references, deterministically.
//!
//! What a message carries besides its words becomes a reference (`refs` of a packet): its type, title, the
//! message it came from and what identifies it at the source (a URL and domain, a document token and type, an
//! attachment key, size and type). Heavy content never goes into a window; the digest Bot fetches it by
//! reference when it needs it (`steve_im_read`, the connector's read tools, the knowledge tools).
//!
//! - text: the adapters already flatten rich text, Markdown and cards into readable text (titles, lists, @
//!   names, links) and keep the structure in the attachment (`card` blocks); the text is used as is;
//! - links: a Feishu / Lark document, wiki, sheet, base, slides, file or mind-note link becomes a `doc`
//!   reference with its token and type; a GitHub pull request, issue or commit link a `pr`, `issue` or `commit`
//!   reference; any other link a `link` with its domain;
//! - item numbers (`ST-12`) become `item` references once the caller confirmed the item exists;
//! - attachments: images (`image`, with size when known), files (`file`, with name, size and MIME), cards
//!   (`card`, with title) become placeholders in the text;
//! - forwarded messages (an attachment `{type: "forward", messages: [{id, sender, text, at}]}`, the shape a
//!   connector gives) are expanded under the message with their original ids; a quoted message outside the
//!   window is shown with its id and the start of its text.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

/// A reference a window found.
#[derive(Debug, Clone, Serialize)]
pub struct Ref {
    /// `r1`, `r2` … within the packet.
    pub n: usize,
    /// `item|doc|link|pr|issue|commit|image|file|card|forward`
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    /// The position and Steve id of the message it came from.
    pub pos: i64,
    pub message: String,
    /// What identifies it at the source: `url`, `domain`, `token`, `doc_type`, `key`, `size`, `mime`, `item`,
    /// `forwarded` (original ids).
    #[serde(flatten)]
    pub at: Value,
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'）)\]】，。；！？]+"#).expect("url"));
static ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-[0-9]{1,6})\b").expect("item"));

/// The Feishu / Lark document types a link path names.
const DOC_PATHS: [&str; 9] = [
    "docx",
    "docs",
    "doc",
    "wiki",
    "sheets",
    "base",
    "slides",
    "file",
    "mindnotes",
];

/// What a link points at: `(type, fields)`.
pub fn classify_url(url: &str) -> (String, Value) {
    let rest = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();
    let host = host
        .rsplit_once('@')
        .map(|x| x.1.to_string())
        .unwrap_or(host);
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let lark = ["feishu.cn", "larksuite.com", "larkoffice.com", "feishu.net"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if lark {
        for (i, s) in segs.iter().enumerate() {
            if DOC_PATHS.contains(s)
                && let Some(token) = segs.get(i + 1)
            {
                return (
                    "doc".into(),
                    json!({"url": url, "domain": host, "doc_type": s, "token": token}),
                );
            }
        }
    }
    if host == "github.com" && segs.len() >= 4 {
        let repo = format!("{}/{}", segs[0], segs[1]);
        let kind = match segs[2] {
            "pull" => Some("pr"),
            "issues" => Some("issue"),
            "commit" => Some("commit"),
            _ => None,
        };
        if let Some(k) = kind {
            return (
                k.into(),
                json!({"url": url, "domain": host, "repo": repo, "number": segs[3]}),
            );
        }
    }
    ("link".into(), json!({"url": url, "domain": host}))
}

/// The links in `text`, in order, without trailing punctuation.
pub fn urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in URL.find_iter(text) {
        let u = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

/// Item numbers written in `text` (to be checked against the items).
pub fn item_numbers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in ITEM.captures_iter(text) {
        let s = c[1].to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// A size in bytes, readable.
pub fn size(n: i64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// One attachment as a reference (without its number): `None` for what is no reference.
pub fn attachment_ref(a: &Value) -> Option<(String, String, Value)> {
    match s(a, "type") {
        "image" => {
            let mut at = json!({"key": s(a, "key")});
            let (w, h) = (
                a.get("width").and_then(Value::as_i64),
                a.get("height").and_then(Value::as_i64),
            );
            if let (Some(w), Some(h)) = (w, h) {
                at["dims"] = json!(format!("{w}×{h}"));
            }
            Some(("image".into(), s(a, "name").to_string(), at))
        }
        "file" => {
            let mut at = json!({"key": s(a, "key")});
            for k in ["kind", "mime"] {
                if !s(a, k).is_empty() {
                    at[k] = json!(s(a, k));
                }
            }
            if let Some(n) = a.get("size").and_then(Value::as_i64) {
                at["size"] = json!(n);
            }
            Some(("file".into(), s(a, "name").to_string(), at))
        }
        "card" => {
            let title = a
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(("card".into(), title, json!({"format": s(a, "format")})))
        }
        _ => None,
    }
}

/// The placeholder of a reference in the text: `〔图片 r3 · 1280×720〕`.
pub fn placeholder(r: &Ref) -> String {
    let label = match r.kind.as_str() {
        "image" => "图片",
        "file" => "文件",
        "card" => "卡片",
        "doc" => "文档",
        "pr" => "PR",
        "issue" => "Issue",
        "commit" => "提交",
        "item" => "事项",
        "forward" => "转发",
        _ => "链接",
    };
    let mut parts = vec![format!("{label} r{}", r.n)];
    if !r.title.is_empty() {
        parts.push(r.title.clone());
    }
    if let Some(d) = r.at.get("dims").and_then(Value::as_str) {
        parts.push(d.to_string());
    }
    if let Some(n) = r.at.get("size").and_then(Value::as_i64) {
        parts.push(size(n));
    }
    if r.kind == "link"
        && let Some(d) = r.at.get("domain").and_then(Value::as_str)
    {
        parts.push(d.to_string());
    }
    if r.kind == "doc"
        && let Some(d) = r.at.get("doc_type").and_then(Value::as_str)
    {
        parts.push(d.to_string());
    }
    format!("〔{}〕", parts.join(" · "))
}

/// The forwarded messages an attachment carries: `(original id, sender, at, text)`.
pub fn forwarded(a: &Value) -> Vec<(String, String, Option<i64>, String)> {
    if !matches!(s(a, "type"), "forward" | "merge_forward") {
        return Vec::new();
    }
    a.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| {
            let sender = m
                .get("sender")
                .map(|x| match x {
                    Value::String(n) => n.clone(),
                    o => s(o, "name").to_string(),
                })
                .unwrap_or_default();
            (
                s(m, "id").to_string(),
                sender,
                m.get("at").and_then(Value::as_i64),
                s(m, "text").to_string(),
            )
        })
        .collect()
}

/// Normalised text for counting and display: whitespace runs inside lines kept, blank-line runs folded.
pub fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for l in text.lines() {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(l.trim_end());
    }
    out.trim().to_string()
}

/// Estimated tokens of `text`: a CJK character is about one, other text about four characters a token.
pub fn tokens(text: &str) -> i64 {
    let (mut cjk, mut other) = (0i64, 0i64);
    for c in text.chars() {
        if steve_knowledge::seg::is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + (other + 3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_classified() {
        let (k, at) = classify_url("https://acme.feishu.cn/docx/AbC123?from=x");
        assert_eq!(k, "doc");
        assert_eq!(at["token"], "AbC123");
        assert_eq!(at["doc_type"], "docx");
        let (k, at) = classify_url("https://acme.larksuite.com/wiki/WkN9");
        assert_eq!((k.as_str(), at["doc_type"].as_str()), ("doc", Some("wiki")));
        let (k, at) = classify_url("https://github.com/o/r/pull/12/files");
        assert_eq!((k.as_str(), at["number"].as_str()), ("pr", Some("12")));
        let (k, at) = classify_url("https://example.com/a/b");
        assert_eq!(
            (k.as_str(), at["domain"].as_str()),
            ("link", Some("example.com"))
        );
    }

    #[test]
    fn urls_and_items_are_found() {
        assert_eq!(
            urls("看 https://a.com/x。 和 https://b.cn/y），再看 https://a.com/x"),
            vec!["https://a.com/x", "https://b.cn/y"]
        );
        assert_eq!(
            item_numbers("ST-12 和 ST-3，不是 st-4"),
            vec!["ST-12", "ST-3"]
        );
    }

    #[test]
    fn tokens_count_cjk_as_one() {
        assert_eq!(tokens("部署脚本"), 4);
        assert_eq!(tokens("abcdefgh"), 2);
    }

    #[test]
    fn forwards_keep_their_ids() {
        let a = json!({"type": "forward", "messages": [{"id": "om_1", "sender": {"name": "张三"}, "text": "原话", "at": 5}]});
        assert_eq!(
            forwarded(&a),
            vec![("om_1".into(), "张三".into(), Some(5), "原话".into())]
        );
    }
}
