//! MIME → what the mail store keeps: headers, addresses, subject
//! (RFC 2047), text (declared charset, HTML → text when there is no plain part), the new part of the
//! text without quotes (`body_new`), a snippet, attachments with their IMAP part paths, the bulk
//! marker, and a rough guess of the sender's organization from the signature.

use std::collections::HashMap;

use mail_parser::{MessageParser, MessagePart, MessageRef};

use crate::adapter::PartMeta;

/// `body_text` is cut at 200 KB.
pub const MAX_TEXT_BYTES: usize = 200 * 1024;
/// `body_new` is cut at 20000 characters.
pub const MAX_NEW_CHARS: usize = 20_000;
/// The snippet keeps at most 200 characters.
pub const MAX_SNIPPET_CHARS: usize = 200;

/// An address with its display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addr {
    pub addr: String,
    pub name: String,
}

/// A parsed message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub refs: Vec<String>,
    pub from: Option<Addr>,
    pub reply_to: Option<String>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    /// Unix ms of the `Date` header.
    pub date: Option<i64>,
    pub text: Option<String>,
    pub body_new: Option<String>,
    pub snippet: String,
    pub has_html: bool,
    pub attachments: Vec<PartMeta>,
    /// `List-Unsubscribe` or `Precedence: bulk|list|junk`.
    pub bulk: bool,
    pub signature_org: Option<String>,
}

impl Parsed {}

/// Parses a message. `header_only`: the raw bytes are only the header (very large mail), so there is
/// no text and no attachment list.
pub fn parse(raw: &[u8], header_only: bool) -> Option<Parsed> {
    let msg = MessageParser::new().parse(raw)?;
    let m = msg.root();
    let addrs = |l: Option<mail_parser::AddressList<'_>>| -> Vec<Addr> {
        l.map(|l| {
            l.mailboxes()
                .filter_map(|mb| {
                    let a = mb.address()?.trim();
                    (!a.is_empty()).then(|| Addr {
                        addr: a.to_string(),
                        name: mb.name().unwrap_or("").trim().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    let from = addrs(m.from()).into_iter().next();
    let reply_to = addrs(m.reply_to()).into_iter().next().map(|a| a.addr);
    let h = m.headers();
    let bulk = h.contains("List-Unsubscribe")
        || h.get("Precedence").is_some_and(|p| {
            let v = String::from_utf8_lossy(p.raw_value())
                .trim()
                .to_ascii_lowercase();
            matches!(v.as_str(), "bulk" | "list" | "junk")
        });
    let mut p = Parsed {
        message_id: m.message_id().map(clean_id).filter(|s| !s.is_empty()),
        in_reply_to: m
            .in_reply_to()
            .and_then(|l| l.first().map(clean_id))
            .filter(|s| !s.is_empty()),
        refs: m
            .references()
            .map(|l| l.iter().map(clean_id).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        from,
        reply_to,
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        subject: m.subject().unwrap_or("").trim().to_string(),
        date: m.date().map(|d| d.to_timestamp() * 1000),
        bulk,
        ..Default::default()
    };
    if header_only {
        return Some(p);
    }
    p.has_html = m
        .html_body()
        .any(|part| part.is_content_type("text", "html"));
    let plain = m
        .text_body()
        .any(|part| part.is_content_type("text", "plain"));
    let text = if plain {
        m.body_text(0).map(|t| t.into_owned())
    } else {
        m.body_html(0)
            .and_then(|h| html2text::from_read(h.as_bytes(), 100).ok())
    };
    if let Some(t) = text {
        let t = normalize_newlines(&t);
        let new = strip_quotes(&t);
        p.snippet = snippet(&new);
        p.signature_org = signature_org(&new, p.from.as_ref().map(|f| f.name.as_str()));
        p.body_new = Some(cut_chars(&new, MAX_NEW_CHARS));
        p.text = Some(cut_bytes(&t, MAX_TEXT_BYTES));
    }
    p.attachments = attachments(&m);
    Some(p)
}

fn clean_id(s: &str) -> String {
    s.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// The first `n` characters.
pub fn cut_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// At most `n` bytes, on a character boundary.
pub fn cut_bytes(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut i = n;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    s[..i].to_string()
}

/// Whitespace collapsed, at most 200 characters.
pub fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cut_chars(&flat, MAX_SNIPPET_CHARS)
}

/// A line that starts a quoted original: `On … wrote:`, `在 … 写道：`, Outlook's separator or
/// `-----Original Message-----`, or a `From:` header block (`From:` followed by `Sent:`/`Date:`/`To:`
/// within the next lines).
fn quote_start(lines: &[&str], i: usize) -> bool {
    let l = lines[i].trim();
    let lower = l.to_ascii_lowercase();
    if (lower.starts_with("on ") && lower.ends_with("wrote:"))
        || (l.starts_with('在') && (l.ends_with("写道：") || l.ends_with("写道:")))
        || lower.contains("-----original message-----")
        || l.contains("原始邮件")
    {
        return true;
    }
    // Outlook: a line of underscores before the header block
    if l.len() >= 10 && l.chars().all(|c| c == '_') {
        return true;
    }
    let is_from =
        lower.starts_with("from:") || l.starts_with("发件人：") || l.starts_with("发件人:");
    if is_from {
        return lines[i + 1..].iter().take(4).any(|n| {
            let n = n.trim().to_ascii_lowercase();
            n.starts_with("sent:")
                || n.starts_with("date:")
                || n.starts_with("to:")
                || n.starts_with("发送时间")
                || n.starts_with("收件人")
        });
    }
    false
}

/// The new content of a reply: drops `>` lines and everything from the first quote marker on;
/// the signature stays. Trailing blank lines are trimmed.
pub fn strip_quotes(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    for i in 0..lines.len() {
        if quote_start(&lines, i) {
            break;
        }
        if lines[i].trim_start().starts_with('>') {
            continue;
        }
        out.push(lines[i].trim_end());
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    out.join("\n")
}

/// The organization a signature names, roughly: in the last 10 lines, `名字 · 组织`
/// → the part after the dot; `组织 | 职位` → the part before the bar, or the next line when the part
/// before is the sender's own name (`Chen Jie | Procurement Lead` / `Partner Co.`).
pub fn signature_org(text: &str, sender_name: Option<&str>) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(10)..];
    let name = sender_name.unwrap_or("").trim();
    let ok = |s: &str| {
        let s = s.trim();
        (!s.is_empty() && s.chars().count() <= 60 && s != name).then(|| s.to_string())
    };
    for (i, l) in tail.iter().enumerate().rev() {
        if l.chars().count() > 80 {
            continue;
        }
        for dot in [" · ", "·", " - "] {
            if let Some((a, b)) = l.split_once(dot)
                && !a.trim().is_empty()
                && (name.is_empty() || a.trim() == name)
                && let Some(o) = ok(b)
            {
                return Some(o);
            }
        }
        if let Some((a, _)) = l.split_once('|').or_else(|| l.split_once('｜')) {
            let a = a.trim();
            if !name.is_empty() && a == name {
                if let Some(next) = tail.get(i + 1)
                    && let Some(o) = ok(next)
                {
                    return Some(o);
                }
                continue;
            }
            if let Some(o) = ok(a) {
                return Some(o);
            }
        }
    }
    None
}

/// IMAP part paths by part id: the children of a multipart are numbered from 1 under its path; a
/// single-part message's body is `1`.
fn part_paths(m: &MessageRef<'_>) -> HashMap<u32, String> {
    fn walk(p: MessagePart<'_>, path: String, out: &mut HashMap<u32, String>) {
        if p.is_multipart() {
            for (i, c) in p.children().enumerate() {
                let sub = if path.is_empty() {
                    format!("{}", i + 1)
                } else {
                    format!("{path}.{}", i + 1)
                };
                walk(c, sub, out);
            }
        }
        out.insert(p.id(), if path.is_empty() { "1".into() } else { path });
    }
    let mut out = HashMap::new();
    walk(m.root_part(), String::new(), &mut out);
    out
}

fn attachments(m: &MessageRef<'_>) -> Vec<PartMeta> {
    let paths = part_paths(m);
    m.attachments()
        .map(|a| {
            let mime = a
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(s) => format!("{}/{}", ct.ctype(), s),
                    None => ct.ctype().to_string(),
                })
                .unwrap_or_else(|| "application/octet-stream".into())
                .to_ascii_lowercase();
            PartMeta {
                part: paths.get(&a.id()).cloned().unwrap_or_else(|| "1".into()),
                name: a
                    .attachment_name()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .unwrap_or("未命名附件")
                    .to_string(),
                mime,
                size: a.decoded_len() as i64,
                content_id: a.content_id().map(clean_id),
                inline: a.is_inline() && a.content_id().is_some(),
            }
        })
        .collect()
}

/// The HTML body of a message (the original `text/html` part, not one converted from text).
pub fn html_body(raw: &[u8]) -> Option<String> {
    let msg = MessageParser::new().parse(raw)?;
    let m = msg.root();
    if !m
        .html_body()
        .any(|part| part.is_content_type("text", "html"))
    {
        return None;
    }
    m.body_html(0).map(|h| h.into_owned())
}

/// The decoded bytes of the attachment at part path `part` (`mail_attachments.part`), taken from the
/// original message.
pub fn attachment_bytes(raw: &[u8], part: &str) -> Option<Vec<u8>> {
    let msg = MessageParser::new().parse(raw)?;
    let m = msg.root();
    let paths = part_paths(&m);
    m.attachments()
        .find(|a| paths.get(&a.id()).map(String::as_str) == Some(part))
        .map(|a| a.decoded().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_are_cut() {
        let t = "三步：部门负责人、财务、CFO。\n\nOn Mon, Oct 26, 2026 at 8:00 AM Alice <a@x> wrote:\n> 你好\n";
        assert_eq!(strip_quotes(t), "三步：部门负责人、财务、CFO。");
        let t = "收到\n\n在 2026年10月26日 周一 17:30，Me <me@example.com> 写道：\n> 三步\n";
        assert_eq!(strip_quotes(t), "收到");
        let t = "fine\n\nChen Jie | Lead\nPartner Co.\n\n________________________________\nFrom: Me\nSent: x\n\nold";
        assert_eq!(strip_quotes(t), "fine\n\nChen Jie | Lead\nPartner Co.");
        let t = "a\n> q\nb";
        assert_eq!(strip_quotes(t), "a\nb");
    }

    #[test]
    fn signature_orgs() {
        assert_eq!(
            signature_org("谢谢\n张伟\n华东销售部 | 区域经理", Some("张伟")).as_deref(),
            Some("华东销售部")
        );
        assert_eq!(
            signature_org(
                "ok\n\nChen Jie | Procurement Lead\nPartner Co.",
                Some("Chen Jie")
            )
            .as_deref(),
            Some("Partner Co.")
        );
        assert_eq!(
            signature_org("hi\nSarah Chen · Acme", Some("Sarah Chen")).as_deref(),
            Some("Acme")
        );
        assert_eq!(signature_org("just text", Some("X")), None);
    }

    #[test]
    fn cuts_on_char_boundaries() {
        assert_eq!(cut_chars("你好世界", 2), "你好");
        assert_eq!(cut_bytes("你好", 4), "你");
        assert_eq!(snippet("a\n\n  b   c"), "a b c");
    }
}
