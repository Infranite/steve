//! Real comms fixture shared by operation tests and the backend conformance hook.
#![allow(dead_code, unexpected_cfgs)]
use rusqlite::{Connection, params};
use steve_store_sqlite::{
    digest::{RECEIPTS_TABLE, Source},
    schema,
};

pub const TABLES: &[&str] = &[
    "spaces",
    "mail_threads",
    Source::ChanMessage.shape().table,
    Source::MailMessage.shape().table,
    "digest_pending",
    "digest_pending_convs",
    RECEIPTS_TABLE,
    "digest_cursors",
];
pub fn install(c: &Connection) -> rusqlite::Result<()> {
    // Extract the real table/index definitions using SQLite itself, not a second
    // handwritten source schema. The reference connection is never a writer file.
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(schema::COMMS)?;
    // Keep only the id from schema/comms/0001_baseline.sql's connections parent.
    // Its account_id references accounts, whose workspace_id reaches CORE.workspaces
    // (the accounts -> workspaces cross-domain FK is listed in schema/debt_comms.txt).
    // The fixture keeps only the comms-local id needed by mail_threads/messages;
    // it must not manufacture core tables to satisfy that legacy dependency.
    c.execute_batch(
        "CREATE TABLE connections(id TEXT PRIMARY KEY); INSERT INTO connections VALUES('fixture');",
    )?;
    for name in TABLES {
        let sql: String = reference.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?",
            [name],
            |r| r.get(0),
        )?;
        c.execute_batch(&sql)?;
        let indexes: Vec<String> = reference
            .prepare(
                "SELECT sql FROM sqlite_schema
                 WHERE type='index' AND tbl_name=? AND sql IS NOT NULL",
            )?
            .query_map([name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for sql in indexes {
            c.execute_batch(&sql)?;
        }
    }
    // An additive future scheduling column proves projection UPSERTs preserve
    // fields they do not own. No rev columns or generic entity codecs are added.
    c.execute_batch(
        "ALTER TABLE digest_pending_convs ADD COLUMN fixture_marker INTEGER NOT NULL DEFAULT 0",
    )?;
    c.pragma_update(None, "foreign_keys", true)?;
    assert_eq!(
        c.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))?,
        1
    );
    let triggers: i64 = c.query_row(
        "SELECT count(*) FROM main.sqlite_schema WHERE type='trigger'
         AND tbl_name IN (?1,?2,'digest_pending','digest_pending_convs',?3)",
        [
            Source::ChanMessage.shape().table,
            Source::MailMessage.shape().table,
            RECEIPTS_TABLE,
        ],
        |r| r.get(0),
    )?;
    assert_eq!(
        triggers, 0,
        "registered operations alone maintain the digest fixture"
    );
    Ok(())
}
pub fn receipt(c: &Connection, workspace: &str, conv: &str, id: &str) -> rusqlite::Result<()> {
    c.execute(
        &format!("INSERT INTO {RECEIPTS_TABLE}(workspace_id,conv,message_id,seq,created) VALUES(?,?,?,1,0)"),
        params![workspace, conv, id],
    )?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn put(
    c: &Connection,
    source: Source,
    id: &str,
    ws: &str,
    conv: &str,
    status: &str,
    at: i64,
    insert_only: bool,
) -> rusqlite::Result<()> {
    let shape = source.shape();
    let table = shape.table;
    let column = shape.conv_column;
    let time = shape.time_column;
    let local = conv
        .strip_prefix(shape.prefix)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let (extra_columns, extra_values) = match source {
        Source::ChanMessage => {
            c.execute(
                "INSERT OR IGNORE INTO spaces
                 (id,workspace_id,system_id,kind,external_id,name,created,updated)
                 VALUES(?1,?2,'fixture','group',?1,'fixture',0,0)",
                params![local, ws],
            )?;
            ("direction", "'in'")
        }
        Source::MailMessage => {
            c.execute(
                "INSERT OR IGNORE INTO mail_threads
                 (id,workspace_id,connection_id,thread_key,subject,last_at,created_by,created,updated)
                 VALUES(?1,?2,'fixture',?1,'fixture',0,'soft-owner',0,0)",
                params![local, ws],
            )?;
            (
                "connection_id,ext_id,direction,from_addr,created",
                "'fixture',?1,'in','fixture@example.invalid',0",
            )
        }
    };
    let upsert = if insert_only {
        String::new()
    } else {
        format!(
            " ON CONFLICT(id) DO UPDATE SET workspace_id=excluded.workspace_id,{column}=excluded.{column},status=excluded.status,{time}=excluded.{time}"
        )
    };
    c.execute(
        &format!(
            "INSERT INTO {table}(id,workspace_id,{column},status,{time},{extra_columns})
             VALUES(?1,?2,?3,?4,?5,{extra_values}){upsert}"
        ),
        params![id, ws, local, status, at],
    )?;
    // All source writes are raw fixture arrangement, deliberately without digest
    // triggers: only a registered operation may maintain the derived tables.
    Ok(())
}
pub fn delete(c: &Connection, source: Source, id: &str) -> rusqlite::Result<()> {
    let table = source.shape().table;
    c.execute(&format!("DELETE FROM {table} WHERE id=?"), [id])?;
    Ok(())
}

#[cfg(any(feature = "conformance", sqlite_conformance))]
pub mod backend {
    use super::*;
    use std::sync::Arc;
    use steve_storage_api::{
        ops::comms::digest::{Source as WireSource, SyncPendingMembership},
        uow::*,
        verify::{
            Harness,
            digest::{self, Change, Fixture, State},
        },
        *,
    };
    use steve_store_sqlite::{publish::Attached, store::SqliteStore};
    fn commit_error(e: rusqlite::Error) -> CommitError {
        CommitError::Fatal(error(steve_types::Code::Internal, e.to_string()))
    }
    fn api_error(e: rusqlite::Error) -> Error {
        error(steve_types::Code::Internal, e.to_string())
    }
    fn string(row: &Row, key: &str) -> String {
        match row.0.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
            Some(Cell::Text(s)) => s.clone(),
            _ => String::new(),
        }
    }
    fn integer(row: &Row, key: &str) -> i64 {
        match row.0.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
            Some(Cell::Int(n)) => *n,
            _ => 0,
        }
    }
    fn native(source: WireSource) -> rusqlite::Result<Source> {
        match source {
            WireSource::ChanMessage => Ok(Source::ChanMessage),
            WireSource::MailMessage => Ok(Source::MailMessage),
            WireSource::Receipt => Err(rusqlite::Error::InvalidQuery),
        }
    }
    pub struct NativeFixture {
        store: Arc<SqliteStore<String>>,
    }
    impl NativeFixture {
        pub fn new(store: Arc<SqliteStore<String>>) -> Self {
            store
                .with_root(Domain::Comms, Lane::Admin, |c, _, _| {
                    install(c).map_err(commit_error)?;
                    for table in ["workspaces", "users", "decisions"] {
                        let n: i64 = c
                            .query_row(
                                "SELECT count(*) FROM main.sqlite_schema WHERE name=?",
                                [table],
                                |r| r.get(0),
                            )
                            .map_err(commit_error)?;
                        assert_eq!(n, 0, "comms fixture must not create a core FK placeholder");
                    }
                    Ok(())
                })
                .unwrap();
            if store.engine(Domain::Core).path() != store.engine(Domain::Comms).path() {
                let c = steve_store_sqlite::open::open_readonly(store.engine(Domain::Core).path())
                    .unwrap();
                for table in TABLES {
                    let n: i64 = c
                        .query_row(
                            "SELECT count(*) FROM main.sqlite_schema WHERE name=?",
                            [table],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(n, 0, "core main must not shadow real comms table {table}");
                }
            }
            assert!(
                store
                    .hello()
                    .domains
                    .iter()
                    .all(|d| !d.features.contains("export")),
                "real comms fixture does not claim complete generic entity export"
            );
            assert!(
                store
                    .hello()
                    .domains
                    .iter()
                    .find(|d| d.domain == Domain::Comms)
                    .unwrap()
                    .features
                    .contains("op:comms.digest.sync_pending_membership"),
                "real registry advertises the operation under test"
            );
            Self { store }
        }
        fn read<T>(&self, run: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
            // Inspection/audits use the physical comms file as main, pin one RO
            // snapshot, and never allocate a root revision just to read state.
            let c =
                steve_store_sqlite::open::open_readonly(self.store.engine(Domain::Comms).path())?;
            let tx = c.unchecked_transaction().map_err(api_error)?;
            let result = run(&tx)?;
            tx.commit().map_err(api_error)?;
            Ok(result)
        }
    }
    fn apply_change(
        c: &Connection,
        change: Change,
    ) -> Result<Option<SyncPendingMembership>, CommitError> {
        match change {
            Change::Write { row, insert_only } => {
                let source = match string(&row, "source").as_str() {
                    "chan_message" => Source::ChanMessage,
                    "mail_message" => Source::MailMessage,
                    _ => {
                        return Err(CommitError::Fatal(error(
                            steve_types::Code::Invalid,
                            "unknown fixture source",
                        )));
                    }
                };
                put(
                    c,
                    source,
                    &string(&row, "id"),
                    &string(&row, "workspace_id"),
                    &string(&row, "conv"),
                    &string(&row, "status"),
                    integer(&row, "at"),
                    insert_only,
                )
                .map_err(commit_error)?;
            }
            Change::Delete { source, id } => {
                delete(c, native(source).map_err(commit_error)?, &id).map_err(commit_error)?
            }
            Change::Receipt {
                workspace,
                conv,
                id,
            } => {
                receipt(c, &workspace, &conv, &id).map_err(commit_error)?;
            }
            Change::Marker {
                workspace,
                conv,
                value,
            } => {
                c.execute(
                    "UPDATE digest_pending_convs SET fixture_marker=? WHERE workspace_id=? AND conv=?",
                    params![value,workspace,conv],
                ).map_err(commit_error)?;
            }
            Change::SkewProjection { workspace, conv } => {
                c.execute(
                    "UPDATE digest_pending_convs SET messages=-1,oldest=-1,newest=-1 WHERE workspace_id=? AND conv=?",
                    params![workspace,conv],
                ).map_err(commit_error)?;
            }
            Change::Op(op) => return Ok(Some(op)),
        }
        Ok(None)
    }
    fn operation_unit(
        h: &dyn Harness,
        id: &str,
        index: usize,
        op: SyncPendingMembership,
    ) -> Result<UnitOfWork, CommitError> {
        let mut unit = UnitOfWork {
            domain: Domain::Comms,
            id: IdempotencyKey {
                issued_at: h.now(),
                key: format!("{id}/{index}"),
            },
            digest: RequestDigest([0; 32]),
            lane: Lane::Admin,
            idempotency: Idempotency::Recorded,
            preconditions: vec![],
            mutations: vec![Mutation::Op(op.to_wire().map_err(CommitError::Fatal)?)],
            facts: vec![],
        };
        unit.digest = unit.request_digest().map_err(CommitError::Fatal)?;
        Ok(unit)
    }
    impl Fixture for NativeFixture {
        fn transact(
            &self,
            h: &dyn Harness,
            id: &str,
            changes: Vec<Change>,
        ) -> Result<(), CommitError> {
            self.store
                .with_root(Domain::Comms, Lane::Admin, |c, runner, root| {
                    let tx = rusqlite::Transaction::new_unchecked(
                        c,
                        rusqlite::TransactionBehavior::Immediate,
                    )
                    .map_err(commit_error)?;
                    for (index, change) in changes.into_iter().enumerate() {
                        if let Some(op) = apply_change(&tx, change)? {
                            let unit = operation_unit(h, id, index, op)?;
                            drop(runner.apply_in(&tx, Attached::new(unit, vec![]), root)?);
                        }
                    }
                    tx.commit().map_err(commit_error)
                })
        }
        fn state(&self) -> Result<State, Error> {
            self.read(|c| {
                let mut sources = vec![];
                for (native, wire) in [
                    (Source::ChanMessage, WireSource::ChanMessage),
                    (Source::MailMessage, WireSource::MailMessage),
                ] {
                    for row in native
                        .messages(c, steve_store_sqlite::digest::MessageFilter::All)
                        .map_err(api_error)?
                    {
                        sources.push(digest::message(
                            &row.id,
                            wire,
                            &row.scope.workspace,
                            &row.scope.conv,
                            &row.status,
                            row.at,
                        ));
                    }
                }
                sources.sort_by_key(|r| (string(r, "source"), string(r, "id")));
                let read = |table: &str,
                            columns: &[&str],
                            text_columns: usize|
                 -> rusqlite::Result<Vec<Row>> {
                    let sql = format!(
                        "SELECT {} FROM {table} ORDER BY workspace_id,conv{}",
                        columns.join(","),
                        if columns.contains(&"message_id") {
                            ",message_id"
                        } else {
                            ""
                        }
                    );
                    c.prepare(&sql)?
                        .query_map([], |r| {
                            Ok(Row(columns
                                .iter()
                                .enumerate()
                                .map(|(i, k)| {
                                    Ok((
                                        if *k == "fixture_marker" {
                                            "marker".into()
                                        } else {
                                            (*k).into()
                                        },
                                        if i < text_columns {
                                            Cell::Text(r.get(i)?)
                                        } else {
                                            Cell::Int(r.get(i)?)
                                        },
                                    ))
                                })
                                .collect::<rusqlite::Result<_>>()?))
                        })?
                        .collect()
                };
                Ok(State {
                    sources,
                    receipts: read(RECEIPTS_TABLE, &["workspace_id", "conv", "message_id"], 3)
                        .map_err(api_error)?,
                    pending: read(
                        "digest_pending",
                        &["workspace_id", "conv", "message_id", "at", "position"],
                        3,
                    )
                    .map_err(api_error)?,
                    conversations: read(
                        "digest_pending_convs",
                        &[
                            "workspace_id",
                            "conv",
                            "messages",
                            "oldest",
                            "newest",
                            "fixture_marker",
                        ],
                        2,
                    )
                    .map_err(api_error)?,
                })
            })
        }
        fn audit(&self) -> Result<(), String> {
            self.read(|c| {
                let workspaces: Vec<String> = c
                    .prepare(&format!(
                        "SELECT workspace_id FROM {channel}
                         UNION SELECT workspace_id FROM {mail}
                         UNION SELECT workspace_id FROM digest_pending
                         UNION SELECT workspace_id FROM digest_pending_convs
                         UNION SELECT workspace_id FROM {RECEIPTS_TABLE}",
                        channel = Source::ChanMessage.shape().table,
                        mail = Source::MailMessage.shape().table,
                    ))
                    .map_err(api_error)?
                    .query_map([], |r| r.get(0))
                    .map_err(api_error)?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(api_error)?;
                for ws in workspaces {
                    let mismatches =
                        steve_store_sqlite::digest::verify_pending(c, &ws).map_err(api_error)?;
                    assert!(
                        mismatches.is_empty(),
                        "real comms digest audit: {mismatches:?}"
                    );
                }
                assert!(
                    !c.prepare("PRAGMA main.foreign_key_check")
                        .map_err(api_error)?
                        .exists([])
                        .map_err(api_error)?
                );
                Ok(())
            })
            .map_err(|e| e.to_string())
        }
    }
}
