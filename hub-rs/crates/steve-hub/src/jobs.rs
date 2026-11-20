//! Hub task lifetimes and lifecycle polling. Core owns housekeeping and release maintenance.

use std::sync::{Arc, Mutex};
use steve_runtime::{FirstRun, SystemClock, Task, every};

use crate::api::AppState;

/// The Hub owns already-spawned task handles. Lane scopes belong around business
/// futures before spawn, never around this ownership bag or its JoinHandle polling.
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
        if state.stopped {
            return;
        }
        if state.tasks.iter().any(|t| t.name() == name) {
            drop(state);
            panic!("duplicate background job: {name}");
        }
        let task = start();
        assert_eq!(task.name(), name, "task name differs from its registration");
        state.tasks.push(task);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::modules::Job;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
        let duplicate = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jobs.start("period", || panic!("duplicate factory must never run"));
        }))
        .expect_err("duplicate job names must fail at registration");
        assert!(
            duplicate
                .downcast_ref::<String>()
                .unwrap()
                .contains("duplicate background job: period")
        );
        let dropped = Arc::new(AtomicUsize::new(0));
        let d = dropped.clone();
        let job = Job::new("children", async move {
            let a = Dropped(d.clone());
            let b = Dropped(d.clone());
            let run = |guard| async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            };
            tokio::join!(run(a), run(b));
        });
        job.start(&jobs);
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
