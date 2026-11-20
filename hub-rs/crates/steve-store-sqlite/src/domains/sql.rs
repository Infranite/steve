use super::*;
use rusqlite::types::ValueRef;
use steve_storage_api::query::{KeyBound, KeyRange, Pred};

pub fn cell_value(v: &Cell) -> Result<Value, CommitError> {
    v.validate().map_err(CommitError::Fatal)?;
    Ok(match v {
        Cell::Null => Value::Null,
        Cell::Int(n) | Cell::Millis(n) => Value::Integer(*n),
        Cell::Bool(b) => Value::Integer(i64::from(*b)),
        Cell::Real(n) => Value::Real(*n),
        Cell::Text(s) => Value::Text(s.clone()),
        Cell::Bytes(b) => Value::Blob(b.clone()),
        Cell::Json(j) => Value::Text(raw_text(j)?),
    })
}
pub fn read_cell(v: ValueRef<'_>, f: &FieldDesc) -> Result<Cell, CommitError> {
    let out = match (&f.ty, v) {
        (_, ValueRef::Null) => Cell::Null,
        (FieldType::Int, ValueRef::Integer(n)) => Cell::Int(n),
        (FieldType::Millis, ValueRef::Integer(n)) => Cell::Millis(n),
        (FieldType::Bool, ValueRef::Integer(n)) if n == 0 || n == 1 => Cell::Bool(n == 1),
        (FieldType::Real, ValueRef::Real(n)) => Cell::Real(n),
        (FieldType::Real, ValueRef::Integer(n)) => Cell::Real(n as f64),
        (FieldType::Text | FieldType::Enum(_), ValueRef::Text(s)) => Cell::Text(
            std::str::from_utf8(s)
                .map_err(|_| invalid("stored text is not UTF-8"))?
                .into(),
        ),
        (FieldType::Json, ValueRef::Text(s)) => Cell::Json(
            RawJson::from_str(
                std::str::from_utf8(s).map_err(|_| invalid("stored JSON is not UTF-8"))?,
            )
            .map_err(|_| invalid("invalid stored JSON"))?,
        ),
        (FieldType::Bytes, ValueRef::Blob(s)) => Cell::Bytes(s.to_vec()),
        _ => return Err(invalid("stored field has wrong type")),
    };
    check_cell(f, &out)?;
    Ok(out)
}
pub(super) fn equal(fields: &[String]) -> String {
    fields
        .iter()
        .map(|f| format!("{} IS ?", quote(f)))
        .collect::<Vec<_>>()
        .join(" AND ")
}
pub(super) fn install(c: &Connection, e: &SqlEntity) -> Result<(), CommitError> {
    let mut cols = Vec::new();
    for f in &e.desc.fields {
        let ty = match f.ty {
            FieldType::Int | FieldType::Millis | FieldType::Bool => "INTEGER",
            FieldType::Real => "REAL",
            FieldType::Bytes => "BLOB",
            _ => "TEXT COLLATE BINARY",
        };
        cols.push(format!(
            "{} {ty}{}",
            quote(&f.name),
            if !f.nullable || e.desc.key.contains(&f.name) {
                " NOT NULL"
            } else {
                ""
            }
        ));
    }
    cols.push("rev INTEGER NOT NULL".into());
    cols.push(format!(
        "PRIMARY KEY({})",
        e.desc
            .key
            .iter()
            .map(|k| quote(k))
            .collect::<Vec<_>>()
            .join(",")
    ));
    c.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {}({})",
        quote(&e.table),
        cols.join(",")
    ))
    .map_err(sql_error)?;
    for i in &e.desc.indexes {
        c.execute_batch(&format!(
            "CREATE {}INDEX IF NOT EXISTS {} ON {}({})",
            if i.unique { "UNIQUE " } else { "" },
            quote(&format!("{}_{}", e.table, i.name)),
            quote(&e.table),
            i.fields
                .iter()
                .map(|f| quote(f))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .map_err(sql_error)?;
    }
    Ok(())
}
fn cmp(a: &Cell, b: &Cell) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Cell::Null, Cell::Null) => Equal,
        (Cell::Null, _) => Less,
        (_, Cell::Null) => Greater,
        (Cell::Int(a), Cell::Int(b)) | (Cell::Millis(a), Cell::Millis(b)) => a.cmp(b),
        (Cell::Real(a), Cell::Real(b)) => a.partial_cmp(b).unwrap_or(Equal),
        (Cell::Text(a), Cell::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Cell::Bytes(a), Cell::Bytes(b)) => a.cmp(b),
        (Cell::Bool(a), Cell::Bool(b)) => a.cmp(b),
        (Cell::Json(a), Cell::Json(b)) => raw_text(a)
            .unwrap_or_default()
            .cmp(&raw_text(b).unwrap_or_default()),
        _ => format!("{a:?}").cmp(&format!("{b:?}")),
    }
}
fn key_cmp(a: &[Cell], b: &[Cell]) -> std::cmp::Ordering {
    a.iter()
        .zip(b)
        .map(|(a, b)| cmp(a, b))
        .find(|o| *o != std::cmp::Ordering::Equal)
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}
pub(super) fn delete_where(
    e: &SqlEntity,
    c: &Connection,
    index: &str,
    range: &KeyRange,
    filter: &[Pred],
    limit: u32,
) -> Result<(), CommitError> {
    if !(1..=500).contains(&limit) {
        return Err(invalid("invalid delete limit"));
    }
    let fields = e.index_fields(index)?;
    for b in [&range.start, &range.end] {
        if let KeyBound::Included(k) | KeyBound::Excluded(k) = b {
            e.check_key(&fields, k, true)?;
        }
    }
    for p in filter {
        let (field, vs) = match p {
            Pred::Eq { field, value }
            | Pred::Ne { field, value }
            | Pred::Lt { field, value }
            | Pred::Le { field, value }
            | Pred::Gt { field, value }
            | Pred::Ge { field, value } => (field, vec![value]),
            Pred::In { field, values } => (field, values.iter().collect()),
            Pred::IsNull { field, .. } => (field, vec![]),
        };
        e.field(field)?;
        for v in vs {
            e.check_cell(field, v)?;
            if *v == Cell::Null {
                return Err(invalid("null predicate requires IsNull"));
            }
        }
    }
    // Bounded output, streaming index traversal. This is deletion, not scan budget semantics.
    let keys: Vec<Vec<Cell>> = {
        let sql = format!(
            "SELECT {} FROM {} ORDER BY {}",
            e.desc
                .fields
                .iter()
                .map(|f| quote(&f.name))
                .collect::<Vec<_>>()
                .join(","),
            quote(&e.table),
            fields
                .iter()
                .map(|f| quote(f))
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut stmt = c.prepare_cached(&sql).map_err(sql_error)?;
        let mut cursor = stmt.query([]).map_err(sql_error)?;
        let mut keys = Vec::new();
        while let Some(r) = cursor.next().map_err(sql_error)? {
            let row = Row(e
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
                .collect::<Result<_, CommitError>>()?);
            let key = values(&row, &fields);
            if !accept_bound(&key, &range.start, true) || !accept_bound(&key, &range.end, false) {
                continue;
            }
            let matches = matches_filter(&row, filter);
            if matches {
                keys.push(values(&row, &e.desc.key));
                if keys.len() == limit as usize {
                    break;
                }
            }
        }
        keys
    };
    for key in keys {
        e.delete(c, &key)?;
    }
    Ok(())
}

fn matches_filter(row: &Row, filter: &[Pred]) -> bool {
    filter.iter().all(|p| {
        let test = |field: &str, v: &Cell, ok: fn(std::cmp::Ordering) -> bool| {
            value(row, field)
                .is_some_and(|old| *old != Cell::Null && *v != Cell::Null && ok(cmp(old, v)))
        };
        match p {
            Pred::Eq { field, value } => test(field, value, |o| o.is_eq()),
            Pred::Ne { field, value } => test(field, value, |o| !o.is_eq()),
            Pred::Lt { field, value } => test(field, value, |o| o.is_lt()),
            Pred::Le { field, value } => test(field, value, |o| !o.is_gt()),
            Pred::Gt { field, value } => test(field, value, |o| o.is_gt()),
            Pred::Ge { field, value } => test(field, value, |o| !o.is_lt()),
            Pred::In { field, values } => values.iter().any(|v| test(field, v, |o| o.is_eq())),
            Pred::IsNull { field, is_null } => {
                value(row, field).is_some_and(|v| (*v == Cell::Null) == *is_null)
            }
        }
    })
}

fn accept_bound(key: &[Cell], bound: &KeyBound, start: bool) -> bool {
    match bound {
        KeyBound::Unbounded => true,
        KeyBound::Included(b) => {
            let n = key_cmp(&key[..b.len()], b);
            if start { !n.is_lt() } else { !n.is_gt() }
        }
        KeyBound::Excluded(b) => {
            let n = key_cmp(&key[..b.len()], b);
            if start { n.is_gt() } else { n.is_lt() }
        }
    }
}
