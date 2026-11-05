//! A durable admission gate shared by workers using the same Store.
//! Permits span external work, never SQLite transactions. Closing the gate stops new work;
//! sealing it with zero permits also stops authenticated continuations at the ready boundary.
use crate::{Result, Store};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    active: bool,
    /// Once ready is published, even an old running-session token cannot admit more work.
    sealed: bool,
    running: BTreeMap<String, usize>,
}
#[derive(Default)]
pub(crate) struct Gate {
    state: Mutex<State>,
    wake: tokio::sync::Notify,
}
pub struct Permit {
    gate: Arc<Gate>,
    kind: String,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut s = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = s.running.get_mut(&self.kind) {
            *n -= 1;
            if *n == 0 {
                s.running.remove(&self.kind);
            }
        }
    }
}
impl Gate {
    pub(crate) fn load(c: &rusqlite::Connection) -> Result<Self> {
        let active = c.query_row("SELECT value FROM settings WHERE workspace_id='__system__' AND key='upgrade.maintenance'", [], |r| r.get::<_,String>(0)).optional()?.is_some_and(|s| s == "true");
        Ok(Self {
            state: Mutex::new(State {
                active,
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}
impl Store {
    pub fn maintenance(&self) -> bool {
        self.inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
    }
    pub fn maintenance_set(&self, on: bool) -> Result<()> {
        let mut g = self
            .inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.with_conn(|c| { c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES('__system__','upgrade.maintenance',?,?) ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value,updated=excluded.updated", rusqlite::params![if on {"true"} else {"false"},crate::now()])?; Ok(()) })?;
        // Retrying POST is idempotent: only DELETE may reopen a sealed drain.
        if !on || !g.active {
            g.sealed = false;
        }
        g.active = on;
        drop(g);
        self.inner.maintenance.wake.notify_waiters();
        Ok(())
    }
    /// Atomically close even continuation admission at the final ready boundary. The caller
    /// must first confirm its adapters are gone. Empty counters and sealing share the same lock
    /// with both normal and continuation admission, so no late token can race ready=true.
    pub fn maintenance_seal(&self) -> bool {
        let mut g = self
            .inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !g.active || !g.running.is_empty() {
            return false;
        }
        g.sealed = true;
        true
    }
    pub fn try_activity(&self, kind: &str) -> Option<Permit> {
        let mut g = self
            .inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if g.sealed
            || (g.active
                && !ADMITTED_STORE
                    .try_with(|id| *id == Arc::as_ptr(&self.inner) as usize)
                    .unwrap_or(false))
        {
            return None;
        }
        *g.running.entry(kind.into()).or_default() += 1;
        Some(Permit {
            gate: self.inner.maintenance.clone(),
            kind: kind.into(),
        })
    }
    pub async fn activity(&self, kind: &str) -> Permit {
        loop {
            let wake = self.inner.maintenance.wake.notified();
            if let Some(p) = self.try_activity(kind) {
                return p;
            }
            wake.await;
        }
    }
    pub fn activities(&self) -> BTreeMap<String, usize> {
        self.inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .clone()
    }
}

// Scope is entered only by a worker which already owns an admission permit. Its nested
// operations may finish while maintenance is on; unrelated tasks never inherit the scope.
tokio::task_local! { static ADMITTED_STORE: usize; }
impl Store {
    pub async fn admitted<F: std::future::Future>(&self, future: F) -> F::Output {
        ADMITTED_STORE
            .scope(Arc::as_ptr(&self.inner) as usize, future)
            .await
    }
    /// A separately authenticated RPC belonging to an already-running session.
    /// Callers must verify that session; this is not a general bypass for new work.
    pub fn continuing_activity(
        &self,
        kind: &str,
        still_running: impl FnOnce() -> bool,
    ) -> Option<Permit> {
        // The check may read the database, so it runs before the gate lock, which every permit and
        // drop waits on. Sealing still decides under the lock: it needs `running` empty, and this
        // admission counts itself under the same lock.
        let ok = still_running();
        let mut g = self
            .inner
            .maintenance
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if g.sealed || !ok {
            return None;
        }
        *g.running.entry(kind.into()).or_default() += 1;
        Some(Permit {
            gate: self.inner.maintenance.clone(),
            kind: kind.into(),
        })
    }
}
