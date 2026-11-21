//! One retention slice, shared by legacy tables and domain entity adapters.
//! The caller owns the write grant. Cursors always name the last inspected candidate,
//! including protected/non-selected rows; a time-limited slice never skips prefetched rows.
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, time::Instant};
pub use steve_storage_api::{
    entity::{Cell, Row},
    retention::{Action, Budget, KeepFrom, RetentionPolicy, Select},
};

pub mod policies;

pub const MAX_SLICE_ROWS: u32 = 500;
pub const SLICE_TARGET_MS: u32 = 20;
pub const DEFAULT_BUDGET: Budget = Budget {
    rows: MAX_SLICE_ROWS,
    target_ms: SLICE_TARGET_MS,
};

fn effective_budget<P: Policy + ?Sized>(p: &P, budget: Budget) -> rusqlite::Result<Budget> {
    if budget.rows == 0
        || budget.target_ms == 0
        || p.definition().budget.rows == 0
        || p.definition().budget.target_ms == 0
    {
        return Err(invalid("retention budget must be positive"));
    }
    Ok(Budget {
        rows: budget
            .rows
            .min(p.definition().budget.rows)
            .min(MAX_SLICE_ROWS),
        target_ms: budget.target_ms.min(p.definition().budget.target_ms),
    })
}

pub struct Window<C> {
    pub rows: Vec<(Row, C)>,
    pub more: bool,
}
#[derive(Debug, Clone)]
pub struct SliceReport<C> {
    pub affected: u64,
    pub inspected: u32,
    pub elapsed_ms: u64,
    pub overruns: u32,
    pub failures: u32,
    pub next: Option<C>,
}
impl<C> SliceReport<C> {
    pub fn more(&self) -> bool {
        self.next.is_some()
    }
}
pub fn value<'a>(row: &'a Row, field: &str) -> Option<&'a Cell> {
    row.0.iter().find(|(name, _)| name == field).map(|(_, v)| v)
}
pub fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.into())
}
/// An indexed window and domain hooks. Implementations must keep cursor keys immutable.
/// MaxRows/Bytes need an indexed domain predicate; age/terminal selection is shared here.
pub trait Policy {
    type Cursor: Clone;
    fn definition(&self) -> &RetentionPolicy;
    fn window(
        &self,
        c: &Connection,
        after: Option<&Self::Cursor>,
        limit: u32,
    ) -> rusqlite::Result<Window<Self::Cursor>>;
    /// Override to resolve scoped settings; absent a resolver, use the contract fallback.
    fn keep_ms(&self, _: &Connection, _: &Row) -> rusqlite::Result<Option<u64>> {
        match self.definition().keep_from {
            KeepFrom::Fixed { millis } => Ok(Some(millis)),
            KeepFrom::Setting { default_ms, .. } => Ok(Some(default_ms)),
        }
    }
    fn overflow(&self, _: &Connection, _: &Row) -> rusqlite::Result<bool> {
        Err(invalid("MaxRows/Bytes domain predicate required"))
    }
    fn protect(&self, _: &Connection, _: &Row) -> rusqlite::Result<bool> {
        if self.definition().protect.is_some() {
            Err(invalid("protection handler required"))
        } else {
            Ok(false)
        }
    }
    /// Stable identity for before hooks; adapters encode composite keys unambiguously.
    fn primary_key(&self, _: &Row) -> rusqlite::Result<String> {
        Err(invalid("before hook requires a primary key handler"))
    }
    fn ready(&self, _: &Row) -> rusqlite::Result<bool> {
        if self.definition().before.is_some() {
            Err(invalid(
                "before hook must be prepared outside the write grant",
            ))
        } else {
            Ok(true)
        }
    }
    fn preparation_failures(&self) -> u32 {
        0
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64>;
}
/// External work is deliberately separate from run_slice. Release the reader/write grant
/// before external I/O; pure payload preparation may share the candidate read snapshot.
/// Failed rows are untouched; changed rows lose their preparation.
pub struct BeforeCandidates(Vec<Row>);
/// Read and filter under a read snapshot; release it before external I/O callbacks.
/// Pure preparation can use that same snapshot. Fresh/protected rows must never reach
/// an external resource-deletion callback.
pub fn before_candidates<P: Policy>(
    c: &Connection,
    p: &P,
    now: i64,
    budget: Budget,
    after: Option<&P::Cursor>,
) -> rusqlite::Result<BeforeCandidates> {
    let budget = effective_budget(p, budget)?;
    let window = p.window(c, after, budget.rows)?;
    let mut rows = vec![];
    for (r, _) in window.rows {
        if is_selected(c, p, &r, now)? && !p.protect(c, &r)? {
            rows.push(r);
        }
    }
    Ok(BeforeCandidates(rows))
}
pub struct Prepared<'a, P: Policy> {
    pub policy: &'a P,
    ready: HashSet<(String, [u8; 32])>,
    pub failed: u32,
}
pub fn prepare<'a, P: Policy>(
    policy: &'a P,
    candidates: BeforeCandidates,
    mut before: impl FnMut(&Row) -> rusqlite::Result<bool>,
) -> Prepared<'a, P> {
    let mut prepared = Prepared {
        policy,
        ready: HashSet::new(),
        failed: 0,
    };
    for row in candidates.0 {
        let result: rusqlite::Result<_> = (|| {
            let key = policy.primary_key(&row)?;
            let fingerprint = row_fingerprint(&row)?;
            Ok(before(&row)?.then_some((key, fingerprint)))
        })();
        match result {
            Ok(Some(key)) => {
                prepared.ready.insert(key);
            }
            _ => prepared.failed += 1,
        }
    }
    prepared
}
fn row_fingerprint(row: &Row) -> rusqlite::Result<[u8; 32]> {
    let bytes = serde_json::to_vec(row)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(Sha256::digest(bytes).into())
}
impl<P: Policy> Policy for Prepared<'_, P> {
    type Cursor = P::Cursor;
    fn definition(&self) -> &RetentionPolicy {
        self.policy.definition()
    }
    fn window(
        &self,
        c: &Connection,
        a: Option<&Self::Cursor>,
        n: u32,
    ) -> rusqlite::Result<Window<Self::Cursor>> {
        self.policy.window(c, a, n)
    }
    fn keep_ms(&self, c: &Connection, r: &Row) -> rusqlite::Result<Option<u64>> {
        self.policy.keep_ms(c, r)
    }
    fn overflow(&self, c: &Connection, r: &Row) -> rusqlite::Result<bool> {
        self.policy.overflow(c, r)
    }
    fn protect(&self, c: &Connection, r: &Row) -> rusqlite::Result<bool> {
        self.policy.protect(c, r)
    }
    fn primary_key(&self, r: &Row) -> rusqlite::Result<String> {
        self.policy.primary_key(r)
    }
    fn ready(&self, r: &Row) -> rusqlite::Result<bool> {
        Ok(self
            .ready
            .contains(&(self.primary_key(r)?, row_fingerprint(r)?)))
    }
    fn preparation_failures(&self) -> u32 {
        self.failed
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        self.policy.apply(c, r)
    }
}
/// Strict age comparison is the contract's `now - at > keep`, with no integer overflow.
pub fn selected(row: &Row, select: &Select, now: i64, keep: Option<u64>) -> bool {
    let old = |f: &str| matches!((value(row,f),keep),(Some(Cell::Millis(at)),Some(ms)) if i128::from(now)-i128::from(*at)>i128::from(ms));
    match select {
        Select::MaxAge { field, .. } => old(field),
        Select::Terminal {
            state, done, since, ..
        } => value(row, state).is_some_and(|v| done.contains(v)) && old(since),
        _ => false,
    }
}
fn is_selected<P: Policy + ?Sized>(
    c: &Connection,
    p: &P,
    row: &Row,
    now: i64,
) -> rusqlite::Result<bool> {
    match p.definition().select {
        Select::MaxRows { .. } | Select::Bytes { .. } => p.overflow(c, row),
        _ => Ok(selected(
            row,
            &p.definition().select,
            now,
            p.keep_ms(c, row)?,
        )),
    }
}
struct Scope<'a> {
    c: &'a Connection,
    name: &'static str,
    done: bool,
    root: bool,
}
impl<'a> Scope<'a> {
    fn new(c: &'a Connection, name: &'static str) -> rusqlite::Result<Self> {
        let root = c.is_autocommit();
        if root {
            c.execute_batch("BEGIN IMMEDIATE")?;
        }
        if let Err(e) = c.execute_batch(&format!("SAVEPOINT {name}")) {
            if root {
                let _ = c.execute_batch("ROLLBACK");
            }
            return Err(e);
        }
        Ok(Self {
            c,
            name,
            done: false,
            root,
        })
    }
    fn rollback(mut self) -> rusqlite::Result<()> {
        self.c
            .execute_batch(&format!("ROLLBACK TO {0}; RELEASE {0}", self.name))?;
        if self.root {
            self.c.execute_batch("ROLLBACK")?;
        }
        self.done = true;
        Ok(())
    }
    fn commit(mut self) -> rusqlite::Result<()> {
        self.c.execute_batch(&format!("RELEASE {}", self.name))?;
        if self.root {
            self.c.execute_batch("COMMIT")?;
        }
        self.done = true;
        Ok(())
    }
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = if self.root {
                self.c.execute_batch("ROLLBACK")
            } else {
                self.c
                    .execute_batch(&format!("ROLLBACK TO {0}; RELEASE {0}", self.name))
            };
        }
    }
}

pub fn run_slice<P: Policy + ?Sized>(
    c: &Connection,
    p: &P,
    now: i64,
    budget: Budget,
    after: Option<&P::Cursor>,
) -> rusqlite::Result<SliceReport<P::Cursor>> {
    let start = Instant::now();
    run_slice_timed(c, p, now, budget, after, || {
        start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    })
}
/// Injectable monotonic elapsed time for deterministic budget tests; wall time is `now`.
pub fn run_slice_timed<P: Policy + ?Sized>(
    c: &Connection,
    p: &P,
    now: i64,
    budget: Budget,
    after: Option<&P::Cursor>,
    mut elapsed: impl FnMut() -> u64,
) -> rusqlite::Result<SliceReport<P::Cursor>> {
    let budget = effective_budget(p, budget)?;
    let scope = Scope::new(c, "steve_retention_slice")?;
    let window = p.window(c, after, budget.rows)?;
    if window.rows.len() > budget.rows as usize {
        return Err(invalid("retention window exceeds budget"));
    }
    let mut report = SliceReport {
        affected: 0,
        inspected: 0,
        elapsed_ms: 0,
        overruns: 0,
        failures: p.preparation_failures(),
        next: None,
    };
    let mut last = None;
    for (row, cursor) in &window.rows {
        if report.inspected > 0 && elapsed() >= u64::from(budget.target_ms) {
            break;
        }
        report.inspected += 1;
        last = Some(cursor.clone());
        let action = Scope::new(c, "steve_retention_row")?;
        let result = (|| {
            let eligible = is_selected(c, p, row, now)?;
            if !eligible || !p.ready(row)? || p.protect(c, row)? {
                return Ok(0);
            }
            if let Action::Purge { fields } = &p.definition().action
                && fields
                    .iter()
                    .all(|f| value(row, f).is_none_or(|v| *v == Cell::Null))
            {
                return Ok(0);
            }
            p.apply(c, row)
        })();
        match result {
            Ok(n) => {
                action.commit()?;
                report.affected += n;
            }
            Err(error) => {
                tracing::warn!(entity = %p.definition().entity, error = %error, "retention row failed; retrying next sweep");
                action.rollback()?;
                report.failures += 1;
            }
        }
    }
    if (report.inspected as usize) < window.rows.len() || window.more {
        report.next = last;
    }
    scope.commit()?;
    report.elapsed_ms = elapsed();
    report.overruns = u32::from(report.elapsed_ms > u64::from(budget.target_ms));
    let entity = p.definition().entity.clone();
    metrics::counter!("steve_retention_affected_total","entity"=>entity.clone())
        .increment(report.affected);
    metrics::histogram!("steve_retention_slice_seconds","entity"=>entity.clone())
        .record(report.elapsed_ms as f64 / 1000.0);
    metrics::counter!("steve_retention_overruns_total","entity"=>entity)
        .increment(u64::from(report.overruns));
    Ok(report)
}
