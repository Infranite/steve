//! The breakers the executor trips (without the budget breaker B4 — the user
//! dropped budgets): B2 sessions failing on many items at once, B3 the user's undos spreading, B5
//! the executor itself failing. B1 (releases) comes with the release slice. A trip pauses the whole workspace
//! ([`Yolo::trip`]): scheduled actions are cancelled and a failure notice goes out.

use rusqlite::{OptionalExtension, params};
use steve_modules::ApiResult;
use steve_store::yolo as repo;

use super::Yolo;

/// B2: sessions failed (an error, or a Hub restart cut them) on this many different items…
pub const B2_ITEMS: i64 = 3;
/// …within this window.
pub const B2_WINDOW_MS: i64 = 30 * 60_000;
/// B3: within [`B3_WINDOW_MS`], undos on this many different items…
pub const B3_ITEMS: i64 = 3;
/// …or one rule undone on this many different items…
pub const B3_SAME_RULE_ITEMS: i64 = 2;
/// …or this many channel messages recalled (the one signal before the next message that can't be).
pub const B3_RECALLS: i64 = 2;
pub const B3_WINDOW_MS: i64 = 60 * 60_000;
/// Why an item left YOLO after an undo.
pub const UNDONE_REASON: &str = "你撤销了 YOLO 在这件事项上的决定";

impl Yolo {
    /// B2: something systemic (the model route is down…) makes sessions fail together. Checked when
    /// a `fail` card comes up; only session errors and restarts count, once per item (a stopped agent
    /// has its own cap per item). True when it tripped (or the workspace was already paused).
    pub fn check_systemic(&self, ws: &str) -> ApiResult<bool> {
        let since = self.now() - B2_WINDOW_MS;
        let items: i64 = self.store().reader(|c| {
            c.prepare_cached(
                "SELECT count(DISTINCT item_id) FROM decisions
                  WHERE workspace_id = ?1 AND kind = 'fail' AND item_id IS NOT NULL AND created >= ?2
                    AND (substr(question, 1, length(?3)) = ?3 OR substr(question, 1, length(?4)) = ?4)",
            )?
            .query_row(
                params![ws, since, super::facts::SESSION_ERROR, super::facts::INTERRUPTED],
                |r| r.get(0),
            )
            .map_err(steve_modules::ApiError::from)
        })?;
        if items >= B2_ITEMS {
            self.trip(
                ws,
                "B2",
                &format!("30 分钟内 {items} 件事项的会话出错，可能是系统性问题"),
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// B3: the user took back one of YOLO's decisions. Stamps the action and takes its item out of
    /// YOLO; the whole workspace pauses only when undos spread (3 items, one rule on 2 items, or 2
    /// recalls within an hour). True when it tripped.
    pub fn undone(&self, ws: &str, action: &str) -> ApiResult<bool> {
        let now = self.now();
        let (n, item) = self.store().transaction(|tx| {
            if !repo::action_undone(tx, action, now)? {
                return Ok::<_, steve_modules::ApiError>((repo::Undone::default(), None));
            }
            // an item already out of YOLO (a pause from the digest says so itself) stays as it is
            let item = repo::action_get(tx, action)?
                .and_then(|a| a.item_id)
                .filter(|i| {
                    !repo::scope_get(tx, ws, repo::ITEM, i)
                        .ok()
                        .flatten()
                        .is_some_and(|s| s.mode == "off")
                });
            Ok((repo::undone_since(tx, ws, now - B3_WINDOW_MS)?, item))
        })?;
        if let Some(item) = item {
            let title: String = self
                .store()
                .reader(|c| {
                    c.prepare_cached("SELECT title FROM items WHERE id = ?")?
                        .query_row([&item], |r| r.get(0))
                        .optional()
                        .map_err(steve_modules::ApiError::from)
                })?
                .unwrap_or_default();
            self.item_off(ws, &item, &title, UNDONE_REASON)?;
        }
        let why = if n.recalls >= B3_RECALLS {
            format!("1 小时内你撤回了 {} 条 YOLO 发出的消息", n.recalls)
        } else if n.same_rule_items >= B3_SAME_RULE_ITEMS {
            format!(
                "1 小时内你在 {} 件事项上撤销了 YOLO 按同一条规则做的决定",
                n.same_rule_items
            )
        } else if n.items >= B3_ITEMS {
            format!("1 小时内你在 {} 件事项上撤销了 YOLO 的决定", n.items)
        } else {
            return Ok(false);
        };
        self.trip(ws, "B3", &why)?;
        Ok(true)
    }

    /// B5: the guard or the executor failed (anything but a parse failure), or what was judged
    /// differs from the check right before acting.
    pub fn executor_failed(&self, ws: &str, what: &str) -> ApiResult<bool> {
        tracing::error!("yolo executor in {ws}: {what}");
        self.trip(ws, "B5", &format!("YOLO 执行出错：{what}"))
    }
}
