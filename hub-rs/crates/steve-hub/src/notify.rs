//! Notifications: typed `thread_notices` rows (no longer `messages(space='notify')` or
//! `settings.noticeDismissed`). A failure also leaves one line in the main conversation pointing
//! back at it (`meta.notice.ref`). Closing a notice or superseding a release failure emits
//! `WorkEvent::Message` for that host line inside the same store transaction.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::bus::Bus;
use crate::error::BResult;
use crate::wire::store::{Obj, StoreBackend};

use steve_store::NOTIFY_SEEN;

/// Maximum notice rows scanned when filtering visible conversation messages.
const WINDOW: i64 = 500;

fn notice_of<'a>(m: &'a Obj, k: &str) -> Option<&'a str> {
    m.get("meta")
        .and_then(|x| x.get("notice"))
        .and_then(|n| n.get(k))
        .and_then(Value::as_str)
}

/// Ids of closed alerts/notices dismissed by hand (for `GET /api/shell`).
pub fn dismissed(r: &StoreBackend, ws: &str) -> BResult<Vec<String>> {
    r.notices_dismissed(ws)
}

/// The user closes a notice (a notice id, its line in the conversation, or an alert/decision id).
pub fn dismiss(r: &StoreBackend, ws: &str, id: &str) -> BResult<()> {
    r.notices_dismiss(ws, id, None)?;
    Ok(())
}

/// `notify(ws, kind, level, text, item)`; returns the notice row id.
pub fn notify(
    r: &StoreBackend,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> BResult<String> {
    r.notices_post(ws, kind, level, text, item, level == "fail")
}
/// A stored notice text the Hub wrote itself in the request's language (`steve_core::notice_line`, and
/// the Hub's own: a device's disk, a device that went offline, background delivery that gave up);
/// `None`: as stored.
pub fn shown(stored: &str) -> Option<String> {
    const HUB: &[&str] = &[
        "hub.node.disk_low",
        "hub.node.handed_over",
        "hub.node.harness_auto_upgraded",
        "hub.node.harness_auto_upgrade_failed",
        "hub.node.harness_upgraded",
        "hub.node.harness_upgrade_failed",
        "hub.notice.outbox_dead",
    ];
    steve_core::notice_line::shown(stored)
        .or_else(|| steve_channels::unsendable::shown(stored))
        .or_else(|| steve_i18n::reword!(stored, HUB))
}

/// `text` of a message or a notice row in the request's language, when it is a notice.
fn show_text(text: Option<&Value>) -> Option<Value> {
    let t = text?.as_str()?;
    Some(Value::String(shown(t)?))
}

/// The latest notices, newest first (ties: later insert first), at most `limit`.
pub fn list(r: &StoreBackend, ws: &str, limit: i64) -> BResult<Vec<Value>> {
    let rows = r.notices_list(ws, limit.max(0))?;
    Ok(rows
        .into_iter()
        .map(|n| {
            let text = shown(&n.text).unwrap_or(n.text);
            json!({
                "id": n.id,
                "kind": n.kind,
                "level": n.level,
                "text": text,
                "item_id": n.item_id,
                "created": n.created,
                "closed": n.closed_at.is_some(),
            })
        })
        .collect())
}

/// What the conversation said about `q` (`GET /api/thread/search`), newest first.
pub fn thread_search(r: &StoreBackend, ws: &str, q: &str, limit: i64) -> BResult<Vec<Obj>> {
    let msgs = r.messages_search(ws, "steve", q, limit)?;
    visible(r, ws, msgs)
}

fn visible(r: &StoreBackend, ws: &str, msgs: Vec<Obj>) -> BResult<Vec<Obj>> {
    let shut: HashSet<String> = r
        .notices_list(ws, WINDOW)?
        .into_iter()
        .filter(|n| n.closed_at.is_some())
        .map(|n| n.id)
        .collect();
    Ok(msgs
        .into_iter()
        .filter(|m| !notice_of(m, "ref").is_some_and(|id| shut.contains(id)))
        /* flow-back folded into an item's card, notices that belong to the notifications */
        .filter(|m| {
            !m.get("meta")
                .and_then(|x| x.get("hidden"))
                .is_some_and(|h| h.as_bool() == Some(true))
        })
        /* a notice's line in the conversation reads like the notice */
        .map(|mut m| {
            if notice_of(&m, "kind").is_some()
                && let Some(t) = show_text(m.get("text"))
            {
                m.insert("text".into(), t);
            }
            /* the first meeting's welcome Steve said in zh-CN, in the reader's language */
            if m.get("meta").and_then(|x| x.get("onboard")) == Some(&Value::Bool(true))
                && let Some(t) = m
                    .get("text")
                    .and_then(Value::as_str)
                    .and_then(steve_core::thread::onboard::welcome_shown)
            {
                m.insert("text".into(), Value::String(t));
            }
            m
        })
        .collect())
}

/// Missing cursors start at zero so existing notifications can be read without a write.
pub fn seen(r: &StoreBackend, ws: &str) -> BResult<i64> {
    Ok(r.setting_raw(ws, NOTIFY_SEEN)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

/// Everything up to `at` has been seen; publishes `settings` (no revision row).
pub fn mark_seen(r: &StoreBackend, bus: &Bus, ws: &str, at: i64) -> BResult<i64> {
    r.setting_put_raw(ws, NOTIFY_SEEN, &at.to_string())?;
    bus.publish(steve_types::DomainEvent::from(
        steve_types::ConfigEvent::Settings {
            ws: ws.into(),
            id: {
                let value: String = ws.into();
                steve_types::WorkspaceId::from(value)
            },
        },
    ));
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StoreBackend) {
        let dir = tempfile::tempdir().unwrap();
        let s =
            steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        s.ensure_workspace().unwrap();
        (dir, StoreBackend(s))
    }

    fn closed_of(r: &StoreBackend) -> Vec<(String, bool)> {
        list(r, "default", 100)
            .expect("list")
            .iter()
            .map(|n| {
                (
                    n["text"].as_str().unwrap_or_default().into(),
                    n["closed"] == json!(true),
                )
            })
            .collect()
    }

    #[test]
    fn a_later_release_of_the_item_closes_the_failure() {
        let (_d, r) = store();
        notify(
            &r,
            "default",
            "release",
            "fail",
            "没有发布：冲突",
            Some("ST-16"),
        )
        .expect("notify");
        notify(
            &r,
            "default",
            "release",
            "fail",
            "没有发布：别的",
            Some("ST-9"),
        )
        .expect("notify");
        notify(&r, "default", "release", "success", "已发布", Some("ST-16")).expect("notify");
        assert_eq!(
            closed_of(&r),
            [
                ("已发布".into(), false),
                ("没有发布：别的".into(), false),
                ("没有发布：冲突".into(), true)
            ]
        );
    }

    /// Notices use the request language for built-in text and preserve other text.
    #[test]
    fn notices_by_language() {
        let (_d, r) = store();
        let disk = steve_i18n::t!("hub.node.disk_low", name = "studio", free = "3 GB");
        notify(&r, "default", "disk", "info", &disk, None).expect("notify");
        // preserve the text of an existing historical notice
        notify(&r, "default", "system", "fail", "自定义系统提醒", None).expect("notify");
        notify(
            &r,
            "default",
            "release",
            "fail",
            "没有发布：冲突",
            Some("ST-1"),
        )
        .expect("notify");
        let en = steve_i18n::with(steve_i18n::EN, || closed_of(&r));
        let texts: Vec<_> = en.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(
            texts,
            [
                "没有发布：冲突",
                "自定义系统提醒",
                "Device “studio” has only 3 GB left on its project disk. Free some space on that machine, or check it on the devices page."
            ]
        );
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || closed_of(&r));
        assert_eq!(zh[1].0, "自定义系统提醒");
    }

    #[test]
    fn a_new_failure_replaces_the_older_one() {
        let (_d, r) = store();
        notify(&r, "default", "release", "fail", "第一次", Some("ST-1")).expect("notify");
        notify(&r, "default", "release", "fail", "第二次", Some("ST-1")).expect("notify");
        assert_eq!(
            closed_of(&r),
            [("第二次".into(), false), ("第一次".into(), true)]
        );
    }

    #[test]
    fn a_dismissed_failure_stays_closed() {
        let (_d, r) = store();
        let nid = notify(&r, "default", "release", "fail", "没有发布", Some("ST-1")).expect("notify");
        /* closed from its line in the conversation: the id is that of the line, the notice is what gets remembered */
        let line = r.messages_in("default", "steve", 10).expect("rows")[0]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        dismiss(&r, "default", &line).expect("dismiss");
        dismiss(&r, "default", &line).expect("dismiss twice");
        assert_eq!(closed_of(&r), [("没有发布".into(), true)]);
        assert_eq!(dismissed(&r, "default").expect("dismissed"), vec![nid]);
    }
}
