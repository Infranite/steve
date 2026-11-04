//! Releases under YOLO (modules/yolo.md §2.2 release rows, §3.1 spacing, §3.2 B1, §5; Y5): how often
//! YOLO may approve a release of the user's own Steve, one release at a time, rebase conflicts handed
//! back, what a failed or refused release card is about, and the breaker for releases YOLO approved
//! that went wrong. The judge decides with these facts; nothing here answers a card.

use serde_json::Value;
use steve_modules::{ApiError, ApiResult};
use steve_store::yolo::{self as repo, ActionFilter};

use super::engine::Engine;
use super::facts;
use super::judge::StallCause;
use crate::release::NO_RESULT;
use crate::repo::{Decision, Item};

/// The rule of a release YOLO approved.
pub const RULE: &str = "release.self";
/// At most this many YOLO releases per hour…
pub const PER_HOUR: usize = 3;
/// …and per window…
pub const PER_WINDOW: usize = 12;
/// …at least this far apart (ms).
pub const MIN_GAP_MS: i64 = 15 * 60_000;
const HOUR_MS: i64 = 60 * 60_000;

/// What the timeline says when a rebase before a release was handed back (release.rs `rebase_for_release`).
pub const REBASE_RETURNED: &str = "rebase 到 main 有冲突";
/// The fail-card texts of a release that never started (release.rs `start_release`).
const BUSY: &str = "另一个发布（";
const NO_SCRIPT: &str = "这个项目的发布脚本不存在";
const NOT_STARTED: &str = "发布脚本没有启动";

/// Whether one more YOLO release fits: `done` = when earlier YOLO releases were approved (ms), `in_window`
/// = how many of them fall in the current window.
pub fn spacing_ok(done: &[i64], in_window: usize, now: i64) -> bool {
    let last_hour = done.iter().filter(|t| now - **t < HOUR_MS).count();
    let last = done.iter().copied().max();
    last_hour < PER_HOUR && in_window < PER_WINDOW && last.is_none_or(|t| now - t >= MIN_GAP_MS)
}

/// The version a failed release went back to, from the note the release report left (`已回到 <v>`).
fn prev_of(note: &str) -> String {
    let n = note.trim();
    n.strip_prefix("已回到 ")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(if n.is_empty() { "上一个版本" } else { n })
        .to_string()
}

impl Engine {
    /// Why a `fail` card about an item came up: a stall, a session error, or one of the release
    /// failures (refused, failed and rolled back).
    pub(crate) fn stall_cause(&self, d: &Decision, item: Option<&Item>) -> ApiResult<StallCause> {
        let base = facts::stall_cause(d);
        if !matches!(base, StallCause::Other) {
            return Ok(base);
        }
        let q = d.question.as_str();
        if q.starts_with(BUSY) && q.contains("正在进行") {
            return Ok(StallCause::ReleaseRefused {
                busy: true,
                still_busy: self.core().release_busy(),
            });
        }
        if q.starts_with(NO_SCRIPT) || q.starts_with(NOT_STARTED) {
            return Ok(StallCause::ReleaseRefused {
                busy: false,
                still_busy: false,
            });
        }
        let Some(item) = item else {
            return Ok(base);
        };
        let releases = self.release_decisions(&item.id)?;
        let Some(last) = releases.last() else {
            return Ok(base);
        };
        if last.result.get("outcome").and_then(Value::as_str) != Some("failed") {
            return Ok(base);
        }
        let s = |k: &str| {
            last.result
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let reason = s("reason");
        if reason == NO_RESULT || q.contains(NO_RESULT) {
            // nobody knows which version is live: not something to hand back to the bot
            return Ok(StallCause::Other);
        }
        if last.result.get("degraded").and_then(Value::as_bool) == Some(true) {
            // the failed version stays on a migrated database: yours to decide, not the bot's to retry
            return Ok(StallCause::Other);
        }
        let window_start = self.window_start(&d.workspace_id)?;
        let nth = releases
            .iter()
            .filter(|r| r.result.get("outcome").and_then(Value::as_str) == Some("failed"))
            .filter(|r| r.resolved.unwrap_or(r.created) >= window_start)
            .count();
        Ok(StallCause::ReleaseFailed {
            prev: prev_of(&s("note")),
            reason: if reason.is_empty() {
                "健康检查没有通过".into()
            } else {
                reason
            },
            log: s("log"),
            nth: u32::try_from(nth.max(1)).unwrap_or(u32::MAX),
        })
    }

    /// The item's approved release decisions, oldest first.
    fn release_decisions(&self, item: &str) -> ApiResult<Vec<Decision>> {
        Ok(self
            .core()
            .store()
            .decisions_of_item(item)
            .map_err(ce)?
            .into_iter()
            .filter(|d| {
                d.kind == "approve"
                    && d.subject_type == "release"
                    && d.status == "resolved"
                    && d.result.get("approved").and_then(Value::as_bool) == Some(true)
            })
            .collect())
    }

    /// When the current window began (0 without one).
    fn window_start(&self, ws: &str) -> ApiResult<i64> {
        Ok(self
            .yolo()
            .store()
            .reader(|c| repo::window_current(c, ws))?
            .map(|w| w.started)
            .unwrap_or(0))
    }

    /// YOLO's release approvals of `ws` (newest first), optionally only this window's.
    fn yolo_releases(&self, ws: &str, window: Option<&str>) -> ApiResult<Vec<repo::ActionRow>> {
        Ok(self.yolo().store().reader(|c| {
            repo::actions_where(
                c,
                &ActionFilter {
                    ws,
                    window,
                    outcome: Some("decided"),
                    rules: &[RULE],
                    limit: 200,
                    ..Default::default()
                },
            )
        })?)
    }

    /// The spacing rule (§3.1): 3 an hour, 12 a window, 15 minutes apart.
    pub fn release_spacing_ok(&self, ws: &str) -> ApiResult<bool> {
        let window = self
            .yolo()
            .store()
            .reader(|c| repo::window_current(c, ws))?
            .map(|w| w.id);
        let all = self.yolo_releases(ws, None)?;
        let done: Vec<i64> = all.iter().map(|a| a.done_at.unwrap_or(a.created)).collect();
        let in_window = all
            .iter()
            .filter(|a| window.is_some() && a.window_id == window)
            .count();
        Ok(spacing_ok(&done, in_window, self.yolo().now()))
    }

    /// Rebase conflicts handed back to the bot for this item since the window began.
    pub(crate) fn rebase_returns(&self, ws: &str, item: &str) -> ApiResult<u32> {
        let since = self.window_start(ws)?;
        let n = self
            .core()
            .store()
            .timeline(item)
            .map_err(ce)?
            .iter()
            .filter(|t| t.ts >= since && t.actor == "steve" && t.text.starts_with(REBASE_RETURNED))
            .count();
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Another release card is already scheduled by YOLO: one release at a time, in the order they
    /// came up (§5).
    pub(crate) fn release_queued(&self, d: &Decision) -> ApiResult<bool> {
        let scheduled = self.yolo().store().reader(|c| {
            repo::actions_where(
                c,
                &ActionFilter {
                    ws: &d.workspace_id,
                    outcome: Some("scheduled"),
                    rules: &[RULE],
                    limit: 50,
                    ..Default::default()
                },
            )
        })?;
        Ok(scheduled
            .iter()
            .any(|a| a.decision_id.as_deref() != Some(d.id.as_str())))
    }

    /// Breaker B1 (§3.2), looked at when a failure card comes up: YOLO's release approvals of this
    /// window that failed twice in a row, or one that never reported a result. True when it tripped.
    pub(crate) fn release_breaker(&self, d: &Decision) -> ApiResult<bool> {
        if d.kind != "fail" || d.item_id.is_none() {
            return Ok(false);
        }
        let Some(w) = self
            .yolo()
            .store()
            .reader(|c| repo::window_current(c, &d.workspace_id))?
        else {
            return Ok(false);
        };
        let mut failed = 0;
        for a in self.yolo_releases(&d.workspace_id, Some(&w.id))? {
            let Some(rel) = a
                .decision_id
                .as_deref()
                .map(|id| self.core().store().decision(id))
                .transpose()
                .map_err(ce)?
                .flatten()
            else {
                continue;
            };
            match rel.result.get("outcome").and_then(Value::as_str) {
                Some("failed") => {
                    if rel.result.get("reason").and_then(Value::as_str) == Some(NO_RESULT) {
                        return self.yolo().trip(
                            &d.workspace_id,
                            "B1",
                            "YOLO 批准的发布没有报告结果，不确定现在是哪个版本",
                        );
                    }
                    failed += 1;
                }
                Some("released") => break,
                _ => {}
            }
        }
        if failed >= 2 {
            return self
                .yolo()
                .trip(&d.workspace_id, "B1", "YOLO 批准的发布连续 2 次失败");
        }
        Ok(false)
    }
}

fn ce(e: crate::CoreError) -> ApiError {
    match e {
        crate::CoreError::Module(e) => e,
        e => ApiError::Internal(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_an_hour_fifteen_minutes_apart_twelve_a_window() {
        let m = 60_000;
        let now = 10 * HOUR_MS;
        assert!(spacing_ok(&[], 0, now));
        assert!(!spacing_ok(&[now - 10 * m], 1, now));
        assert!(spacing_ok(&[now - 15 * m], 1, now));
        assert!(!spacing_ok(
            &[now - 20 * m, now - 40 * m, now - 59 * m],
            3,
            now
        ));
        assert!(spacing_ok(
            &[now - 20 * m, now - 40 * m, now - 61 * m],
            3,
            now
        ));
        assert!(!spacing_ok(&[now - 5 * HOUR_MS], 12, now));
    }

    #[test]
    fn the_version_gone_back_to() {
        assert_eq!(prev_of("已回到 v41"), "v41");
        assert_eq!(prev_of(""), "上一个版本");
        assert_eq!(
            prev_of("Hub 已是新版本，前端仍是 w3"),
            "Hub 已是新版本，前端仍是 w3"
        );
    }
}
