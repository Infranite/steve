//! One Vault-owned worker owns every accepted operation and its children until actual exit.
use super::lifecycle::{Phase, Work};
use super::{
    Job, Queued, SecretUses, UseCx, UseOutcome, UseRequest, VaultError, VaultResult, lock,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};

pub(super) fn closed() -> VaultError {
    crate::ApiError::conflict("vault secret-use service is closing").into()
}
impl SecretUses {
    pub(super) fn ensure_worker(&self) {
        let Some(rx) = lock(&self.0.rx).take() else {
            return;
        };
        let me = self.clone();
        let evidence = self.0.lifecycle.clone();
        let worker = tokio::spawn(async move { me.run_worker(rx).await });
        // This monitor owns no Vault/Store/sink. Only an actual join sets the exit evidence.
        tokio::spawn(async move {
            evidence.joined(worker.await.is_ok());
        });
    }
    async fn run_worker(self, mut rx: mpsc::UnboundedReceiver<Queued>) {
        let mut children = JoinSet::new();
        let me = self.clone();
        children.spawn(async move { me.hourly_loop().await });
        loop {
            let changed = self.0.lifecycle.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.lifecycle.idle_and_closing() {
                break;
            }
            tokio::select! {
                message = rx.recv() => {
                    let Some(queued) = message else { break; };
                    let me = self.clone();
                    children.spawn(steve_i18n::scope(queued.locale, async move { me.run_queued(queued).await }));
                }
                _ = changed => {}
                result = children.join_next(), if !children.is_empty() => {
                    if matches!(result, Some(Err(_))) { self.0.lifecycle.task_failed(); }
                }
            }
        }
        // Never abort: a completed outer worker must not leave a child or tick behind.
        while let Some(result) = children.join_next().await {
            if result.is_err() {
                self.0.lifecycle.task_failed();
            }
        }
    }
    async fn hourly_loop(self) {
        let mut phase = self.0.lifecycle.subscribe();
        while *phase.borrow_and_update() == Phase::New {
            if phase.changed().await.is_err() {
                return;
            }
        }
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(3600));
        timer.tick().await;
        loop {
            if *phase.borrow_and_update() == Phase::Closing {
                return;
            }
            tokio::select! {
                _ = phase.changed() => {}
                _ = timer.tick() => {
                    let Some(work) = self.0.lifecycle.admit("hourly", None) else { return; };
                    if let Err(e) = self.hourly_inner(&work).await {
                        work.fail_error("hourly failed", &e);
                        tracing::error!(error = %e, "vault: hourly cleanup failed");
                    }
                    work.finish();
                }
            }
        }
    }
    async fn run_queued(&self, queued: Queued) {
        let Queued { job, work, .. } = queued;
        match job {
            Job::Request(cx, request, reply) => {
                let result = self.request_inner(&cx, *request, &work).await;
                work.result(&result);
                let _ = reply.send(result);
            }
            Job::Hourly(reply) => {
                let result = self.hourly_inner(&work).await;
                if let Err(e) = &result {
                    work.fail_error("hourly failed", e);
                }
                let _ = reply.send(result);
            }
            Job::Notes(notes) => self.notify(notes, &work).await,
            job => self.job(job, &work).await,
        }
        work.finish();
    }
    pub(super) fn enqueue(&self, job: Job, work: Work) -> VaultResult<()> {
        if let Err(error) = self.0.jobs.send(Queued {
            job,
            work,
            locale: steve_i18n::current(),
        }) {
            let Queued { job, work, .. } = error.0;
            if let Job::Notes(notes) = job {
                for (ws, item, session, text) in notes {
                    work.notice(&ws, item.as_deref(), session.as_deref(), &text);
                }
            }
            work.uncertain("worker did not accept queued work");
            return Err(closed());
        }
        self.ensure_worker();
        Ok(())
    }
    pub(super) async fn submit_request(
        &self,
        cx: &UseCx,
        request: UseRequest,
    ) -> VaultResult<UseOutcome> {
        let work = self.0.lifecycle.admit("request", None).ok_or_else(closed)?;
        let (send, receive) = oneshot::channel();
        self.enqueue(
            Job::Request(Box::new(cx.clone()), Box::new(request), send),
            work,
        )?;
        receive
            .await
            .map_err(|_| VaultError::internal("vault request ended without a result"))?
    }
    pub(super) async fn submit_hourly(&self) -> VaultResult<()> {
        let work = self.0.lifecycle.admit("hourly", None).ok_or_else(closed)?;
        let (send, receive) = oneshot::channel();
        self.enqueue(Job::Hourly(send), work)?;
        receive
            .await
            .map_err(|_| VaultError::internal("vault hourly ended without a result"))?
    }
}
