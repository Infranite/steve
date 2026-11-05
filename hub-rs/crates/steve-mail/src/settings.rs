//! Per-mailbox settings (`mail_account_settings`): signature, duty, send delay, archive
//! on done, look-back days, append to “已发送”. Created with defaults on the first sync; changes are
//! validated like `PATCH /mail/accounts/:connection_id` and written with a revision.

use serde::Deserialize;
use serde_json::json;
use steve_modules::{ApiError, Caller};
use steve_store::base;

use crate::Mail;
use crate::error::{MailError, MailResult};
use crate::repo::{self, AccountSettings};

pub use crate::repo::AccountSettings as Settings;

/// The fields `PATCH /mail/accounts/:connection_id` may change.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub signature: Option<String>,
    pub duty: Option<String>,
    pub send_delay_s: Option<i64>,
    pub archive_on_done: Option<bool>,
    pub sync_days: Option<i64>,
    /// `auto` | `yes` | `no`.
    pub append_sent: Option<String>,
}

impl SettingsPatch {
    fn apply(&self, s: &mut AccountSettings) -> Result<(), ApiError> {
        let mut bad = Vec::new();
        if let Some(v) = &self.signature {
            if v.chars().count() > 1000 {
                bad.push("signature".to_string());
            }
            s.signature = v.clone();
        }
        if let Some(v) = &self.duty {
            if v.chars().count() > 4000 {
                bad.push("duty".to_string());
            }
            s.duty = v.clone();
        }
        if let Some(v) = self.send_delay_s {
            if !(0..=60).contains(&v) {
                bad.push("send_delay_s".to_string());
            }
            s.send_delay_s = v;
        }
        if let Some(v) = self.archive_on_done {
            s.archive_on_done = v;
        }
        if let Some(v) = self.sync_days {
            if ![7, 30, 90].contains(&v) {
                bad.push("sync_days".to_string());
            }
            s.sync_days = v;
        }
        if let Some(v) = &self.append_sent {
            if !["auto", "yes", "no"].contains(&v.as_str()) {
                bad.push("append_sent".to_string());
            }
            s.append_sent = v.clone();
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Invalid(bad))
        }
    }
}

impl Mail {
    /// The settings of a mailbox the caller may see (`404 没有这个连接` otherwise); defaults when the
    /// row does not exist yet.
    pub fn settings(&self, who: &Caller, connection_id: &str) -> MailResult<AccountSettings> {
        self.0
            .conns
            .get(who, connection_id)
            .map_err(MailError::from)?;
        Ok(self
            .0
            .m
            .store
            .reader(|c| repo::settings(c, connection_id))?
            .unwrap_or_default())
    }

    /// Validates and writes a patch (revision `mail_account`, audit `mail_account.patch`).
    pub fn patch_settings(
        &self,
        who: &Caller,
        connection_id: &str,
        p: &SettingsPatch,
    ) -> MailResult<AccountSettings> {
        self.0
            .conns
            .get(who, connection_id)
            .map_err(MailError::from)?;
        let now = self.now();
        self.0
            .m
            .store
            .transaction(|tx| patch_tx(tx, &who.ws, &who.me, connection_id, p, now))
    }
}

/// The fields a revision of `mail_account` records.
pub fn fields(s: &AccountSettings) -> serde_json::Value {
    json!({"signature": s.signature, "duty": s.duty, "send_delay_s": s.send_delay_s,
           "archive_on_done": s.archive_on_done, "sync_days": s.sync_days, "append_sent": s.append_sent})
}

/// [`Mail::patch_settings`] inside a transaction, without the ownership check (the caller did it):
/// validates, writes, records the revision and the audit `mail_account.patch`. Unchanged → no write.
pub fn patch_tx(
    tx: &steve_store::Tx,
    ws: &str,
    actor: &str,
    connection_id: &str,
    p: &SettingsPatch,
    now: i64,
) -> MailResult<AccountSettings> {
    repo::ensure_settings(tx, ws, connection_id, now)?;
    let before = repo::settings(tx, connection_id)?.unwrap_or_default();
    let mut after = before.clone();
    p.apply(&mut after)?;
    if after == before {
        return Ok(before);
    }
    write_tx(tx, ws, actor, connection_id, &before, &after, now)?;
    Ok(repo::settings(tx, connection_id)?.unwrap_or(after))
}

/// Writes `after` over `before` (revision + 1, revision row, audit `mail_account.patch`).
pub(crate) fn write_tx(
    tx: &steve_store::Tx,
    ws: &str,
    actor: &str,
    connection_id: &str,
    before: &AccountSettings,
    after: &AccountSettings,
    now: i64,
) -> MailResult<()> {
    repo::update_settings(tx, connection_id, after, now)?;
    base::revision(
        tx,
        ws,
        "mail_account",
        connection_id,
        before.revision + 1,
        &fields(before),
        &fields(after),
        actor,
    )?;
    base::audit(
        tx,
        ws,
        actor,
        "mail_account.patch",
        connection_id,
        &json!({"fields": changed(before, after)}),
    )?;
    Ok(())
}

fn changed(a: &AccountSettings, b: &AccountSettings) -> Vec<&'static str> {
    let mut out = Vec::new();
    if a.signature != b.signature {
        out.push("signature");
    }
    if a.duty != b.duty {
        out.push("duty");
    }
    if a.send_delay_s != b.send_delay_s {
        out.push("send_delay_s");
    }
    if a.archive_on_done != b.archive_on_done {
        out.push("archive_on_done");
    }
    if a.sync_days != b.sync_days {
        out.push("sync_days");
    }
    if a.append_sent != b.append_sent {
        out.push("append_sent");
    }
    out
}

/// Whether to write sent mail to “已发送” for this setting (`None` = adapter decides).
pub fn append_sent(s: &AccountSettings) -> Option<bool> {
    match s.append_sent.as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
