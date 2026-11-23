//! Keeping originals only as long as the user chose (job
//! `flow.retention`): after `keepRaw` days an event's payload is dropped, and so are the contexts and
//! step inputs/outputs of the runs that handled it (nodes, branches and timings stay); the event row
//! itself — summary and triage result — goes after a year.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_store::{Read, RunRows, SignalRows, W, WorkspaceSettings, Workspaces};

use crate::{FlowCx, FlowResult};

/// The setting key (shared with channel messages).
pub const KEEP_RAW: &str = "keepRaw";
/// Allowed values in days; the first is the default.
pub const KEEP_RAW_DAYS: [i64; 3] = [7, 30, 90];
/// Event rows are deleted after this long.
pub const KEEP_EVENT_MS: i64 = 365 * 24 * 3_600_000;
/// What replaces purged run data.
pub const PURGED: &str = r#"{"purged":true}"#;
/// How often the job runs.
pub const EVERY: Duration = Duration::from_secs(3600);

/// The entry steve-core's `SETTINGS.data` gets (integrator adds it to the constant).
pub fn keep_raw_setting() -> Value {
    json!({
        "key": KEEP_RAW,
        "label": "事件与消息原文保留",
        "values": ["7", "30", "90"],
        "def": "7",
        "live": true,
        "labels": {"7": "7 天", "30": "30 天", "90": "90 天"},
    })
}

/// The workspace's `keepRaw` in days (a JSON string or number, or the bare text; anything else →
/// 7).
pub fn keep_raw_days(c: &mut impl Read, ws: &str) -> steve_store::Result<i64> {
    let v = WorkspaceSettings(c).get(ws, KEEP_RAW)?;
    let days = v.and_then(|v| match serde_json::from_str::<Value>(&v) {
        Ok(Value::String(s)) => s.parse::<i64>().ok(),
        Ok(Value::Number(n)) => n.as_i64(),
        _ => v.trim().parse::<i64>().ok(),
    });
    Ok(days
        .filter(|d| KEEP_RAW_DAYS.contains(d))
        .unwrap_or(KEEP_RAW_DAYS[0]))
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    pub payloads: usize,
    pub runs: usize,
    pub events_deleted: usize,
}

/// Events purged per transaction; the writer is let go between batches.
pub const PURGE_BATCH: usize = 500;
/// Event rows deleted per transaction.
pub const DELETE_BATCH: usize = 1000;

/// One pass over all workspaces at `now`, in short transactions: per workspace, batches of
/// [`PURGE_BATCH`] events lose their payload together with the data of the runs that handled them,
/// then year-old rows go [`DELETE_BATCH`] at a time. Synchronous; [`run`] calls it off the runtime.
pub fn sweep(cx: &FlowCx, now: i64) -> FlowResult<Swept> {
    let policies = cx.store().read(|r| {
        Workspaces(r)
            .ids()?
            .into_iter()
            .map(|ws| {
                let days = keep_raw_days(r, &ws)?;
                Ok((ws, days))
            })
            .collect::<steve_store::Result<Vec<_>>>()
    })?;
    let mut out = Swept::default();
    for (ws, days) in policies {
        let cut = now - days * 24 * 3_600_000;
        loop {
            let (runs, payloads) = cx.store().write(|w| purge_batch(w, &ws, cut, now))?;
            out.runs += runs;
            out.payloads += payloads;
            if payloads < PURGE_BATCH {
                break;
            }
        }
    }
    loop {
        let n = cx
            .store()
            .write(|w| SignalRows(w).delete_before(now - KEEP_EVENT_MS, DELETE_BATCH as u64))?
            as usize;
        out.events_deleted += n;
        if n < DELETE_BATCH {
            break;
        }
    }
    Ok(out)
}

/// Purges up to [`PURGE_BATCH`] of `ws`'s events received before `cut`: their payload, and the
/// contexts and step data of their runs, in the caller's transaction so the three stay consistent.
/// Returns `(runs, events)` purged.
fn purge_batch(w: &mut W<'_>, ws: &str, cut: i64, now: i64) -> steve_store::Result<(usize, usize)> {
    let ids = SignalRows(w).with_payload_before(ws, cut, PURGE_BATCH as u64)?;
    if ids.is_empty() {
        return Ok((0, 0));
    }
    let runs = RunRows(w).purge_event_data(&ids, PURGED)?;
    let events = SignalRows(w).purge_payloads(&ids, now)?;
    Ok((runs as usize, events as usize))
}

fn report_pass(pass: FlowResult<Swept>) {
    match pass {
        Ok(s) if s.payloads + s.events_deleted > 0 => tracing::info!(?s, "flow.retention"),
        Ok(_) => {}
        Err(e) => tracing::warn!("flow.retention: {e}"),
    }
}

fn retention_job<F, Fut>(clock: Arc<dyn steve_runtime::Clock>, mut pass: F) -> steve_runtime::Task
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = FlowResult<Swept>> + Send,
{
    steve_runtime::every_after(
        "flow.retention",
        clock,
        EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let work = pass();
            async move {
                report_pass(work.await);
            }
        },
    )
}

/// The hourly job; each pass runs on the blocking pool.
pub fn run(cx: Arc<FlowCx>) -> steve_runtime::Task {
    let clock = cx.modules.clock.clone();
    retention_job(clock, move || {
        let cx = cx.clone();
        async move {
            let now = cx.now();
            cx.clone()
                .blocking(move |_| {
                    // Schedule-run retention formerly lived in the scheduler's polling loop.
                    cx.store().write(|tx| crate::runs::prune(tx, now))?;
                    sweep(&cx, now)
                })
                .await
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_runtime::testkit::eventually_sync;
    use steve_runtime::{Clock, FakeClock};
    use tokio::sync::Notify;

    struct ObservedClock {
        time: Arc<FakeClock>,
        active: Arc<AtomicUsize>,
    }
    struct ActiveSleep(Arc<AtomicUsize>);
    impl Drop for ActiveSleep {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl Clock for ObservedClock {
        fn now(&self) -> i64 {
            self.time.now()
        }
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let sleeping = self.time.sleep(duration);
            let active = self.active.clone();
            Box::pin(async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveSleep(active);
                sleeping.await;
            })
        }
    }

    #[tokio::test]
    async fn retention_cadence_table() {
        for (fails, work_ms) in [(false, 0), (false, 60_000), (true, 0), (true, 60_000)] {
            let time = Arc::new(FakeClock::new(0));
            let active = Arc::new(AtomicUsize::new(0));
            let clock = Arc::new(ObservedClock {
                time: time.clone(),
                active: active.clone(),
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let release = Arc::new(Notify::new());
            let job = tokio::spawn(retention_job(clock, {
                let calls = calls.clone();
                let release = release.clone();
                move || {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    let release = release.clone();
                    async move {
                        if first {
                            release.notified().await;
                            if fails {
                                return Err(steve_modules::ApiError::Internal(
                                    "scripted retention failure".into(),
                                ));
                            }
                        }
                        Ok(Swept::default())
                    }
                }
            }));
            eventually_sync("immediate retention pass", || {
                calls.load(Ordering::SeqCst) == 1
            })
            .await;
            assert_eq!(
                active.load(Ordering::SeqCst),
                0,
                "period is not started alongside work"
            );
            time.advance(work_ms);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "passes never overlap");
            release.notify_one();
            eventually_sync("retention fixed delay starts after work", || {
                time.observed(work_ms)
            })
            .await;
            assert_eq!(active.load(Ordering::SeqCst), 1);
            let period = EVERY.as_millis() as i64;
            time.advance(period - 1);
            eventually_sync("retention before deadline", || {
                time.observed(work_ms + period - 1) || calls.load(Ordering::SeqCst) > 1
            })
            .await;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "fails={fails}, work={work_ms}"
            );
            time.advance(1);
            eventually_sync("retention next pass", || {
                calls.load(Ordering::SeqCst) == 2 && time.observed(work_ms + period)
            })
            .await;
            job.abort();
            assert!(job.await.unwrap_err().is_cancelled());
            eventually_sync("retention wait cancellation", || {
                active.load(Ordering::SeqCst) == 0
            })
            .await;
            time.advance(period * 2);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "cancelled job never dispatches again"
            );
        }
    }
}
