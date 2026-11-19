//! Explicit SQLite entity, operation and legacy-outbox mappings; values are always bound.
pub mod digest;
pub mod fixtures;
mod reads;
pub(crate) mod retention;
mod sql;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params, types::Value};
pub use sql::{cell_value, read_cell};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use steve_storage_api::{
    entity::{FieldDesc, FieldType},
    fact::FactRecord,
    uow::AnyOp,
    *,
};

pub(crate) fn failure(code: steve_types::Code, detail: &str) -> CommitError {
    CommitError::Fatal(steve_storage_api::error(code, detail))
}
pub(crate) fn invalid(detail: &str) -> CommitError {
    failure(steve_types::Code::Invalid, detail)
}
pub(crate) fn unsupported(detail: &str) -> CommitError {
    failure(steve_types::Code::Unsupported, detail)
}
pub(crate) fn sql_error(e: rusqlite::Error) -> CommitError {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => CommitError::Retryable {
            after_ms: None,
            detail: "SQLite writer busy".into(),
        },
        Some(ErrorCode::ConstraintViolation) => invalid("SQLite constraint violation"),
        _ => failure(steve_types::Code::Internal, "SQLite operation failed"),
    }
}
pub fn domain_name(d: Domain) -> &'static str {
    match d {
        Domain::Core => "core",
        Domain::Stream => "stream",
        Domain::Comms => "comms",
    }
}
pub(crate) fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
pub fn value<'a>(row: &'a Row, name: &str) -> Option<&'a Cell> {
    row.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}
pub fn values(row: &Row, fields: &[String]) -> Vec<Cell> {
    fields
        .iter()
        .map(|f| value(row, f).cloned().unwrap_or(Cell::Null))
        .collect()
}

pub struct SqlEntity {
    pub desc: EntityDesc,
    pub table: String,
    shapes: Mutex<BTreeMap<String, Arc<str>>>,
}
impl SqlEntity {
    pub fn sql(&self, text: String) -> Arc<str> {
        self.shapes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(text.clone())
            .or_insert_with(|| Arc::from(text))
            .clone()
    }
    pub fn shape_count(&self) -> usize {
        self.shapes.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub fn field(&self, name: &str) -> Result<&FieldDesc, CommitError> {
        self.desc
            .fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| unsupported("unknown entity field"))
    }
    pub fn check_cell(&self, name: &str, v: &Cell) -> Result<(), CommitError> {
        check_cell(self.field(name)?, v)
    }
    pub fn check_row(&self, row: &Row) -> Result<(), CommitError> {
        let names: BTreeSet<_> = row.0.iter().map(|(n, _)| n).collect();
        if names.len() != row.0.len() || row.0.len() != self.desc.fields.len() {
            return Err(invalid("row fields do not match descriptor"));
        }
        for f in &self.desc.fields {
            let v = value(row, &f.name).ok_or_else(|| invalid("missing row field"))?;
            self.check_cell(&f.name, v)?;
            if *v == Cell::Null && self.desc.key.contains(&f.name) {
                return Err(invalid("null primary key"));
            }
        }
        Ok(())
    }
    pub fn check_key(
        &self,
        fields: &[String],
        key: &[Cell],
        prefix: bool,
    ) -> Result<(), CommitError> {
        if key.is_empty() || key.len() > fields.len() || (!prefix && key.len() != fields.len()) {
            return Err(invalid("invalid key shape"));
        }
        for (f, v) in fields.iter().zip(key) {
            self.check_cell(f, v)?;
            if self.desc.key.contains(f) && *v == Cell::Null {
                return Err(invalid("null primary key"));
            }
        }
        Ok(())
    }
    pub fn index_fields(&self, index: &str) -> Result<Vec<String>, CommitError> {
        let mut fields = if index.is_empty() {
            self.desc.key.clone()
        } else {
            self.desc
                .indexes
                .iter()
                .find(|i| i.name == index)
                .ok_or_else(|| unsupported("unknown index"))?
                .fields
                .clone()
        };
        for k in &self.desc.key {
            if !fields.contains(k) {
                fields.push(k.clone());
            }
        }
        Ok(fields)
    }
    pub fn get(&self, c: &Connection, key: &[Cell]) -> Result<Option<(Row, Rev)>, CommitError> {
        self.check_key(&self.desc.key, key, false)?;
        let columns = self
            .desc
            .fields
            .iter()
            .map(|f| quote(&f.name))
            .collect::<Vec<_>>()
            .join(",");
        let sql = self.sql(format!(
            "SELECT {columns},rev FROM {} WHERE {}",
            quote(&self.table),
            sql::equal(&self.desc.key)
        ));
        let params = key.iter().map(cell_value).collect::<Result<Vec<_>, _>>()?;
        let mut stmt = c.prepare_cached(&sql).map_err(sql_error)?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params))
            .map_err(sql_error)?;
        let Some(r) = rows.next().map_err(sql_error)? else {
            return Ok(None);
        };
        let cells = self
            .desc
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                Ok((
                    f.name.clone(),
                    read_cell(r.get_ref(i).map_err(sql_error)?, f)?,
                ))
            })
            .collect::<Result<Vec<_>, CommitError>>()?;
        let rev: i64 = r.get(self.desc.fields.len()).map_err(sql_error)?;
        let rev = u64::try_from(rev).map_err(|_| invalid("invalid stored revision"))?;
        Ok(Some((Row(cells), Rev(rev))))
    }
    pub fn insert(
        &self,
        c: &Connection,
        row: &Row,
        rev: Rev,
        upsert: bool,
    ) -> Result<(), CommitError> {
        self.check_row(row)?;
        self.unique(
            c,
            row,
            if upsert {
                Some(values(row, &self.desc.key))
            } else {
                None
            },
        )?;
        let cols = self
            .desc
            .fields
            .iter()
            .map(|f| quote(&f.name))
            .collect::<Vec<_>>();
        let marks = vec!["?"; cols.len() + 1].join(",");
        let mut sql = format!(
            "INSERT INTO {}({},rev) VALUES({marks})",
            quote(&self.table),
            cols.join(",")
        );
        if upsert {
            sql.push_str(&format!(
                " ON CONFLICT({}) DO UPDATE SET {}",
                self.desc
                    .key
                    .iter()
                    .map(|k| quote(k))
                    .collect::<Vec<_>>()
                    .join(","),
                cols.iter()
                    .map(|f| format!("{f}=excluded.{f}"))
                    .chain(std::iter::once("rev=excluded.rev".into()))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        let mut params = self
            .desc
            .fields
            .iter()
            .map(|f| cell_value(value(row, &f.name).unwrap()))
            .collect::<Result<Vec<_>, _>>()?;
        params.push(Value::Integer(rev.0 as i64));
        let sql = self.sql(sql);
        c.prepare_cached(&sql)
            .map_err(sql_error)?
            .execute(rusqlite::params_from_iter(params))
            .map_err(sql_error)?;
        Ok(())
    }
    fn unique(
        &self,
        c: &Connection,
        row: &Row,
        ignore: Option<Vec<Cell>>,
    ) -> Result<(), CommitError> {
        for (name, fields) in std::iter::once(("", &self.desc.key)).chain(
            self.desc
                .indexes
                .iter()
                .filter(|i| i.unique)
                .map(|i| (i.name.as_str(), &i.fields)),
        ) {
            let key = values(row, fields);
            if key.contains(&Cell::Null) {
                continue;
            }
            let query = format!(
                "SELECT {} FROM {} WHERE {}",
                self.desc
                    .key
                    .iter()
                    .map(|k| quote(k))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&self.table),
                sql::equal(fields)
            );
            let mut stmt = c.prepare_cached(&query).map_err(sql_error)?;
            let params = key.iter().map(cell_value).collect::<Result<Vec<_>, _>>()?;
            let existing: Option<Vec<Cell>> = stmt
                .query_row(rusqlite::params_from_iter(params), |r| {
                    self.desc
                        .key
                        .iter()
                        .enumerate()
                        .map(|(i, k)| {
                            read_cell(r.get_ref(i)?, self.field(k).unwrap())
                                .map_err(|_| rusqlite::Error::InvalidQuery)
                        })
                        .collect()
                })
                .optional()
                .map_err(sql_error)?;
            if existing.is_some() && existing != ignore {
                return Err(CommitError::Unique {
                    entity: self.desc.name.clone(),
                    index: name.into(),
                });
            }
        }
        Ok(())
    }
    pub fn update(
        &self,
        c: &Connection,
        key: &[Cell],
        patch: &Row,
        rev: Rev,
        cas: Option<Rev>,
    ) -> Result<usize, CommitError> {
        self.check_key(&self.desc.key, key, false)?;
        let names: BTreeSet<_> = patch.0.iter().map(|(n, _)| n).collect();
        if names.len() != patch.0.len() {
            return Err(invalid("duplicate update field"));
        }
        for (f, v) in &patch.0 {
            self.check_cell(f, v)?;
        }
        if let Some((mut row, _)) = self.get(c, key)? {
            for (f, v) in &patch.0 {
                let dest = row.0.iter_mut().find(|(n, _)| n == f).unwrap();
                dest.1 = v.clone();
            }
            self.check_row(&row)?;
            self.unique(c, &row, Some(key.to_vec()))?;
        }
        let mut params = Vec::new();
        let mut sets = Vec::new();
        for f in names {
            sets.push(format!("{}=?", quote(f)));
            params.push(cell_value(value(patch, f).unwrap())?);
        }
        sets.push("rev=?".into());
        params.push(Value::Integer(rev.0 as i64));
        params.extend(key.iter().map(cell_value).collect::<Result<Vec<_>, _>>()?);
        let mut sql = format!(
            "UPDATE {} SET {} WHERE {}",
            quote(&self.table),
            sets.join(","),
            sql::equal(&self.desc.key)
        );
        if let Some(old) = cas {
            sql.push_str(" AND rev=?");
            params.push(Value::Integer(old.0 as i64));
        }
        let sql = self.sql(sql);
        c.prepare_cached(&sql)
            .map_err(sql_error)?
            .execute(rusqlite::params_from_iter(params))
            .map_err(sql_error)
    }
    pub fn delete(&self, c: &Connection, key: &[Cell]) -> Result<(), CommitError> {
        self.check_key(&self.desc.key, key, false)?;
        let sql = self.sql(format!(
            "DELETE FROM {} WHERE {}",
            quote(&self.table),
            sql::equal(&self.desc.key)
        ));
        let p = key.iter().map(cell_value).collect::<Result<Vec<_>, _>>()?;
        c.prepare_cached(&sql)
            .map_err(sql_error)?
            .execute(rusqlite::params_from_iter(p))
            .map_err(sql_error)?;
        Ok(())
    }
    pub fn delete_where(
        &self,
        c: &Connection,
        index: &str,
        range: &query::KeyRange,
        filter: &[query::Pred],
        limit: u32,
    ) -> Result<(), CommitError> {
        sql::delete_where(self, c, index, range, filter, limit)
    }
}
fn check_cell(f: &FieldDesc, v: &Cell) -> Result<(), CommitError> {
    v.validate().map_err(CommitError::Fatal)?;
    let valid = match (&f.ty, v) {
        (_, Cell::Null) => f.nullable,
        (FieldType::Int, Cell::Int(_))
        | (FieldType::Real, Cell::Real(_))
        | (FieldType::Text, Cell::Text(_))
        | (FieldType::Bytes, Cell::Bytes(_))
        | (FieldType::Bool, Cell::Bool(_))
        | (FieldType::Millis, Cell::Millis(_))
        | (FieldType::Json, Cell::Json(_)) => true,
        (FieldType::Enum(codes), Cell::Text(s)) => {
            if !codes.contains(s) {
                return Err(failure(
                    steve_types::Code::Incompatible,
                    "unknown enum code",
                ));
            }
            true
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid("invalid field type or nullability"))
    }
}
pub struct OutboxFields {
    pub workspace_id: String,
    pub version: i64,
    pub payload: RawJson,
}
type FactAdapter = fn(&FactRecord) -> Result<OutboxFields, CommitError>;
struct Topic {
    domain: Domain,
    adapt: FactAdapter,
    decode: Option<reads::FactDecoder>,
}
type Handler = dyn Fn(&Connection, &AnyOp, Rev) -> Result<(), CommitError> + Send + Sync;
struct Operation {
    domain: Domain,
    run: Arc<Handler>,
}
pub struct Registry {
    entities: BTreeMap<String, SqlEntity>,
    topics: BTreeMap<String, Topic>,
    operations: BTreeMap<String, Operation>,
    queries: BTreeMap<(Domain, String), Arc<reads::QueryHandler>>,
    export_complete: BTreeSet<Domain>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            entities: BTreeMap::new(),
            topics: BTreeMap::new(),
            operations: BTreeMap::new(),
            queries: reads::builtin_queries(),
            export_complete: BTreeSet::new(),
        }
    }
}
impl Registry {
    pub fn register(
        &mut self,
        desc: EntityDesc,
        table: impl Into<String>,
    ) -> Result<(), CommitError> {
        let table = table.into();
        let names: BTreeSet<_> = desc.fields.iter().map(|f| f.name.as_str()).collect();
        if table.is_empty()
            || table.contains('\0')
            || desc.name.is_empty()
            || desc.key.is_empty()
            || desc.key.iter().collect::<BTreeSet<_>>().len() != desc.key.len()
            || desc
                .indexes
                .iter()
                .map(|i| &i.name)
                .collect::<BTreeSet<_>>()
                .len()
                != desc.indexes.len()
            || names.contains("rev")
            || names.len() != desc.fields.len()
            || names.iter().any(|n| n.is_empty() || n.contains('\0'))
            || desc.key.iter().any(|k| !names.contains(k.as_str()))
            || desc.indexes.iter().any(|i| {
                i.name.is_empty()
                    || i.name.contains('\0')
                    || i.fields.iter().collect::<BTreeSet<_>>().len() != i.fields.len()
                    || i.fields.is_empty()
                    || i.fields.iter().any(|f| !names.contains(f.as_str()))
            })
            || self.entities.contains_key(&desc.name)
            || self.entities.values().any(|e| e.table == table)
        {
            return Err(invalid("invalid or duplicate entity mapping"));
        }
        self.entities.insert(
            desc.name.clone(),
            SqlEntity {
                desc,
                table,
                shapes: Mutex::default(),
            },
        );
        self.export_complete.clear();
        Ok(())
    }
    pub fn entity(&self, name: &str, domain: Domain) -> Result<&SqlEntity, CommitError> {
        let e = self
            .entities
            .get(name)
            .ok_or_else(|| unsupported("unknown entity"))?;
        if e.desc.domain != domain {
            return Err(invalid("mixed-domain entity"));
        }
        Ok(e)
    }
    pub fn entities(&self) -> impl Iterator<Item = &SqlEntity> {
        self.entities.values()
    }
    pub fn register_topic(
        &mut self,
        name: impl Into<String>,
        domain: Domain,
        adapt: FactAdapter,
    ) -> Result<(), CommitError> {
        let name = name.into();
        if self.topics.contains_key(&name) {
            return Err(invalid("duplicate fact topic"));
        }
        self.topics.insert(
            name,
            Topic {
                domain,
                adapt,
                decode: None,
            },
        );
        self.export_complete.remove(&domain);
        Ok(())
    }
    pub fn check_topic(&self, name: &str, domain: Domain) -> Result<(), CommitError> {
        let t = self
            .topics
            .get(name)
            .ok_or_else(|| unsupported("unknown fact topic"))?;
        if t.domain != domain {
            return Err(invalid("mixed-domain fact"));
        }
        Ok(())
    }
    pub fn fact(
        &self,
        c: &Connection,
        domain: Domain,
        f: &FactRecord,
        now: Millis,
    ) -> Result<(), CommitError> {
        self.check_topic(&f.topic, domain)?;
        let mapped = (self.topics[&f.topic].adapt)(f)?;
        let exists: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM outbox_events WHERE id=?)",
                [&f.fact_id],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if exists {
            return Err(CommitError::Unique {
                entity: "facts".into(),
                index: "fact_id".into(),
            });
        }
        c.execute("INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created) VALUES(?,?,?,?,?,?)",params![f.fact_id,mapped.workspace_id,f.topic,mapped.version,raw_text(&mapped.payload)?,now.0]).map_err(sql_error)?;
        Ok(())
    }
    pub fn register_op(
        &mut self,
        name: impl Into<String>,
        domain: Domain,
        handler: fn(&Connection, &Row) -> Result<(), CommitError>,
    ) -> Result<(), CommitError> {
        self.add_op(
            name.into(),
            domain,
            Arc::new(move |c, op, _| {
                handler(c, &query::decode(&op.body).map_err(CommitError::Fatal)?)
            }),
        )
    }
    pub fn register_typed_op<O: uow::Op>(
        &mut self,
        handler: fn(&Connection, &O, Rev) -> Result<(), CommitError>,
    ) -> Result<(), CommitError> {
        self.add_op(
            O::NAME.into(),
            O::DOMAIN,
            Arc::new(move |c, op, rev| {
                handler(c, &op.decode::<O>().map_err(CommitError::Fatal)?, rev)
            }),
        )
    }
    fn add_op(
        &mut self,
        name: String,
        domain: Domain,
        run: Arc<Handler>,
    ) -> Result<(), CommitError> {
        if self.operations.contains_key(&name)
            || self
                .operations
                .values()
                .filter(|op| op.domain == domain)
                .count()
                + self.queries.keys().filter(|(d, _)| *d == domain).count()
                >= steve_storage_api::registry::DOMAIN_LIMIT
        {
            return Err(invalid("duplicate operation"));
        }
        self.operations.insert(name, Operation { domain, run });
        Ok(())
    }
    pub fn check_op(&self, op: &AnyOp, domain: Domain) -> Result<(), CommitError> {
        if op.domain != domain {
            return Err(invalid("mixed-domain operation"));
        }
        let r = self
            .operations
            .get(&op.name)
            .ok_or_else(|| unsupported("unknown operation"))?;
        if r.domain != domain {
            return Err(invalid("mixed-domain operation"));
        }
        Ok(())
    }
    pub fn operation_names(&self, domain: Domain) -> impl Iterator<Item = &str> {
        self.operations
            .iter()
            .filter(move |(_, op)| op.domain == domain)
            .map(|(name, _)| name.as_str())
    }
    pub fn operate(&self, c: &Connection, op: &AnyOp, rev: Rev) -> Result<(), CommitError> {
        self.check_op(op, op.domain)?;
        (self.operations[&op.name].run)(c, op, rev)
    }
    /// Explicit opt-in namespace installation, for verification fixtures and fresh declared tables.
    /// Existing legacy tables are never altered or automatically enrolled by this registry.
    pub fn install(&self, c: &Connection) -> Result<(), CommitError> {
        for e in self.entities() {
            sql::install(c, e)?;
        }
        Ok(())
    }
    pub fn install_domain(&self, c: &Connection, domain: Domain) -> Result<(), CommitError> {
        for e in self.entities().filter(|e| e.desc.domain == domain) {
            sql::install(c, e)?;
        }
        Ok(())
    }
    /// Register the production comms operation against the real comms schema.
    /// This does not enroll legacy tables for generic CRUD or full-domain export.
    pub fn register_digest(&mut self) -> Result<(), CommitError> {
        self.register_typed_op(digest::apply)
    }
    pub fn verification() -> Result<Self, CommitError> {
        let mut r = Self::default();
        for e in fixtures::descriptors() {
            let name = e.name.clone();
            r.register(e, name)?;
        }
        for (name, d) in [
            ("verify.core.fact", Domain::Core),
            ("verify.stream.fact", Domain::Stream),
            ("verify.comms.fact", Domain::Comms),
        ] {
            r.register_transfer_topic(
                name,
                d,
                |f| {
                    Ok(OutboxFields {
                        workspace_id: "verify".into(),
                        version: 1,
                        payload: query::encode(f).map_err(CommitError::Fatal)?,
                    })
                },
                |raw| query::decode(raw).map_err(CommitError::Fatal),
            )?;
        }
        for d in [Domain::Core, Domain::Stream, Domain::Comms] {
            r.allow_export(d)?;
        }
        Ok(r)
    }
}
pub(crate) fn raw_text(raw: &RawJson) -> Result<String, CommitError> {
    serde_json::to_string(raw).map_err(|_| invalid("invalid JSON"))
}
pub(crate) fn next_sequence(c: &Connection, domain: Domain, key: &str) -> Result<i64, CommitError> {
    let s: Option<String> = c
        .query_row(
            "SELECT value FROM meta WHERE domain=? AND key=?",
            params![domain_name(domain), key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let old = match s {
        None => 0,
        Some(s) => s
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| invalid("invalid stored sequence"))?,
    };
    let next = old
        .checked_add(1)
        .ok_or_else(|| failure(steve_types::Code::Exhausted, "sequence exhausted"))?;
    c.execute("INSERT INTO meta(domain,key,value) VALUES(?,?,?) ON CONFLICT(domain,key) DO UPDATE SET value=excluded.value",params![domain_name(domain),key,next.to_string()]).map_err(sql_error)?;
    Ok(next)
}
