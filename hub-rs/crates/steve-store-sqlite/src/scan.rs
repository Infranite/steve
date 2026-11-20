//! Descriptor-driven reads. Filters run outside a bounded, ordered index window;
//! the last logically examined key, including PK tie breakers, controls continuation.
use crate::open::db_error;
use rusqlite::{
    Connection, OptionalExtension, params_from_iter,
    types::{Value, ValueRef},
};
use serde::Deserialize;
use steve_storage_api::{
    entity::{FieldDesc, FieldType},
    query::*,
    *,
};
use steve_types::Code;

pub(crate) fn invalid(detail: &str) -> Error {
    error(Code::Invalid, detail)
}
fn incompatible(detail: &str) -> Error {
    error(Code::Incompatible, detail)
}
/// Quote even trusted descriptor identifiers; never interpolate caller-supplied SQL.
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub(crate) fn field<'a>(e: &'a EntityDesc, name: &str) -> Result<&'a FieldDesc, Error> {
    e.fields
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| error(Code::Unsupported, "unknown field"))
}
/// Validate the resolved shape before building SQL. `rev` is a backend-owned column.
pub fn validate_descriptor(e: &EntityDesc) -> Result<(), Error> {
    let names: std::collections::BTreeSet<_> = e.fields.iter().map(|f| &f.name).collect();
    let indexes: std::collections::BTreeSet<_> = e.indexes.iter().map(|i| &i.name).collect();
    if e.name.is_empty()
        || e.key.is_empty()
        || names.len() != e.fields.len()
        || names
            .iter()
            .any(|n| n.is_empty() || n.as_str() == "rev" || n.contains('\0'))
        || e.name.contains('\0')
        || e.key.iter().any(|k| !names.contains(k))
        || indexes.len() != e.indexes.len()
        || e.indexes.iter().any(|i| {
            i.name.is_empty() || i.fields.is_empty() || i.fields.iter().any(|k| !names.contains(k))
        })
    {
        return Err(invalid("invalid entity descriptor"));
    }
    Ok(())
}
/// Bind contract scalars without passing opaque JSON through a JSON tree.
pub fn bind_cell(f: &FieldDesc, value: &Cell) -> Result<Value, Error> {
    value.validate()?;
    match (&f.ty, value) {
        (_, Cell::Null) if f.nullable => Ok(Value::Null),
        (FieldType::Int, Cell::Int(v)) | (FieldType::Millis, Cell::Millis(v)) => {
            Ok(Value::Integer(*v))
        }
        (FieldType::Bool, Cell::Bool(v)) => Ok(Value::Integer(i64::from(*v))),
        (FieldType::Real, Cell::Real(v)) => Ok(Value::Real(*v)),
        (FieldType::Bytes, Cell::Bytes(v)) => Ok(Value::Blob(v.clone())),
        (FieldType::Text, Cell::Text(v)) => Ok(Value::Text(v.clone())),
        (FieldType::Enum(codes), Cell::Text(v)) => {
            if codes.contains(v) {
                Ok(Value::Text(v.clone()))
            } else {
                Err(incompatible("unknown enum code"))
            }
        }
        (FieldType::Json, Cell::Json(v)) => Ok(Value::Text(
            serde_json::to_string(v).map_err(|_| invalid("invalid JSON"))?,
        )),
        _ => Err(invalid("invalid cell type or nullability")),
    }
}
fn read_cell(f: &FieldDesc, value: ValueRef<'_>) -> Result<Cell, Error> {
    let bad = || incompatible("stored cell does not match descriptor");
    Ok(match (&f.ty, value) {
        (_, ValueRef::Null) if f.nullable => Cell::Null,
        (FieldType::Int, ValueRef::Integer(v)) => Cell::Int(v),
        (FieldType::Millis, ValueRef::Integer(v)) => Cell::Millis(v),
        (FieldType::Bool, ValueRef::Integer(v @ (0 | 1))) => Cell::Bool(v == 1),
        (FieldType::Real, ValueRef::Real(v)) if v.is_finite() => Cell::Real(v),
        (FieldType::Real, ValueRef::Integer(v)) => Cell::Real(v as f64),
        (FieldType::Bytes, ValueRef::Blob(v)) => Cell::Bytes(v.to_vec()),
        (FieldType::Text | FieldType::Enum(_), ValueRef::Text(v)) => {
            let text = std::str::from_utf8(v).map_err(|_| bad())?.to_owned();
            if let FieldType::Enum(codes) = &f.ty
                && !codes.contains(&text)
            {
                return Err(incompatible("unknown enum code"));
            }
            Cell::Text(text)
        }
        (FieldType::Json, ValueRef::Text(v)) => Cell::Json(
            RawJson::from_str(std::str::from_utf8(v).map_err(|_| bad())?).map_err(|_| bad())?,
        ),
        _ => return Err(bad()),
    })
}
pub(crate) fn columns(e: &EntityDesc) -> String {
    e.fields
        .iter()
        .map(|f| ident(&f.name))
        .chain(std::iter::once(ident("rev")))
        .collect::<Vec<_>>()
        .join(",")
}
pub(crate) fn read_row(row: &rusqlite::Row<'_>, e: &EntityDesc) -> Result<(Row, Rev), Error> {
    let cells = e
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            Ok((
                f.name.clone(),
                read_cell(f, row.get_ref(i).map_err(db_error)?)?,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let rev: i64 = row.get(e.fields.len()).map_err(db_error)?;
    Ok((
        Row(cells),
        Rev(u64::try_from(rev).map_err(|_| incompatible("negative stored revision"))?),
    ))
}
pub(crate) fn values(row: &Row, fields: &[String]) -> Vec<Cell> {
    fields
        .iter()
        .map(|name| {
            row.0
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap_or(Cell::Null)
        })
        .collect()
}
pub(crate) fn index_fields(e: &EntityDesc, index: &str) -> Result<Vec<String>, Error> {
    let mut fields = if index.is_empty() {
        e.key.clone()
    } else {
        e.indexes
            .iter()
            .find(|i| i.name == index)
            .ok_or_else(|| invalid("undeclared index"))?
            .fields
            .clone()
    };
    for k in &e.key {
        if !fields.contains(k) {
            fields.push(k.clone());
        }
    }
    Ok(fields)
}
fn column(e: &EntityDesc, name: &str) -> Result<String, Error> {
    let f = field(e, name)?;
    Ok(
        if matches!(f.ty, FieldType::Text | FieldType::Enum(_) | FieldType::Json) {
            format!("{} COLLATE BINARY", ident(name))
        } else {
            ident(name)
        },
    )
}
pub(crate) fn ordering(e: &EntityDesc, fields: &[String], order: Order) -> Result<String, Error> {
    fields
        .iter()
        .map(|f| {
            Ok(format!(
                "{} {}",
                column(e, f)?,
                if order == Order::Asc { "ASC" } else { "DESC" }
            ))
        })
        .collect::<Result<Vec<_>, Error>>()
        .map(|v| v.join(","))
}
#[derive(Default)]
pub(crate) struct Bindings(pub Vec<Value>);
impl Bindings {
    fn push(&mut self, value: Value) -> String {
        self.0.push(value);
        format!("?{}", self.0.len())
    }
    /// Lexicographic comparison with NULL first, unlike SQLite row-value comparisons
    /// where any NULL can produce UNKNOWN. Prefix bounds compare exactly that prefix.
    pub(crate) fn compare(
        &mut self,
        e: &EntityDesc,
        fields: &[String],
        key: &[Cell],
        op: &str,
        prefix: bool,
    ) -> Result<String, Error> {
        if key.is_empty() || key.len() > fields.len() || (!prefix && key.len() != fields.len()) {
            return Err(invalid("invalid key arity"));
        }
        if !matches!(op, "=" | ">" | ">=" | "<" | "<=") {
            return Err(invalid("invalid comparison"));
        }
        // Keep ordinary continuations sargable: SQLite can seek a composite index
        // directly with a row-value bound. NULL-aware lower comparisons need the
        // explicit expansion below because SQL considers NULL < value UNKNOWN.
        if !key.contains(&Cell::Null)
            && (matches!(op, ">" | ">=" | "=")
                || fields
                    .iter()
                    .take(key.len())
                    .all(|n| field(e, n).is_ok_and(|f| !f.nullable)))
        {
            let mut cs = Vec::new();
            let mut ps = Vec::new();
            for (name, value) in fields.iter().zip(key) {
                cs.push(column(e, name)?);
                ps.push(self.push(bind_cell(field(e, name)?, value)?));
            }
            return Ok(if key.len() == 1 {
                format!("({} {op} {})", cs[0], ps[0])
            } else {
                format!("(({}) {op} ({}))", cs.join(","), ps.join(","))
            });
        }
        let mut equal = Vec::new();
        let mut terms = Vec::new();
        for (name, value) in fields.iter().zip(key) {
            if e.key.contains(name) && *value == Cell::Null {
                return Err(invalid("null primary key"));
            }
            let p = self.push(bind_cell(field(e, name)?, value)?);
            let c = column(e, name)?;
            let cmp = match op {
                ">" | ">=" => format!("(({c} IS NOT NULL AND {p} IS NULL) OR {c}>{p})"),
                "<" | "<=" => format!("(({c} IS NULL AND {p} IS NOT NULL) OR {c}<{p})"),
                "=" => "0".into(),
                _ => return Err(invalid("invalid comparison")),
            };
            let mut and = equal.clone();
            and.push(cmp);
            terms.push(format!("({})", and.join(" AND ")));
            equal.push(format!("{c} IS {p}"));
        }
        if matches!(op, "=" | ">=" | "<=") {
            terms.push(format!("({})", equal.join(" AND ")));
        }
        Ok(format!("({})", terms.join(" OR ")))
    }
    fn predicates(&mut self, e: &EntityDesc, preds: &[Pred]) -> Result<String, Error> {
        let mut terms = Vec::new();
        for pred in preds {
            let (name, vals, op) = match pred {
                Pred::Eq { field, value } => (field, vec![value], "="),
                Pred::Ne { field, value } => (field, vec![value], "<>"),
                Pred::Lt { field, value } => (field, vec![value], "<"),
                Pred::Le { field, value } => (field, vec![value], "<="),
                Pred::Gt { field, value } => (field, vec![value], ">"),
                Pred::Ge { field, value } => (field, vec![value], ">="),
                Pred::In { field, values } => (field, values.iter().collect(), "IN"),
                Pred::IsNull { field, is_null } => {
                    terms.push(format!(
                        "{} IS {}NULL",
                        column(e, field)?,
                        if *is_null { "" } else { "NOT " }
                    ));
                    continue;
                }
            };
            let c = column(e, name)?;
            let mut ps = Vec::new();
            for value in vals {
                if *value == Cell::Null {
                    return Err(invalid("null predicates require IsNull"));
                }
                ps.push(self.push(bind_cell(field(e, name)?, value)?));
            }
            terms.push(if op == "IN" {
                format!("{c} IN ({})", ps.join(","))
            } else {
                format!("{c}{op}{}", ps[0])
            });
        }
        Ok(if terms.is_empty() {
            "1".into()
        } else {
            terms.join(" AND ")
        })
    }
    pub(crate) fn range(
        &mut self,
        e: &EntityDesc,
        fields: &[String],
        range: &KeyRange,
    ) -> Result<String, Error> {
        let mut terms = vec!["1".into()];
        for (bound, lower) in [(&range.start, true), (&range.end, false)] {
            match bound {
                KeyBound::Unbounded => {}
                KeyBound::Included(v) | KeyBound::Excluded(v) => {
                    let op = match (lower, matches!(bound, KeyBound::Included(_))) {
                        (true, true) => ">=",
                        (true, false) => ">",
                        (false, true) => "<=",
                        (false, false) => "<",
                    };
                    terms.push(self.compare(e, fields, v, op, true)?);
                }
            }
        }
        Ok(terms.join(" AND "))
    }
}
/// A read savepoint pins all continuation/endpoint probes to the same SQLite snapshot.
/// Nested use is safe; it never commits or rolls back the caller's outer transaction.
pub(crate) fn atomic_read<T>(
    conn: &Connection,
    read: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    conn.execute_batch("SAVEPOINT storage_read")
        .map_err(db_error)?;
    let guard = ReadGuard { conn, done: false };
    let result = read();
    match result {
        Ok(v) => {
            guard.finish()?;
            Ok(v)
        }
        Err(e) => {
            drop(guard);
            Err(e)
        }
    }
}
// RAII savepoints also roll back on unwinding, without requiring &mut Connection.
struct ReadGuard<'a> {
    conn: &'a Connection,
    done: bool,
}
impl ReadGuard<'_> {
    fn finish(mut self) -> Result<(), Error> {
        self.conn
            .execute_batch("RELEASE storage_read")
            .map_err(db_error)?;
        self.done = true;
        Ok(())
    }
}
impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self
                .conn
                .execute_batch("ROLLBACK TO storage_read; RELEASE storage_read");
        }
    }
}

#[derive(Deserialize)]
struct Lookup {
    key: Key<Row>,
}
#[derive(Deserialize)]
struct Window {
    index: IndexName,
    range: KeyRange,
    filter: Vec<Pred>,
    budget: ScanBudget,
    #[serde(default)]
    order: Option<Order>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    resume: Option<ResumeToken>,
}
/// Execute a generic wire query on a resolved entity. Descriptor-only results contain
/// Row; a typed facade can use query_with_codec to retain its domain-owned row mapping.
pub fn query(conn: &Connection, e: &EntityDesc, q: AnyQuery) -> Result<AnyOutput, Error> {
    query_with_codec(conn, e, q, |r| encode(&r))
}
/// Domain row codecs run before the snapshot is released; no registry lookup occurs here.
pub fn query_with_codec(
    conn: &Connection,
    e: &EntityDesc,
    q: AnyQuery,
    codec: impl Fn(Row) -> Result<RawJson, Error>,
) -> Result<AnyOutput, Error> {
    validate_descriptor(e)?;
    if q.domain != e.domain {
        return Err(invalid("mixed-domain query"));
    }
    let body = atomic_read(conn, || match q.name.as_str() {
        "get" | "exists" => {
            let request: Lookup = q.body.decode()?;
            if request.key.entity != e.name {
                return Err(invalid("query entity mismatch"));
            }
            let mut bindings = Bindings::default();
            let clause = bindings.compare(e, &e.key, &request.key.values, "=", false)?;
            let sql = format!(
                "SELECT {} FROM {} WHERE {} LIMIT 1",
                columns(e),
                ident(&e.name),
                clause
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(db_error)?;
            let mut rows = stmt
                .query(params_from_iter(&bindings.0))
                .map_err(db_error)?;
            let found = rows
                .next()
                .map_err(db_error)?
                .map(|r| read_row(r, e))
                .transpose()?;
            if q.name == "exists" {
                encode(&found.is_some())
            } else {
                encode(
                    &found
                        .map(|(r, v)| Ok::<_, Error>((codec(r)?, v)))
                        .transpose()?,
                )
            }
        }
        "scan" | "count" => window_query(conn, e, &q, codec),
        _ => Err(error(Code::Unsupported, "unknown generic query")),
    })?;
    Ok(AnyOutput {
        name: q.name,
        domain: q.domain,
        body: Payload::Json(body),
    })
}
type ExaminedRow = ((Row, Rev), i64);

fn endpoint(
    conn: &Connection,
    e: &EntityDesc,
    window: &str,
    params: &[Value],
    fields: &[String],
    order: Order,
    ordinal: &str,
) -> Result<Option<ExaminedRow>, Error> {
    let reverse = ordering(
        e,
        fields,
        if order == Order::Asc {
            Order::Desc
        } else {
            Order::Asc
        },
    )?;
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {},{ordinal} FROM ({window}) w ORDER BY {reverse} LIMIT 1",
            columns(e)
        ))
        .map_err(db_error)?;
    let mut rows = stmt.query(params_from_iter(params)).map_err(db_error)?;
    rows.next()
        .map_err(db_error)?
        .map(|r| {
            Ok((
                read_row(r, e)?,
                r.get::<_, i64>(e.fields.len() + 1).map_err(db_error)?,
            ))
        })
        .transpose()
}
pub(crate) fn has_more(
    conn: &Connection,
    e: &EntityDesc,
    fields: &[String],
    range: &KeyRange,
    order: Order,
    key: &[Cell],
) -> Result<bool, Error> {
    let mut b = Bindings::default();
    let range = b.range(e, fields, range)?;
    let after = b.compare(
        e,
        fields,
        key,
        if order == Order::Asc { ">" } else { "<" },
        false,
    )?;
    conn.query_row(
        &format!(
            "SELECT 1 FROM {} WHERE {range} AND {after} LIMIT 1",
            ident(&e.name)
        ),
        params_from_iter(&b.0),
        |_| Ok(true),
    )
    .optional()
    .map(|v| v.unwrap_or(false))
    .map_err(db_error)
}

struct WindowSql {
    fields: Vec<String>,
    order: Order,
    ordinal_name: String,
    window: String,
    window_params: Vec<Value>,
    b: Bindings,
    filter: String,
    limit: u32,
}
impl WindowSql {
    fn new(e: &EntityDesc, w: &Window) -> Result<Self, Error> {
        if w.index.entity != e.name {
            return Err(invalid("query entity mismatch"));
        }
        let limit = w.limit.unwrap_or(500);
        if w.budget.0 == 0 || !(1..=500).contains(&limit) {
            return Err(invalid("invalid scan limit or budget"));
        }
        let fields = index_fields(e, &w.index.name)?;
        let order = w.order.unwrap_or(Order::Asc);
        let mut b = Bindings::default();
        let mut clause = b.range(e, &fields, &w.range)?;
        if let Some(resume) = &w.resume {
            clause.push_str(" AND ");
            clause.push_str(&b.compare(
                e,
                &fields,
                &resume.key,
                if order == Order::Asc { ">" } else { "<" },
                false,
            )?);
        }
        let sort = ordering(e, &fields, order)?;
        let mut ordinal_name = String::from("__storage_scan_ordinal");
        while e.fields.iter().any(|f| f.name == ordinal_name) {
            ordinal_name.push('_');
        }
        let ordinal_name = ident(&ordinal_name);
        let window = format!(
            "SELECT {},ROW_NUMBER() OVER (ORDER BY {sort}) AS {ordinal_name} FROM {} WHERE {clause} ORDER BY {sort} LIMIT {}",
            columns(e),
            ident(&e.name),
            w.budget.0
        );
        let window_params = b.0.clone();
        let filter = b.predicates(e, &w.filter)?;
        Ok(Self {
            fields,
            order,
            ordinal_name,
            window,
            window_params,
            b,
            filter,
            limit,
        })
    }
}
fn window_query(
    conn: &Connection,
    e: &EntityDesc,
    q: &AnyQuery,
    codec: impl Fn(Row) -> Result<RawJson, Error>,
) -> Result<RawJson, Error> {
    let w: Window = q.body.decode()?;
    let WindowSql {
        fields,
        order,
        ordinal_name,
        window,
        window_params,
        b,
        filter,
        limit,
    } = WindowSql::new(e, &w)?;
    if q.name == "count" {
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM ({window}) w WHERE {filter}"),
                params_from_iter(&b.0),
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let endpoint = endpoint(
            conn,
            e,
            &window,
            &window_params,
            &fields,
            order,
            &ordinal_name,
        )?;
        let more = if let Some(((r, _), _)) = endpoint {
            has_more(conn, e, &fields, &w.range, order, &values(&r, &fields))?
        } else {
            false
        };
        encode(&if more {
            CountResult::AtLeast(count as u64)
        } else {
            CountResult::Exact(count as u64)
        })
    } else {
        let sql = format!(
            "SELECT {},{ordinal_name} FROM ({window}) w WHERE {filter} LIMIT {limit}",
            columns(e)
        );
        let mut stmt = conn.prepare_cached(&sql).map_err(db_error)?;
        let mut rows = stmt.query(params_from_iter(&b.0)).map_err(db_error)?;
        let mut found = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            found.push((
                read_row(row, e)?,
                row.get::<_, i64>(e.fields.len() + 1).map_err(db_error)?,
            ));
        }
        let endpoint = if found.len() == limit as usize {
            found.last().cloned()
        } else {
            endpoint(
                conn,
                e,
                &window,
                &window_params,
                &fields,
                order,
                &ordinal_name,
            )?
        };
        let mut resume = None;
        let mut exhausted_budget = false;
        if let Some(((r, _), inspected)) = endpoint {
            let key = values(&r, &fields);
            if has_more(conn, e, &fields, &w.range, order, &key)? {
                // Do not traverse the rest of the window to count the ordinal
                // after limit has stopped us; ROW_NUMBER travels with the row.
                exhausted_budget = inspected == i64::from(w.budget.0);
                resume = Some(ResumeToken { key });
            }
        }
        encode(&Page {
            rows: found
                .into_iter()
                .map(|((r, v), _)| Ok((codec(r)?, v)))
                .collect::<Result<Vec<_>, Error>>()?,
            resume,
            exhausted_budget,
        })
    }
}
