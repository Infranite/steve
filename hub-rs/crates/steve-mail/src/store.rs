//! The local mail library (email.md §3.5): [`StoreSink`] takes what an adapter fetched and stores it —
//! parse, dedupe across folders and rescans, raw file (0600), attachments (metadata only),
//! identification of the people involved, threads, and the events to send once the sync is done.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::TimeZone;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::accounts::identify::{Hints, identify};
use steve_modules::accounts::repo as arepo;
use steve_modules::accounts::service::current_party;
use steve_modules::contacts::repo as people;
use steve_store::{Store, Tx, outbox, uid};

use crate::adapter::{FetchedMessage, MailSink, ScopeMode};
use crate::error::{MailError, MailResult};
use crate::events::{Actor, Delivery, MailEvent, classify};
use crate::parse::{self, Addr, Parsed};
use crate::repo::{self, MessageRow, ThreadRow};

/// The text of an event payload is cut at 8000 characters.
pub const EVENT_TEXT_CHARS: usize = 8000;

/// What the store knows about the mailbox it fills.
#[derive(Debug, Clone)]
pub struct Mailbox {
    pub ws: String,
    pub connection_id: String,
    /// The connection's creator: owner of the private threads, actor of identification.
    pub owner: String,
    /// The mailbox's account (“我”'s email account).
    pub account_id: String,
    /// Every email address of “我” (lowercase): mail from one of them is outgoing.
    pub my_addrs: Vec<String>,
}

impl Mailbox {
    /// Loads it from the connection row.
    pub fn load(c: &Connection, ws: &str, connection_id: &str) -> MailResult<Self> {
        let r = steve_modules::accounts::connections::repo::get(c, connection_id)?
            .filter(|r| r.workspace_id == ws)
            .ok_or_else(|| MailError::Api(steve_modules::ApiError::not_found("连接")))?;
        let mut my_addrs = my_addresses(c, ws, &r.account_id)?;
        for k in ["address", "username"] {
            if let Some(a) = r.config.get(k).and_then(Value::as_str) {
                let a = a.trim().to_lowercase();
                if a.contains('@') && !my_addrs.contains(&a) {
                    my_addrs.push(a);
                }
            }
        }
        Ok(Self {
            ws: ws.into(),
            connection_id: connection_id.into(),
            owner: r.created_by,
            account_id: r.account_id,
            my_addrs,
        })
    }

    fn is_mine(&self, addr: &str) -> bool {
        let a = addr.trim().to_lowercase();
        self.my_addrs.contains(&a)
    }
}

/// Every email handle of the party that owns `account` (the mailbox owner's “我”).
pub fn my_addresses(c: &Connection, ws: &str, account: &str) -> MailResult<Vec<String>> {
    let party: Option<String> = c
        .prepare_cached("SELECT party_id FROM accounts WHERE id = ?")?
        .query_row([account], |r| r.get(0))
        .optional()?;
    let Some(party) = party else {
        return Ok(Vec::new());
    };
    let mut st = c.prepare_cached(
        "SELECT h.handle FROM account_handles h JOIN accounts a ON a.id = h.account_id
          WHERE h.workspace_id = ? AND h.family = 'email' AND a.party_id = ? ORDER BY h.handle",
    )?;
    Ok(st
        .query_map([ws, party.as_str()], |r| r.get::<_, String>(0))?
        .map(|r| r.map(|h| h.to_lowercase()))
        .collect::<rusqlite::Result<_>>()?)
}

/// `data/mail/<ws>/<id[4..6]>/<id>.eml`, relative to the Steve home.
pub fn raw_rel_path(ws: &str, id: &str) -> String {
    let shard: String = id.chars().skip(4).take(2).collect();
    let shard = if shard.is_empty() { "00".into() } else { shard };
    format!("data/mail/{ws}/{shard}/{id}.eml")
}

/// Resolves a stored `raw_path` under the home, refusing anything that leaves `data/mail/`.
pub fn raw_abs_path(home: &Path, rel: &str) -> Option<PathBuf> {
    let ok = rel.starts_with("data/mail/") && !rel.split('/').any(|p| p == ".." || p.is_empty());
    ok.then(|| home.join(rel))
}

pub(crate) fn write_raw(home: &Path, rel: &str, bytes: &[u8]) -> MailResult<()> {
    let path = raw_abs_path(home, rel).ok_or_else(|| MailError::Other("bad raw path".into()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// The outbox topic of a stored mail's event: emitted in the transaction that stores the mail,
/// delivered to the signals pipeline by [`crate::outbox::MailEventsOut`] after the commit, at least
/// once (a crash between storing and delivering loses nothing).
pub const EVENT_TOPIC: &str = "mail.event.ready";
pub const EVENT_V: i64 = 1;

/// The payload of [`EVENT_TOPIC`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEvent {
    pub message_id: String,
    pub connection_id: String,
    pub event: MailEvent,
    pub how: Delivery,
}

/// The sink of one sync run. Collects the threads it changed (one SSE each); each new mail's event
/// goes to the outbox with it.
pub struct StoreSink<'a> {
    store: &'a Store,
    home: &'a Path,
    now: i64,
    mailbox: &'a Mailbox,
    pub changed: BTreeSet<String>,
    pub stored: usize,
}

impl<'a> StoreSink<'a> {
    pub fn new(store: &'a Store, home: &'a Path, mailbox: &'a Mailbox, now: i64) -> Self {
        Self {
            store,
            home,
            now,
            mailbox,
            changed: BTreeSet::new(),
            stored: 0,
        }
    }
}

/// One address entry of `to_addrs`/`cc_addrs`.
fn addr_json(a: &Addr, account: Option<&str>, party: Option<&str>) -> Value {
    json!({"addr": a.addr, "name": a.name, "account_id": account, "party_id": party})
}

/// The account and current party of an email address, without creating anything.
fn lookup(c: &Connection, ws: &str, addr: &str) -> MailResult<(Option<String>, Option<String>)> {
    let h = addr.trim().to_lowercase();
    let Some(acc) = arepo::by_handle(c, ws, "email", &h)? else {
        return Ok((None, None));
    };
    let party = arepo::get(c, &acc)?
        .and_then(|a| current_party(c, &a.party_id).ok().flatten())
        .map(|p| p.id);
    Ok((Some(acc), party))
}

fn local_day(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%m-%d").to_string())
        .unwrap_or_default()
}

impl StoreSink<'_> {
    /// Identifies `a` (creating an unconfirmed contact when unknown).
    fn ident(
        &self,
        tx: &Tx,
        a: &Addr,
        org: Option<&str>,
        date: i64,
    ) -> MailResult<(Option<String>, Option<String>)> {
        let hints = Hints {
            display_name: (!a.name.is_empty()).then(|| a.name.clone()),
            signature_org: org.map(String::from),
            source_label: Some(format!("邮件 · {}", local_day(date))),
            ..Default::default()
        };
        match identify(
            tx,
            &self.mailbox.ws,
            &self.mailbox.owner,
            "mail",
            &a.addr,
            &hints,
        ) {
            Ok(i) => Ok((Some(i.account_id), Some(i.party_id))),
            // not a usable address (no `@`): keep the mail, without a contact
            Err(steve_modules::ApiError::Rule(_) | steve_modules::ApiError::Invalid(_)) => {
                Ok((None, None))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// An existing copy of this message (another folder, or the same folder after a UIDVALIDITY
    /// change). Returns it so the copy is merged instead of stored twice.
    fn existing(
        &self,
        c: &Connection,
        m: &FetchedMessage,
        p: &Parsed,
    ) -> MailResult<Option<MessageRow>> {
        let mb = self.mailbox;
        if let Some(r) = repo::message_by_ext(c, &mb.connection_id, &m.ext_id)? {
            return Ok(Some(r));
        }
        let date = p.date.or(m.internal_date);
        if let Some(mid) = &p.message_id {
            let rows = repo::messages_by_mid(c, &mb.ws, &mb.connection_id, mid)?;
            // the same Message-ID and date; a different date is a different message reusing the id
            return Ok(rows
                .into_iter()
                .find(|r| date.is_none_or(|d| (r.date - d).abs() < 1000 * 60 * 60 * 24)));
        }
        let from = p.from.as_ref().map(|f| f.addr.as_str()).unwrap_or("");
        match date {
            Some(d) => repo::message_by_shape(c, &mb.connection_id, from, d, &p.subject),
            None => Ok(None),
        }
    }

    fn merge_copy(&mut self, tx: &Tx, r: &MessageRow, m: &FetchedMessage) -> MailResult<()> {
        let mut folders = r.folders.clone();
        if !folders.contains(&m.folder) {
            folders.push(m.folder.clone());
        }
        // a rescan of the folder this row came from: follow the new UIDs
        let same_folder = r.ext_id == m.ext_id
            || r.ext_id
                .rsplitn(3, ':')
                .nth(2)
                .is_some_and(|f| f == m.folder);
        let ext = if same_folder { &m.ext_id } else { &r.ext_id };
        repo::update_copy(tx, &r.id, ext, &folders, &m.flags)?;
        repo::refresh_thread(tx, &r.thread_id, self.now)?;
        self.changed.insert(r.thread_id.clone());
        Ok(())
    }

    fn find_thread(
        &self,
        tx: &Tx,
        m: &FetchedMessage,
        p: &Parsed,
        date: i64,
    ) -> MailResult<ThreadRow> {
        let mb = self.mailbox;
        let found = match &m.provider_thread {
            Some(t) => repo::thread_by_key(tx, &mb.connection_id, t)?,
            None => {
                let mut ids: Vec<String> = p.in_reply_to.iter().cloned().collect();
                ids.extend(p.refs.iter().rev().cloned());
                match repo::thread_of_ids(tx, &mb.ws, &mb.connection_id, &ids)? {
                    Some(id) => repo::thread(tx, &id)?,
                    None => None,
                }
            }
        };
        if let Some(t) = found {
            return Ok(t);
        }
        let key = match (&m.provider_thread, &p.message_id) {
            (Some(t), _) => t.clone(),
            (None, Some(mid)) => mid.clone(),
            (None, None) => format!("{}@steve", m.ext_id),
        };
        if let Some(t) = repo::thread_by_key(tx, &mb.connection_id, &key)? {
            return Ok(t);
        }
        let t = ThreadRow {
            id: uid("mt"),
            workspace_id: mb.ws.clone(),
            connection_id: mb.connection_id.clone(),
            thread_key: key,
            subject: p.subject.clone(),
            last_at: date,
            count: 0,
            unread: 0,
            participants: Vec::new(),
            state: "open".into(),
            visibility: "private".into(),
            created_by: mb.owner.clone(),
        };
        repo::insert_thread(tx, &t, self.now)?;
        Ok(t)
    }

    fn store_new(&mut self, m: FetchedMessage, p: Parsed) -> MailResult<()> {
        let mb = self.mailbox.clone();
        let id = uid("ml");
        let date = p.date.or(m.internal_date).unwrap_or(self.now);
        let raw_path = if m.partial {
            None
        } else {
            let rel = raw_rel_path(&mb.ws, &id);
            write_raw(self.home, &rel, &m.raw)?;
            Some(rel)
        };
        let from = p.from.clone().unwrap_or(Addr {
            addr: String::new(),
            name: String::new(),
        });
        let out = mb.is_mine(&from.addr);
        let store = self.store;
        let res = store.transaction(|tx| -> MailResult<String> {
            // identification (§3.5 step 4)
            let (from_acc, from_party, to, cc) = if out {
                let (fa, fp) = lookup(tx, &mb.ws, &from.addr)?;
                let mut to = Vec::new();
                for a in &p.to {
                    let (acc, party) = self.ident(tx, a, None, date)?;
                    to.push(addr_json(a, acc.as_deref(), party.as_deref()));
                }
                let mut cc = Vec::new();
                for a in &p.cc {
                    let (acc, party) = self.ident(tx, a, None, date)?;
                    cc.push(addr_json(a, acc.as_deref(), party.as_deref()));
                }
                (fa, fp, to, cc)
            } else {
                let (fa, fp) = if from.addr.is_empty() {
                    (None, None)
                } else {
                    self.ident(tx, &from, p.signature_org.as_deref(), date)?
                };
                // recipients of received mail are only looked up: a mass mailing must not create
                // dozens of contacts to confirm
                let mut to = Vec::new();
                for a in &p.to {
                    let (acc, party) = lookup(tx, &mb.ws, &a.addr)?;
                    to.push(addr_json(a, acc.as_deref(), party.as_deref()));
                }
                let mut cc = Vec::new();
                for a in &p.cc {
                    let (acc, party) = lookup(tx, &mb.ws, &a.addr)?;
                    cc.push(addr_json(a, acc.as_deref(), party.as_deref()));
                }
                (fa, fp, to, cc)
            };
            let thread = self.find_thread(tx, &m, &p, date)?;
            let attachments = if m.partial {
                m.parts.clone()
            } else {
                p.attachments.clone()
            };
            let row = MessageRow {
                id: id.clone(),
                workspace_id: mb.ws.clone(),
                connection_id: mb.connection_id.clone(),
                thread_id: thread.id.clone(),
                ext_id: m.ext_id.clone(),
                message_id: p.message_id.clone(),
                in_reply_to: p.in_reply_to.clone(),
                refs: p.refs.clone(),
                folders: vec![m.folder.clone()],
                flags: m.flags.clone(),
                direction: if out { "out" } else { "in" }.into(),
                from_addr: from.addr.clone(),
                from_name: from.name.clone(),
                from_account_id: from_acc.clone(),
                from_party_id: from_party.clone(),
                reply_to: p.reply_to.clone(),
                to_addrs: Value::Array(to.clone()),
                cc_addrs: Value::Array(cc.clone()),
                subject: p.subject.clone(),
                snippet: p.snippet.clone(),
                date,
                size: m.size.max(m.raw.len() as i64),
                has_attachments: attachments.iter().any(|a| !a.inline),
                bulk: p.bulk,
                body_text: p.text.clone(),
                body_new: p.body_new.clone(),
                has_html: p.has_html,
                raw_path: raw_path.clone(),
                purged: None,
                event_id: None,
                status: "received".into(),
                created: self.now,
            };
            repo::insert_message(tx, &row)?;
            for a in &attachments {
                repo::insert_attachment(tx, &mb.ws, &id, a, self.now)?;
            }
            repo::refresh_thread(tx, &thread.id, self.now)?;
            // the thread's state follows who wrote last (§4)
            let latest = date >= thread.last_at;
            match (out, thread.state.as_str()) {
                (true, "open" | "needs_reply") if latest => {
                    repo::set_thread_state(tx, &thread.id, "waiting", self.now)?
                }
                (false, "waiting") if latest => {
                    repo::set_thread_state(tx, &thread.id, "open", self.now)?
                }
                _ => {}
            }
            if out || m.mode != ScopeMode::Receive {
                return Ok(thread.id);
            }
            let how = if m.backfill {
                Delivery::backfill()
            } else if thread.state == "muted" {
                Delivery::muted()
            } else {
                Delivery::Ingest
            };
            let ev = self.event(
                tx,
                &row,
                &p,
                &thread,
                &from,
                &to,
                &cc,
                attachments.iter().any(|a| !a.inline),
            )?;
            let pending = PendingEvent {
                message_id: id.clone(),
                connection_id: mb.connection_id.clone(),
                event: ev,
                how,
            };
            let payload =
                serde_json::to_value(&pending).map_err(|e| MailError::Other(e.to_string()))?;
            outbox::emit(tx, &mb.ws, EVENT_TOPIC, EVENT_V, &payload)?;
            Ok(thread.id)
        });
        match res {
            Ok(thread) => {
                self.stored += 1;
                self.changed.insert(thread);
                Ok(())
            }
            Err(e) => {
                if let Some(rel) = &raw_path
                    && let Some(path) = raw_abs_path(self.home, rel)
                {
                    let _ = fs::remove_file(path);
                }
                Err(e)
            }
        }
    }

    /// The event of a received message (§3.5 step 6).
    #[allow(clippy::too_many_arguments)]
    fn event(
        &self,
        c: &Connection,
        row: &MessageRow,
        p: &Parsed,
        thread: &ThreadRow,
        from: &Addr,
        to: &[Value],
        cc: &[Value],
        attachment: bool,
    ) -> MailResult<MailEvent> {
        let mb = self.mailbox;
        let party = match &row.from_party_id {
            Some(id) => current_party(c, id)?,
            None => None,
        };
        let vip = party.as_ref().is_some_and(|p| p.important);
        let reply_to_mine = match &p.in_reply_to {
            Some(irt) => repo::is_mine(c, &mb.connection_id, irt)?,
            None => false,
        };
        let (type_key, also) = classify(reply_to_mine, vip, attachment);
        // the sender's organization: by the domain, else the signature
        let domain = from
            .addr
            .rsplit_once('@')
            .map(|(_, d)| d.to_lowercase())
            .unwrap_or_default();
        let org = if domain.is_empty() {
            None
        } else {
            people::domain_owner(c, &mb.ws, &domain)?
                .and_then(|o| people::get(c, &o).ok().flatten())
        };
        let org_name = org
            .as_ref()
            .map(|o| o.name.clone())
            .or_else(|| p.signature_org.clone());
        let group_key = match &org {
            Some(o) => people::domains(c, &mb.ws, &o.id)?
                .into_iter()
                .next()
                .unwrap_or_else(|| domain.clone()),
            None => from.addr.to_lowercase(),
        };
        let name = if from.name.is_empty() {
            from.addr.clone()
        } else {
            from.name.clone()
        };
        let subject = if p.subject.is_empty() {
            "（无主题）".to_string()
        } else {
            p.subject.clone()
        };
        let summary = match &org_name {
            Some(o) => format!("{name}（{o}）：{subject}"),
            None => format!("{name}：{subject}"),
        };
        let text = parse::cut_chars(p.body_new.as_deref().unwrap_or(""), EVENT_TEXT_CHARS);
        Ok(MailEvent {
            type_key,
            also,
            external_id: row.id.clone(),
            occurred: row.date,
            thread_key: Some(thread.id.clone()),
            group_key: Some(group_key),
            scope_ext_id: row.folders.first().cloned(),
            actor: Some(Actor {
                family: "email".into(),
                handle: from.addr.to_lowercase(),
                display_name: (!from.name.is_empty()).then(|| from.name.clone()),
                signature_org: p.signature_org.clone(),
                account_id: row.from_account_id.clone(),
                party_id: row.from_party_id.clone(),
            }),
            summary: parse::cut_chars(&summary, 200),
            payload: json!({
                "mail_message_id": row.id,
                "from": {"addr": from.addr, "name": from.name, "party_id": row.from_party_id},
                "to": to,
                "cc": cc,
                "subject": p.subject,
                "date": row.date,
                "bulk": p.bulk,
                "has_attachments": attachment,
                "text": text,
            }),
        })
    }
}

impl MailSink for StoreSink<'_> {
    fn put(&mut self, m: FetchedMessage) -> MailResult<()> {
        let Some(p) = parse::parse(&m.raw, m.partial) else {
            tracing::warn!(ext = %m.ext_id, "mail did not parse; skipped");
            return Ok(());
        };
        let existing = self.store.reader(|c| self.existing(c, &m, &p))?;
        if let Some(r) = existing {
            let store = self.store;
            return store.transaction(|tx| self.merge_copy(tx, &r, &m));
        }
        self.store_new(m, p)
    }

    fn flags(&mut self, ext_id: &str, flags: &[String]) -> MailResult<()> {
        let conn = self.mailbox.connection_id.clone();
        let Some(r) = self
            .store
            .reader(|c| repo::message_by_ext(c, &conn, ext_id))?
        else {
            return Ok(());
        };
        if r.flags == flags {
            return Ok(());
        }
        let now = self.now;
        self.store.transaction(|tx| -> MailResult<()> {
            repo::set_flags(tx, &r.id, flags)?;
            repo::refresh_thread(tx, &r.thread_id, now)
        })?;
        self.changed.insert(r.thread_id);
        Ok(())
    }

    fn removed(&mut self, ext_id: &str) -> MailResult<()> {
        let conn = self.mailbox.connection_id.clone();
        let Some(r) = self
            .store
            .reader(|c| repo::message_by_ext(c, &conn, ext_id))?
        else {
            return Ok(());
        };
        // the local copy stays (it is a cache of the mailbox and may be linked to items); only the
        // folder it was in is dropped
        let folder = ext_id.rsplitn(3, ':').nth(2).unwrap_or("");
        let folders: Vec<String> = r.folders.iter().filter(|f| *f != folder).cloned().collect();
        self.store
            .transaction(|tx| repo::set_folders(tx, &r.id, &folders))?;
        self.changed.insert(r.thread_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_paths_stay_under_mail() {
        assert_eq!(
            raw_rel_path("default", "ml_abcdef"),
            "data/mail/default/bc/ml_abcdef.eml"
        );
        let home = Path::new("/h");
        assert!(raw_abs_path(home, "data/mail/w/ab/ml_x.eml").is_some());
        assert!(raw_abs_path(home, "data/mail/../../etc/passwd").is_none());
        assert!(raw_abs_path(home, "/etc/passwd").is_none());
    }
}
