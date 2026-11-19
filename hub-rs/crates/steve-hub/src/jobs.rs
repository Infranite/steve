//! Background jobs: hourly housekeeping (housekeeping.ts) and the release watcher (orchestrator `watchReleases`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use steve_runtime::{Clock, FirstRun, SystemClock, Task, every};

use serde_json::{Value, json};

use crate::api::AppState;
use crate::backend::{BResult, Obj, Repo};
use crate::js;

pub const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(3600);
pub const RELEASE_POLL: Duration = Duration::from_millis(3000);

/// The Hub owns task lifetimes; this is an ownership bag, not a scheduler.
#[derive(Default)]
pub struct Tasks {
    state: Mutex<TaskState>,
    stopping: tokio::sync::Mutex<()>,
}
#[derive(Default)]
struct TaskState {
    stopped: bool,
    tasks: Vec<Task>,
}
impl Tasks {
    pub fn start(&self, name: &'static str, start: impl FnOnce() -> Task) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.stopped && !state.tasks.iter().any(|t| t.name() == name) {
            let task = start();
            assert_eq!(task.name(), name, "task name differs from its registration");
            state.tasks.push(task);
        }
    }
    pub fn names(&self) -> Vec<&'static str> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .iter()
            .map(Task::name)
            .collect()
    }
    pub async fn stop(&self) {
        let _stopping = self.stopping.lock().await;
        let tasks = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            for task in &state.tasks {
                task.stop();
            }
            std::mem::take(&mut state.tasks)
        };
        for task in tasks {
            task.await;
        }
    }
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
            // the lifecycle decides what is over (completed, failed, cancelled, superseded alike)
            let phase = i.get("phase").and_then(Value::as_str).unwrap_or_default();
            if phase != "closed" {
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
            let text = steve_core::notice_line::archived(&archived, &label.to_string());
            let item = (archived.len() == 1).then(|| archived[0].as_str());
            crate::notify::notify(r, &ws, "archive", "info", &text, item)?;
        }
    }
    Ok(tidy)
}

/// Runs housekeeping now and then every hour.
pub fn spawn_housekeeping(st: AppState) -> Task {
    every(
        "hub.housekeeping",
        Arc::new(SystemClock),
        HOUSEKEEPING_EVERY,
        FirstRun::Immediate,
        move || {
            let st = st.clone();
            async move {
                let tidy = match st
                    .write(|r| {
                        Ok(housekeeping(r, SystemClock.now(), |p| {
                            Path::new(p).exists()
                        })?)
                    })
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!("housekeeping: {e}");
                        return;
                    }
                };
                for id in tidy {
                    if let Err(e) = st.domain.tidy_branch(&id).await {
                        tracing::error!("tidy {id} {e}");
                    }
                }
            }
        },
    )
}

/// What `fs.watchFile` compares: a missing file is a state of its own.
fn stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// How often the closings that wait for something are looked at again (a queued turn or the release
/// hold may end without a turn-end callback).
pub const LIFECYCLE_TICK: std::time::Duration = std::time::Duration::from_secs(10);

/// Commits the closings whose gate holds now, every [`LIFECYCLE_TICK`].
pub fn spawn_lifecycle_tick(st: AppState) -> Task {
    every(
        "hub.lifecycle",
        Arc::new(SystemClock),
        LIFECYCLE_TICK,
        FirstRun::Immediate,
        move || {
            let st = st.clone();
            async move {
                match st.domain.lifecycle_tick().await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(closed = n, "lifecycle tick"),
                    Err(e) => tracing::warn!("lifecycle tick: {}", e.message),
                }
            }
        },
    )
}

/// Polls `last.json` (and the `rollback.json` next to it) every 3 s and calls `reportRelease` on every
/// change (create, rewrite, delete).
pub fn spawn_release_watcher(st: AppState, file: PathBuf) -> steve_runtime::Task {
    let stamps = |f: &Path| (stamp(f), stamp(&f.with_file_name("rollback.json")));
    let last = Arc::new(Mutex::new(stamps(&file)));
    every(
        "hub.release",
        Arc::new(SystemClock),
        RELEASE_POLL,
        FirstRun::AfterPeriod,
        move || {
            let (st, f, last) = (st.clone(), file.clone(), last.clone());
            async move {
                let now = tokio::task::spawn_blocking(move || stamps(&f))
                    .await
                    .unwrap_or((None, None));
                let previous = std::mem::replace(&mut *last.lock().unwrap(), now);
                if now != previous {
                    if let Err(e) = st.domain.report_release().await {
                        tracing::error!("report release: {e}");
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::modules::Job;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn settle() {
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn stopping_the_hub_cancels_periodic_jobs_and_adopted_children() {
        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let jobs = Arc::new(Tasks::default());
        let clock = Arc::new(steve_runtime::FakeClock::new(0));
        let runs = Arc::new(AtomicUsize::new(0));
        let n = runs.clone();
        let c = clock.clone();
        jobs.start("period", || {
            every(
                "period",
                c,
                Duration::from_secs(1),
                FirstRun::Immediate,
                move || {
                    n.fetch_add(1, Ordering::SeqCst);
                    async {}
                },
            )
        });
        jobs.start("period", || panic!("duplicate registration must not spawn"));
        let dropped = Arc::new(AtomicUsize::new(0));
        let d = dropped.clone();
        let mut job = Job::spawned("children", move || {
            (0..2)
                .map(|_| {
                    let dropped = Dropped(d.clone());
                    tokio::spawn(async move {
                        let _dropped = dropped;
                        std::future::pending::<()>().await;
                    })
                })
                .collect()
        });
        job.bind(&jobs);
        tokio::spawn(job).await.unwrap();
        settle().await;
        assert_eq!(jobs.names(), ["period", "children"]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        jobs.stop().await;
        clock.advance(100_000);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            2,
            "all adopted children were dropped"
        );
        jobs.stop().await;
        jobs.start("late", || panic!("stopped owner must reject new tasks"));
    }
}
