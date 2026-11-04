//! Background jobs: hourly housekeeping (housekeeping.ts) and the release watcher (orchestrator `watchReleases`).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::api::AppState;
use crate::backend::{BResult, Obj, Repo};
use crate::js;

pub const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(3600);
pub const RELEASE_POLL: Duration = Duration::from_millis(3000);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Archive finished items after the workspace's archive period (`archDays`, default 30; `0`/NaN = never).
/// Returns the finished items that still have a working directory, for `tidyBranch`.
pub fn housekeeping(r: &dyn Repo, now: i64, exists: impl Fn(&str) -> bool) -> BResult<Vec<String>> {
    let mut tidy = vec![];
    for w in r.workspaces_all()? {
        let ws = w
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Number(settings.get(ws, "archDays", "30"))
        let raw = r
            .settings_get(&ws, "archDays")?
            .unwrap_or_else(|| json!("30"));
        let days = match &raw {
            Value::Number(x) => x.as_f64().unwrap_or(f64::NAN),
            Value::String(s) => crate::config::js_number(s),
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Null => 0.0,
            _ => f64::NAN,
        };
        let archive = !days.is_nan() && days != 0.0;
        let before = now as f64 - days * 24.0 * 3_600_000.0;
        let label = serde_json::Number::from_f64(days)
            .map(|n| js::number_string(&n))
            .unwrap_or_default();
        let mut archived: Vec<String> = vec![];
        for i in r.items_all(&ws)? {
            let status = i.get("status").and_then(Value::as_str).unwrap_or_default();
            if status != "done" && status != "drop" {
                continue;
            }
            let id = i
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let updated = i.get("updated").and_then(Value::as_f64).unwrap_or(f64::MAX);
            if archive && !js::truthy(i.get("archived")) && updated < before {
                let mut p = Obj::new();
                p.insert("archived".into(), json!(true));
                r.item_patch(&id, &p)?;
                r.timeline_add(
                    &id,
                    "steve",
                    "sys",
                    &format!("完成满 {label} 天，自动归档"),
                    None,
                )?;
                archived.push(id.clone());
            }
            if let Some(cwd) = i
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                && exists(cwd)
            {
                tidy.push(id);
            }
        }
        // one notification per run, not one per item
        if !archived.is_empty() {
            let more = if archived.len() > 3 { " 等" } else { "" };
            let text = format!(
                "自动归档了 {} 件完成满 {label} 天的事项：{}{more}",
                archived.len(),
                archived[..archived.len().min(3)].join("、")
            );
            let item = (archived.len() == 1).then(|| archived[0].as_str());
            crate::notify::notify(r, &ws, "archive", "info", &text, item)?;
        }
    }
    Ok(tidy)
}

/// Runs housekeeping now and then every hour.
pub fn spawn_housekeeping(st: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(HOUSEKEEPING_EVERY);
        loop {
            tick.tick().await;
            let tidy = match st
                .write(|r| Ok(housekeeping(r, now_ms(), |p| Path::new(p).exists())?))
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("housekeeping: {e}");
                    continue;
                }
            };
            for id in tidy {
                if let Err(e) = st.domain.tidy_branch(&id).await {
                    tracing::error!("tidy {id} {e}");
                }
            }
        }
    });
}

/// What `fs.watchFile` compares: a missing file is a state of its own.
fn stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Polls `last.json` (and the `rollback.json` next to it) every 3 s and calls `reportRelease` on every
/// change (create, rewrite, delete).
pub fn spawn_release_watcher(st: AppState, file: PathBuf) {
    let stamp = |f: &Path| (stamp(f), stamp(&f.with_file_name("rollback.json")));
    tokio::spawn(async move {
        let mut last = stamp(&file);
        let mut tick = tokio::time::interval(RELEASE_POLL);
        tick.tick().await;
        loop {
            tick.tick().await;
            let f = file.clone();
            let now = tokio::task::spawn_blocking(move || stamp(&f))
                .await
                .unwrap_or((None, None));
            if now != last {
                last = now;
                if let Err(e) = st.domain.report_release().await {
                    tracing::error!("report release: {e}");
                }
            }
        }
    });
}
