#![allow(dead_code)] // Shared by independent integration test binaries.

use rusqlite::{Connection, types::Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const DOMAINS: [&str; 3] = ["core", "stream", "comms"];
pub const CONTRACT_TABLES: [&str; 4] = ["meta", "uow_applied", "cursor", "domain_schema"];
pub const LEGACY_TABLES: [&str; 2] = ["schema_version", "schema_compat"];
pub type Domains = BTreeMap<String, String>;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schema")
}

pub fn domains() -> Domains {
    std::fs::read_to_string(root().join("domains.tsv"))
        .unwrap()
        .lines()
        .filter(|s| !s.starts_with('#') && !s.is_empty())
        .map(|s| {
            let (table, domain) = s.split_once('\t').unwrap();
            assert!(DOMAINS.contains(&domain), "unmapped domain: {s}");
            (table.to_owned(), domain.to_owned())
        })
        .collect()
}

pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

pub fn rows(c: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut s = c.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    let n = s.column_count();
    s.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

pub fn strings(c: &Connection, sql: &str) -> Vec<String> {
    c.prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Object {
    pub kind: String,
    pub name: String,
    pub table: String,
    pub sql: String,
}

pub fn shadows(c: &Connection) -> BTreeSet<String> {
    strings(c, "SELECT name FROM pragma_table_list WHERE type='shadow'")
        .into_iter()
        .collect()
}

/// Export SQLite's own stored DDL. Virtual tables own their shadow objects.
pub fn objects(c: &Connection) -> Vec<Object> {
    let shadows = shadows(c);
    c.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
        ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'trigger' THEN 2 ELSE 3 END,name")
        .unwrap().query_map([], |r| Ok(Object { kind:r.get(0)?, name:r.get(1)?, table:r.get(2)?, sql:r.get(3)? }))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap().into_iter()
        .filter(|o| !shadows.contains(&o.table) && !LEGACY_TABLES.contains(&o.table.as_str())).collect()
}

pub(crate) mod legacy_input;

/// Fresh legacy installations and upgraded legacy installations have two supported
/// physical column orders. The history fixture keeps the latter; baseline export
/// uses the former, without replaying any archived pre-113 migration.
fn fresh_legacy(conn: &Connection, version: i64) {
    const FACTS: &str = "phase TEXT NOT NULL DEFAULT 'active', cycle INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 0, outcome TEXT, acceptance TEXT, closing TEXT, closed_at INTEGER, closed_reason TEXT";
    let fixture = include_str!("../../legacy/113_fixture.sql");
    let old = format!("{FACTS}, design_gate INTEGER NOT NULL DEFAULT 0");
    assert_eq!(fixture.matches(&old).count(), 1);
    let fixture = fixture.replace(
        &old,
        &format!("design_gate INTEGER NOT NULL DEFAULT 0, {FACTS}"),
    );
    let old = "import_key TEXT, reasoning_effort TEXT, first_tool_at INTEGER";
    assert_eq!(fixture.matches(old).count(), 1);
    let fixture = fixture.replace(
        old,
        "import_key TEXT, first_tool_at INTEGER, reasoning_effort TEXT",
    );
    legacy_input::load(conn, &fixture, version);
}

pub fn legacy() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    fresh_legacy(&c, 114);
    c
}

pub fn baseline() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    for domain in DOMAINS {
        c.execute_batch(
            &std::fs::read_to_string(root().join(domain).join("0001_baseline.sql")).unwrap(),
        )
        .unwrap_or_else(|e| panic!("{domain}: {e}"));
    }
    c.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    c
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Debt {
    pub kind: String,
    pub id: String,
    pub description: String,
    pub object: String,
}

impl Debt {
    pub fn line(&self) -> String {
        format!("{}\t{}\t{}\n", self.kind, self.id, self.description)
    }
}

fn crosses(cut: &str, a: &str, b: &str) -> bool {
    match cut {
        "stream" => (a == "stream") != (b == "stream"),
        "comms" => (a == "comms" && b == "core") || (a == "core" && b == "comms"),
        _ => panic!("unknown cut {cut}"),
    }
}

/// SQLite identifier tokens, excluding string literals and comments. Quoted identifiers are kept.
/// This deliberately handles FROM/JOIN/UPDATE/INTO references rather than matching table substrings.
fn tokens(sql: &str) -> Vec<String> {
    let s: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let start = i;
        let ch = s[i];
        i += 1;
        if ch == '-' && s.get(i) == Some(&'-') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && s.get(i) == Some(&'*') {
            i += 1;
            while i + 1 < s.len() && !(s[i] == '*' && s[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(s.len());
        } else if ['\'', '"', '`', '['].contains(&ch) {
            let end = if ch == '[' { ']' } else { ch };
            let mut token = String::new();
            while i < s.len() {
                if s[i] == end {
                    i += 1;
                    if s.get(i) != Some(&end) {
                        break;
                    }
                }
                token.push(s[i]);
                i += 1;
            }
            if ch != '\'' {
                out.push(token.to_lowercase());
            }
        } else if ch == '.' {
            out.push(".".into());
        } else if ch.is_alphanumeric() || ch == '_' {
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push(s[start..i].iter().collect::<String>().to_lowercase());
        }
    }
    out
}

pub fn debts(c: &Connection, map: &Domains, cut: &str) -> Vec<Debt> {
    let mut debt = Vec::new();
    for o in objects(c) {
        let domain = map
            .get(&o.table)
            .unwrap_or_else(|| panic!("no domain for {}", o.table));
        if o.kind == "table" {
            // One debt per FK constraint, with compound column order intact.
            let sql = format!(
                "SELECT id,\"table\",\"from\",\"to\",on_update,on_delete FROM pragma_foreign_key_list({}) ORDER BY id,seq",
                ident(&o.name)
            );
            let mut fks: BTreeMap<i64, Vec<Vec<Value>>> = BTreeMap::new();
            for row in rows(c, &sql) {
                let Value::Integer(id) = row[0] else {
                    panic!("FK id")
                };
                fks.entry(id).or_default().push(row);
            }
            for fk in fks.values() {
                let text = |v: &Value| match v {
                    Value::Text(s) => s.clone(),
                    _ => panic!("FK text {v:?}"),
                };
                let parent = text(&fk[0][1]);
                let other = map
                    .get(&parent)
                    .unwrap_or_else(|| panic!("unmapped FK parent {parent}"));
                if !crosses(cut, domain, other) {
                    continue;
                }
                let from = fk.iter().map(|r| text(&r[2])).collect::<Vec<_>>().join(",");
                let to = fk.iter().map(|r| text(&r[3])).collect::<Vec<_>>().join(",");
                debt.push(Debt {
                    kind: "fk".into(),
                    id: format!("{cut}/fk/{}.({from}) -> {parent}.({to})", o.name),
                    description: format!(
                        "ON DELETE {}; ON UPDATE {}",
                        text(&fk[0][5]),
                        text(&fk[0][4])
                    ),
                    object: o.name.clone(),
                });
            }
        } else if o.kind == "trigger" {
            let t = tokens(&o.sql);
            let refs: BTreeSet<_> = t
                .iter()
                .enumerate()
                .filter(|(_, word)| ["from", "join", "update", "into"].contains(&word.as_str()))
                .filter_map(|(i, _)| {
                    let mut next = i + 1;
                    // UPDATE OR <conflict-action>, and main.<quoted-table> references.
                    if t.get(next).is_some_and(|w| w == "or") {
                        next += 2;
                    }
                    if t.get(next + 1).is_some_and(|w| w == ".") {
                        next += 2;
                    }
                    t.get(next).and_then(|table| {
                        map.get(table)
                            .filter(|d| crosses(cut, domain, d))
                            .map(|_| table.clone())
                    })
                })
                .collect();
            if !refs.is_empty() {
                debt.push(Debt {
                    kind: "trigger".into(),
                    id: format!("{cut}/trigger/{}", o.name),
                    description: format!(
                        "{} -> {}",
                        o.table,
                        refs.into_iter().collect::<Vec<_>>().join(",")
                    ),
                    object: o.name,
                });
            }
        }
    }
    debt.sort();
    debt
}

/// SQL literals preserve all seed values. Only attention's fresh epoch is nondeterministic.
/// Includes empty-table sqlite_sequence rows: MAX(live row) would lose their high water marks.
pub fn seeds(c: &Connection, map: &Domains, domain: &str) -> String {
    let mut sql =
        String::from("-- Final fresh-install seeds; historical data backfills are not replayed.\n");
    for o in objects(c)
        .into_iter()
        .filter(|o| o.kind == "table" && map[&o.table] == domain)
    {
        if o.name == "attention_meta" {
            sql.push_str("INSERT INTO attention_meta VALUES (1, lower(hex(randomblob(16))));\n");
            continue;
        }
        let columns = strings(
            c,
            &format!("SELECT name FROM pragma_table_info({})", ident(&o.name)),
        );
        let expr = columns
            .iter()
            .map(|s| format!("quote({})", ident(s)))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        for row in strings(
            c,
            &format!("SELECT {expr} FROM {} ORDER BY {expr}", ident(&o.name)),
        ) {
            sql.push_str(&format!("INSERT INTO {} VALUES ({row});\n", ident(&o.name)));
        }
    }
    for row in rows(c, "SELECT name,seq FROM sqlite_sequence ORDER BY name") {
        let Value::Text(table) = &row[0] else {
            panic!("sequence name")
        };
        if map.get(table).is_some_and(|d| d == domain) {
            let Value::Integer(seq) = row[1] else {
                panic!("sequence value")
            };
            sql.push_str(&format!(
                "INSERT INTO sqlite_sequence(name,seq) VALUES ('{table}',{seq});\n"
            ));
        }
    }
    sql
}
