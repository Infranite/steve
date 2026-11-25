//! Notifications: typed `thread_notices` rows (migration 125). A failure without an item also
//! leaves a host line in the main conversation (`space = 'steve'`, `meta.notice.ref = id`) in the
//! same transaction.

use crate::Core;
use crate::error::Result;
use crate::repo::Db;
use serde::Serialize;

/// One notification (`GET /api/notifications` entries).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub id: String,
    /// release | archive | system
    pub kind: String,
    /// info | success | fail
    pub level: String,
    pub text: String,
    pub item_id: Option<String>,
    pub created: i64,
}

/// Record a notification; a failure without an item also goes into the conversation.
/// Returns the notification's id.
pub fn notify(
    db: &Db,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> Result<String> {
    Ok(db
        .store()
        .write(|tx| notify_in(tx, ws, kind, level, text, item))?)
}

/// [`notify`] inside the caller's transaction: what happened and its notification are written
/// together, or neither is.
pub fn notify_in(
    tx: &mut steve_store::W<'_>,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> steve_store::Result<String> {
    let post_host_line = level == "fail" && item.is_none();
    let row = steve_store::thread::post_notice(
        tx,
        ws,
        kind,
        level,
        text,
        item,
        post_host_line,
        steve_store::now(),
    )?;
    Ok(row.id)
}

impl Core {
    /// Notifications of a workspace, newest first (<=100).
    pub fn notifications(&self, ws: &str) -> Result<Vec<Notice>> {
        let rows = self
            .store()
            .store()
            .read(|r| steve_store::thread::list_notices(r, ws, 100))?;
        Ok(rows
            .into_iter()
            .map(|n| Notice {
                id: n.id,
                kind: n.kind,
                level: n.level,
                text: n.text,
                item_id: n.item_id,
                created: n.created,
            })
            .collect())
    }
}
