//! Process-local event attachments. Publication occurs under writer ownership, after COMMIT.
use crate::{
    exec::{Completion, Executor},
    uow::SqliteBatchRunner,
};
use rusqlite::Connection;
use steve_storage_api::{CommitError, Committed, Rev, UnitOfWork};

pub struct Attached<E> {
    pub unit: UnitOfWork,
    pub events: Vec<E>,
}
impl<E> Attached<E> {
    pub fn new(unit: UnitOfWork, events: Vec<E>) -> Self {
        Self { unit, events }
    }
}
pub trait Publisher<E>: Send + Sync + 'static {
    fn publish(&self, rev: Rev, events: Vec<E>);
}
impl<E, F: Fn(Rev, Vec<E>) + Send + Sync + 'static> Publisher<E> for F {
    fn publish(&self, rev: Rev, events: Vec<E>) {
        self(rev, events)
    }
}
/// Returned by apply_in; only the outer transaction owner may release it after COMMIT.
/// Dropping it on root rollback discards events. Never serialize this attachment.
pub struct PendingEvents<E> {
    pub rev: Rev,
    pub events: Vec<E>,
}
impl<E: 'static> PendingEvents<E> {
    pub fn publish(self, publisher: &dyn Publisher<E>) {
        publisher.publish(self.rev, self.events);
    }
}
pub fn commit_with_events<E: Send + 'static>(
    executor: &Executor<Connection, SqliteBatchRunner<E>>,
    unit: UnitOfWork,
    events: Vec<E>,
) -> Result<Completion<Committed>, CommitError> {
    executor.submit(unit.lane, Attached { unit, events })
}
