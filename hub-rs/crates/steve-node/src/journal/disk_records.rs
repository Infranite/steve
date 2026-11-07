//! Additive private statistics, independent of command bodies, event delivery and run admission.

use super::*;
use crate::disk_observation::{Page, Record, State, Unavailable};

#[cfg(test)]
mod tests;

impl Tx<'_> {
    /// Keep optional statistics from rolling back the business phase that records them. SQLite
    /// errors such as RAISE(ROLLBACK) or a full journal can end the outer transaction: those must
    /// still fail the caller. Only a successful rollback of this savepoint permits degradation.
    pub(crate) fn optional_disk_update(
        &self,
        update: impl FnOnce() -> ApiResult<()>,
    ) -> ApiResult<()> {
        let c = self.c();
        if c.is_autocommit() {
            return Err(internal("disk statistics require the business transaction"));
        }
        c.execute_batch("SAVEPOINT steve_optional_disk_statistics")
            .map_err(internal)?;
        match update() {
            Ok(()) => c
                .execute_batch("RELEASE steve_optional_disk_statistics")
                .map_err(internal),
            Err(error) => {
                if c.is_autocommit() {
                    return Err(error);
                }
                c.execute_batch(
                    "ROLLBACK TO steve_optional_disk_statistics; RELEASE steve_optional_disk_statistics",
                )
                .map_err(internal)?;
                if c.is_autocommit() {
                    return Err(error);
                }
                tracing::warn!(code = ?error.code, "node: optional disk statistics were not recorded");
                Ok(())
            }
        }
    }

    pub(crate) fn queue_disk_record(&self, record: &Record) -> ApiResult<()> {
        let exists: bool = self
            .c()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM disk_observations WHERE op=?1 AND phase=?2)",
                params![record.operation.as_str(), record.phase.key()],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if exists {
            return Ok(());
        }
        let cutoff = record
            .triggered_at
            .saturating_sub(record.policy.keep_ms as i64);
        self.c()
            .execute(
                "DELETE FROM disk_observations WHERE completed IS NOT NULL AND completed < ?1",
                [cutoff],
            )
            .map_err(internal)?;
        let count: i64 = self
            .c()
            .query_row("SELECT count(*) FROM disk_observations", [], |r| r.get(0))
            .map_err(internal)?;
        let needed = (count - record.policy.keep_rows as i64 + 1).max(0);
        if needed > 0 {
            self.c()
                .execute(
                    "DELETE FROM disk_observations WHERE seq IN
                     (SELECT seq FROM disk_observations WHERE completed IS NOT NULL ORDER BY seq LIMIT ?1)",
                    [needed],
                )
                .map_err(internal)?;
        }
        let count: i64 = self
            .c()
            .query_row("SELECT count(*) FROM disk_observations", [], |r| r.get(0))
            .map_err(internal)?;
        if count >= record.policy.keep_rows as i64 {
            // A stuck scan or a backlog must not make execution wait or grow an unbounded queue.
            self.c()
                .execute(
                    "INSERT INTO meta(key,value) VALUES('disk_observations_skipped',1)
                     ON CONFLICT(key) DO UPDATE SET value=CASE WHEN value<9223372036854775807 THEN value+1 ELSE value END",
                    [],
                )
                .map_err(internal)?;
            return Ok(());
        }
        self.c()
            .execute(
                "INSERT INTO disk_observations
                 (op,phase,node_incarnation,triggered,completed,stage,body) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    record.operation.as_str(),
                    record.phase.key(),
                    i64::try_from(record.node_incarnation).map_err(internal)?,
                    record.triggered_at,
                    record.completed_at,
                    if record.state.terminal() { "done" } else { "pending" },
                    json(record),
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub(crate) fn claim_disk_record(&self, at: i64) -> ApiResult<Option<Record>> {
        let body: Option<String> = self
            .c()
            .query_row(
                "SELECT body FROM disk_observations WHERE stage='pending' ORDER BY seq LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        let Some(body) = body else { return Ok(None) };
        let mut record: Record = from(&body).map_err(internal)?;
        record.sampled_at = Some(at);
        record.state = State::Sampling;
        self.c()
            .execute(
                "UPDATE disk_observations SET stage='sampling',body=?3 WHERE op=?1 AND phase=?2 AND stage='pending'",
                params![record.operation.as_str(), record.phase.key(), json(&record)],
            )
            .map_err(internal)?;
        Ok(Some(record))
    }

    pub(crate) fn finish_disk_record(&self, record: &Record) -> ApiResult<()> {
        self.c()
            .execute(
                "UPDATE disk_observations SET stage='done',completed=?4,body=?5
                 WHERE op=?1 AND phase=?2 AND node_incarnation=?3 AND stage='sampling'",
                params![
                    record.operation.as_str(),
                    record.phase.key(),
                    i64::try_from(record.node_incarnation).map_err(internal)?,
                    record.completed_at,
                    json(record),
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub(crate) fn interrupt_disk_records(&self, why: Unavailable, at: i64) -> ApiResult<()> {
        let records: Vec<String> = self.rows(
            "SELECT body FROM disk_observations WHERE stage IN ('pending','sampling')",
            [],
            |r| r.get(0),
        )?;
        for body in records {
            let mut record: Record = from(&body).map_err(internal)?;
            record.state = State::Unavailable(why);
            record.completed_at = Some(at);
            self.c()
                .execute(
                    "UPDATE disk_observations SET stage='done',completed=?3,body=?4 WHERE op=?1 AND phase=?2",
                    params![record.operation.as_str(), record.phase.key(), at, json(&record)],
                )
                .map_err(internal)?;
        }
        Ok(())
    }

    pub(crate) fn disk_records(&self, journal: &str, after: u64, max: u32) -> ApiResult<Page> {
        let skipped: Option<i64> = self
            .c()
            .query_row(
                "SELECT value FROM meta WHERE key='disk_observations_skipped'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        let after = i64::try_from(after).map_err(internal)?;
        // A caller chooses a bounded page; zero requests no rows and never advances its cursor.
        let mut records: Vec<(i64, Record)> = self.rows(
            "SELECT seq,body FROM disk_observations WHERE seq>?1 ORDER BY seq LIMIT ?2",
            params![after, i64::from(max)],
            |r| Ok((r.get(0)?, from(&r.get::<_, String>(1)?)?)),
        )?;
        let next = records.last().map(|(seq, _)| *seq as u64);
        Ok(Page {
            journal: journal.into(),
            records: records.drain(..).map(|(_, record)| record).collect(),
            next,
            skipped: skipped.unwrap_or(0) as u64,
        })
    }
}
