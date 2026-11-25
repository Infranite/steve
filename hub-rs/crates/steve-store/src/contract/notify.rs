//! Workspace notifications: things that happened (a release went out, items were archived,
//! a system/security alert fired) rather than conversation.
//!
//! Backed by typed `thread_notices` rows ([`crate::thread::post_notice`] /
//! [`crate::thread::list_notices`]). A failure without an item (`level == "fail" && item.is_none()`)
//! also leaves one line in the main conversation ([`MAIN_SPACE`]) pointing back at the notice via
//! `meta.notice.ref`. An item's failure is already represented by its own failure card, so it does
//! not post a second line in the main conversation.

use serde::Serialize;

use crate::{Result, Store, W};

/// Message space for the workspace's main conversation.
pub const MAIN_SPACE: &str = "steve";

/// One workspace notification (`GET /api/notifications` entry shape, minus hub-side `closed`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub id: String,
    /// release | archive | system | disk | memory | channel | security
    pub kind: String,
    /// info | success | warn | fail
    pub level: String,
    pub text: String,
    pub item_id: Option<String>,
    pub created: i64,
}

/// Record a workspace notification in its own transaction. Returns the notification's id.
pub fn notify(
    store: &Store,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> Result<String> {
    store.write(|tx| notify_in(tx, ws, kind, level, text, item))
}

/// Record a workspace notification inside the caller's transaction: what happened and its
/// notification commit or roll back together.
pub fn notify_in(
    tx: &mut W<'_>,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> Result<String> {
    let post_host_line = level == "fail" && item.is_none();
    let row = crate::thread::post_notice(
        tx,
        ws,
        kind,
        level,
        text,
        item,
        post_host_line,
        crate::now(),
    )?;
    Ok(row.id)
}

/// Recent workspace notifications, newest first (ties: later insert first), at most `limit`.
pub fn list_recent(store: &Store, ws: &str, limit: i64) -> Result<Vec<Notice>> {
    let rows = store.read(|r| crate::thread::list_notices(r, ws, limit.max(0)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoPublisher, WS};
    use serde_json::json;

    #[test]
    fn only_itemless_failures_mirror_into_main_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.db"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();

        let info_id = notify(&store, WS, "memory", "info", "remembered", None).unwrap();
        let item_fail_id = notify(
            &store,
            WS,
            "release",
            "fail",
            "release failed",
            Some("ST-1"),
        )
        .unwrap();
        let sys_fail_id = notify(&store, WS, "system", "fail", "system down", None).unwrap();

        let notices = list_recent(&store, WS, 10).unwrap();
        assert_eq!(notices.len(), 3);
        let ids: Vec<&str> = notices.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&info_id.as_str()));
        assert!(ids.contains(&item_fail_id.as_str()));
        assert!(ids.contains(&sys_fail_id.as_str()));

        let main = store.messages().list(WS, MAIN_SPACE, 10).unwrap();
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].text, "system down");
        assert_eq!(main[0].meta["notice"]["ref"], json!(sys_fail_id));
    }
}
