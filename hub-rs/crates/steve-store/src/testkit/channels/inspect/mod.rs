//! SQL-backed assertions over channel fixtures. The tests name persisted outcomes and bind identifiers.
use rusqlite::{
    Row, params_from_iter,
    types::{FromSql, ValueRef},
};
use serde_json::{Value, json};

use crate::{Result, Store, StoreError};

pub mod audit;
pub mod chan_ext_ws_dead_letters;
pub mod connections;
pub mod conversations;
pub mod decisions;
pub mod delivery;
pub mod digest_entries;
pub mod flows;
pub mod items;
pub mod merge_proposals;
pub mod messages;
pub mod output_channel_preferences;
pub mod people;
pub mod signals;
pub mod spaces;

pub mod feed_entries;
pub mod merges;
pub mod notifications;
pub mod outbox_receipts;
pub mod pragma_foreign_key_check;
pub mod vault_items;

fn cell<T: FromSql, const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<T> {
    store.reader(|c| Ok(c.query_row(sql, params_from_iter(args), |row| row.get(0))?))
}

fn read_cell(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(match row.get_ref(index)? {
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
    })
}

fn json_cell<const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<Value> {
    store.reader(|c| Ok(c.query_row(sql, params_from_iter(args), |row| read_cell(row, 0))?))
}

fn row<const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<Vec<Value>> {
    store.reader(|c| {
        let mut statement = c.prepare(sql)?;
        let columns = statement.column_count();
        Ok(statement.query_row(params_from_iter(args), |row| {
            (0..columns).map(|index| read_cell(row, index)).collect()
        })?)
    })
}

fn rows<const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<Vec<Vec<Value>>> {
    store.reader(|c| {
        let mut statement = c.prepare(sql)?;
        let columns = statement.column_count();
        Ok(statement
            .query_map(params_from_iter(args), |row| {
                (0..columns).map(|index| read_cell(row, index)).collect()
            })?
            .collect::<rusqlite::Result<_>>()?)
    })
}

fn texts<const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<Vec<String>> {
    store.reader(|c| {
        let mut statement = c.prepare(sql)?;
        Ok(statement
            .query_map(params_from_iter(args), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

fn single_row<const N: usize>(store: &Store, sql: &str, args: [&str; N]) -> Result<Vec<Value>> {
    let mut result = rows(store, sql, args)?;
    if result.len() != 1 {
        return Err(StoreError::Rejected(format!(
            "fixture expected one result for {args:?}, got {result:?}"
        )));
    }
    Ok(result.remove(0))
}
