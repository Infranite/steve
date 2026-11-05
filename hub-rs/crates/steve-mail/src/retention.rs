//! Retention (email.md §3.6): every hour, mail older than the workspace's `keepMail` days loses its
//! raw file and its text (`body_text`, `body_new`), `purged = now`; snippet, headers and attachment
//! metadata stay — the original is still in the mailbox and can be fetched again. Bodies of drafts
//! sent more than 30 days ago are emptied (the sent copy in `mail_messages` is the record).

use serde_json::Value;

use crate::Mail;
use crate::error::MailResult;
use crate::repo;
use crate::store::raw_abs_path;

/// `keepMail` values (days) and the default.
pub const KEEP_CHOICES: [i64; 3] = [30, 90, 365];
pub const KEEP_DEFAULT: i64 = 90;
const SENT_DRAFT_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;
const BATCH: i64 = 500;

/// The `keepMail` setting of a workspace (stored as `"90"` or `90`).
pub fn keep_days(store: &steve_store::Store, ws: &str) -> i64 {
    let v: Value = store
        .settings()
        .get(ws, "keepMail", Value::Null)
        .unwrap_or(Value::Null);
    let n = match &v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    };
    n.filter(|n| KEEP_CHOICES.contains(n))
        .unwrap_or(KEEP_DEFAULT)
}

impl Mail {
    /// Runs retention once (blocking: call it off the runtime). Returns how many messages it purged.
    pub fn retention(&self) -> MailResult<usize> {
        self.retention_paged(BATCH)
    }

    /// [`Mail::retention`] with a given page size (tests use a small one).
    #[doc(hidden)]
    pub fn retention_paged(&self, batch: i64) -> MailResult<usize> {
        let store = &self.0.m.store;
        let now = self.now();
        let mut purged = 0;
        for ws in store.reader(repo::mail_workspaces)? {
            let before = now - keep_days(store, &ws) * DAY_MS;
            // keyset paging: rows whose file could not be deleted stay behind the cursor
            let mut after: Option<(i64, String)> = None;
            loop {
                let rows = store.reader(|c| {
                    repo::expired(
                        c,
                        &ws,
                        before,
                        after.as_ref().map(|(d, i)| (*d, i.as_str())),
                        batch,
                    )
                })?;
                let Some((last_id, _, last_date)) = rows.last() else {
                    break;
                };
                after = Some((*last_date, last_id.clone()));
                let mut done = Vec::with_capacity(rows.len());
                for (id, raw, _) in &rows {
                    if let Some(path) = raw
                        .as_deref()
                        .and_then(|r| raw_abs_path(self.0.cfg.home(), r))
                    {
                        match std::fs::remove_file(&path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            // keep the row as it is; the next run tries again
                            Err(e) => {
                                tracing::warn!(message = %id, error = %e, "could not delete a mail file");
                                continue;
                            }
                        }
                    }
                    done.push(id.as_str());
                }
                // one write transaction per page
                if !done.is_empty() {
                    store.transaction(|tx| {
                        for id in &done {
                            repo::purge(tx, id, now)?;
                        }
                        Ok::<_, crate::MailError>(())
                    })?;
                    purged += done.len();
                }
                if (rows.len() as i64) < batch {
                    break;
                }
            }
        }
        store.transaction(|tx| {
            tx.prepare_cached(
                "UPDATE mail_drafts SET body = '' WHERE status = 'sent' AND sent IS NOT NULL AND sent < ? AND body != ''",
            )?
            .execute([now - SENT_DRAFT_DAYS * DAY_MS])?;
            Ok::<_, crate::MailError>(())
        })?;
        Ok(purged)
    }
}
