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

#[derive(Debug)]
struct DdlToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    quoted: bool,
}

/// Lexical boundaries only: quotes and comments must never introduce delimiters.
fn ddl_tokens(sql: &str) -> Option<Vec<DdlToken<'_>>> {
    let b = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            i += sql[i..].find("*/")? + 2;
            continue;
        }
        let start = i;
        let quoted = b"'\"`[".contains(&b[i]);
        if quoted {
            let close = if b[i] == b'[' { b']' } else { b[i] };
            i += 1;
            loop {
                let c = *b.get(i)?;
                i += 1;
                if c == close {
                    if b.get(i) == Some(&close) {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 {
            i += 1;
            while b
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c >= 128)
            {
                i += 1;
            }
        } else {
            i += 1;
        }
        tokens.push(DdlToken {
            text: &sql[start..i],
            start,
            end: i,
            quoted,
        });
    }
    Some(tokens)
}

fn whitespace(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Ordinary table equality ignores physical column order. Every declaration uses
/// the same whitespace normalization as the header/suffix and other objects.
/// Constraints remain ordered; virtual tables stay strict.
/// This is comparison data, not SQL to execute or a request to rebuild a table.
fn table_identity(sql: &str) -> Option<String> {
    let tokens = ddl_tokens(sql)?;
    if !tokens.first()?.text.eq_ignore_ascii_case("CREATE")
        || !tokens.get(1)?.text.eq_ignore_ascii_case("TABLE")
    {
        return None;
    }
    let open = tokens.iter().position(|t| !t.quoted && t.text == "(")?;
    let mut depth = 1;
    let mut start = tokens[open].end;
    let mut declarations = Vec::new();
    let mut close = None;
    for t in &tokens[open + 1..] {
        if t.quoted {
            continue;
        }
        match t.text {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    declarations.push(sql[start..t.start].trim());
                    close = Some(t);
                    break;
                }
            }
            "," if depth == 1 => {
                declarations.push(sql[start..t.start].trim());
                start = t.end;
            }
            _ => {}
        }
    }
    let close = close?;
    let mut columns = BTreeMap::new();
    let mut constraints = Vec::new();
    for declaration in declarations {
        let tokens = ddl_tokens(declaration)?;
        let first = tokens.first()?;
        if !first.quoted
            && ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
                .iter()
                .any(|word| first.text.eq_ignore_ascii_case(word))
        {
            constraints.push(whitespace(declaration));
        } else {
            let name = if first.quoted {
                let quote = first.text.chars().next()?;
                let end = if quote == '[' { ']' } else { quote };
                first.text[1..first.text.len() - 1]
                    .replace(&format!("{end}{end}"), &end.to_string())
            } else {
                first.text.to_owned()
            };
            if columns.insert(name, whitespace(declaration)).is_some() {
                return None;
            }
        }
    }
    Some(
        serde_json::to_string(&(
            whitespace(&sql[..tokens[open].start]),
            columns,
            constraints,
            whitespace(&sql[close.end..]),
        ))
        .expect("DDL strings are serializable"),
    )
}

/// Tables compare complete whitespace-normalized declarations by name. Columns,
/// constraints, header, suffix and other objects share one normalization rule.
/// sql is a comparison representation only.
/// Excluding contracts also excludes their indexes and both retired version tables.
pub fn normalized_structure(
    conn: &Connection,
    exclude_contract: bool,
) -> rusqlite::Result<Vec<SchemaObject>> {
    let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_stat%' ORDER BY type,name")?;
    let objects = stmt.query_map([], |r| {
        let sql: Option<String> = r.get(3)?;
        let kind: String = r.get(0)?;
        let sql = sql.map(|s| {
            if kind == "table" {
                table_identity(&s).unwrap_or_else(|| whitespace(&s))
            } else {
                whitespace(&s)
            }
        });
        Ok(SchemaObject {
            kind,
            name: r.get(1)?,
            table: r.get(2)?,
            sql,
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
    if matches!(pragma, "index_list" | "table_info") {
        // Enumeration position / physical cid is not logical schema identity.
        for row in &mut rows {
            row.remove(0);
        }
    }
    if pragma == "foreign_key_list" {
        for row in &mut rows {
            // SQLite assigns FK ids in declaration order. Compare the actual
            // target/columns/actions instead; retain composite-column sequence.
            row.remove(0);
            row.rotate_left(1); // table, from, to, actions, match, seq
        }
    }
    rows.sort_by_key(|r| match (pragma, r.first()) {
        ("table_info", Some(Value::Text(name))) => name.clone(),
        _ => format!("{r:?}"),
    });
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
