//! Optional per-turn disk observations. These are asynchronous path-stock samples, never a
//! baseline, attributable growth, historical estimate, authorization or permission to clean up.
//! Original workspace/principal/project/seed identity is not inferred from path segments.
//!
//! The journal transaction recording a turn's phase records its observation too. One background
//! worker measures outside that transaction. Recovery never resamples a past timestamp.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use steve_node_api::*;

use crate::disk_usage::{Budget, Measurement};
use crate::journal::{Tx, internal};
use crate::{Inner, LocalNode, now};

#[cfg(test)]
mod tests;

/// Work and retention settings, not disk quotas. Configuring this does not change run admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub budget: Budget,
    pub keep_rows: u64,
    pub keep_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            budget: Budget {
                entries: 100_000,
                depth: 64,
                elapsed: Duration::from_millis(500),
            },
            keep_rows: 1024,
            keep_ms: 7 * 24 * 3600 * 1000,
        }
    }
}

impl Policy {
    pub fn check(&self) -> Result<(), &'static str> {
        if self.budget.entries == 0
            || self.budget.depth == 0
            || self.budget.elapsed.is_zero()
            || self.keep_rows == 0
            || self.keep_rows > i64::MAX as u64
            || self.keep_ms == 0
            || self.keep_ms > i64::MAX as u64
        {
            Err("disk observation budgets and retention must be positive and representable")
        } else {
            Ok(())
        }
    }

    fn configured(mut get: impl FnMut(&str) -> Option<String>) -> Result<Option<Self>, String> {
        match get("STEVE_DISK_OBSERVE").as_deref().map(str::trim) {
            None | Some("") | Some("off") => return Ok(None),
            Some("on") => {}
            Some(_) => return Err("STEVE_DISK_OBSERVE must be on or off".into()),
        }
        let mut p = Self::default();
        let mut number = |key: &str, default: u64| -> Result<u64, String> {
            get(key).map_or(Ok(default), |s| {
                s.trim()
                    .parse::<u64>()
                    .map_err(|_| format!("{key} must be an unsigned integer"))
            })
        };
        p.budget.entries = number("STEVE_DISK_OBSERVE_ENTRIES", p.budget.entries)?;
        p.budget.depth =
            usize::try_from(number("STEVE_DISK_OBSERVE_DEPTH", p.budget.depth as u64)?)
                .map_err(|_| "STEVE_DISK_OBSERVE_DEPTH is not representable")?;
        p.budget.elapsed = Duration::from_millis(number(
            "STEVE_DISK_OBSERVE_MS",
            p.budget.elapsed.as_millis() as u64,
        )?);
        p.keep_rows = number("STEVE_DISK_OBSERVE_ROWS", p.keep_rows)?;
        p.keep_ms = number("STEVE_DISK_OBSERVE_KEEP_MS", p.keep_ms)?;
        p.check().map_err(str::to_string)?;
        Ok(Some(p))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Started,
    Finished,
    Unknown,
}

impl Phase {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Finished => "finished",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    Restarted,
    Stopping,
    Disabled,
    WorkspaceChanged,
    RootUnavailable,
    PhaseAlreadyPassed,
    UnknownExecution,
    WorkerFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum State {
    Pending,
    Sampling,
    Observed(Measurement),
    Unavailable(Unavailable),
}

impl State {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Observed(_) | Self::Unavailable(_))
    }
}

/// Node-local evidence only. `workspace` identifies the Node resource, not the original Hub
/// workspace or project. No trustworthy principal/project/seed context is present yet.
/// Even a complete measurement is asynchronous, permits concurrent writers, and is not pinned
/// to the directory inode at the phase timestamp. Never use it for growth, p90 or cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub interpretation: Interpretation,
    pub operation: OperationId,
    pub execution: ExecutionRef,
    pub workspace: WorkspaceRef,
    pub resource: String,
    pub workspace_incarnation: u64,
    pub workspace_revision: Revision,
    pub node_incarnation: u64,
    pub phase: Phase,
    pub triggered_at: i64,
    pub sampled_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub policy: Policy,
    pub state: State,
    // Physical path stays in Node control-plane storage, never in a Node event or MCP response.
    pub(crate) path: PathBuf,
    pub(crate) requires_root: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpretation {
    /// No baseline, attributable growth, original principal/project or physical inode binding.
    AsynchronousUnattributedPathStock,
}

/// Bounded host-only readback. Skipped counts are cumulative per journal, not per retained page.
/// The cursor orders insertions, not completions: a pending record is updated at the same sequence.
/// Each page has its own read snapshot; rows may change or expire between pages. This is not an
/// incremental completion feed. Restart at zero to refresh retained rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub journal: String,
    pub records: Vec<Record>,
    pub next: Option<u64>,
    pub skipped: u64,
}

pub(crate) struct Observer {
    policy: RwLock<Option<Policy>>,
    wake: Arc<tokio::sync::Notify>,
    started: AtomicBool,
    scan: Mutex<Option<Arc<AtomicBool>>>,
}

impl Observer {
    pub(crate) fn from_env() -> Self {
        let policy = match Policy::configured(|key| std::env::var(key).ok()) {
            Ok(p) => p,
            Err(reason) => {
                tracing::warn!(%reason, "node: disk observations disabled (invalid statistics settings)");
                None
            }
        };
        Self {
            policy: RwLock::new(policy),
            wake: Arc::new(tokio::sync::Notify::new()),
            started: AtomicBool::new(false),
            scan: Mutex::new(None),
        }
    }

    /// Called only inside the transaction recording this phase; never traverses a filesystem.
    pub(crate) fn record(
        &self,
        t: &Tx<'_>,
        operation: &OperationId,
        execution: &ExecutionRef,
        phase: Phase,
        incarnation: u64,
    ) -> ApiResult<()> {
        let Some(policy) = self.policy.read().unwrap().clone() else {
            return Ok(());
        };
        let ExecutionRef::Turn { session, .. } = execution else {
            return Ok(());
        };
        let Some(row) = t.session(session)? else {
            return Ok(());
        };
        let Some(work) = t.workspace(&row.open.workspace.id)? else {
            return Ok(());
        };
        let triggered_at = now();
        let record = Record {
            interpretation: Interpretation::AsynchronousUnattributedPathStock,
            operation: operation.clone(),
            execution: execution.clone(),
            workspace: row.open.workspace.clone(),
            resource: work.resource,
            workspace_incarnation: work.incarnation,
            workspace_revision: work.view.revision,
            node_incarnation: incarnation,
            phase,
            triggered_at,
            sampled_at: None,
            completed_at: (phase == Phase::Unknown).then_some(triggered_at),
            policy,
            state: if phase == Phase::Unknown {
                State::Unavailable(Unavailable::UnknownExecution)
            } else {
                State::Pending
            },
            path: work.path.into(),
            requires_root: matches!(
                work.kind,
                PrepareKind::Existing { .. } | PrepareKind::Worktree { at: Some(_), .. }
            ),
        };
        t.optional_disk_update(|| t.queue_disk_record(&record))
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn cancel_scan(&self) {
        if let Some(cancelled) = &*self.scan.lock().unwrap() {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl LocalNode {
    /// Host-only settings: enabling statistics does not enable any execution or grant a root.
    /// Invalid settings leave the previous policy intact. Pending work is explicitly disabled.
    pub fn configure_disk_observations(&self, policy: Option<Policy>) -> ApiResult<()> {
        if let Some(p) = &policy {
            p.check().map_err(internal)?;
        }
        if self.0.closed.load(Ordering::SeqCst) {
            return Err(internal("node is stopping"));
        }
        let disabled = policy.is_none();
        // The journal serializes policy changes with claiming and registering a scan token.
        // Otherwise a rapid off/on could miss a claimed but not yet registered traversal.
        let saved = self.0.j.tx(|t| {
            *self.0.disk.policy.write().unwrap() = policy;
            if disabled {
                self.0.disk.cancel_scan();
                t.interrupt_disk_records(Unavailable::Disabled, now())?;
            }
            Ok(())
        });
        self.start_disk_observer();
        self.0.disk.wake();
        saved
    }

    /// Host-only current-state pagination of private accounting evidence. `after` is an insertion
    /// sequence; transitions update that same row, so refresh from zero to see later completions.
    /// Never an incremental completion feed or arbitrary filesystem access.
    pub fn disk_observations(&self, after: u64, max: u32) -> ApiResult<Page> {
        self.0.j.read(|t| t.disk_records(self.0.j.id(), after, max))
    }

    pub(crate) fn start_disk_observer(&self) {
        if self.0.disk.policy.read().unwrap().is_none() {
            return;
        }
        if self.0.disk.started.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.spawn_owned(worker(Arc::downgrade(&self.0))) {
            self.0.disk.started.store(false, Ordering::SeqCst);
        }
    }
}

fn same_workspace(t: &Tx<'_>, record: &Record) -> ApiResult<bool> {
    Ok(t.workspace(&record.workspace.id)?.is_some_and(|row| {
        row.incarnation == record.workspace_incarnation
            && row.view.revision == record.workspace_revision
            && row.resource == record.resource
            && std::path::Path::new(&row.path) == record.path
    }))
}

fn phase_current(t: &Tx<'_>, record: &Record) -> ApiResult<bool> {
    if record.phase != Phase::Started {
        return Ok(true);
    }
    let ExecutionRef::Turn { session, .. } = &record.execution else {
        return Err(internal("non-turn disk observation"));
    };
    Ok(
        t.session(session)?.and_then(|r| r.view.active).as_ref() == Some(&record.execution)
            && t.op(&record.operation)?.is_some_and(|op| {
                matches!(
                    op.receipt.state,
                    Observed::Accepted | Observed::Running | Observed::Waiting { .. }
                )
            }),
    )
}

async fn worker(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        if inner.closed.load(Ordering::SeqCst) {
            let _ = inner
                .j
                .tx(|t| t.interrupt_disk_records(Unavailable::Stopping, now()));
            return;
        }
        // Bind notification before reading: a committed enqueue cannot be lost between read/wait.
        let wake = inner.disk.wake.clone();
        let notified = wake.notified();
        let picked = inner.j.tx(|t| {
            if inner.disk.policy.read().unwrap().is_none() {
                t.interrupt_disk_records(Unavailable::Disabled, now())?;
                return Ok(None);
            }
            let Some(mut record) = t.claim_disk_record(now())? else {
                return Ok(None);
            };
            let invalid = if record.node_incarnation != inner.incarnation {
                Some(Unavailable::Restarted)
            } else if !same_workspace(t, &record)? {
                Some(Unavailable::WorkspaceChanged)
            } else if !phase_current(t, &record)? {
                Some(Unavailable::PhaseAlreadyPassed)
            } else {
                None
            };
            if let Some(why) = invalid {
                record.state = State::Unavailable(why);
                record.completed_at = Some(now());
                t.finish_disk_record(&record)?;
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            if !record.state.terminal() {
                *inner.disk.scan.lock().unwrap() = Some(cancelled.clone());
            }
            Ok(Some((record, cancelled)))
        });
        let (mut record, cancelled) = match picked {
            Ok(Some(picked)) => picked,
            Ok(None) => {
                drop(inner);
                notified.await;
                continue;
            }
            Err(error) => {
                // Statistics failure does not invent run admission or free any reservations.
                tracing::warn!(code = ?error.code, "node: disk observation journal unavailable");
                drop(inner);
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                continue;
            }
        };
        if record.state.terminal() {
            continue;
        }
        let roots = inner.roots();
        let sampled_roots = roots.clone();
        let path = record.path.clone();
        let budget = record.policy.budget;
        let requires_root = record.requires_root;
        // Shutdown may race token registration; observe closed after registration too.
        // A disabled token is never reset, including after a rapid off/on.
        if inner.closed.load(Ordering::SeqCst) || inner.disk.policy.read().unwrap().is_none() {
            cancelled.store(true, Ordering::SeqCst);
        }
        // This closure owns no Inner/journal or HomeLock. Shutdown can detach at most one read-only
        // traversal blocked in the kernel; it never starts replacement samplers in this worker.
        let mut task = tokio::task::spawn_blocking(move || {
            if cancelled.load(Ordering::SeqCst) {
                return Err(Unavailable::Disabled);
            }
            crate::workspace::still_valid(&path, &roots, requires_root)
                .map_err(|_| Unavailable::RootUnavailable)?;
            Ok(crate::disk_usage::measure_cancellable(
                &[path],
                budget,
                &|| cancelled.load(Ordering::SeqCst),
            ))
        });
        let sampled = loop {
            tokio::select! {
                result = &mut task => break match result {
                    Ok(result) => result.map(State::Observed)
                        .unwrap_or_else(State::Unavailable),
                    Err(_) => State::Unavailable(Unavailable::WorkerFailed),
                },
                _ = inner.disk.wake.notified() => {
                    if inner.closed.load(Ordering::SeqCst) {
                        inner.disk.cancel_scan();
                        break State::Unavailable(Unavailable::Stopping);
                    }
                }
            }
        };
        record.completed_at = Some(now());
        loop {
            let saved = inner.j.tx(|t| {
                record.state = if inner.closed.load(Ordering::SeqCst) {
                    State::Unavailable(Unavailable::Stopping)
                } else if inner.disk.policy.read().unwrap().is_none() {
                    State::Unavailable(Unavailable::Disabled)
                } else if !Arc::ptr_eq(&sampled_roots, &inner.roots()) {
                    State::Unavailable(Unavailable::RootUnavailable)
                } else if !same_workspace(t, &record)? {
                    State::Unavailable(Unavailable::WorkspaceChanged)
                } else if !phase_current(t, &record)? {
                    State::Unavailable(Unavailable::PhaseAlreadyPassed)
                } else {
                    sampled.clone()
                };
                t.finish_disk_record(&record)
            });
            match saved {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(code = ?error.code, "node: disk observation result not committed");
                    if inner.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    // Retry this same result, never take another scan while it is not recorded.
                    tokio::select! {
                        _ = inner.disk.wake.notified() => {}
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
        }
    }
}
