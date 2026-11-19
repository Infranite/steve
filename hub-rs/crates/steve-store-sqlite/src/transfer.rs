//! Backend-neutral transfer for the supplied complete declaration set, never discovery
//! of legacy tables. Sessions own independent read connections and have no lease deadline.
use crate::{
    engine::Engine,
    layout::{CORE_FILE, domain_name},
    open::db_error,
    pragmas,
    scan::{self, Bindings, invalid},
    snapshot,
};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use steve_storage_api::{
    cursor::Position,
    fact::FactRecord,
    query::{Order, decode, encode},
    uow::{Committed, RequestDigest},
    *,
};
use steve_types::Code;

pub const BATCH_ROWS: usize = 128;
/// Natural identity uses the reference backend's serialized tuple-key bytes.
pub type NaturalKey = (Domain, String, String, Vec<u8>);
/// Same owned metadata shape as the reference backend; do not hash SQLite's physical
/// encoding. Non-row contract state is scoped to one domain and repeated in every batch.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TransferState {
    pub receipts: Vec<((Domain, i64, String), TransferReceipt)>,
    pub natural: Vec<(NaturalKey, TransferReceipt)>,
    pub cursors: Vec<((Domain, String), Position)>,
    pub seqs: Vec<((Domain, String), u64)>,
    pub facts: Vec<((Domain, String), FactRecord)>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TransferReceipt {
    pub digest: RequestDigest,
    pub result: Committed,
}
impl TransferState {
    fn validate(&self, domain: Domain) -> Result<(), Error> {
        if self.receipts.iter().any(|(k, _)| k.0 != domain)
            || self.natural.iter().any(|(k, _)| k.0 != domain)
            || self.cursors.iter().any(|(k, _)| k.0 != domain)
            || self.seqs.iter().any(|(k, _)| k.0 != domain)
            || self
                .facts
                .iter()
                .any(|(k, v)| k.0 != domain || k.1 != v.fact_id)
        {
            return Err(invalid("mixed-domain or invalid transfer metadata"));
        }
        Ok(())
    }
}
/// Reference-compatible SHA-256 over canonical batch contents, excluding digest itself.
pub fn batch_digest(batch: &ExportBatch) -> Result<RequestDigest, Error> {
    #[derive(Serialize)]
    struct Contents<'a> {
        session: &'a ExportSession,
        id: &'a str,
        entity: &'a EntityDesc,
        rows: &'a [(Row, Rev)],
        state: &'a RawJson,
        next: &'a Option<ExportCursor>,
    }
    let contents = Contents {
        session: &batch.session,
        id: &batch.id,
        entity: &batch.entity,
        rows: &batch.rows,
        state: &batch.state,
        next: &batch.next,
    };
    Ok(RequestDigest(
        Sha256::digest(uow::canonical_json(&encode(&contents)?)?).into(),
    ))
}
struct Export {
    session: ExportSession,
    conn: Connection,
    entities: Vec<EntityDesc>,
    metadata: RawJson,
}
impl Drop for Export {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}
/// An application must supply all its declared entities; names define deterministic order. An empty
/// domain cannot export. Existing undeclared/legacy tables are deliberately never exposed.
pub struct ExportManager {
    engine: Arc<Engine>,
    entities: Vec<EntityDesc>,
    sessions: Mutex<BTreeMap<String, Export>>,
}
impl ExportManager {
    pub fn new(engine: Arc<Engine>, mut entities: Vec<EntityDesc>) -> Result<Self, Error> {
        let mut names = BTreeSet::new();
        for e in &entities {
            scan::validate_descriptor(e)?;
            if !names.insert(e.name.clone()) {
                return Err(invalid("duplicate entity declaration"));
            }
        }
        entities.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            engine,
            entities,
            sessions: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn begin(&self, domain: Domain) -> Result<ExportSession, Error> {
        self.begin_with(domain, |c, d| {
            let has_facts: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM outbox_events LIMIT 1)",
                    [],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if has_facts {
                return Err(error(
                    Code::Unsupported,
                    "outbox facts require a domain snapshot adapter",
                ));
            }
            read_state(c, d)
        })
    }
    /// Integration seam for domain-owned non-row state. The callback reads inside the
    /// already-pinned session snapshot, so metadata and rows describe the same commit.
    pub fn begin_with(
        &self,
        domain: Domain,
        metadata: impl FnOnce(&Connection, Domain) -> Result<TransferState, Error>,
    ) -> Result<ExportSession, Error> {
        let entities: Vec<_> = self
            .entities
            .iter()
            .filter(|e| e.domain == domain)
            .cloned()
            .collect();
        if entities.is_empty() {
            return Err(error(Code::Unsupported, "no declared entities in domain"));
        }
        let conn = Connection::open_with_flags(
            self.engine.data_dir().join(CORE_FILE),
            pragmas::readonly_flags(),
        )
        .map_err(db_error)?;
        pragmas::connection(&conn, false).map_err(db_error)?;
        pragmas::attach_readonly(
            &conn,
            self.engine.data_dir(),
            self.engine.layout(),
            CORE_FILE,
        )
        .map_err(db_error)?;
        conn.execute_batch("BEGIN DEFERRED").map_err(db_error)?;
        snapshot::pin(&conn)?;
        let state = metadata(&conn, domain)?;
        state.validate(domain)?;
        let mut rev = 0u64;
        for e in &entities {
            let maximum: Option<i64> = conn
                .query_row(
                    &format!("SELECT MAX(rev) FROM {}", scan::ident(&e.name)),
                    [],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if let Some(v) = maximum {
                rev = rev.max(u64::try_from(v).map_err(|_| invalid("negative revision"))?);
            }
        }
        for receipt in state
            .receipts
            .iter()
            .map(|(_, v)| v)
            .chain(state.natural.iter().map(|(_, v)| v))
        {
            rev = rev.max(receipt.result.rev.0);
        }
        let schema: u32 = conn
            .query_row(
                &format!(
                    "SELECT version FROM {}.domain_schema WHERE domain=?1",
                    scan::ident(&namespace(&conn, domain)?)
                ),
                [domain_name(domain)],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let session = ExportSession {
            id: uuid::Uuid::new_v4().to_string(),
            domain,
            version: ContractVersion(1),
            layout: self.engine.layout().generation,
            schema,
            rev: Rev(rev),
        };
        let export = Export {
            session: session.clone(),
            conn,
            entities,
            metadata: encode(&state)?,
        };
        self.sessions
            .lock()
            .map_err(|_| error(Code::Internal, "export registry poisoned"))?
            .insert(session.id.clone(), export);
        Ok(session)
    }
    pub fn export(
        &self,
        session: &ExportSession,
        cursor: ExportCursor,
    ) -> Result<ExportBatch, Error> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| error(Code::Internal, "export registry poisoned"))?;
        let export = sessions
            .get(&session.id)
            .filter(|e| e.session == *session)
            .ok_or_else(|| error(Code::Stale, "unknown export session"))?;
        let index = if cursor.entity.is_empty() {
            if cursor.after.is_some() {
                return Err(invalid("initial export cursor has a key"));
            }
            0
        } else {
            export
                .entities
                .iter()
                .position(|e| e.name == cursor.entity)
                .ok_or_else(|| {
                    if self.entities.iter().any(|e| e.name == cursor.entity) {
                        invalid("mixed-domain export cursor")
                    } else {
                        error(Code::Unsupported, "undeclared export entity")
                    }
                })?
        };
        let e = &export.entities[index];
        let mut b = Bindings::default();
        let clause = match &cursor.after {
            Some(key) => b.compare(e, &e.key, key, ">", false)?,
            None => "1".into(),
        };
        let sql = format!(
            "SELECT {} FROM {} WHERE {clause} ORDER BY {} LIMIT {}",
            scan::columns(e),
            scan::ident(&e.name),
            scan::ordering(e, &e.key, Order::Asc)?,
            BATCH_ROWS + 1
        );
        let mut stmt = export.conn.prepare_cached(&sql).map_err(db_error)?;
        let mut read = stmt.query(params_from_iter(&b.0)).map_err(db_error)?;
        let mut rows = Vec::new();
        while let Some(row) = read.next().map_err(db_error)? {
            rows.push(scan::read_row(row, e)?);
        }
        let more = rows.len() > BATCH_ROWS;
        rows.truncate(BATCH_ROWS);
        let next = if more {
            Some(ExportCursor {
                entity: e.name.clone(),
                after: rows.last().map(|(r, _)| scan::values(r, &e.key)),
            })
        } else {
            export.entities.get(index + 1).map(|e| ExportCursor {
                entity: e.name.clone(),
                after: None,
            })
        };
        let id = format!(
            "{}:{}:{}",
            session.id,
            e.name,
            serde_json::to_string(&cursor.after).map_err(|_| invalid("invalid export cursor"))?
        );
        let mut batch = ExportBatch {
            session: session.clone(),
            id,
            entity: e.clone(),
            rows,
            state: export.metadata.clone(),
            next,
            digest: RequestDigest([0; 32]),
        };
        batch.digest = batch_digest(&batch)?;
        Ok(batch)
    }
    /// Explicitly release the independent read snapshot when pagination ends or is abandoned.
    pub fn end(&self, session: &ExportSession) -> Result<(), Error> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| error(Code::Internal, "export registry poisoned"))?;
        if sessions
            .get(&session.id)
            .is_none_or(|s| s.session != *session)
        {
            return Err(error(Code::Stale, "unknown export session"));
        }
        sessions.remove(&session.id);
        Ok(())
    }
}
/// Resolve shared contract tables to the domain's physical file on a core-main reader.
/// Standalone contract-only unit-test connections have no placement metadata.
fn namespace(conn: &Connection, domain: Domain) -> Result<String, Error> {
    let file: Option<String> = conn
        .query_row(
            "SELECT value FROM main.meta WHERE domain=?1 AND key='file'",
            [domain_name(domain)],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(file) = file else {
        return Ok("main".into());
    };
    let mut stmt = conn.prepare("PRAGMA database_list").map_err(db_error)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(db_error)?;
    for row in rows {
        let (alias, path) = row.map_err(db_error)?;
        if std::path::Path::new(&path)
            .file_name()
            .is_some_and(|n| n == file.as_str())
        {
            return Ok(alias);
        }
    }
    Err(invalid("domain file is not attached"))
}
/// Shared metadata layout used by W2a.3c: recorded intent keys are JSON tuples,
/// natural receipts use meta's natural:<tuple>, and append counters use seq:<name>.
/// Facts require the domain's outbox adapter and are supplied through begin_with.
pub fn read_state(conn: &Connection, domain: Domain) -> Result<TransferState, Error> {
    let mut state = TransferState::default();
    let ns = scan::ident(&namespace(conn, domain)?);
    let mut stmt=conn.prepare(&format!("SELECT key,issued_at,digest,result FROM {ns}.uow_applied WHERE domain=?1 ORDER BY issued_at,key COLLATE BINARY")).map_err(db_error)?;
    let receipts = stmt
        .query_map([domain_name(domain)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(db_error)?;
    for receipt in receipts {
        let (identity, issued, digest, result) = receipt.map_err(db_error)?;
        let (encoded_issued, key): (i64, String) =
            serde_json::from_str(&identity).map_err(|_| invalid("invalid intent identity"))?;
        if encoded_issued != issued {
            return Err(invalid("receipt timestamp mismatch"));
        }
        state.receipts.push((
            (domain, issued, key),
            TransferReceipt {
                digest: RequestDigest(
                    digest
                        .try_into()
                        .map_err(|_| invalid("invalid receipt digest"))?,
                ),
                result: serde_json::from_slice(&result)
                    .map_err(|_| invalid("invalid receipt result"))?,
            },
        ));
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name,key FROM {ns}.cursor WHERE domain=?1 ORDER BY name COLLATE BINARY"
        ))
        .map_err(db_error)?;
    let cursors = stmt
        .query_map([domain_name(domain)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?;
    for cursor in cursors {
        let (name, value) = cursor.map_err(db_error)?;
        state.cursors.push((
            (domain, name),
            serde_json::from_str(&value).map_err(|_| invalid("invalid stored cursor"))?,
        ));
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT key,value FROM {ns}.meta WHERE domain=?1 ORDER BY key COLLATE BINARY"
        ))
        .map_err(db_error)?;
    let rows = stmt
        .query_map([domain_name(domain)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?;
    for row in rows {
        let (key, value) = row.map_err(db_error)?;
        if let Some(key) = key.strip_prefix("natural:") {
            let (entity, index, cells): (String, String, Vec<Cell>) =
                serde_json::from_str(key).map_err(|_| invalid("invalid natural identity"))?;
            state.natural.push((
                (
                    domain,
                    entity,
                    index,
                    serde_json::to_vec(&cells).map_err(|_| invalid("invalid natural key"))?,
                ),
                serde_json::from_str(&value).map_err(|_| invalid("invalid natural receipt"))?,
            ));
        } else if let Some(name) = key.strip_prefix("seq:") {
            state.seqs.push((
                (domain, name.into()),
                value
                    .parse()
                    .map_err(|_| invalid("invalid append counter"))?,
            ));
        }
    }
    state.receipts.sort_by(|a, b| a.0.cmp(&b.0));
    state.natural.sort_by(|a, b| a.0.cmp(&b.0));
    state.seqs.sort_by(|a, b| a.0.cmp(&b.0));
    state.validate(domain)?;
    Ok(state)
}
/// Restore shared metadata in the importing writer's transaction. Domain-specific
/// outbox rows must be restored by import_with's adapter; reject rather than discard.
pub fn write_state(conn: &Connection, domain: Domain, state: &TransferState) -> Result<(), Error> {
    if !state.facts.is_empty() {
        return Err(error(
            Code::Unsupported,
            "facts require a domain outbox restore adapter",
        ));
    }
    write_common_state(conn, domain, state)
}
/// Common half of an import adapter; the adapter must additionally restore every fact
/// through its registered outbox mapping, without publishing or emitting new facts.
pub fn write_common_state(
    conn: &Connection,
    domain: Domain,
    state: &TransferState,
) -> Result<(), Error> {
    state.validate(domain)?;
    if namespace(conn, domain)? != "main" {
        return Err(invalid("import requires the domain writer"));
    }
    for ((_, issued, key), receipt) in &state.receipts {
        let identity = serde_json::to_string(&(issued, key))
            .map_err(|_| invalid("invalid intent identity"))?;
        conn.execute("INSERT INTO uow_applied(domain,key,issued_at,digest,rev,result) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(domain,key) DO UPDATE SET issued_at=excluded.issued_at,digest=excluded.digest,rev=excluded.rev,result=excluded.result",params![domain_name(domain),identity,issued,receipt.digest.0.as_slice(),i64::try_from(receipt.result.rev.0).map_err(|_|invalid("revision overflow"))?,serde_json::to_vec(&receipt.result).map_err(|_|invalid("invalid receipt"))?]).map_err(db_error)?;
    }
    for ((_, entity, index, key), receipt) in &state.natural {
        let cells: Vec<Cell> =
            serde_json::from_slice(key).map_err(|_| invalid("invalid natural key"))?;
        let key = format!(
            "natural:{}",
            serde_json::to_string(&(entity, index, cells))
                .map_err(|_| invalid("invalid natural identity"))?
        );
        crate::layout::put_meta(
            conn,
            domain_name(domain),
            &key,
            &serde_json::to_string(receipt).map_err(|_| invalid("invalid natural receipt"))?,
        )?;
    }
    for ((_, name), position) in &state.cursors {
        conn.execute("INSERT INTO cursor(domain,name,key,updated) VALUES(?1,?2,?3,0) ON CONFLICT(domain,name) DO UPDATE SET key=excluded.key",params![domain_name(domain),name,serde_json::to_string(position).map_err(|_|invalid("invalid cursor"))?]).map_err(db_error)?;
    }
    for ((_, name), seq) in &state.seqs {
        if *seq > i64::MAX as u64 {
            return Err(invalid("append counter overflow"));
        }
        crate::layout::put_meta(
            conn,
            domain_name(domain),
            &format!("seq:{name}"),
            &seq.to_string(),
        )?;
    }
    Ok(())
}
/// Import on the executor-owned writer connection. The savepoint makes the entire batch
/// (including its durable retry receipt) atomic, also inside an existing root transaction.
/// The caller must schedule this through its writer executor; this never emits events.
pub fn import(
    conn: &Connection,
    e: &EntityDesc,
    batch: ExportBatch,
) -> Result<ImportReport, Error> {
    import_with(conn, e, batch, write_state)
}
/// Domain integration seam for validating/restoring additional non-row state atomically.
pub fn import_with(
    conn: &Connection,
    e: &EntityDesc,
    batch: ExportBatch,
    restore: impl FnOnce(&Connection, Domain, &TransferState) -> Result<(), Error>,
) -> Result<ImportReport, Error> {
    scan::validate_descriptor(e)?;
    if batch_digest(&batch)? != batch.digest {
        return Err(invalid("import checksum mismatch"));
    }
    if batch.session.version != ContractVersion(1)
        || e != &batch.entity
        || e.domain != batch.session.domain
    {
        return Err(invalid("incompatible import descriptor/domain/version"));
    }
    let state: TransferState = decode(&batch.state)?;
    state.validate(e.domain)?;
    scan::atomic_read(conn, || {
        let receipt_key = format!("import:{}", batch.id);
        let existing: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE domain=?1 AND key=?2",
                [domain_name(e.domain), receipt_key.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(existing) = existing {
            let digest: RequestDigest =
                serde_json::from_str(&existing).map_err(|_| invalid("invalid import receipt"))?;
            if digest != batch.digest {
                return Err(invalid("import batch identity mismatch"));
            }
            return Ok(ImportReport {
                batch_id: batch.id.clone(),
                imported: 0,
                replayed: true,
            });
        }
        let mut previous: Option<Vec<rusqlite::types::Value>> = None;
        for (row, rev) in &batch.rows {
            if row.0.len() != e.fields.len()
                || row.0.iter().map(|(n, _)| n).collect::<BTreeSet<_>>().len() != e.fields.len()
            {
                return Err(invalid("invalid import row fields"));
            }
            if rev.0 > batch.session.rev.0 {
                return Err(invalid("import revision exceeds snapshot"));
            }
            let mut b = Bindings::default();
            let key = scan::values(row, &e.key);
            let check = b.compare(e, &e.key, &key, ">=", false)?;
            if let Some(last) = &previous {
                // Let SQLite perform the same byte/numeric comparison used by export.
                let projection = e
                    .key
                    .iter()
                    .zip(last)
                    .enumerate()
                    .map(|(i, (name, _))| {
                        format!("?{} AS {}", b.0.len() + i + 1, scan::ident(name))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let mut bindings = b.0.clone();
                bindings.extend(last.clone());
                let backwards: bool = conn
                    .query_row(
                        &format!("SELECT {check} FROM (SELECT {projection})"),
                        params_from_iter(&bindings),
                        |r| r.get(0),
                    )
                    .map_err(db_error)?;
                if backwards {
                    return Err(invalid("import rows not in primary key order"));
                }
                // Equal keys are also rejected by the destination PK, atomically.
            }
            previous = Some(
                e.key
                    .iter()
                    .zip(&key)
                    .map(|(n, v)| scan::bind_cell(scan::field(e, n)?, v))
                    .collect::<Result<_, _>>()?,
            );
            let mut bound = Vec::new();
            for f in &e.fields {
                let v = row
                    .0
                    .iter()
                    .find(|(n, _)| *n == f.name)
                    .ok_or_else(|| invalid("missing import field"))?;
                bound.push(scan::bind_cell(f, &v.1)?);
            }
            bound.push(rusqlite::types::Value::Integer(
                i64::try_from(rev.0).map_err(|_| invalid("revision overflow"))?,
            ));
            let sql = format!(
                "INSERT INTO {}({}) VALUES({})",
                scan::ident(&e.name),
                scan::columns(e),
                vec!["?"; bound.len()].join(",")
            );
            conn.execute(&sql, params_from_iter(&bound))
                .map_err(|err| {
                    if err.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
                        invalid("import key or constraint conflict")
                    } else {
                        db_error(err)
                    }
                })?;
        }
        restore(conn, e.domain, &state)?;
        crate::layout::put_meta(
            conn,
            domain_name(e.domain),
            &receipt_key,
            &serde_json::to_string(&batch.digest).map_err(|_| invalid("invalid checksum"))?,
        )?;
        Ok(ImportReport {
            batch_id: batch.id.clone(),
            imported: batch.rows.len() as u64,
            replayed: false,
        })
    })
}
