//! What the fakes need to know about a message, read once with `mail-parser`
//! (the same parser the real sync uses, email.md §3.1).

use mail_parser::{AddressList, MessageParser};

use super::bare_id;

/// Header facts of one message. Used by the fakes for ENVELOPE, threading
/// (`X-GM-THRID`, Gmail `threadId`, Graph `conversationId`) and the JSON
/// resources of the HTTP fakes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MailMeta {
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub subject: String,
    /// `(display name, address)`.
    pub from: Option<(String, String)>,
    pub to: Vec<(String, String)>,
    pub cc: Vec<(String, String)>,
    pub bcc: Vec<(String, String)>,
    pub reply_to: Vec<(String, String)>,
    /// Unix seconds of the `Date` header.
    pub date: Option<i64>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub snippet: String,
    pub has_attachments: bool,
    /// Raw (still encoded) header values, as ENVELOPE wants them.
    pub raw_subject: Option<String>,
    pub raw_date: Option<String>,
}

fn addrs(list: Option<AddressList<'_>>) -> Vec<(String, String)> {
    list.map(|l| {
        l.mailboxes()
            .filter_map(|m| {
                Some((
                    m.name().unwrap_or_default().to_string(),
                    m.address()?.to_string(),
                ))
            })
            .collect()
    })
    .unwrap_or_default()
}

impl MailMeta {
    /// Parses the message; never fails (missing fields stay empty).
    pub fn parse(raw: &[u8]) -> Self {
        let Some(msg) = MessageParser::new().parse(raw) else {
            return Self::default();
        };
        let root = msg.root();
        let headers = root.headers();
        let raw_value = |name: &str| {
            headers
                .get(name)
                .map(|h| String::from_utf8_lossy(h.raw_value()).trim().to_string())
        };
        Self {
            message_id: root.message_id().map(bare_id),
            subject: root.subject().unwrap_or_default().to_string(),
            from: addrs(root.from()).into_iter().next(),
            to: addrs(root.to()),
            cc: addrs(root.cc()),
            bcc: addrs(root.bcc()),
            reply_to: addrs(root.reply_to()),
            date: root.date().map(|d| d.to_timestamp()),
            in_reply_to: root
                .in_reply_to()
                .map(|l| l.iter().map(bare_id).collect())
                .unwrap_or_default(),
            references: root
                .references()
                .map(|l| l.iter().map(bare_id).collect())
                .unwrap_or_default(),
            snippet: root
                .body_preview(200)
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default(),
            has_attachments: root.attachments().any(|p| !p.is_inline()),
            raw_subject: raw_value("Subject"),
            raw_date: raw_value("Date"),
        }
    }

    /// Ids this message points back at (`In-Reply-To` first, then `References`).
    pub fn parent_ids(&self) -> impl Iterator<Item = &String> {
        self.in_reply_to.iter().chain(self.references.iter().rev())
    }
}
