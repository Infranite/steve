//! Vault-owned admission and exit evidence. No Store, sink or Hub owner is retained here.
use super::{UseOutcome, VaultError, VaultResult};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Running,
    Completed,
    Failed,
    Unconfirmed,
}

/// Local work evidence; `Completed` never means a peer acknowledged a notification.
#[derive(Clone)]
pub struct WorkReceipt {
    pub id: u64,
    pub kind: &'static str,
    pub use_id: Option<String>,
    pub completion: Completion,
    pub exited: bool,
    pub reason: Option<&'static str>,
    result: Option<VaultResult<UseOutcome>>,
    error: Option<VaultError>,
}
impl WorkReceipt {
    /// The original request result is retained even if its waiting caller disappeared.
    pub fn result(&self) -> Option<&VaultResult<UseOutcome>> {
        self.result.as_ref()
    }
    /// Original service failure, including cleanup/recording failures.
    pub fn error(&self) -> Option<&VaultError> {
        self.error.as_ref()
    }
}

/// Retained only in Vault's report. Do not log this payload: command output may be sensitive.
#[derive(Clone)]
pub struct UnconfirmedNotification {
    pub work_id: u64,
    pub workspace: String,
    pub item: Option<String>,
    pub session: Option<String>,
    pub text: String,
    /// The sink call returned; the current `SessionSink` interface supplies no peer ACK.
    pub call_returned: bool,
}

/// This proves only the covered local work exited, not Node flush or peer delivery.
#[derive(Clone)]
pub struct DrainReport {
    pub worker_joined: bool,
    pub work: Vec<WorkReceipt>,
    pub unconfirmed_notifications: Vec<UnconfirmedNotification>,
    pub pending_approvals: usize,
}
impl DrainReport {
    pub fn local_work_exited(&self) -> bool {
        self.worker_joined && self.work.iter().all(|w| w.exited)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    New,
    Running,
    Closing,
}
struct State {
    accepting: bool,
    started: bool,
    next: u64,
    active: usize,
    receipts: BTreeMap<u64, WorkReceipt>,
    notices: Vec<UnconfirmedNotification>,
    joined: Option<bool>,
}
pub(super) struct Lifecycle {
    state: Mutex<State>,
    pub changed: Notify,
    phase: watch::Sender<Phase>,
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
impl Lifecycle {
    pub fn new() -> Arc<Self> {
        let (phase, _) = watch::channel(Phase::New);
        Arc::new(Self {
            state: Mutex::new(State {
                accepting: true,
                started: false,
                next: 0,
                active: 0,
                receipts: BTreeMap::new(),
                notices: Vec::new(),
                joined: None,
            }),
            changed: Notify::new(),
            phase,
        })
    }
    pub fn subscribe(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }
    pub fn start(&self) -> bool {
        let mut s = lock(&self.state);
        if !s.accepting || s.started {
            return false;
        }
        s.started = true;
        self.phase.send_replace(Phase::Running);
        true
    }
    pub fn admit(self: &Arc<Self>, kind: &'static str, use_id: Option<String>) -> Option<Work> {
        let mut s = lock(&self.state);
        if !s.accepting {
            return None;
        }
        Some(self.insert(&mut s, kind, use_id))
    }
    fn insert(self: &Arc<Self>, s: &mut State, kind: &'static str, use_id: Option<String>) -> Work {
        let id = s.next;
        s.next += 1;
        s.active += 1;
        s.receipts.insert(
            id,
            WorkReceipt {
                id,
                kind,
                use_id,
                completion: Completion::Running,
                exited: false,
                reason: None,
                result: None,
                error: None,
            },
        );
        Work {
            id,
            owner: self.clone(),
            finished: false,
        }
    }
    pub fn close(&self) {
        let mut s = lock(&self.state);
        s.accepting = false;
        self.phase.send_replace(Phase::Closing);
        drop(s);
        self.changed.notify_waiters();
    }
    pub fn idle_and_closing(&self) -> bool {
        let s = lock(&self.state);
        !s.accepting && s.active == 0
    }
    pub fn task_failed(self: &Arc<Self>) {
        let work = self.insert(&mut lock(&self.state), "owned task", None);
        work.uncertain("owned task panicked or was cancelled");
        drop(work);
    }
    pub fn joined(&self, ok: bool) {
        lock(&self.state).joined = Some(ok);
        self.changed.notify_waiters();
    }
    pub async fn wait_joined(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if lock(&self.state).joined.is_some() {
                return;
            }
            changed.await;
        }
    }
    pub fn report(&self, pending_approvals: usize) -> DrainReport {
        let s = lock(&self.state);
        DrainReport {
            worker_joined: s.joined == Some(true),
            work: s.receipts.values().cloned().collect(),
            unconfirmed_notifications: s.notices.clone(),
            pending_approvals,
        }
    }
}

pub(super) struct Work {
    id: u64,
    owner: Arc<Lifecycle>,
    finished: bool,
}
impl Work {
    /// An admitted operation may still produce necessary children after new admission closes.
    pub fn child(&self, kind: &'static str) -> Self {
        self.owner.insert(&mut lock(&self.owner.state), kind, None)
    }
    pub fn fail(&self, reason: &'static str) {
        let mut s = lock(&self.owner.state);
        let r = s.receipts.get_mut(&self.id).unwrap();
        if r.completion != Completion::Unconfirmed {
            r.completion = Completion::Failed;
        }
        r.reason = Some(reason);
    }
    pub fn uncertain(&self, reason: &'static str) {
        let mut s = lock(&self.owner.state);
        let r = s.receipts.get_mut(&self.id).unwrap();
        r.completion = Completion::Unconfirmed;
        r.reason = Some(reason);
    }
    pub fn fail_error(&self, reason: &'static str, error: &VaultError) {
        self.fail(reason);
        lock(&self.owner.state)
            .receipts
            .get_mut(&self.id)
            .unwrap()
            .error = Some(error.clone());
    }
    pub fn result(&self, result: &VaultResult<UseOutcome>) {
        {
            let mut s = lock(&self.owner.state);
            let r = s.receipts.get_mut(&self.id).unwrap();
            r.result = Some(result.clone());
            if let Ok(UseOutcome::Ran { use_id, .. } | UseOutcome::Asked { use_id, .. }) = result {
                r.use_id = Some(use_id.clone());
            }
        }
        if let Err(e) = result {
            self.fail_error("request failed", e);
        }
        if let Ok(UseOutcome::Ran { out, .. }) = result {
            if !out.ok() {
                self.fail("command failed");
            }
        }
    }
    pub fn notice(&self, ws: &str, item: Option<&str>, session: Option<&str>, text: &str) -> usize {
        let mut s = lock(&self.owner.state);
        let n = s.notices.len();
        s.notices.push(UnconfirmedNotification {
            work_id: self.id,
            workspace: ws.into(),
            item: item.map(Into::into),
            session: session.map(Into::into),
            text: text.into(),
            call_returned: false,
        });
        n
    }
    pub fn notice_returned(&self, index: usize) {
        lock(&self.owner.state).notices[index].call_returned = true;
    }
    pub fn finish(mut self) {
        self.end(true);
        self.finished = true;
    }
    fn end(&self, exited: bool) {
        let mut s = lock(&self.owner.state);
        let r = s.receipts.get_mut(&self.id).unwrap();
        r.exited = exited;
        if !exited {
            r.completion = Completion::Unconfirmed;
            r.reason = Some("work unwound or was cancelled");
        } else if r.completion == Completion::Running {
            r.completion = Completion::Completed;
        }
        s.active -= 1;
        drop(s);
        self.owner.changed.notify_waiters();
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        if !self.finished {
            self.end(false);
        }
    }
}
