//! Single-writer revisions. Persist reservation before issuance, never refund issued numbers.
use crate::domains::{failure, sql_error};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::sync::Mutex;
use steve_storage_api::{CommitError, Rev};

pub const BLOCK: u64 = 4096;
#[derive(Default)]
struct Range {
    next: u64,
    end: u64,
}
pub struct RevAllocator(Mutex<Range>);
impl RevAllocator {
    /// One allocator per physical writer. Reopening deliberately skips the previous reserved tail.
    pub fn open(c: &Connection) -> Result<Self, CommitError> {
        let end = upper(c)?;
        Ok(Self(Mutex::new(Range { next: end + 1, end })))
    }
    /// Call before BEGIN, including before a legacy root transaction. Reserving a batch up front
    /// ensures a unit never attempts an independent reservation transaction inside its savepoint.
    pub fn issue(&self, c: &Connection, count: usize) -> Result<Vec<Rev>, CommitError> {
        if !c.is_autocommit() {
            return Err(failure(
                steve_types::Code::Invalid,
                "reserve revisions before opening a transaction",
            ));
        }
        if count == 0 {
            return Ok(vec![]);
        }
        let count = u64::try_from(count)
            .map_err(|_| failure(steve_types::Code::Exhausted, "revision range exhausted"))?;
        let mut r = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let available = r.end.saturating_add(1).saturating_sub(r.next);
        if available < count {
            let tx =
                Transaction::new_unchecked(c, TransactionBehavior::Immediate).map_err(sql_error)?;
            let previous = upper(&tx)?.max(r.end);
            let size = count
                .div_ceil(BLOCK)
                .checked_mul(BLOCK)
                .ok_or_else(|| failure(steve_types::Code::Exhausted, "revision range exhausted"))?;
            let end = previous
                .checked_add(size)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| failure(steve_types::Code::Exhausted, "revision range exhausted"))?;
            tx.execute("INSERT INTO meta(domain,key,value) VALUES('core','rev_reserved',?1) ON CONFLICT(domain,key) DO UPDATE SET value=excluded.value",[end.to_string()]).map_err(sql_error)?;
            tx.commit().map_err(sql_error)?;
            r.next = previous + 1;
            r.end = end;
        }
        let start = r.next;
        r.next += count;
        Ok((start..start + count).map(Rev).collect())
    }
}
fn upper(c: &Connection) -> Result<u64, CommitError> {
    let value: Option<String> = c
        .query_row(
            "SELECT value FROM meta WHERE domain='core' AND key='rev_reserved'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    match value {
        None => Ok(0),
        Some(s) => s
            .parse::<u64>()
            .ok()
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| {
                failure(
                    steve_types::Code::Incompatible,
                    "invalid persisted revision reservation",
                )
            }),
    }
}
