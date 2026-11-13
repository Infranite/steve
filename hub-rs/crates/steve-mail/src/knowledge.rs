//! Mail and the knowledge base.
//!
//! Mail bodies are not indexed by default. Two ways in:
//! - “加进知识库” on a thread ([`Mail::to_knowledge`], `POST /mail/threads/:id/to-knowledge`): the
//!   thread (sender, date and `body_new` of every message) becomes one document of the built-in
//!   `files` source, `private` and level `reasoning`, indexed in the same transaction so it is
//!   searchable at once. [`ThreadLoader`] re-reads it when the index queue asks.
//! - A thread becoming `done` ([`Mail::submit_done`], called from [`Mail::on_item_finished`]): what
//!   you sent and the other side's last message go to `steve_knowledge::extract::submit`
//!   (`origin = mail`, default level `reasoning`), once per state of the thread (`kb_marks
//!   extract:mail:<thread>` = its last message).

use rusqlite::Connection;
use serde_json::json;
use steve_knowledge::candidates::OriginKind;
use steve_knowledge::chunk::TextKind;
use steve_knowledge::extract_text::{Extracted, FrontMatter};
use steve_knowledge::index::{self, BoxFuture, DocLoader, Loaded};
use steve_knowledge::repo::{self as kb_repo, DocRow, NewDoc, SourceRow};
use steve_modules::{Caller, Modules};
use steve_store::base::{self, Level, ObjRef, Visibility};
use steve_store::uid;

use crate::Mail;
use crate::error::MailResult;
use crate::repo::{self, MessageRow, ThreadRow};

/// The built-in knowledge source mail threads are added to.
pub const SOURCE_KIND: &str = "files";
/// Level of a thread document and default level of mail candidates.
pub const LEVEL: Level = Level::Reasoning;
/// `kb_marks` name prefix of the candidate extraction of a thread.
pub const MARK_PREFIX: &str = "extract:mail:";

fn date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn sender(m: &MessageRow) -> String {
    let name = m.from_name.trim();
    if name.is_empty() {
        m.from_addr.clone()
    } else {
        format!("{name} <{}>", m.from_addr)
    }
}

fn body(m: &MessageRow) -> String {
    match m.body_new.as_deref().map(str::trim) {
        Some(b) => b.to_string(),
        None => format!("（正文已过本地保留期限，摘要：{}）", m.snippet),
    }
}

/// The document text of a thread: one Markdown section per message, oldest first (so each chunk's
/// heading names who wrote it and when).
pub fn thread_text(msgs: &[MessageRow]) -> String {
    msgs.iter()
        .map(|m| format!("## {} · {}\n\n{}", sender(m), date(m.date), body(m)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The extraction input of a finished thread: every message you sent and the other side's last one.
/// Empty when there is neither.
pub fn candidate_text(msgs: &[MessageRow]) -> String {
    let last_in = msgs.iter().rposition(|m| m.direction == "in");
    msgs.iter()
        .enumerate()
        .filter(|(i, m)| m.direction == "out" || Some(*i) == last_in)
        .map(|(_, m)| {
            let who = if m.direction == "out" {
                format!("你发出的邮件（{}）", date(m.date))
            } else {
                format!("对方最后一封（{}，{}）", sender(m), date(m.date))
            };
            format!("{who}：\n{}", body(m))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extracted(text: String) -> Extracted {
    Extracted {
        text,
        kind: TextKind::Markdown,
        front: FrontMatter::default(),
    }
}

/// The parties who wrote to you in the thread (the document's “关于”).
fn senders(msgs: &[MessageRow]) -> Vec<ObjRef> {
    let mut out: Vec<ObjRef> = Vec::new();
    for p in msgs
        .iter()
        .filter(|m| m.direction == "in")
        .filter_map(|m| m.from_party_id.as_deref())
    {
        let r = ObjRef::new("party", p);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Whether a knowledge document is a mail thread: in `files`, no attachment, `ref` = `mt_…`.
pub fn is_thread_doc(doc: &DocRow) -> bool {
    doc.file_id.is_none() && doc.r#ref.starts_with("mt_")
}

/// Reads thread documents for the knowledge index queue. Register it before `FilesLoader`:
/// `Knowledge::with_loaders(notify, vec![Arc::new(ThreadLoader), Arc::new(FilesLoader::new(..))])`.
pub struct ThreadLoader;

fn load_thread(c: &Connection, doc: &DocRow) -> MailResult<Option<String>> {
    let Some(t) = repo::thread(c, &doc.r#ref)?.filter(|t| t.workspace_id == doc.workspace_id)
    else {
        return Ok(None);
    };
    Ok(Some(thread_text(&repo::thread_messages(c, &t.id)?)))
}

impl DocLoader for ThreadLoader {
    fn kinds(&self) -> &[&str] {
        &[SOURCE_KIND]
    }
    fn accepts(&self, doc: &DocRow) -> bool {
        is_thread_doc(doc)
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        doc: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            match m.store.reader(|c| load_thread(c, doc)) {
                Ok(Some(text)) => Loaded::Text(extracted(text)),
                Ok(None) => Loaded::Gone,
                Err(e) => Loaded::Failed(format!("读取邮件往来失败：{e}")),
            }
        })
    }
}

impl Mail {
    /// “加进知识库”: adds a thread the caller sees as a `files` document (`private`,
    /// `reasoning`) and indexes it now. Adding it again re-indexes the same document. Returns its id.
    pub fn to_knowledge(&self, who: &Caller, thread_id: &str) -> MailResult<String> {
        let viewer = self.viewer_of(who)?;
        let t: ThreadRow = self
            .0
            .m
            .store
            .reader(|c| self.own_thread(c, &viewer, thread_id))?;
        steve_knowledge::service::prepare(&self.0.m, who)?;
        let now = self.now();
        let id = self.0.m.store.transaction(|tx| -> MailResult<String> {
            let msgs = repo::thread_messages(tx, &t.id)?;
            let src = steve_knowledge::sources::builtin(tx, &who.ws, SOURCE_KIND, &who.me, now)?;
            let id = match kb_repo::doc_by_ref(tx, &src.id, &t.id)? {
                Some(d) => d.id,
                None => {
                    let id = uid("kd");
                    let title = if t.subject.trim().is_empty() {
                        "（无主题）".to_string()
                    } else {
                        t.subject.chars().take(120).collect()
                    };
                    kb_repo::insert_doc(
                        tx,
                        &who.ws,
                        &NewDoc {
                            id: id.clone(),
                            source_id: src.id.clone(),
                            r#ref: t.id.clone(),
                            title,
                            mime: "text/markdown".into(),
                            visibility: Some(Visibility::Private),
                            level: Some(LEVEL),
                            created_by: who.me.clone(),
                            ..Default::default()
                        },
                        now,
                    )?;
                    base::audit(
                        tx,
                        &who.ws,
                        &who.me,
                        "mail_thread.to_knowledge",
                        &t.id,
                        &json!({ "doc_id": id }),
                    )?;
                    id
                }
            };
            kb_repo::set_doc_about(tx, &who.ws, &id, &senders(&msgs))?;
            let doc = kb_repo::doc(tx, &id)?.ok_or(steve_store::StoreError::NotFound("文档"))?;
            index::index_doc(tx, &doc, &src, &extracted(thread_text(&msgs)), now, false)?;
            Ok(id)
        })?;
        if let Some(kb) = &self.0.kb {
            kb.notify(&who.ws, &id);
        }
        Ok(id)
    }

    /// A thread became `done`: submits candidate memories from it. Skipped without the
    /// knowledge module, when nothing was said, or when this state of the thread (same last message)
    /// was already submitted. Returns how many candidates were written.
    pub async fn submit_done(&self, ws: &str, thread_id: &str) -> MailResult<usize> {
        let Some(kb) = self.0.kb.clone() else {
            return Ok(0);
        };
        let mark = format!("{MARK_PREFIX}{thread_id}");
        let found = self.0.m.store.reader(|c| -> MailResult<_> {
            let Some(t) = repo::thread(c, thread_id)?.filter(|t| t.workspace_id == ws) else {
                return Ok(None);
            };
            let msgs = repo::thread_messages(c, &t.id)?;
            let done = kb_repo::mark(c, ws, &mark)?;
            Ok(Some((t, msgs, done)))
        })?;
        let Some((t, msgs, done)) = found else {
            return Ok(0);
        };
        let Some(last) = msgs.last().map(|m| m.id.clone()) else {
            return Ok(0);
        };
        if done.as_deref() == Some(last.as_str()) {
            return Ok(0);
        }
        let text = candidate_text(&msgs);
        let n = if text.trim().is_empty() {
            0
        } else {
            let subject = if t.subject.trim().is_empty() {
                "（无主题）"
            } else {
                t.subject.trim()
            };
            let label = format!("邮件往来「{subject}」");
            steve_knowledge::extract::submit(
                &self.0.m,
                &kb,
                ws,
                OriginKind::Mail,
                &t.id,
                &label,
                &text,
                LEVEL,
                Some(&t.created_by),
            )
            .await?
            .len()
        };
        let now = self.now();
        self.0
            .m
            .store
            .transaction(|tx| kb_repo::set_mark(tx, ws, &mark, &last, now))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn msg(id: &str, dir: &str, name: &str, body: Option<&str>, date: i64) -> MessageRow {
        MessageRow {
            id: id.into(),
            workspace_id: "w".into(),
            connection_id: "con".into(),
            thread_id: "mt_1".into(),
            ext_id: id.into(),
            message_id: None,
            in_reply_to: None,
            refs: vec![],
            folders: vec![],
            flags: vec![],
            direction: dir.into(),
            from_addr: format!("{id}@example.com"),
            from_name: name.into(),
            from_account_id: None,
            from_party_id: (dir == "in").then(|| format!("ct_{id}")),
            reply_to: None,
            to_addrs: Value::Null,
            cc_addrs: Value::Null,
            subject: "s".into(),
            snippet: "摘要".into(),
            date,
            size: 0,
            has_attachments: false,
            bulk: false,
            body_text: None,
            body_new: body.map(String::from),
            has_html: false,
            raw_path: None,
            purged: None,
            event_id: None,
            status: "received".into(),
            created: 0,
        }
    }

    #[test]
    fn texts_of_a_thread() {
        let msgs = vec![
            msg("a", "in", "Sarah", Some("能改到两点吗？"), 0),
            msg("b", "out", "", Some("可以，两点见。"), 60_000),
            msg("c", "in", "Sarah", None, 120_000),
            msg("d", "in", "", Some("好的"), 180_000),
        ];
        let t = thread_text(&msgs);
        assert!(t.starts_with("## Sarah <a@example.com> · 1970-01-01 00:00\n\n能改到两点吗？"));
        assert!(t.contains("（正文已过本地保留期限，摘要：摘要）"));
        assert!(t.contains("## d@example.com · 1970-01-01 00:03\n\n好的"));
        let c = candidate_text(&msgs);
        assert!(c.contains("你发出的邮件（1970-01-01 00:01）：\n可以，两点见。"));
        assert!(c.contains("对方最后一封（d@example.com，1970-01-01 00:03）：\n好的"));
        assert!(
            !c.contains("能改到两点吗"),
            "only the last incoming message"
        );
        assert_eq!(
            senders(&msgs),
            vec![
                ObjRef::new("party", "ct_a"),
                ObjRef::new("party", "ct_c"),
                ObjRef::new("party", "ct_d")
            ]
        );
        assert!(candidate_text(&[]).is_empty());
    }
}
