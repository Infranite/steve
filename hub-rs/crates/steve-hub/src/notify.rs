//! Notifications (notify.ts): things that happened (a release went out, items were archived) rather than
//! conversation. They are `messages` rows in space `notify`; a failure also leaves one line in the
//! conversation pointing back at it. Release results from before this were plain Steve messages in the
//! conversation: those are recognised by their text, listed here too, and the successful ones are left out
//! of the conversation.
//! A failure stops asking for attention once it is closed: the user dismissed it, or a later release notice
//! of the same item (it went out, or failed again) took its place. Its line then leaves the conversation.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::backend::{BResult, Obj, Repo};
use crate::bus::{Bus, HubEvent};

const SPACE: &str = "notify";
const SEEN: &str = "notifySeen";
const DISMISSED: &str = "noticeDismissed";
/// notify rows looked at when deciding what is closed, and dismissed ids kept
const WINDOW: i64 = 500;
static LEGACY_OK: LazyLock<Regex> = LazyLock::new(|| Regex::new("^已发布新版本 ").expect("regex"));
static LEGACY_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^发布 .*失败，已回到 ").expect("regex"));

fn text(m: &Obj) -> &str {
    m.get("text").and_then(Value::as_str).unwrap_or_default()
}

fn legacy(m: &Obj) -> bool {
    m.get("author").and_then(Value::as_str) == Some("steve")
        && !crate::js::truthy(m.get("meta"))
        && (LEGACY_OK.is_match(text(m)) || LEGACY_FAIL.is_match(text(m)))
}

fn str_of<'a>(m: &'a Obj, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}
fn notice_of<'a>(m: &'a Obj, k: &str) -> Option<&'a str> {
    m.get("meta")
        .and_then(|x| x.get("notice"))
        .and_then(|n| n.get(k))
        .and_then(Value::as_str)
}

/// Ids the user closed by hand, oldest first.
fn dismissed(r: &dyn Repo, ws: &str) -> BResult<Vec<String>> {
    Ok(r.setting_raw(ws, DISMISSED)?
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .unwrap_or_default())
}

/// Ids of the notify rows that no longer ask for attention: dismissed, or a failure followed by a later
/// release notice of the same item.
fn closed(r: &dyn Repo, ws: &str, rows: &[Obj]) -> BResult<HashSet<String>> {
    let mut out: HashSet<String> = dismissed(r, ws)?.into_iter().collect();
    let created = |m: &Obj| m.get("created").and_then(Value::as_f64).unwrap_or(0.0);
    /* ascending by time; among equal times insert order (rows come ascending) */
    let mut rows: Vec<&Obj> = rows.iter().collect();
    rows.sort_by(|a, b| {
        created(a)
            .partial_cmp(&created(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (n, m) in rows.iter().enumerate() {
        let (Some(id), Some(item)) = (str_of(m, "id"), str_of(m, "item_id")) else {
            continue;
        };
        if notice_of(m, "level") != Some("fail") || notice_of(m, "kind") != Some("release") {
            continue;
        }
        if rows[n + 1..]
            .iter()
            .any(|x| str_of(x, "item_id") == Some(item) && notice_of(x, "kind") == Some("release"))
        {
            out.insert(id.into());
        }
    }
    Ok(out)
}

/// The user closes a notice (a notify row id, or its line in the conversation): it stays in the list,
/// unpinned, and its line leaves the conversation. Publishes `settings`.
pub fn dismiss(r: &dyn Repo, bus: &Bus, ws: &str, id: &str) -> BResult<()> {
    let id = r
        .messages_in(ws, "steve", WINDOW)?
        .iter()
        .find(|m| str_of(m, "id") == Some(id))
        .and_then(|m| notice_of(m, "ref").map(String::from))
        .unwrap_or_else(|| id.to_string());
    let mut ids = dismissed(r, ws)?;
    if !ids.contains(&id) {
        ids.push(id);
    }
    let skip = ids.len().saturating_sub(WINDOW as usize);
    r.setting_put_raw(
        ws,
        DISMISSED,
        &serde_json::to_string(&ids[skip..]).unwrap_or_default(),
    )?;
    bus.publish(HubEvent::Settings { id: ws.into() });
    Ok(())
}

/// `notify(ws, kind, level, text, item)`; returns the notify row id.
pub fn notify(
    r: &dyn Repo,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> BResult<String> {
    let id = r.message_add(
        ws,
        "steve",
        text,
        item,
        &json!({ "notice": { "kind": kind, "level": level } }),
        SPACE,
    )?;
    if level == "fail" {
        r.message_add(
            ws,
            "steve",
            text,
            item,
            &json!({ "notice": { "kind": kind, "level": level, "ref": id } }),
            "steve",
        )?;
    }
    Ok(id)
}

/// The latest notices, newest first (ties: later insert first), at most `limit`.
pub fn list(r: &dyn Repo, ws: &str, limit: i64) -> BResult<Vec<Value>> {
    let all = r.messages_in(ws, SPACE, WINDOW)?;
    let shut = closed(r, ws, &all)?;
    let skip = all.len().saturating_sub(limit.max(0) as usize);
    let mut rows = all[skip..].to_vec();
    rows.extend(r.messages_in(ws, "steve", 500)?.into_iter().filter(legacy));
    rows.reverse();
    let created = |m: &Obj| m.get("created").and_then(Value::as_f64).unwrap_or(0.0);
    rows.sort_by(|a, b| {
        created(b)
            .partial_cmp(&created(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(rows
        .into_iter()
        .take(limit.max(0) as usize)
        .map(|m| {
            let notice = m.get("meta").and_then(|x| x.get("notice"));
            let kind = notice.and_then(|n| n.get("kind")).filter(|x| !x.is_null()).cloned().unwrap_or_else(|| json!("release"));
            let level = notice
                .and_then(|n| n.get("level"))
                .filter(|x| !x.is_null())
                .cloned()
                .unwrap_or_else(|| json!(if LEGACY_FAIL.is_match(text(&m)) { "fail" } else { "success" }));
            let closed = str_of(&m, "id").is_some_and(|id| shut.contains(id));
            json!({ "id": m.get("id"), "kind": kind, "level": level, "text": m.get("text"), "item_id": m.get("item_id"), "created": m.get("created"), "closed": closed })
        })
        .collect())
}

/// The conversation without the notices that used to be written into it (successful releases) and without
/// the lines of closed failures.
pub fn thread(r: &dyn Repo, ws: &str) -> BResult<Vec<Obj>> {
    let shut = closed(r, ws, &r.messages_in(ws, SPACE, WINDOW)?)?;
    Ok(r.messages_list(ws)?
        .into_iter()
        .filter(|m| !(legacy(m) && LEGACY_OK.is_match(text(m))))
        .filter(|m| !notice_of(m, "ref").is_some_and(|id| shut.contains(id)))
        /* flow-back folded into an item's card, notices that belong to the notifications */
        .filter(|m| {
            !m.get("meta")
                .and_then(|x| x.get("hidden"))
                .is_some_and(|h| h.as_bool() == Some(true))
        })
        .collect())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The read cursor; the first look after the upgrade counts everything so far as seen (writes it, no event).
pub fn seen(r: &dyn Repo, ws: &str) -> BResult<Value> {
    if let Some(v) = r.setting_raw(ws, SEEN)? {
        // Number(r.value)
        let n = crate::config::js_number(&v);
        return Ok(crate::js::num_value(n));
    }
    let at = now_ms();
    r.setting_put_raw(ws, SEEN, &at.to_string())?;
    Ok(json!(at))
}

/// Everything up to `at` has been seen; publishes `settings` (no revision row).
pub fn mark_seen(r: &dyn Repo, bus: &Bus, ws: &str, at: i64) -> BResult<i64> {
    r.setting_put_raw(ws, SEEN, &at.to_string())?;
    bus.publish(HubEvent::Settings { id: ws.into() });
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// messages and raw settings in memory; ids `m1, m2…` in insert order, `created` = insert order unless given
    #[derive(Default)]
    struct Fake {
        rows: RefCell<Vec<(String, Obj)>>,
        settings: RefCell<HashMap<String, String>>,
    }
    impl Fake {
        fn put(&self, space: &str, text: &str, item: Option<&str>, meta: Value) -> String {
            let mut rows = self.rows.borrow_mut();
            let id = format!("m{}", rows.len() + 1);
            let m = json!({ "id": id, "author": "steve", "text": text, "item_id": item, "meta": meta, "created": rows.len() as i64 + 1 });
            rows.push((space.into(), m.as_object().cloned().unwrap_or_default()));
            id
        }
    }
    impl Repo for Fake {
        fn messages_in(&self, _ws: &str, space: &str, limit: i64) -> BResult<Vec<Obj>> {
            let all: Vec<Obj> = self
                .rows
                .borrow()
                .iter()
                .filter(|(s, _)| s == space)
                .map(|(_, m)| m.clone())
                .collect();
            Ok(all[all.len().saturating_sub(limit as usize)..].to_vec())
        }
        fn message_add(
            &self,
            _ws: &str,
            _a: &str,
            text: &str,
            item: Option<&str>,
            meta: &Value,
            space: &str,
        ) -> BResult<String> {
            Ok(self.put(space, text, item, meta.clone()))
        }
        fn setting_raw(&self, _ws: &str, key: &str) -> BResult<Option<String>> {
            Ok(self.settings.borrow().get(key).cloned())
        }
        fn setting_put_raw(&self, _ws: &str, key: &str, value: &str) -> BResult<()> {
            self.settings.borrow_mut().insert(key.into(), value.into());
            Ok(())
        }
    }

    fn lines(r: &Fake) -> Vec<String> {
        thread(r, "w")
            .expect("thread")
            .iter()
            .map(|m| text(m).to_string())
            .collect()
    }
    fn closed_of(r: &Fake) -> Vec<(String, bool)> {
        list(r, "w", 100)
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
        let r = Fake::default();
        notify(&r, "w", "release", "fail", "没有发布：冲突", Some("ST-16")).expect("notify");
        notify(&r, "w", "release", "fail", "没有发布：别的", Some("ST-9")).expect("notify");
        assert_eq!(lines(&r), ["没有发布：冲突", "没有发布：别的"]);
        notify(&r, "w", "release", "success", "已发布", Some("ST-16")).expect("notify");
        assert_eq!(lines(&r), ["没有发布：别的"]);
        assert_eq!(
            closed_of(&r),
            [
                ("已发布".into(), false),
                ("没有发布：别的".into(), false),
                ("没有发布：冲突".into(), true)
            ]
        );
    }

    #[test]
    fn a_new_failure_replaces_the_older_one() {
        let r = Fake::default();
        notify(&r, "w", "release", "fail", "第一次", Some("ST-1")).expect("notify");
        notify(&r, "w", "release", "fail", "第二次", Some("ST-1")).expect("notify");
        assert_eq!(lines(&r), ["第二次"]);
    }

    #[test]
    fn a_dismissed_failure_stays_closed() {
        let r = Fake::default();
        let bus = Bus::new();
        notify(&r, "w", "release", "fail", "没有发布", Some("ST-1")).expect("notify");
        /* closed from its line in the conversation: the id is that of the line, the notice is what gets remembered */
        let line = r.messages_in("w", "steve", 10).expect("rows")[0]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        dismiss(&r, &bus, "w", &line).expect("dismiss");
        dismiss(&r, &bus, "w", &line).expect("dismiss twice");
        assert!(lines(&r).is_empty());
        assert_eq!(closed_of(&r), [("没有发布".into(), true)]);
        assert_eq!(
            r.settings.borrow().get(DISMISSED).map(String::as_str),
            Some(r#"["m1"]"#)
        );
    }
}
