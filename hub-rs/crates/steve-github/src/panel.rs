//! The item panel “PR 与检查” (embedding point `item.sidebar`): one panel per PR
//! linked to the item, open ones first. Actions: `rerun_failed` (only while a failed check comes
//! from GitHub Actions; your click, so it runs directly) and `open` (a link).

use std::collections::BTreeSet;

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::json;
use steve_modules::panels::{
    ActionOutcome, ItemPanel, Panel, PanelAction, PanelHead, PanelRow, RowState,
};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::base;
use steve_store::base::decisions as base_decisions;

use crate::accept::{PR_MERGE, Slot};
use crate::repo::{self, CheckRow, PrRow, RepoRow};
use crate::repos::check_state;
use crate::sync::latest_checks;

/// Provider id and panel id prefix.
pub const PLUGIN: &str = "github";
pub const TITLE: &str = "PR 与检查";
pub const RERUN: &str = "rerun_failed";
pub const OPEN: &str = "open";

/// `github.pr:<ghp>`.
pub fn panel_id(pr: &str) -> String {
    format!("{PLUGIN}.pr:{pr}")
}

/// “2 分钟前”.
pub fn ago(now: i64, at: i64) -> String {
    let mins = (now - at).max(0) / 60_000;
    match mins {
        0 => "刚刚".into(),
        m if m < 60 => format!("{m} 分钟前"),
        m if m < 24 * 60 => format!("{} 小时前", m / 60),
        m => format!("{} 天前", m / (24 * 60)),
    }
}

fn row_state(k: &CheckRow) -> RowState {
    match check_state(k) {
        "ok" => RowState::Ok,
        "fail" => RowState::Fail,
        "skipped" => RowState::Skipped,
        _ => RowState::Pending,
    }
}

/// Workflow runs with a failed job: what `rerun_failed` reruns.
pub fn failed_runs(checks: &[&CheckRow]) -> BTreeSet<i64> {
    checks
        .iter()
        .filter(|k| check_state(k) == "fail")
        .filter_map(|k| k.run_id)
        .collect()
}

fn badges(pr: &PrRow) -> Vec<String> {
    let mut b = Vec::new();
    if pr.draft && pr.state == "open" {
        b.push("草稿");
    }
    match pr.state.as_str() {
        "merged" => b.push("已合并"),
        "closed" => b.push("已关闭"),
        _ => {}
    }
    if pr.state == "open" && pr.mergeable.as_deref() == Some("dirty") {
        b.push("有冲突");
    }
    if pr.review_state == "changes_requested" {
        b.push("要求修改");
    }
    b.into_iter().map(String::from).collect()
}

/// `3 项检查通过 · 等你验收后合并`.
fn foot(
    c: &Connection,
    pr: &PrRow,
    checks: &[&CheckRow],
    item_status: Option<&str>,
) -> ApiResult<String> {
    let n = |s: &str| checks.iter().filter(|k| check_state(k) == s).count();
    let (fail, pending, ok) = (n("fail"), n("pending"), n("ok") + n("skipped"));
    let mut parts = Vec::new();
    if checks.is_empty() {
        parts.push("还没有检查".to_string());
    } else if fail > 0 {
        parts.push(format!("{fail} 项检查失败"));
    } else if pending > 0 {
        parts.push(format!("{pending} 项检查进行中"));
    } else {
        parts.push(format!("{ok} 项检查通过"));
    }
    let green = !checks.is_empty() && fail == 0 && pending == 0;
    match pr.state.as_str() {
        "merged" => parts.push("已合并".into()),
        "closed" => parts.push("已关闭".into()),
        _ if green
            && base_decisions::open_for(c, &pr.workspace_id, PR_MERGE, &pr.id)?.is_some() =>
        {
            parts.push("等你确认合并".into())
        }
        _ if green && item_status == Some("wait") => parts.push("等你验收后合并".into()),
        _ => {}
    }
    Ok(parts.join(" · "))
}

/// The panel of one PR.
pub fn pr_panel(c: &Connection, repo: &RepoRow, pr: &PrRow, now: i64) -> ApiResult<Panel> {
    let rows = repo::checks_for_sha(c, &repo.id, &pr.head_sha)?;
    let checks = latest_checks(&rows);
    let item_status = match &pr.item_id {
        Some(i) => repo::item_status(c, &pr.workspace_id, i)?,
        None => None,
    };
    let mut updated = pr.updated;
    let rows_out = checks
        .iter()
        .map(|k| {
            updated = updated.max(k.updated);
            let sub = match k.status.as_str() {
                "completed" => k.completed.map(|t| ago(now, t)),
                "queued" => Some("排队中".into()),
                _ => Some("进行中".into()),
            };
            PanelRow {
                state: row_state(k),
                text: k.name.clone(),
                sub,
                href: Some(k.url.clone()).filter(|u| !u.is_empty()),
            }
        })
        .collect();
    let mut actions = Vec::new();
    if pr.state == "open" && !failed_runs(&checks).is_empty() {
        actions.push(PanelAction {
            id: RERUN.into(),
            label: "重跑失败的检查".into(),
            href: None,
        });
    }
    actions.push(PanelAction {
        id: OPEN.into(),
        label: "在 GitHub 打开".into(),
        href: Some(pr.url.clone()),
    });
    Ok(Panel {
        id: panel_id(&pr.id),
        plugin: PLUGIN.into(),
        title: TITLE.into(),
        head: Some(PanelHead {
            text: format!("#{} {}", pr.number, pr.title),
            href: Some(pr.url.clone()),
            badges: badges(pr),
        }),
        rows: rows_out,
        foot: Some(foot(c, pr, &checks, item_status.as_deref())?),
        actions,
        updated,
    })
}

/// The panels of an item: its PRs, open first, then the newest.
pub fn item_panels(c: &Connection, ws: &str, item: &str, now: i64) -> ApiResult<Vec<Panel>> {
    let mut prs = repo::prs(c, ws, None, false, Some(item))?;
    prs.sort_by_key(|p| (p.state != "open", -p.updated));
    let mut out = Vec::new();
    for pr in prs {
        if let Some(r) = repo::repo_get(c, &pr.repo_id)? {
            out.push(pr_panel(c, &r, &pr, now)?);
        }
    }
    Ok(out)
}

/// [`ItemPanel`] of this module (registered by [`crate::accept::register`]).
pub(crate) struct GhPanels(pub(crate) Slot);

#[async_trait]
impl ItemPanel for GhPanels {
    fn id(&self) -> &'static str {
        PLUGIN
    }

    fn panels(&self, c: &Connection, ws: &str, _me: &str, item_id: &str) -> ApiResult<Vec<Panel>> {
        let now = self.0.get().map_or_else(steve_store::now, |g| g.now());
        item_panels(c, ws, item_id, now)
    }

    async fn action(
        &self,
        m: &Modules,
        caller: &Caller,
        item_id: &str,
        panel: &str,
        action: &str,
    ) -> ApiResult<ActionOutcome> {
        let gh = self.0.need()?;
        let pr_id = panel
            .strip_prefix(&format!("{PLUGIN}.pr:"))
            .ok_or_else(|| ApiError::not_found("面板"))?;
        let ws = caller.ws.clone();
        let (pr, r) = m
            .store
            .reader(|c| {
                let pr = repo::pr_get(c, pr_id)?
                    .filter(|p| p.workspace_id == ws && p.item_id.as_deref() == Some(item_id));
                let r = match &pr {
                    Some(p) => repo::repo_get(c, &p.repo_id)?,
                    None => None,
                };
                Ok::<_, ApiError>(pr.zip(r))
            })?
            .ok_or_else(|| ApiError::not_found("面板"))?;
        if action != RERUN {
            return Err(ApiError::not_found("操作"));
        }
        let runs = m.store.reader(|c| {
            let rows = repo::checks_for_sha(c, &r.id, &pr.head_sha)?;
            Ok::<_, ApiError>(failed_runs(&latest_checks(&rows)))
        })?;
        if pr.state != "open" || runs.is_empty() {
            return Err(ApiError::conflict("没有失败的检查可以重跑"));
        }
        let conn = gh
            .conn_for_write(&ws, &r.connection_id)
            .map_err(|e| match e {
                steve_modules::vault::VaultError::Api(a) => a,
                e => ApiError::rule(e.to_string()),
            })?;
        let mut done = 0;
        for run in &runs {
            let path = format!(
                "/repos/{}/actions/runs/{run}/rerun-failed-jobs",
                r.full_name
            );
            match conn.post(&path, &json!({})).await {
                Ok(_) => done += 1,
                Err(e) => {
                    tracing::warn!(repo = %r.full_name, run, error = %e, "github rerun failed");
                    if done == 0 {
                        return Err(ApiError::rule(format!("重跑失败：{e}")));
                    }
                }
            }
        }
        m.store.transaction(|tx| {
            base::audit(
                tx,
                &ws,
                &caller.me,
                "github.rerun",
                &pr.id,
                &json!({"repo": r.full_name, "number": pr.number, "runs": runs}),
            )
            .map_err(ApiError::from)
        })?;
        Ok(ActionOutcome {
            message: Some(format!("已重跑 {done} 个工作流里失败的检查")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_reads_like_the_prototype() {
        let now = 10 * 86_400_000;
        assert_eq!(ago(now, now - 30_000), "刚刚");
        assert_eq!(ago(now, now - 2 * 60_000), "2 分钟前");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3 小时前");
        assert_eq!(ago(now, now - 2 * 86_400_000), "2 天前");
    }
}
