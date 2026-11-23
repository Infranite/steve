//! Durable local fence for Hub tool calls. Publication of external effects cannot share this
//! database transaction, so the journal only records intent and observed results.

use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::Mutex,
};

use rusqlite::{Connection, OptionalExtension, params};
use steve_node_api::{ApiResult, Code, Digest, Error, NodeId, OperationId, Revision, ToolResult};

pub struct Journal {
    db: Mutex<Connection>,
    _lock: File,
}

pub struct Intent<'a> {
    pub key: &'a str,
    pub digest: &'a Digest,
    pub operation: &'a OperationId,
    pub node: &'a NodeId,
    pub holder: &'a str,
    pub workspace: &'a str,
    pub name: &'a str,
    pub catalog: &'a Revision,
    pub deadline: i64,
    pub at: i64,
}

pub struct Recorded {
    pub name: String,
    pub result: ToolResult,
}

fn internal(error: impl std::fmt::Display) -> Error {
    tracing::error!("tool journal: {error}");
    Error::new(Code::Internal, "Node host storage unavailable")
}

impl Journal {
    pub fn open(home: &Path) -> ApiResult<Self> {
        std::fs::create_dir_all(home).map_err(internal)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("host-tools.lock"))
            .map_err(internal)?;
        lock.try_lock()
            .map_err(|_| Error::new(Code::Busy, "another tool host owns this ledger"))?;
        let mut db = Connection::open(home.join("host-tools.sqlite3")).map_err(internal)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(internal)?;
        let tx = db.transaction().map_err(internal)?;
        let version: i64 = tx
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(internal)?;
        if version > 2 {
            return Err(Error::new(Code::Unsupported, "newer tool journal"));
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS calls (key TEXT PRIMARY KEY, digest TEXT NOT NULL, operation TEXT NOT NULL,
                node TEXT NOT NULL, session TEXT NOT NULL, workspace TEXT NOT NULL, name TEXT NOT NULL,
                catalog TEXT NOT NULL, deadline INTEGER NOT NULL, created INTEGER NOT NULL,
                updated INTEGER NOT NULL, state TEXT NOT NULL, result TEXT, cancel_requested INTEGER NOT NULL DEFAULT 0);
             UPDATE calls SET state='unknown' WHERE state='running';
             PRAGMA user_version=2;",
        ).map_err(internal)?;
        if version == 1 {
            tx.execute_batch(
                "UPDATE calls SET result=json_set(result,'$.error',json_object(
                   'code', CASE json_extract(result,'$.error.code')
                       WHEN 'offline' THEN 'unavailable' WHEN 'denied' THEN 'forbidden'
                       WHEN 'expired' THEN 'stale' WHEN 'downgrade' THEN 'invalid'
                       ELSE json_extract(result,'$.error.code') END,
                   'retry',json_object('kind','never'), 'reason',NULL, 'params',json_object(),
                   'safe_detail',json_extract(result,'$.error.safe_detail')))
                 WHERE json_extract(result,'$.status')='failed'
                   AND json_type(result,'$.error.retry')='text';",
            )
            .map_err(internal)?;
        }
        tx.commit().map_err(internal)?;
        Ok(Self {
            db: Mutex::new(db),
            _lock: lock,
        })
    }

    pub fn read(&self, key: &str, digest: Option<&Digest>) -> ApiResult<Option<Recorded>> {
        find(&self.db.lock().unwrap(), key, digest)
    }

    /// Return the existing call or durably reserve the key. The process lock keeps the journal
    /// exclusive and this mutex makes the lookup and insert indivisible across callers.
    pub fn reserve(&self, intent: Intent<'_>) -> ApiResult<Option<ToolResult>> {
        let db = self.db.lock().unwrap();
        if let Some(record) = find(&db, intent.key, Some(intent.digest))? {
            return Ok(Some(record.result));
        }
        db.execute(
            "INSERT INTO calls(key,digest,operation,node,session,workspace,name,catalog,deadline,created,updated,state)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,'running')",
            params![intent.key, intent.digest.as_str(), intent.operation.as_str(), intent.node.as_str(),
                intent.holder, intent.workspace, intent.name, intent.catalog.as_str(), intent.deadline, intent.at],
        ).map_err(internal)?;
        Ok(None)
    }

    pub fn cancelled(&self, key: &str) -> ApiResult<bool> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT cancel_requested FROM calls WHERE key=?",
                [key],
                |row| row.get(0),
            )
            .map_err(internal)
    }

    pub fn complete(&self, key: &str, result: &ToolResult, at: i64) -> ApiResult<()> {
        let encoded = serde_json::to_string(result).map_err(internal)?;
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE calls SET state='done',result=?2,updated=?3 WHERE key=?1",
                params![key, encoded, at],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn unknown(&self, key: &str, at: i64) -> ApiResult<()> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE calls SET state='unknown',updated=?2 WHERE key=?1 AND state='running'",
                params![key, at],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn cancel(&self, key: &str, at: i64) -> ApiResult<()> {
        self.db.lock().unwrap().execute(
            "UPDATE calls SET cancel_requested=1,state=CASE WHEN state='running' THEN 'unknown' ELSE state END,updated=?2 WHERE key=?1",
            params![key, at],
        ).map_err(internal)?;
        Ok(())
    }
}

fn find(db: &Connection, key: &str, digest: Option<&Digest>) -> ApiResult<Option<Recorded>> {
    let row: Option<(String, String, String, Option<String>, String)> = db
        .query_row(
            "SELECT digest,operation,state,result,name FROM calls WHERE key=?",
            [key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    let Some((saved_digest, op, state, result, name)) = row else {
        return Ok(None);
    };
    if digest.is_some_and(|digest| digest.as_str() != saved_digest) {
        return Err(Error::new(
            Code::Conflict,
            "ToolKey reused with different name/arguments",
        ));
    }
    let operation = OperationId::new(op).map_err(internal)?;
    let result = match result {
        Some(raw) => serde_json::from_str(&raw).map_err(internal)?,
        None if state == "running" => ToolResult::Running { operation },
        None => ToolResult::Unknown { operation },
    };
    Ok(Some(Recorded { name, result }))
}
