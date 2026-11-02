//! The sync job (email.md §3.2): one sync at a time per mailbox; folders and their scope modes
//! (`source_settings.scopes`, new folders get the defaults); the adapter fills a [`StoreSink`]; folder
//! state, cursor and health are written back; then the events go out and one SSE per changed thread.
//!
//! In the background ([`Mail::spawn_jobs`]) every mailbox has a runner: a watcher (IMAP IDLE) that
//! wakes the sync on new mail, and the sync loop itself polling every 2 minutes. Failures back off
//! from 5 s to 5 min. Runners start and stop with the connections (checked every minute).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use steve_modules::accounts::connections::{ConnCx, Health};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapter::{FolderState, MailAdapter, ScopeMode, SyncReport, SyncState};
use crate::error::{AdapterError, MailError, MailResult};
use crate::repo::{self, ScopeEntry};
use crate::store::{Mailbox, StoreSink};
use crate::{Mail, lock};

/// Polling interval of a mailbox (and of folders other than the watched inbox).
pub const POLL: Duration = Duration::from_secs(120);
/// Reconnect backoff after a failure: doubles from `BACKOFF_MIN` to `BACKOFF_MAX`.
pub const BACKOFF_MIN: Duration = Duration::from_secs(5);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How often the job looks for new or removed mailboxes, and runs retention.
const SCAN: Duration = Duration::from_secs(60);
const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// The next backoff delay.
pub fn backoff(prev: Option<Duration>) -> Duration {
    match prev {
        None => BACKOFF_MIN,
        Some(d) => (d * 2).min(BACKOFF_MAX),
    }
}

/// A running mailbox: stop token, wake channel and its tasks.
pub(crate) struct Runner {
    stop: CancellationToken,
    wake: mpsc::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop.cancel();
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Mail {
    fn conn_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.0.locks).entry(id.into()).or_default().clone()
    }

    pub(crate) fn adapter_of(
        &self,
        ws: &str,
        id: &str,
    ) -> MailResult<(Arc<dyn MailAdapter>, ConnCx)> {
        let cx = self.0.conns.cx(ws, id).map_err(MailError::from)?;
        let a = self
            .adapter(cx.adapter())
            .ok_or_else(|| MailError::Other(format!("no mail adapter {}", cx.adapter())))?;
        Ok((a, cx))
    }

    /// Syncs one mailbox now and sends its events. Errors also go to the connection's health.
    pub async fn sync_now(&self, ws: &str, connection_id: &str) -> MailResult<SyncReport> {
        let l = self.conn_lock(connection_id);
        let _g = l.lock().await;
        let (adapter, cx) = self.adapter_of(ws, connection_id)?;
        let folders = match adapter.folders(&cx).await {
            Ok(f) => f,
            Err(e) => {
                cx.report(health_of(&e)).await;
                return Err(e.into());
            }
        };
        let now = self.now();
        let (ws_s, conn_s) = (ws.to_string(), connection_id.to_string());
        let checks: HashMap<String, i64> = lock(&self.0.flag_checks)
            .iter()
            .filter(|((c, _), _)| c == connection_id)
            .map(|((_, f), t)| (f.clone(), *t))
            .collect();
        let m = self.clone();
        let prepared =
            tokio::task::spawn_blocking(move || m.prepare(&ws_s, &conn_s, &folders, now, &checks))
                .await
                .map_err(|e| MailError::Other(format!("task failed: {e}")))??;
        let Some((mut state, mailbox)) = prepared else {
            return Ok(SyncReport::default());
        };
        let home = self.0.cfg.home().to_path_buf();
        let store = self.0.m.store.clone();
        let mut sink = StoreSink::new(&store, &home, &mailbox, now);
        let res = adapter.sync(&cx, &mut state, &mut sink).await;
        let (changed, pending) = (
            std::mem::take(&mut sink.changed),
            std::mem::take(&mut sink.events),
        );
        drop(sink);
        // what got stored is kept, also after an error midway
        {
            let mut checks = lock(&self.0.flag_checks);
            for f in &state.folders {
                if let Some(t) = f.flags_checked {
                    checks.insert((connection_id.to_string(), f.ext_id.clone()), t);
                }
            }
        }
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            for f in &state.folders {
                repo::save_folder(tx, connection_id, f)?;
            }
            repo::set_cursor(tx, connection_id, state.cursor.as_deref())
        })?;
        self.deliver(ws, connection_id, pending).await;
        for t in &changed {
            self.0.notify.thread(ws, t);
        }
        match res {
            Ok(r) => {
                if let Err(e) = self.0.conns.touch_sync(ws, connection_id) {
                    tracing::warn!(connection = connection_id, error = %e, "touch_sync failed");
                }
                cx.report(Health::Ok).await;
                Ok(r)
            }
            Err(e) => {
                tracing::info!(connection = connection_id, error = %e, "mail sync failed");
                cx.report(health_of(&e)).await;
                Err(e.into())
            }
        }
    }

    /// Folders and scopes in the database; the state to sync. `None` when the source is paused.
    fn prepare(
        &self,
        ws: &str,
        conn: &str,
        folders: &[crate::adapter::FolderInfo],
        now: i64,
        checks: &HashMap<String, i64>,
    ) -> MailResult<Option<(SyncState, Mailbox)>> {
        let store = &self.0.m.store;
        store.transaction(|tx| -> MailResult<()> {
            repo::ensure_settings(tx, ws, conn, now)?;
            for f in folders {
                repo::upsert_folder(tx, ws, conn, f)?;
            }
            let (_, mut scopes, _) = repo::source(tx, conn)?.unwrap_or_default();
            let before = scopes.len();
            for f in folders {
                if !scopes.iter().any(|s| s.ext_id == f.ext_id) {
                    scopes.push(ScopeEntry {
                        ext_id: f.ext_id.clone(),
                        name: f.name.clone(),
                        mode: ScopeMode::default_for(&f.role),
                    });
                }
            }
            if scopes.len() != before || repo::source(tx, conn)?.is_none() {
                repo::set_scopes(tx, ws, conn, &scopes, now)?;
            }
            Ok(())
        })?;
        store.reader(|c| {
            let (paused, scopes, cursor) = repo::source(c, conn)?.unwrap_or_default();
            if paused {
                return Ok(None);
            }
            let settings = repo::settings(c, conn)?.unwrap_or_default();
            let listed: Vec<&str> = folders.iter().map(|f| f.ext_id.as_str()).collect();
            let folders: Vec<FolderState> = repo::folders(c, conn)?
                .into_iter()
                .filter(|f| listed.contains(&f.ext_id.as_str()))
                .filter_map(|mut f| {
                    let mode = scopes.iter().find(|s| s.ext_id == f.ext_id)?.mode;
                    (mode != ScopeMode::Ignore).then(|| {
                        f.mode = mode;
                        f.flags_checked = checks.get(&f.ext_id).copied();
                        f
                    })
                })
                .collect();
            let mailbox = Mailbox::load(c, ws, conn)?;
            Ok(Some((
                SyncState {
                    folders,
                    cursor,
                    sync_days: settings.sync_days,
                    now,
                },
                mailbox,
            )))
        })
    }

    async fn deliver(&self, ws: &str, conn: &str, pending: Vec<crate::store::PendingEvent>) {
        for p in pending {
            match self.0.events.deliver(ws, conn, p.event, p.how).await {
                Ok(Some(id)) => {
                    if let Err(e) = self
                        .0
                        .m
                        .store
                        .transaction(|tx| repo::set_event(tx, &p.message_id, &id))
                    {
                        tracing::warn!(error = %e, "could not link the mail to its event");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(connection = conn, error = %e, "mail event not delivered"),
            }
        }
    }

    /// Wakes a mailbox's runner (`POST /mail/accounts/:id/sync`); without a runner, syncs in a task.
    pub fn sync_soon(&self, ws: &str, connection_id: &str) {
        if let Some(r) = lock(&self.0.runners).get(connection_id) {
            let _ = r.wake.try_send(());
            return;
        }
        let (m, ws, id) = (self.clone(), ws.to_string(), connection_id.to_string());
        tokio::spawn(async move {
            if let Err(e) = m.sync_now(&ws, &id).await {
                tracing::info!(connection = %id, error = %e, "mail sync failed");
            }
        });
    }

    /// Starts the runner of a mailbox if it has none.
    pub fn start(&self, ws: &str, connection_id: &str) {
        let mut runners = lock(&self.0.runners);
        if runners.contains_key(connection_id) {
            return;
        }
        let stop = CancellationToken::new();
        let (wake, mut rx) = mpsc::channel::<()>(4);
        let mut tasks = Vec::new();
        // watcher
        {
            let (m, ws, id, stop, wake) = (
                self.clone(),
                ws.to_string(),
                connection_id.to_string(),
                stop.clone(),
                wake.clone(),
            );
            tasks.push(tokio::spawn(async move {
                let mut delay = None;
                loop {
                    let started = tokio::time::Instant::now();
                    let r = match m.adapter_of(&ws, &id) {
                        Ok((a, cx)) => a.watch(&cx, stop.clone(), wake.clone()).await,
                        Err(e) => Err(AdapterError::Config(e.to_string())),
                    };
                    if stop.is_cancelled() || matches!(r, Err(AdapterError::Unsupported)) {
                        return;
                    }
                    if let Err(e) = &r {
                        tracing::debug!(connection = %id, error = %e, "mail watch ended");
                    }
                    // a watch that held for a while was healthy: start the backoff over
                    if started.elapsed() > Duration::from_secs(60) {
                        delay = None;
                    }
                    let d = backoff(delay);
                    delay = Some(d);
                    tokio::select! {
                        _ = tokio::time::sleep(d) => {}
                        _ = stop.cancelled() => return,
                    }
                    // reconnected: catch up on what arrived meanwhile
                    let _ = wake.try_send(());
                }
            }));
        }
        // sync loop
        {
            let (m, ws, id, stop) = (
                self.clone(),
                ws.to_string(),
                connection_id.to_string(),
                stop.clone(),
            );
            tasks.push(tokio::spawn(async move {
                let mut delay = None;
                loop {
                    let wait = match m.sync_now(&ws, &id).await {
                        Ok(_) => {
                            delay = None;
                            POLL
                        }
                        Err(_) => {
                            let d = backoff(delay);
                            delay = Some(d);
                            d
                        }
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        w = rx.recv() => if w.is_none() { return },
                        _ = stop.cancelled() => return,
                    }
                }
            }));
        }
        runners.insert(connection_id.into(), Runner { stop, wake, tasks });
    }

    /// Stops a mailbox's runner.
    pub fn stop(&self, connection_id: &str) {
        lock(&self.0.runners).remove(connection_id);
    }

    /// The mailboxes that should run: live connections of a registered mail adapter with `mail.read`.
    fn wanted(&self) -> MailResult<Vec<(String, String)>> {
        let rows = self
            .0
            .m
            .store
            .reader(steve_modules::accounts::connections::repo::live)?;
        Ok(rows
            .into_iter()
            .filter(|r| self.0.adapters.contains_key(&r.adapter))
            .filter(|r| r.caps.iter().any(|c| c == "mail.read"))
            .filter(|r| matches!(r.status.as_str(), "ok" | "error"))
            .map(|r| (r.workspace_id, r.id))
            .collect())
    }

    /// Starts and stops runners to match the connections. Returns how many run.
    pub fn reconcile(&self) -> MailResult<usize> {
        let wanted = self.wanted()?;
        let gone: Vec<String> = lock(&self.0.runners)
            .keys()
            .filter(|k| !wanted.iter().any(|(_, id)| id == *k))
            .cloned()
            .collect();
        for id in gone {
            self.stop(&id);
        }
        for (ws, id) in &wanted {
            self.start(ws, id);
        }
        Ok(wanted.len())
    }

    /// The background job: runners per mailbox (reconciled every minute) and hourly retention.
    pub fn spawn_jobs(&self) -> JoinHandle<()> {
        let m = self.clone();
        tokio::spawn(async move {
            let mut last_retention: Option<tokio::time::Instant> = None;
            loop {
                if let Err(e) = m.reconcile() {
                    tracing::warn!(error = %e, "mail runners");
                }
                if last_retention.is_none_or(|t| t.elapsed() >= RETENTION_EVERY) {
                    let mm = m.clone();
                    match tokio::task::spawn_blocking(move || mm.retention()).await {
                        Ok(Err(e)) => tracing::warn!(error = %e, "mail retention"),
                        Err(e) => tracing::warn!(error = %e, "mail retention task"),
                        Ok(Ok(_)) => {}
                    }
                    last_retention = Some(tokio::time::Instant::now());
                }
                tokio::time::sleep(SCAN).await;
            }
        })
    }
}

fn health_of(e: &AdapterError) -> Health {
    match e {
        AdapterError::Auth(m) => Health::Auth(m.clone()),
        e => Health::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_five_minutes() {
        let mut d = None;
        let mut seen = Vec::new();
        for _ in 0..9 {
            let n = backoff(d);
            seen.push(n.as_secs());
            d = Some(n);
        }
        assert_eq!(seen, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
    }
}
