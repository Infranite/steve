use super::*;
use std::{future::Future, sync::Mutex, time::Duration};
use steve_runtime::{
    Clock, SystemClock,
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use steve_store::retention::{ALL, DEFAULT_BUDGET, Kind, SliceReport};

const INTERVAL: Duration = Duration::from_secs(5 * 60);
const OPTIMIZE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const SLICES_PER_POLICY: usize = 4;

install! { install(reg, cx) [db, mut jobs] => [db, jobs] {
    jobs.push(Job::new("storage.retention", run(db.clone(), cx.cfg.home.clone(), cx.cfg.no_resume)));
}}

struct State {
    due: i64,
    optimize_due: i64,
    after: [Option<i64>; ALL.len()],
    done: [bool; ALL.len()],
    resources_done: bool,
}
struct Source<F, O, R> {
    execute: F,
    optimize: O,
    resources: R,
    state: Mutex<State>,
}
impl<F, O, R> Source<F, O, R> {
    fn new(first_due: i64, execute: F, optimize: O, resources: R) -> Self {
        Self {
            execute,
            optimize,
            resources,
            state: Mutex::new(State {
                due: first_due,
                optimize_due: first_due.saturating_add(OPTIMIZE_INTERVAL.as_millis() as i64),
                after: [None; ALL.len()],
                done: [false; ALL.len()],
                resources_done: false,
            }),
        }
    }
}
impl<F, O, R, Fut, Opt, Res> DueSource for Source<F, O, R>
where
    F: Fn(Kind, i64, Option<i64>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = steve_store::Result<SliceReport<i64>>> + Send,
    O: Fn() -> Opt + Send + Sync + 'static,
    Opt: Future<Output = steve_store::Result<()>> + Send,
    R: Fn(i64) -> Res + Send + Sync + 'static,
    Res: Future<Output = steve_store::Result<bool>> + Send,
{
    fn name(&self) -> &'static str {
        "storage.retention"
    }
    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Some(state.due.min(state.optimize_due)))
    }
    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let optimize_due = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .optimize_due;
        let mut error = None;
        if optimize_due <= now {
            match (self.optimize)().await {
                Ok(()) => {
                    self.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .optimize_due = now.saturating_add(OPTIMIZE_INTERVAL.as_millis() as i64)
                }
                Err(e) => error = Some(DueError(e.to_string())),
            }
        }
        let due = self.state.lock().unwrap_or_else(|e| e.into_inner()).due;
        if due <= now {
            for (index, kind) in ALL.into_iter().enumerate() {
                for _ in 0..SLICES_PER_POLICY {
                    let after = {
                        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.done[index] {
                            break;
                        }
                        state.after[index]
                    };
                    match (self.execute)(kind, now, after).await {
                        Ok(report) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            state.after[index] = report.next;
                            if !report.more() {
                                state.done[index] = true;
                                break;
                            }
                        }
                        Err(e) => {
                            error = Some(DueError(e.to_string()));
                            break;
                        }
                    }
                }
            }
            // Filesystem resources share this Source and its retry/continuation cadence,
            // but run outside SQLite row policies and all database write grants.
            let resources_done = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .resources_done;
            if !resources_done {
                match (self.resources)(now).await {
                    Ok(more) => {
                        self.state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .resources_done = !more
                    }
                    Err(e) => {
                        // Retry resources next interval, without freezing completed row policies.
                        self.state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .resources_done = true;
                        error = Some(DueError(e.to_string()));
                    }
                }
            }
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.done.iter().all(|v| *v) && state.resources_done {
                state.due = now.saturating_add(INTERVAL.as_millis() as i64);
                state.done.fill(false);
                state.resources_done = false;
            } else {
                state.due = now;
            }
        }
        error.map_or(Ok(()), Err)
    }
}
fn source(
    db: steve_store::Store,
    home: std::path::PathBuf,
    no_resume: bool,
    first_due: i64,
) -> impl DueSource {
    let optimizer = db.clone();
    let backup_store = db.clone();
    Source::new(
        first_due,
        move |kind: Kind, now: i64, after: Option<i64>| {
            let db = db.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    db.retention_slice(kind, now, DEFAULT_BUDGET, after)
                })
                .await
                .map_err(|e| steve_store::StoreError::Conflict(e.to_string()))?
            }
        },
        move || {
            let db = optimizer.clone();
            async move {
                tokio::task::spawn_blocking(move || db.optimize())
                    .await
                    .map_err(|e| steve_store::StoreError::Conflict(e.to_string()))?
            }
        },
        move |now: i64| {
            let home = home.clone();
            let store = backup_store.clone();
            async move {
                // A Hub on a data copy still maintains its database rows, but is not the
                // installation that owns these backup files. Do not even inventory them.
                if no_resume {
                    return Ok(false);
                }
                tokio::task::spawn_blocking(move || {
                    // retain records the whole sweep's failures for /system/backup before
                    // this adapter returns only the continuation bit to the shared Source.
                    crate::backup::retain(&store, &home, now.max(0) as u64)
                        .map(|report| report.more)
                        .map_err(|e| steve_store::StoreError::Conflict(e.to_string()))
                })
                .await
                .map_err(|e| steve_store::StoreError::Conflict(e.to_string()))?
            }
        },
    )
}
async fn run(db: steve_store::Store, home: std::path::PathBuf, no_resume: bool) {
    let clock = Arc::new(SystemClock);
    let source = Arc::new(source(db, home, no_resume, clock.now()));
    due_loop(
        source,
        clock,
        Arc::new(tokio::sync::Notify::new()),
        Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_runtime::FakeClock;

    async fn settle(clock: &FakeClock, at: i64) {
        for _ in 0..1000 {
            if clock.observed(at) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("due_loop did not reach virtual sleep at {at}");
    }

    #[tokio::test]
    async fn retention_installs_when_no_resume_is_set() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_path_buf();
        let vars: crate::wire::Vars = Arc::new(move |key| match key {
            "HOME" | "STEVE_HOME" => Some(path.display().to_string()),
            "STEVE_NO_RESUME" => Some("1".into()),
            "STEVE_PORT" => Some("1".into()),
            "STEVE_VAULT_KEYSTORE" => Some("file".into()),
            _ => None,
        });
        let cfg = Arc::new(Config::from_vars(|key| vars(key)).unwrap());
        assert!(cfg.no_resume);
        let db = steve_store::Store::open(
            home.path().join("steve.sqlite3"),
            steve_store::NoPublisher,
        )
        .unwrap();
        let mut registry = HubRegistry {
            db: Some(db),
            jobs: Some(vec![]),
            ..Default::default()
        };
        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(Default::default())));
        let asm = crate::extension::Assembly::empty();
        install(
            &mut registry,
            &HubEnv {
                cfg: &cfg,
                bus: &bus,
                vars: &vars,
                asm: &asm,
            },
        )
        .unwrap();
        assert_eq!(
            registry.jobs.unwrap().len(),
            1,
            "the real installer registers retention in NO_RESUME mode"
        );
    }

    #[tokio::test]
    async fn retention_no_resume_cleans_rows_but_never_backup_directories() {
        let home = tempfile::tempdir().unwrap();
        let db = steve_store::Store::open(home.path().join("db.sqlite3"), steve_store::NoPublisher)
            .unwrap();
        let old = crate::daily_backup::make(home.path(), db.path(), "old").unwrap();
        let latest = crate::daily_backup::make(home.path(), db.path(), "latest").unwrap();
        let path = old.database.parent().unwrap().join("manifest.json");
        let mut manifest: steve_store_sqlite::backup::Manifest =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest.created_ms = 0;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        // A pending retirement must also remain untouched on a copied installation.
        let retiring = crate::backup::root(home.path()).join("foreign.retiring");
        std::fs::create_dir(&retiring).unwrap();
        let pending = steve_store_sqlite::backup::backup(
            db.path(),
            &crate::backup::plan(home.path(), steve_store_sqlite::backup::Reason::Diagnostic),
            &|_| {},
            &steve_store_sqlite::backup::CancelFlag::default(),
        )
        .unwrap();
        let pending_path =
            crate::backup::root(home.path()).join(format!("{}.retiring", pending.id));
        std::fs::rename(&pending.directory, &pending_path).unwrap();
        steve_store::testkit::hub::hub_fixtures::seed_node_vitals(&db).unwrap();
        let now = 10 * 86_400_000;
        source(db.clone(), home.path().to_owned(), true, now)
            .run_due(now)
            .await
            .unwrap();
        let remaining: i64 = steve_store::testkit::hub::node_fixtures::node_vitals_count(&db).unwrap();
        assert_eq!(
            remaining, 0,
            "NO_RESUME must not disable database row policies"
        );
        assert!(
            old.database.exists()
                && latest.database.exists()
                && retiring.exists()
                && pending_path.exists()
        );
        assert_eq!(
            crate::daily_backup::status(home.path(), db.path(), false, 7)["retention"]["failures"],
            0
        );
        source(db, home.path().to_owned(), false, now)
            .run_due(now)
            .await
            .unwrap();
        assert!(!old.database.exists() && latest.database.exists() && retiring.exists());
        assert!(
            !pending_path.exists(),
            "normal mode retries retired backups too"
        );
        // This is produced by the real Source adapter, which previously dropped failures.
        assert_eq!(crate::backup::retention_failures(home.path()), 1);
    }

    #[tokio::test]
    async fn retention_resource_failure_never_blocks_the_next_row_sweep() {
        let rows = Arc::new(AtomicUsize::new(0));
        let row_calls = rows.clone();
        let source = Source::new(
            0,
            move |_: Kind, _: i64, _: Option<i64>| {
                row_calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok(SliceReport {
                        affected: 0,
                        inspected: 0,
                        elapsed_ms: 0,
                        overruns: 0,
                        failures: 0,
                        next: None,
                    })
                }
            },
            || async { Ok(()) },
            |_: i64| async {
                Err(steve_store::StoreError::Conflict(
                    "filesystem unavailable".into(),
                ))
            },
        );
        for sweep in 0..3 {
            let now = sweep * INTERVAL.as_millis() as i64;
            assert!(source.run_due(now).await.is_err());
            assert_eq!(
                rows.load(Ordering::SeqCst),
                (sweep as usize + 1) * ALL.len()
            );
            assert_eq!(
                source.next_due().await.unwrap(),
                Some(now + INTERVAL.as_millis() as i64)
            );
        }
    }

    #[tokio::test]
    async fn retention_continues_cursors_and_optimizes_hourly_on_virtual_time() {
        let clock = Arc::new(FakeClock::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let optimized = Arc::new(AtomicUsize::new(0));
        let resources = Arc::new(AtomicUsize::new(0));
        let resource_calls = resources.clone();
        let observed = calls.clone();
        let count_opt = optimized.clone();
        let source = Arc::new(Source::new(
            100,
            move |kind: Kind, _: i64, after: Option<i64>| {
                let count = observed.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(SliceReport {
                        affected: 0,
                        inspected: 1,
                        elapsed_ms: 0,
                        overruns: 0,
                        failures: 0,
                        next: if kind == Kind::Vitals && after.unwrap_or(0) < 5 {
                            Some(after.unwrap_or(0) + 1)
                        } else {
                            None
                        },
                    })
                }
            },
            move || {
                count_opt.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            move |_: i64| {
                let n = resource_calls.fetch_add(1, Ordering::SeqCst);
                async move { Ok(n == 0) }
            },
        ));
        assert_eq!(source.name(), "storage.retention");
        let task = tokio::spawn(due_loop(
            source.clone(),
            clock.clone(),
            Arc::new(tokio::sync::Notify::new()),
            Backoff {
                base: Duration::from_secs(1),
                max: Duration::from_secs(1),
                factor: 1,
                jitter: Jitter::None,
                attempts: None,
                reset_after: None,
            },
        ));
        settle(&clock, 0).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        clock.advance_to(100);
        settle(&clock, 100).await;
        let sweep = ALL.len() + 5;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            sweep,
            "four-slice quota continues in a second round"
        );
        assert_eq!(optimized.load(Ordering::SeqCst), 0);
        assert_eq!(
            resources.load(Ordering::SeqCst),
            2,
            "resource continuation uses the same Source"
        );
        assert_eq!(
            source.next_due().await.unwrap(),
            Some(100 + INTERVAL.as_millis() as i64)
        );
        let hour = 100 + OPTIMIZE_INTERVAL.as_millis() as i64;
        clock.advance_to(hour);
        settle(&clock, hour).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2 * sweep);
        assert_eq!(optimized.load(Ordering::SeqCst), 1);
        assert_eq!(resources.load(Ordering::SeqCst), 3);
        clock.advance_to(hour + INTERVAL.as_millis() as i64);
        settle(&clock, hour + INTERVAL.as_millis() as i64).await;
        assert_eq!(
            optimized.load(Ordering::SeqCst),
            1,
            "ordinary retention sweep does not run optimize again"
        );
        task.abort();
        let _ = task.await;
    }
}
