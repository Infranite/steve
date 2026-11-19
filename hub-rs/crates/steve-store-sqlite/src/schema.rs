//! Final schema for the single-file layout. Cross-domain debt must be cleared before splitting.

pub const CORE: &str = include_str!("../schema/core/0001_baseline.sql");
pub const STREAM: &str = include_str!("../schema/stream/0001_baseline.sql");
pub const COMMS: &str = include_str!("../schema/comms/0001_baseline.sql");
pub const CONTRACT: &str = include_str!("../schema/contract/0001.sql");

use rusqlite::{Connection, types::Value};
use std::collections::BTreeMap;

/// Maximum number of object diagnostics included in a conversion error.
const MAX_DIFFERENCE_DIAGNOSTICS: usize = 8;

pub const CONTRACT_TABLES: [&str; 4] = ["meta", "uow_applied", "cursor", "domain_schema"];
pub const LEGACY_TABLES: [&str; 2] = ["schema_version", "schema_compat"];

/// Logical schema identity; physical root pages and optimizer statistics are excluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub table: String,
    pub sql: Option<String>,
}

/// The same whitespace normalization is used by conversion and equivalence tests.
/// Excluding contracts also excludes their indexes and both retired version tables.
pub fn normalized_structure(
    conn: &Connection,
    exclude_contract: bool,
) -> rusqlite::Result<Vec<SchemaObject>> {
    let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_stat%' ORDER BY type,name")?;
    let objects = stmt.query_map([], |r| {
        let sql: Option<String> = r.get(3)?;
        Ok(SchemaObject {
            kind: r.get(0)?,
            name: r.get(1)?,
            table: r.get(2)?,
            sql: sql.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")),
        })
    })?;
    objects
        .filter_map(|o| match o {
            Ok(o)
                if exclude_contract
                    && (CONTRACT_TABLES.contains(&o.table.as_str())
                        || LEGACY_TABLES.contains(&o.table.as_str())) =>
            {
                None
            }
            other => Some(other),
        })
        .collect()
}

fn pragma_rows(conn: &Connection, pragma: &str, table: &str) -> rusqlite::Result<Vec<Vec<Value>>> {
    let table = table.replace('"', "\"\"");
    let mut stmt = conn.prepare(&format!("PRAGMA {pragma}(\"{table}\")"))?;
    let columns = stmt.column_count();
    let mut rows = stmt
        .query_map([], |r| (0..columns).map(|i| r.get(i)).collect())?
        .collect::<rusqlite::Result<Vec<Vec<Value>>>>()?;
    if pragma == "index_list" {
        // Enumeration position is not index identity or semantics.
        for row in &mut rows {
            row.remove(0);
        }
    }
    rows.sort_by_key(|r| format!("{r:?}"));
    Ok(rows)
}

/// Compare definitions and table/index/FK pragmas, without comparing application data.
/// Diagnostics name objects rather than dumping DDL or rows into an operator's error.
pub fn differences(
    expected: &Connection,
    actual: &Connection,
    exclude_contract: bool,
) -> rusqlite::Result<Vec<String>> {
    let objects = |c| -> rusqlite::Result<BTreeMap<_, _>> {
        Ok(normalized_structure(c, exclude_contract)?
            .into_iter()
            .map(|o| ((o.kind.clone(), o.name.clone()), o))
            .collect())
    };
    let expected_objects = objects(expected)?;
    let actual_objects = objects(actual)?;
    let mut differences = Vec::new();
    for ((kind, name), object) in &expected_objects {
        match actual_objects.get(&(kind.clone(), name.clone())) {
            None => differences.push(format!("missing {kind} {name}")),
            Some(other) if other != object => {
                differences.push(format!("{kind} {name}: definition differs"))
            }
            _ => {}
        }
        if kind == "table" && actual_objects.contains_key(&(kind.clone(), name.clone())) {
            for pragma in ["table_info", "index_list", "foreign_key_list"] {
                if pragma_rows(expected, pragma, name)? != pragma_rows(actual, pragma, name)? {
                    differences.push(format!("{pragma}({name}) differs"));
                }
            }
        }
    }
    for (kind, name) in actual_objects.keys() {
        if !expected_objects.contains_key(&(kind.clone(), name.clone())) {
            differences.push(format!("unexpected {kind} {name}"));
        }
    }
    Ok(differences)
}

/// Validate the legacy head against the embedded single-file baseline. Seeds belong
/// only to the reference database; conversion never overwrites historical data.
pub fn verify_baseline(conn: &Connection) -> Result<(), steve_types::Error> {
    let baseline = Connection::open_in_memory().map_err(crate::open::db_error)?;
    for sql in [CORE, STREAM, COMMS] {
        baseline.execute_batch(sql).map_err(crate::open::db_error)?;
    }
    let differences = differences(&baseline, conn, true).map_err(crate::open::db_error)?;
    if differences.is_empty() {
        Ok(())
    } else {
        Err(crate::open::incompatible(&format!(
            "legacy schema differs from baseline: {}",
            differences
                .into_iter()
                .take(MAX_DIFFERENCE_DIAGNOSTICS)
                .collect::<Vec<_>>()
                .join("; ")
        )))
    }
}
