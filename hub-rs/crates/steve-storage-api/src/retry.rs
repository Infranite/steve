//! UNSTABLE until the first real caller lands. One conflict retry policy for all hosts.
use crate::{CommitError, DomainStore, Error, UnitOfWork, uow::IdempotencyKey};
use std::{future::Future, pin::Pin};
use steve_types::{Code, NOW_MAX_ATTEMPTS, Retry};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// A recomputed commit, no work remaining, or a state-machine rejection.
pub enum Attempt<T> {
    Commit(UnitOfWork, T),
    Done(T),
    Reject(Error),
}
/// UNSTABLE. Re-read and recompute after Conflict, without sleeping. NOW_MAX_ATTEMPTS
/// is the shared immediate-attempt budget; exhaustion is conflict/later. Every commit
/// attempt must preserve domain and the complete intent key, including issued_at.
pub async fn retry_on_conflict<'a, T>(
    store: &dyn DomainStore,
    mut attempt: impl FnMut() -> BoxFuture<'a, Result<Attempt<T>, Error>>,
) -> Result<T, Error> {
    let mut identity: Option<(crate::Domain, IdempotencyKey)> = None;
    for _ in 0..NOW_MAX_ATTEMPTS {
        match attempt().await? {
            Attempt::Done(value) => return Ok(value),
            Attempt::Reject(e) => {
                return Err(Error {
                    code: Code::Conflict,
                    retry: Retry::Never,
                    ..e
                });
            }
            Attempt::Commit(unit, value) => {
                let next = (unit.domain, unit.id.clone());
                if identity.as_ref().is_some_and(|old| old != &next) {
                    return Err(crate::error(
                        Code::Invalid,
                        "retry changed its intent key or domain",
                    ));
                }
                identity = Some(next);
                match store.commit(unit).await {
                    Ok(_) => return Ok(value),
                    Err(CommitError::Conflict(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Err(Error {
        retry: Retry::Later { after_ms: None },
        ..crate::error(Code::Conflict, "immediate conflict attempts exhausted")
    })
}
