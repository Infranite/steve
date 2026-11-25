//! The sync job: one sync at a time per mailbox; folders and their scope modes
//! (`source_settings.scopes`, new folders get the defaults); the adapter fills a [`StoreSink`]; folder
//! state, cursor and health are written back; then one SSE per changed thread. Each new mail's event
//! was put in the outbox with the mail ([`crate::outbox`]).
//!
//! In the background ([`Mail::run_jobs`]) every mailbox has a runner: a watcher (IMAP IDLE) that
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
use crate::repo;
use crate::store::{Mailbox, OffloadSink};
use crate::{Mail, lock};
use steve_store::sources::ScopeEntry;

/// Polling interval of a mailbox (and of folders other than the watched inbox).
pub const POLL: Duration = Duration::from_secs(120);
/// Reconnect backoff after a failure: doubles from `BACKOFF_MIN` to `BACKOFF_MAX`.
pub const BACKOFF_MIN: Duration = Duration::from_secs(5);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How often the job looks for new or removed mailboxes, and runs retention.
const SCAN: Duration = Duration::from_secs(60);
const RETENTION_EVERY: Duration = Duration::from_secs(3600);
/// The longest one sync may take (a first sync of 500 messages takes a few minutes at most).
const SYNC_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// Bounded mail adapter label for metrics.
pub(crate) fn mail_adapter_label(adapter: &str) -> &'static str {
    match adapter {
        "imap" => "imap",
        "gmail" => "gmail",
        "outlook" => "outlook",
        "demo" => "demo",
        _ => "other",
    }
}

fn record_mail_sync(
    adapter: &'static str,
    result: &'static str,
    elapsed_secs: f64,
    fetched: usize,
) {
    thread_local! {
        static SYNC_TOTAL: std::cell::RefCell<HashMap<(&'static str, &'static str), metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
        static SYNC_DURATION: std::cell::RefCell<HashMap<&'static str, metrics::Histogram>> =
            std::cell::RefCell::new(HashMap::new());
        static MESSAGES_SYNCED: std::cell::RefCell<HashMap<&'static str, metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
    }
    SYNC_TOTAL.with(|c| {
        c.borrow_mut()
            .entry((adapter, result))
            .or_insert_with(|| {
                metrics::counter!(
                    "steve_mail_sync_total",
                    "adapter" => adapter,
                    "result" => result
                )
            })
            .increment(1);
    });
    SYNC_DURATION.with(|c| {
        c.borrow_mut()
            .entry(adapter)
            .or_insert_with(
                || metrics::histogram!("steve_mail_sync_duration_seconds", "adapter" => adapter),
            )
            .record(elapsed_secs);
    });
    if fetched > 0 {
        MESSAGES_SYNCED.with(|c| {
            c.borrow_mut()
                .entry(adapter)
                .or_insert_with(
                    || metrics::counter!("steve_mail_messages_synced_total", "adapter" => adapter),
                )
                .increment(fetched as u64);
        });
    }
}

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

/// The named sync future owns all mailbox watches and polling tasks, even while routes retain Mail.
struct SyncScope(Mail);
impl Drop for SyncScope {
    fn drop(&mut self) {
        self.0
            .0
            .sync_stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        lock(&self.0.0.runners).clear();
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

    /// [`Mail::adapter_of`] for a change on the server (archive, flags), checked now: refused while
    /// the connection is paused, to be authorized again or revoked.
    pub(crate) fn adapter_for_write(
        &self,
        ws: &str,
        id: &str,
    ) -> MailResult<(Arc<dyn MailAdapter>, ConnCx)> {
        let cx = self.0.conns.cx_for(ws, id, None).map_err(MailError::from)?;
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
        let adapter_label = mail_adapter_label(cx.adapter());
        let started = std::time::Instant::now();
        let out = self.sync_now_inner(ws, connection_id, adapter, cx).await;
        let (result_label, fetched) = match &out {
            Ok(report) => ("ok", report.fetched),
            Err(MailError::Adapter(AdapterError::Auth(_))) => ("auth", 0),
            Err(MailError::Adapter(AdapterError::Transient(_))) => ("transient", 0),
            Err(_) => ("error", 0),
        };
        record_mail_sync(
            adapter_label,
            result_label,
            started.elapsed().as_secs_f64(),
            fetched,
        );
        out
    }

    async fn sync_now_inner(
        &self,
        ws: &str,
        connection_id: &str,
        adapter: Arc<dyn MailAdapter>,
        cx: ConnCx,
    ) -> MailResult<SyncReport> {
        // one provider session lists the folders and then syncs them
        let (folders, session) = match adapter.open_sync(&cx).await {
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
        let prepared = match tokio::task::spawn_blocking(move || {
            m.prepare(&ws_s, &conn_s, &folders, now, &checks)
        })
        .await
        .map_err(|e| MailError::Other(format!("task failed: {e}")))
        .and_then(|r| r)
        {
            Ok(Some(p)) => p,
            Ok(None) => {
                session.close().await;
                return Ok(SyncReport::default());
            }
            Err(e) => {
                session.close().await;
                return Err(e);
            }
        };
        let (mut state, mailbox) = prepared;
        let home = self.0.cfg.home().to_path_buf();
        let mut sink = OffloadSink::new(self.0.m.store.clone(), home, mailbox, now);
        // a backstop over the per-command timeouts: the mailbox lock is always released
        let res = tokio::time::timeout(SYNC_DEADLINE, session.sync(&mut state, &mut sink))
            .await
            .unwrap_or_else(|_| Err(AdapterError::Transient("邮件同步太久没有完成".into())));
        let changed = std::mem::take(&mut sink.changed);
        drop(sink);
        // What got stored is kept, also after an error midway. Adapters must leave cursors at
        // the successfully stored prefix: a failed put_batch may already have stored some mail,
        // which is safe to fetch again (the store dedupes), but must never be skipped here.
        {
            let mut checks = lock(&self.0.flag_checks);
            for f in &state.folders {
                if let Some(t) = f.flags_checked {
                    checks.insert((connection_id.to_string(), f.ext_id.clone()), t);
                }
            }
        }
        let (m, ws_s, conn_s, ok) = (
            self.clone(),
            ws.to_string(),
            connection_id.to_string(),
            res.is_ok(),
        );
        tokio::task::spawn_blocking(move || -> MailResult<()> {
            m.0.m.store.write(|tx| -> MailResult<()> {
                for f in &state.folders {
                    repo::save_folder(tx, &conn_s, f)?;
                }
                steve_store::sources::set_cursor(tx, &conn_s, state.cursor.as_deref())?;
                if ok && let Err(e) = m.0.conns.touch_sync(&ws_s, &conn_s) {
                    tracing::warn!(connection = %conn_s, error = %e, "touch_sync failed");
                }
                for t in &changed {
                    crate::events::emit_thread(tx, &ws_s, t);
                }
                Ok(())
            })?;
            Ok(())
        })
        .await
        .map_err(|e| MailError::Other(format!("task failed: {e}")))??;
        match res {
            Ok(r) => {
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
        store.write(|tx| -> MailResult<()> {
            repo::ensure_settings(tx, ws, conn, now)?;
            for f in folders {
                repo::upsert_folder(tx, ws, conn, f)?;
            }
            let (_, mut scopes, _) = steve_store::sources::source(tx, conn)?.unwrap_or_default();
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
            if scopes.len() != before || steve_store::sources::source(tx, conn)?.is_none() {
                steve_store::sources::set_scopes(tx, ws, conn, &scopes, now)?;
            }
            Ok(())
        })?;
        store.read(|c| {
            let (paused, scopes, cursor) =
                steve_store::sources::source(c, conn)?.unwrap_or_default();
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

    /// Wakes a mailbox's runner (`POST /mail/accounts/:id/sync`); without a runner, syncs in a task.
    pub fn sync_soon(&self, ws: &str, connection_id: &str) {
        if self
            .0
            .sync_stopped
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
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
        if self
            .0
            .sync_stopped
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
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
            .read(|r| steve_store::connections::Connections::new(r).live_sources())?;
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

    /// The background job as a supervised periodic [`steve_runtime::Task`]: runners per mailbox
    /// (reconciled every minute) and hourly retention.
    pub fn jobs_task(self) -> steve_runtime::Task {
        let clock = self.0.m.clock.clone();
        let scope = Arc::new(SyncScope(self.clone()));
        let last_retention = Arc::new(std::sync::Mutex::new(None::<tokio::time::Instant>));
        steve_runtime::every_after(
            "mail.sync",
            clock,
            SCAN,
            steve_runtime::FirstRun::Immediate,
            move || {
                let _scope = scope.clone();
                let m = self.clone();
                let last_retention = last_retention.clone();
                async move {
                    if let Err(e) = m.reconcile() {
                        tracing::warn!(error = %e, "mail runners");
                    }
                    let due = last_retention
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_none_or(|t| t.elapsed() >= RETENTION_EVERY);
                    if due {
                        m.run_retention().await;
                        *last_retention.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(tokio::time::Instant::now());
                    }
                }
            },
        )
    }

    /// The background job: runners per mailbox (reconciled every minute) and hourly retention.
    pub async fn run_jobs(self) {
        self.jobs_task().await;
    }

    async fn run_retention(&self) {
        let mm = self.clone();
        match tokio::task::spawn_blocking(move || mm.retention()).await {
            Ok(Err(e)) => tracing::warn!(error = %e, "mail retention"),
            Err(e) => tracing::warn!(error = %e, "mail retention task"),
            Ok(Ok(_)) => {}
        }
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

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_the_sync_future_stops_all_mailbox_tasks_even_when_mail_is_retained() {
        use steve_modules::accounts::connections::{Connections, ConnectionsConfig};
        use steve_modules::vault::{Vault, keystore::FileKeyStore};
        let env = steve_modules::testkit::TestEnv::new(|_| {});
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::from_env(|_| None),
        )
        .build();
        let mail = Mail::builder(
            env.modules.clone(),
            conns,
            crate::MailConfig::new(env.dir.path().join("mail")),
        )
        .build();
        let owner = tokio::spawn(mail.clone().run_jobs());
        // The first poll creates the ownership scope and reaches retention's blocking await.
        tokio::task::yield_now().await;
        let (a, ar) = tokio::sync::oneshot::channel::<()>();
        let (b, br) = tokio::sync::oneshot::channel::<()>();
        let tasks = [a, b]
            .into_iter()
            .map(|tx| {
                tokio::spawn(async move {
                    let _tx = tx;
                    std::future::pending::<()>().await;
                })
            })
            .collect();
        let (wake, _rx) = mpsc::channel(1);
        lock(&mail.0.runners).insert(
            "mailbox".into(),
            Runner {
                stop: CancellationToken::new(),
                wake,
                tasks,
            },
        );
        owner.abort();
        let _ = owner.await;
        assert!(
            mail.0
                .sync_stopped
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(lock(&mail.0.runners).is_empty());
        tokio::time::timeout(Duration::from_secs(5), async {
            assert!(ar.await.is_err());
            assert!(br.await.is_err());
        })
        .await
        .unwrap();
        mail.start("default", "late");
        assert!(
            lock(&mail.0.runners).is_empty(),
            "a stopped owner cannot restart mailbox runners"
        );
    }
}
