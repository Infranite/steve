//! Forces every request and response, including failures, through the JSON boundary.
use crate::*;
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;

/// A transport-free wire adapter. Payload serialization calls LocalPayload::to_json;
/// decoding always produces Payload::Json, even when the caller supplied Local.
pub struct Loopback<S: DomainStore + ?Sized> {
    inner: Arc<S>,
    hello: Hello,
}
impl<S: DomainStore + ?Sized> Loopback<S> {
    pub fn new(inner: Arc<S>) -> Result<Self, Error> {
        Ok(Self {
            hello: wire(inner.hello())?,
            inner,
        })
    }
    pub fn inner(&self) -> &Arc<S> {
        &self.inner
    }
}
fn wire<T: Serialize + DeserializeOwned>(value: &T) -> Result<T, Error> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| error(steve_types::Code::Invalid, "loopback encode failed"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| error(steve_types::Code::Invalid, "loopback decode failed"))
}
#[async_trait]
impl<S: DomainStore + ?Sized> DomainStore for Loopback<S> {
    fn hello(&self) -> &Hello {
        &self.hello
    }
    async fn query(&self, q: AnyQuery) -> Result<AnyOutput, Error> {
        wire(&self.inner.query(wire(&q)?).await)?
    }
    async fn snapshot(&self, domain: Domain, qs: Vec<AnyQuery>) -> Result<Vec<AnyOutput>, Error> {
        wire(&self.inner.snapshot(wire(&domain)?, wire(&qs)?).await)?
    }
    async fn begin_snapshot(&self, domain: Domain, lane: Lane) -> Result<LeaseId, Error> {
        wire(
            &self
                .inner
                .begin_snapshot(wire(&domain)?, wire(&lane)?)
                .await,
        )?
    }
    async fn lease_query(&self, lease: LeaseId, q: AnyQuery) -> Result<AnyOutput, Error> {
        wire(&self.inner.lease_query(wire(&lease)?, wire(&q)?).await)?
    }
    async fn lease_end(&self, lease: LeaseId) -> Result<(), Error> {
        wire(&self.inner.lease_end(wire(&lease)?).await)?
    }
    async fn commit(&self, uow: UnitOfWork) -> Result<Committed, CommitError> {
        wire(
            &self
                .inner
                .commit(wire(&uow).map_err(CommitError::Fatal)?)
                .await,
        )
        .map_err(CommitError::Fatal)?
    }
    async fn commit_many(&self, uows: Vec<UnitOfWork>) -> Vec<Result<Committed, CommitError>> {
        match wire(&uows) {
            Ok(requests) => match wire(&self.inner.commit_many(requests).await) {
                Ok(results) => results,
                Err(error) => vec![Err(CommitError::Fatal(error)); uows.len()],
            },
            Err(_) => {
                // Send each valid contiguous segment through commit_many; an invalid
                // payload must not prevent independent neighbors from being attempted.
                let mut results = Vec::with_capacity(uows.len());
                let mut pending = Vec::new();
                for uow in uows {
                    match wire(&uow) {
                        Ok(unit) => pending.push(unit),
                        Err(error) => {
                            if !pending.is_empty() {
                                let len = pending.len();
                                let response =
                                    self.inner.commit_many(std::mem::take(&mut pending)).await;
                                results.extend(
                                    wire(&response)
                                        .unwrap_or_else(|e| vec![Err(CommitError::Fatal(e)); len]),
                                );
                            }
                            results.push(Err(CommitError::Fatal(error)));
                        }
                    }
                }
                if !pending.is_empty() {
                    let len = pending.len();
                    let response = self.inner.commit_many(pending).await;
                    results.extend(
                        wire(&response).unwrap_or_else(|e| vec![Err(CommitError::Fatal(e)); len]),
                    );
                }
                results
            }
        }
    }

    async fn begin_export(&self, domain: Domain) -> Result<ExportSession, Error> {
        wire(&self.inner.begin_export(wire(&domain)?).await)?
    }
    async fn export(
        &self,
        session: &ExportSession,
        cursor: ExportCursor,
    ) -> Result<ExportBatch, Error> {
        wire(&self.inner.export(&wire(session)?, wire(&cursor)?).await)?
    }
    async fn import(&self, batch: ExportBatch) -> Result<ImportReport, Error> {
        wire(&self.inner.import(wire(&batch)?).await)?
    }
    async fn retention(&self, entity: String, budget: Budget) -> Result<RetentionReport, Error> {
        wire(&self.inner.retention(wire(&entity)?, wire(&budget)?).await)?
    }
}
