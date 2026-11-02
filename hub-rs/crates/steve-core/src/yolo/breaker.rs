//! The breakers the executor trips (modules/yolo.md §3.2, without the budget breaker B4 — the user
//! dropped budgets): B2 many failure cards at once, B3 the user undoing YOLO, B5 the executor itself
//! failing. B1 (releases) comes with the release slice. A trip pauses the whole workspace
//! ([`Yolo::trip`]): scheduled actions are cancelled and a failure notice goes out.

use rusqlite::params;
use steve_modules::ApiResult;
use steve_store::yolo as repo;

use super::Yolo;

/// B2: this many `fail` cards…
pub const B2_CARDS: i64 = 5;
/// …on at least this many items…
pub const B2_ITEMS: i64 = 3;
/// …within this window.
pub const B2_WINDOW_MS: i64 = 30 * 60_000;
/// B3: this many undone YOLO decisions within [`B3_WINDOW_MS`].
pub const B3_UNDOS: i64 = 2;
pub const B3_WINDOW_MS: i64 = 60 * 60_000;

impl Yolo {
    /// B2: something systemic (the model route is down…) makes items fail together. Checked when a
    /// `fail` card comes up; true when it tripped (or the workspace was already paused).
    pub fn check_systemic(&self, ws: &str) -> ApiResult<bool> {
        let since = self.now() - B2_WINDOW_MS;
        let (cards, items): (i64, i64) = self.store().reader(|c| {
            c.prepare_cached(
                "SELECT count(*), count(DISTINCT item_id) FROM decisions
                  WHERE workspace_id = ? AND kind = 'fail' AND item_id IS NOT NULL AND created >= ?",
            )?
            .query_row(params![ws, since], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(steve_modules::ApiError::from)
        })?;
        if cards >= B2_CARDS && items >= B2_ITEMS {
            self.trip(
                ws,
                "B2",
                &format!("30 分钟内 {items} 件事项出了 {cards} 张失败卡，可能是系统性问题"),
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// B3: the user took back one of YOLO's decisions. Stamps the action and trips when it is the
    /// second within an hour; true when it tripped.
    pub fn undone(&self, ws: &str, action: &str) -> ApiResult<bool> {
        let now = self.now();
        let n = self.store().transaction(|tx| {
            if !repo::action_undone(tx, action, now)? {
                return Ok::<_, steve_modules::ApiError>(0);
            }
            Ok(repo::undone_since(tx, ws, now - B3_WINDOW_MS)?)
        })?;
        if n >= B3_UNDOS {
            self.trip(ws, "B3", &format!("1 小时内你撤销了 {n} 次 YOLO 的决定"))?;
            return Ok(true);
        }
        Ok(false)
    }

    /// B5: the guard or the executor failed (anything but a parse failure), or what was judged
    /// differs from the check right before acting.
    pub fn executor_failed(&self, ws: &str, what: &str) -> ApiResult<bool> {
        tracing::error!("yolo executor in {ws}: {what}");
        self.trip(ws, "B5", &format!("YOLO 执行出错：{what}"))
    }
}
