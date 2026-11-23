//! Channel fixtures for state transitions and failure injection.
use rusqlite::{Connection, params};

use crate::{Result, Store};

pub mod connections;
pub mod conversations;
pub mod decisions;
pub mod delivery;
pub mod flow;
pub mod harness;
pub mod items;
pub mod knowledge;
pub mod messages;
pub mod outbox;
pub mod people;
pub mod spaces;

fn fixture_write<T>(store: &Store, apply: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    store.write(|tx| apply(tx.conn))
}

fn unchecked_relations<T>(
    store: &Store,
    apply: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    store.write_locked(|c| {
        c.pragma_update(None, "foreign_keys", false)?;
        Ok(())
    })?;
    let result = project_write(store, apply);
    store.write_locked(|c| {
        c.pragma_update(None, "foreign_keys", true)?;
        Ok(())
    })?;
    result
}

use super::project_write;
