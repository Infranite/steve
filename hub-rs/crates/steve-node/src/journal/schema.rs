//! Versioned journal schema and the single receipt/state write path. Legacy upgrades discard
//! command/process ledgers only after all recorded groups are proven gone, preserve pending
//! events, and fence pre-upgrade commands. Initialization and recovery commit together.
use super::*;

/// 4: operation states are checked columns and restart recovery is an indexed bulk update.
pub(super) const VERSION: i64 = 4;
const BASE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS attempts (attempt TEXT PRIMARY KEY, execution TEXT NOT NULL, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS streams (
  stream TEXT PRIMARY KEY, epoch TEXT NOT NULL, produced INTEGER NOT NULL, acknowledged INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events (
  stream TEXT NOT NULL, seq INTEGER NOT NULL, event TEXT NOT NULL, PRIMARY KEY (stream, seq));
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY, resource TEXT NOT NULL, spec TEXT NOT NULL, kind TEXT NOT NULL,
  view TEXT NOT NULL, path TEXT NOT NULL, repo TEXT, incarnation INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY, open TEXT NOT NULL, view TEXT NOT NULL, incarnation INTEGER NOT NULL,
  acp_id TEXT);
CREATE TABLE IF NOT EXISTS waits (
  id TEXT PRIMARY KEY, op TEXT NOT NULL, session TEXT NOT NULL, request TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS reservations (key TEXT PRIMARY KEY, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS command_specs (op TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS harness_setups (harness TEXT PRIMARY KEY, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS artifact_grants (
  attempt TEXT PRIMARY KEY, workspace TEXT NOT NULL, body TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS artifact_grants_workspace ON artifact_grants(workspace);
CREATE TABLE IF NOT EXISTS answers (
  op TEXT PRIMARY KEY, permission TEXT NOT NULL, resolution TEXT NOT NULL);
"#;

// Share the current process shape between fresh creation and the one-time legacy reset.
const PROCS_V4: &str = r#"CREATE TABLE IF NOT EXISTS procs (
  session TEXT PRIMARY KEY, pgid INTEGER NOT NULL, generation INTEGER NOT NULL,
  incarnation INTEGER NOT NULL, meter TEXT NOT NULL, reports INTEGER NOT NULL DEFAULT 0,
  started INTEGER);
"#;

// Keep this versioned definition when a later fresh schema advances to v5.
const OPS_V4: &str = r#"CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY, method TEXT NOT NULL, digest TEXT NOT NULL, receipt TEXT NOT NULL,
  execution TEXT, workspace TEXT, session TEXT, cancel INTEGER NOT NULL DEFAULT 0,
  accepted INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('accepted','running','finished','unknown')));
"#;

/// Unknown still needs reconciliation; Waiting is an active (running) operation.
const OPEN_OPS: &str = "state IN ('accepted','running','unknown')";
static RESTART_OPS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "UPDATE ops SET state=?1, receipt=json_set(receipt,'$.state',json(?2)) WHERE {OPEN_OPS} AND state<>'unknown'"
    )
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpState {
    Accepted,
    Running,
    Finished,
    Unknown,
}
impl OpState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Unknown => "unknown",
        }
    }
}

/// Local extension: the wire's Observed type belongs to steve-node-api and does not change.
pub(super) trait ObservedState {
    fn state(&self) -> OpState;
}
impl ObservedState for Observed {
    fn state(&self) -> OpState {
        match self {
            Self::Accepted => OpState::Accepted,
            Self::Running | Self::Waiting { .. } => OpState::Running,
            Self::Finished { .. } => OpState::Finished,
            Self::Unknown { .. } => OpState::Unknown,
        }
    }
}

pub(super) enum ReceiptWrite<'a> {
    Accept {
        id: &'a OperationId,
        op: &'a Op,
        session: Option<&'a SessionId>,
        at: i64,
    },
    Update {
        id: &'a OperationId,
        receipt: &'a Receipt,
    },
    Restart,
}

impl Tx<'_> {
    fn table_exists(&self, table: &str) -> ApiResult<bool> {
        self.c()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |r| r.get(0),
            )
            .map_err(internal)
    }

    /// `before_upgrade`: see [`super::Journal::open_upgrading`].
    pub(super) fn initialize(
        &self,
        version: i64,
        before_upgrade: &mut dyn FnMut() -> ApiResult<()>,
    ) -> ApiResult<()> {
        if version == 0 && !self.table_exists("ops")? {
            self.c().execute_batch(BASE_SCHEMA).map_err(internal)?;
            self.c().execute_batch(PROCS_V4).map_err(internal)?;
            self.c().execute_batch(OPS_V4).map_err(internal)?;
            self.create_v4_op_indexes()?;
            self.create_identity()?;
        } else {
            // Versions 0-2 predate journal identity; they all used the same receipt-only ops.
            if version < 3 {
                self.migrate_to_v3()?;
            }
            if version < 4 {
                self.migrate_v3_to_v4(before_upgrade)?;
            }
        }
        if version < VERSION {
            self.c()
                .pragma_update(None, "user_version", VERSION)
                .map_err(internal)?;
        }
        Ok(())
    }

    fn create_identity(&self) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES ('journal_id', random())",
                [],
            )
            .map_err(internal)?;
        Ok(())
    }

    fn migrate_to_v3(&self) -> ApiResult<()> {
        // Complete the legacy auxiliary tables once, before migrating the existing ops below.
        self.c().execute_batch(BASE_SCHEMA).map_err(internal)?;
        self.c().execute_batch(PROCS_V4).map_err(internal)?;
        self.create_identity()?;
        self.c()
            .pragma_update(None, "user_version", 3)
            .map_err(internal)
    }

    fn migrate_v3_to_v4(&self, before: &mut dyn FnMut() -> ApiResult<()>) -> ApiResult<()> {
        self.require_legacy_processes_gone()?;
        // past this point no older release can open the journal
        before()?;
        // The old command ledger is intentionally discarded. Keep payloads the Hub has not
        // acknowledged: stream/event shapes did not change and their positions remain valid.
        // Clear whole dependent tables, including orphan rows whose op/attempt is already gone.
        self.c()
            .execute_batch(
                "DELETE FROM artifact_grants;
                 DELETE FROM attempts;
                 DELETE FROM command_specs;
                 DELETE FROM reservations;
                 DELETE FROM answers;
                 DELETE FROM waits;
                 DELETE FROM harness_setups;",
            )
            .map_err(internal)?;
        self.c().execute(
            "DELETE FROM streams WHERE (stream GLOB 'op:*' OR stream GLOB 'job:*') AND produced=acknowledged AND NOT EXISTS (SELECT 1 FROM events WHERE events.stream=streams.stream AND seq>acknowledged)",
            [],
        ).map_err(internal)?;
        self.c()
            .execute_batch("DROP TABLE ops;")
            .map_err(internal)?;
        self.c().execute_batch(OPS_V4).map_err(internal)?;
        self.create_v4_op_indexes()?;
        // Dropping deduplication records must never make an old envelope executable again.
        self.c().execute(
            "INSERT INTO meta (key,value) VALUES ('accept_floor',?1) ON CONFLICT(key) DO UPDATE SET value=MAX(value,excluded.value)",
            [crate::now()],
        ).map_err(internal)?;
        self.c()
            .execute(
                "DELETE FROM meta WHERE key IN ('prune_cursor','prune_after')",
                [],
            )
            .map_err(internal)?;
        // All old groups are proven gone; discard their records without backfilling identity.
        self.c()
            .execute_batch("DROP TABLE IF EXISTS procs;")
            .map_err(internal)?;
        self.c().execute_batch(PROCS_V4).map_err(internal)?;
        self.c()
            .execute_batch("DROP TABLE IF EXISTS disk_observations;")
            .map_err(internal)?;
        Ok(())
    }

    fn require_legacy_processes_gone(&self) -> ApiResult<()> {
        // Legacy tables may lack started. Group absence (ESRCH) is the only proof needed;
        // alive, unsignalable, reused or invalid identities must retain their durable records.
        let groups = self.rows("SELECT pgid FROM procs", [], |r| r.get::<_, i64>(0))?;
        if groups.into_iter().any(|pgid| {
            !(2..=i32::MAX as i64).contains(&pgid) || !crate::procs::state(pgid, None).gone()
        }) {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "stop the old Node's processes before upgrading the journal",
            ));
        }
        Ok(())
    }

    fn create_v4_op_indexes(&self) -> ApiResult<()> {
        self.c().execute_batch(&format!("CREATE INDEX ops_session ON ops(session); CREATE INDEX ops_open ON ops(state) WHERE {OPEN_OPS};")).map_err(internal)
    }

    /// Every receipt/state write (acceptance, transitions and bulk restart) is here.
    pub(super) fn write_receipt(&self, write: ReceiptWrite<'_>) -> ApiResult<()> {
        match write {
            ReceiptWrite::Accept {
                id,
                op,
                session,
                at,
            } => {
                self.c().execute("INSERT INTO ops (id, method, digest, receipt, state, execution, workspace, session, cancel, accepted)
                    VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9)", params![id.as_str(), op.method, op.digest.as_str(), json(&op.receipt), op.receipt.state.state().as_str(), op.execution.as_ref().map(json), op.workspace.as_ref().map(json), session.map(SessionId::as_str), at]).map_err(internal)?;
            }
            ReceiptWrite::Update { id, receipt } => {
                self.c()
                    .execute(
                        "UPDATE ops SET receipt=?2, state=?3 WHERE id=?1",
                        params![id.as_str(), json(receipt), receipt.state.state().as_str()],
                    )
                    .map_err(internal)?;
            }
            ReceiptWrite::Restart => {
                let unknown = Observed::Unknown {
                    reason: steve_node_api::RESTARTED.into(),
                };
                // Preserve existing Unknown reasons, as before; only live states lose their run.
                self.c()
                    .execute(
                        &RESTART_OPS_SQL,
                        params![unknown.state().as_str(), json(&unknown)],
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
