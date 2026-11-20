//! G2-R2 guards. Rust literal/cfg/module scanning follows sql_column_lists.rs.
//! No baseline DELETE exceptions within the Store database: watched tables cannot
//! be cleared wholesale. This scans literal SQL only, not SQL assembled through
//! concatenation/format! at runtime; those call sites require separate review.
//! Excluded boundaries: schema_compat is frozen legacy replay, not runtime SQL;
//! steve-node/src/journal.rs writes an independent Node journal database;
//! steve-hub/src/wire/node_tools.rs writes the independent host-tools ledger.
//! Those independent databases do not share Store table identities. Test-only
//! items/modules are excluded by the same cfg/module scan as sql_column_lists.rs.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
struct Token {
    text: String,
    literal: bool,
    line: usize,
}

/// Decode one ordinary/raw/byte/C literal; return None for identifiers/lifetimes.
fn string_literal(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    let mut p = start;
    if matches!(bytes[p], b'b' | b'c') {
        p += 1;
    }
    let raw = bytes.get(p) == Some(&b'r');
    if raw {
        p += 1;
    }
    let hashes = p;
    if raw {
        while bytes.get(p) == Some(&b'#') {
            p += 1;
        }
    }
    if bytes.get(p) != Some(&b'"') {
        return None;
    }
    let hash_count = p - hashes;
    let mut i = p + 1;
    let body = i;
    let mut decoded = String::new();
    loop {
        assert!(i < bytes.len(), "unterminated Rust string");
        if bytes[i] == b'"'
            && (!raw || bytes.get(i + 1..i + 1 + hash_count) == Some(&bytes[hashes..p]))
        {
            if raw {
                decoded.push_str(&source[body..i]);
            }
            i += 1 + hash_count;
            break;
        }
        if !raw && bytes[i] == b'\\' {
            i += 1;
            match bytes[i] {
                b'n' => decoded.push('\n'),
                b'r' => decoded.push('\r'),
                b't' => decoded.push('\t'),
                b'0' => decoded.push('\0'),
                b'x' => {
                    decoded.push(u8::from_str_radix(&source[i + 1..i + 3], 16).unwrap() as char);
                    i += 2;
                }
                b'u' => {
                    let end = source[i..].find('}').unwrap() + i;
                    let hex = source[i + 2..end].replace('_', "");
                    decoded.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                    i = end;
                }
                b'\n' | b'\r' => {
                    while bytes.get(i + 1).is_some_and(u8::is_ascii_whitespace) {
                        i += 1;
                    }
                }
                c => decoded.push(c as char),
            }
            i += 1;
        } else {
            let ch = source[i..].chars().next().unwrap();
            if !raw {
                decoded.push(ch);
            }
            i += ch.len_utf8();
        }
    }
    Some((decoded, i))
}

fn rust_tokens(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let newlines = bytes
        .iter()
        .enumerate()
        .filter_map(|(i, b)| (*b == b'\n').then_some(i))
        .collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let decoded = string_literal(source, i);
        let literal = decoded.is_some();
        let text;
        if let Some((value, end)) = decoded {
            text = value;
            i = end;
        } else if bytes[i] == b'\''
            && (bytes.get(i + 2) == Some(&b'\'')
                || (bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'')))
        {
            i += if bytes[i + 1] == b'\\' { 4 } else { 3 };
            continue;
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            i += 1;
            while bytes
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                i += 1;
            }
            text = source[start..i].to_owned();
        } else {
            let ch = source[i..].chars().next().unwrap();
            text = ch.to_string();
            i += ch.len_utf8();
        }
        tokens.push(Token {
            text,
            literal,
            line: newlines.partition_point(|n| *n < start) + 1,
        });
    }
    tokens
}

fn group_end(tokens: &[Token], start: usize) -> usize {
    let close = match tokens[start].text.as_str() {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        _ => panic!("expected group"),
    };
    let mut i = start + 1;
    while i < tokens.len() {
        if !tokens[i].literal {
            if tokens[i].text == close {
                return i + 1;
            }
            if matches!(tokens[i].text.as_str(), "(" | "[" | "{") {
                i = group_end(tokens, i);
                continue;
            }
        }
        i += 1;
    }
    panic!("unclosed Rust group");
}

fn test_cfg(tokens: &[Token]) -> bool {
    // A cfg predicate is test-only when test is required: all needs one test-only
    // child; any needs all. Never skip cfg(not(test)) or any(test, unix).
    if tokens.len() == 1 {
        return tokens[0].text == "test";
    }
    if tokens.len() < 3 || tokens[1].text != "(" {
        return false;
    }
    let end = group_end(tokens, 1) - 1;
    let mut args = Vec::new();
    let mut start = 2;
    let mut i = start;
    while i < end {
        if tokens[i].text == "," {
            args.push(test_cfg(&tokens[start..i]));
            start = i + 1;
        } else if tokens[i].text == "(" {
            i = group_end(tokens, i);
            continue;
        }
        i += 1;
    }
    if start < end {
        args.push(test_cfg(&tokens[start..end]));
    }
    match tokens[0].text.as_str() {
        "all" => args.contains(&true),
        "any" => !args.is_empty() && args.iter().all(|v| *v),
        _ => false,
    }
}

fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap();
    let stem = file.file_stem().unwrap().to_str().unwrap();
    if ["lib", "main", "mod"].contains(&stem) {
        parent.to_owned()
    } else {
        parent.join(stem)
    }
}
fn module_file(file: &Path, name: &str, explicit: Option<&str>) -> PathBuf {
    if let Some(path) = explicit {
        return file.parent().unwrap().join(path);
    }
    let dir = module_dir(file);
    let direct = dir.join(format!("{name}.rs"));
    if direct.exists() {
        direct
    } else {
        dir.join(name).join("mod.rs")
    }
}
fn excluded_file(file: &Path, excluded: &BTreeSet<PathBuf>) -> bool {
    excluded.iter().any(|p| {
        file == p
            || file.starts_with(if p.file_name().is_some_and(|n| n == "mod.rs") {
                p.parent().unwrap().to_owned()
            } else {
                p.with_extension("")
            })
    })
}

struct Attributes<'a> {
    end: usize,
    test: bool,
    inner: bool,
    path: Option<&'a str>,
}
fn attributes(tokens: &[Token], start: usize) -> Option<Attributes<'_>> {
    if tokens.get(start)?.text != "#" || tokens[start].literal {
        return None;
    }
    let inner = tokens.get(start + 1)?.text == "!";
    let bracket = start + 1 + usize::from(inner);
    if tokens.get(bracket)?.text != "[" {
        return None;
    }
    let mut result = Attributes {
        end: start,
        test: false,
        inner,
        path: None,
    };
    loop {
        let next = group_end(tokens, result.end + 1 + usize::from(inner));
        let attr = &tokens[result.end + 2 + usize::from(inner)..next - 1];
        if attr.first().is_some_and(|t| t.text == "cfg") && attr.len() >= 3 {
            result.test |= test_cfg(&attr[2..attr.len() - 1]);
        }
        if attr.first().is_some_and(|t| t.text == "path") {
            result.path = attr.iter().find(|t| t.literal).map(|t| t.text.as_str());
        }
        result.end = next;
        if !tokens.get(next).is_some_and(|t| t.text == "#")
            || !tokens
                .get(next + 1 + usize::from(inner))
                .is_some_and(|t| t.text == "[")
            || (inner && !tokens.get(next + 1).is_some_and(|t| t.text == "!"))
        {
            break;
        }
    }
    Some(result)
}
fn item_head(tokens: &[Token], mut i: usize) -> Option<usize> {
    if tokens.get(i)?.text == "pub" {
        i += 1;
        if tokens.get(i)?.text == "(" {
            i = group_end(tokens, i);
        }
    }
    while tokens
        .get(i)
        .is_some_and(|t| ["unsafe", "async", "default", "auto"].contains(&t.text.as_str()))
    {
        i += 1;
    }
    if tokens.get(i)?.text == "extern" {
        i += 1;
        if tokens.get(i)?.literal {
            i += 1;
        }
        if tokens.get(i)?.text == "{" {
            return Some(i);
        }
        if tokens.get(i)?.text == "crate" {
            return Some(i);
        }
    }
    let t = tokens.get(i)?;
    (!t.literal
        && [
            "mod", "fn", "impl", "struct", "enum", "const", "static", "use", "trait", "type",
            "union",
        ]
        .contains(&t.text.as_str()))
    .then_some(i)
}
fn item_end(tokens: &[Token], mut i: usize) -> usize {
    while i < tokens.len() {
        if !tokens[i].literal {
            match tokens[i].text.as_str() {
                "(" | "[" => {
                    i = group_end(tokens, i);
                    continue;
                }
                "{" => return group_end(tokens, i),
                ";" => return i + 1,
                _ => {}
            }
        }
        i += 1;
    }
    i
}

/// Gather all source literals outside explicitly test-only items. Out-of-line
/// test modules are recorded so their entire source trees can be excluded too.
fn production_tokens(
    tokens: &[Token],
    file: &Path,
    excluded: &mut BTreeSet<PathBuf>,
) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut inner_position = true;
    while i < tokens.len() {
        if let Some(attr) = attributes(tokens, i) {
            if attr.inner && attr.test && inner_position {
                return Vec::new();
            }
            if !attr.inner {
                inner_position = false;
            }
            if !attr.inner
                && attr.test
                && let Some(head) = item_head(tokens, attr.end)
            {
                if tokens[head].text == "mod" {
                    let name = &tokens[head + 1].text;
                    excluded.insert(module_file(file, name, attr.path));
                }
                i = item_end(tokens, head);
            } else {
                i = attr.end;
            }
            continue;
        }
        inner_position = false;
        if !tokens[i].literal
            && tokens[i].text == "mod"
            && tokens.get(i + 2).is_some_and(|t| t.text == "{")
        {
            let end = group_end(tokens, i + 2);
            let nested = module_dir(file).join(&tokens[i + 1].text).join("mod.rs");
            out.extend(production_tokens(
                &tokens[i + 3..end - 1],
                &nested,
                excluded,
            ));
            i = end;
            continue;
        }
        if !tokens[i].literal
            && tokens[i].text == "concat"
            && tokens.get(i + 1).is_some_and(|t| t.text == "!")
            && tokens.get(i + 2).is_some_and(|t| t.text == "(")
        {
            let end = group_end(tokens, i + 2);
            let fragments = production_tokens(&tokens[i + 3..end - 1], file, excluded);
            out.push(Token {
                text: fragments
                    .iter()
                    .filter(|t| t.literal)
                    .map(|t| t.text.as_str())
                    .collect(),
                literal: true,
                line: tokens[i].line,
            });
            i = end;
            continue;
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

fn sql_tokens(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = sql.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else if b"'\"`[".contains(&b[i]) {
            let quote = b[i];
            let end = if quote == b'[' { b']' } else { quote };
            i += 1;
            let start = i;
            while i < b.len() {
                if b[i] == end {
                    if b.get(i + 1) == Some(&end) {
                        i += 2;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push(if quote == b'\'' {
                sql[start - 1..(i + 1).min(sql.len())].to_owned()
            } else {
                sql[start..i].to_owned()
            });
            i = (i + 1).min(b.len());
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 || b[i] == b'{' {
            let start = i;
            while i < b.len() {
                if b[i] == b'{' {
                    while i < b.len() && b[i] != b'}' {
                        i += 1;
                    }
                    i = (i + 1).min(b.len());
                } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 {
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(sql[start..i].to_ascii_uppercase());
        } else {
            if !b[i].is_ascii_whitespace() {
                out.push((b[i] as char).to_string());
            }
            i += 1;
        }
    }
    out
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let mut entries = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// R3d's four runtime subscribers: outbound queue (6), notifications (2, shared
// with queue), YOLO (1), Digest Bot (11). Union = 18. Update with those sources
// when introducing a new subscriber; entity subscriptions also map to these tables.
const SUBSCRIBED: &[&str] = &[
    "chan_messages",
    "decisions",
    "chan_send_state",
    "personal_notifications",
    "notification_policies",
    "output_deliveries",
    "yolo_actions",
    "digest_packets",
    "settings",
    "flows",
    "flow_triggers",
    "workspace_members",
    "bots",
    "bot_workspaces",
    "projects",
    "tags",
    "flow_runs",
    "items",
];

fn strip_parens(mut tokens: &[String]) -> &[String] {
    while tokens.first().is_some_and(|s| s == "(") && tokens.last().is_some_and(|s| s == ")") {
        let mut depth = 0;
        let encloses = tokens.iter().take(tokens.len() - 1).all(|s| {
            if s == "(" {
                depth += 1;
            } else if s == ")" {
                depth -= 1;
            }
            depth > 0
        });
        if !encloses {
            break;
        }
        tokens = &tokens[1..tokens.len() - 1];
    }
    tokens
}
// Three-valued static result: None means row/parameter-dependent. Constant SQL
// arithmetic/comparisons use bundled SQLite, then boolean decomposition catches
// tautological OR branches even when another branch references a column/binding.
fn constant_truth(c: &rusqlite::Connection, tokens: &[String]) -> Option<bool> {
    let tokens = strip_parens(tokens);
    for operator in ["OR", "AND"] {
        let mut depth = 0;
        for (i, token) in tokens.iter().enumerate() {
            match token.as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            if depth == 0 && token == operator {
                let a = constant_truth(c, &tokens[..i]);
                let b = constant_truth(c, &tokens[i + 1..]);
                return match (operator, a, b) {
                    ("OR", Some(true), _) | ("OR", _, Some(true)) => Some(true),
                    ("AND", Some(false), _) | ("AND", _, Some(false)) => Some(false),
                    (_, Some(a), Some(b)) => Some(if operator == "OR" { a || b } else { a && b }),
                    _ => None,
                };
            }
        }
    }
    if tokens.first().is_some_and(|s| s == "NOT") {
        return constant_truth(c, &tokens[1..]).map(|b| !b);
    }
    for (i, t) in tokens.iter().enumerate() {
        // Reject self-comparisons as a conservative source rule, including bound
        // parameters and nullable columns; don't disguise a clear-all as id=id.
        if ["=", "IS"].contains(&t.as_str()) && i > 0 && tokens[..i] == tokens[i + 1..] {
            return Some(true);
        }
    }
    if tokens
        .iter()
        .any(|t| ["?", ":", "$", "@"].contains(&t.as_str()) || t.contains('{'))
    {
        return None;
    }
    c.query_row(
        &format!("SELECT CASE WHEN ({}) THEN 1 ELSE 0 END", tokens.join(" ")),
        [],
        |r| r.get::<_, i64>(0),
    )
    .ok()
    .map(|n| n != 0)
}
fn delete_violations(c: &rusqlite::Connection, sql: &str) -> Vec<String> {
    let tokens = sql_tokens(sql);
    let mut found = Vec::new();
    for start in 0..tokens.len().saturating_sub(2) {
        if tokens[start] != "DELETE" || tokens[start + 1] != "FROM" {
            continue;
        }
        let mut i = start + 2;
        let mut table = &tokens[i];
        while tokens.get(i + 1).is_some_and(|s| s == ".") && i + 2 < tokens.len() {
            i += 2;
            table = &tokens[i];
        }
        // Format-selected tables are potentially subscribed. Bounded dynamic SQL
        // remains valid, but dynamic clear-all statements cannot hide their target.
        if !SUBSCRIBED.iter().any(|s| s.eq_ignore_ascii_case(table)) && !table.contains('{') {
            continue;
        }
        let tail = &tokens[i + 1..];
        let end = tail.iter().position(|s| s == ";").unwrap_or(tail.len());
        let tail = &tail[..end];
        let predicate = tail.iter().position(|s| s == "WHERE").map(|where_at| {
            let tail = &tail[where_at + 1..];
            let end = tail
                .iter()
                .position(|s| ["RETURNING", "ORDER", "LIMIT", "END"].contains(&s.as_str()))
                .unwrap_or(tail.len());
            &tail[..end]
        });
        if predicate.is_none_or(|p| constant_truth(c, p) == Some(true)) {
            found.push(table.to_ascii_lowercase());
        }
    }
    found
}
#[test]
fn production_watched_tables_never_use_unconditional_delete() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(workspace.join("crates")).unwrap() {
        source_files(&entry.unwrap().path().join("src"), &mut files);
    }
    files.sort();
    let mut excluded = BTreeSet::new();
    let parsed: Vec<_> = files
        .iter()
        .map(|file| {
            let source = std::fs::read_to_string(file).unwrap();
            (
                file,
                production_tokens(&rust_tokens(&source), file, &mut excluded),
            )
        })
        .collect();
    let c = rusqlite::Connection::open_in_memory().unwrap();
    let mut violations = Vec::new();
    for (file, tokens) in parsed {
        // schema_compat is the frozen legacy replay, outside runtime repositories.
        // Independent databases have their own table identities, not Store tables.
        let relative = file
            .strip_prefix(&workspace)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let independent = [
            "crates/steve-node/src/journal.rs",
            "crates/steve-hub/src/wire/node_tools.rs",
        ]
        .contains(&relative.as_str());
        if independent
            || excluded_file(file, &excluded)
            || file.components().any(|p| p.as_os_str() == "schema_compat")
            || file.file_stem().is_some_and(|p| p == "schema_compat")
        {
            continue;
        }
        for token in tokens.iter().filter(|t| t.literal) {
            for table in delete_violations(&c, &token.text) {
                violations.push(format!(
                    "{}:{}: clear-all DELETE on {table}",
                    file.strip_prefix(&workspace).unwrap().display(),
                    token.line
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "G2-R2 has no allowed exceptions:\n{}",
        violations.join("\n")
    );
}
#[test]
fn guard_rejects_constant_true_deletes_and_respects_real_predicates() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(SUBSCRIBED.iter().collect::<BTreeSet<_>>().len(), 18);
    for sql in [
        "DELETE FROM items",
        "delete from main.\"items\" RETURNING id",
        "DELETE FROM items WHERE true",
        "DELETE FROM items WHERE 1",
        "DELETE FROM items WHERE (1 = 1)",
        "DELETE FROM items WHERE 2 > 1",
        "DELETE FROM items WHERE NOT (0)",
        "DELETE FROM items WHERE (1+2)=3",
        "DELETE FROM items WHERE id=?1 OR true",
        "DELETE FROM items WHERE (true OR id=?1)",
        "DELETE FROM items WHERE NOT (false AND id=?1)",
        "DELETE FROM items WHERE id=id",
        "DELETE FROM items WHERE 'constant'='constant'",
        "DELETE FROM {table} WHERE true",
        "DELETE /* comment */ FROM [items] WHERE ((true))",
    ] {
        assert_eq!(delete_violations(&c, sql).len(), 1, "{sql}");
    }
    for sql in [
        "DELETE FROM items WHERE id=?1",
        "DELETE FROM items WHERE true AND id=?1",
        "DELETE FROM items WHERE false",
        "DELETE FROM independent_queue",
        "SELECT 'DELETE FROM items'",
        "DELETE FROM items WHERE id IN (SELECT id FROM doomed WHERE true)",
    ] {
        assert!(delete_violations(&c, sql).is_empty(), "{sql}");
    }
}
#[test]
fn guard_excludes_test_items_but_keeps_production_and_concat() {
    let source = r###"
        #[cfg(test)] mod tests { fn clear() { let sql="DELETE FROM items"; } }
        #[cfg(all(unix,test))] const TEST: &str = "DELETE FROM items WHERE true";
        #[cfg(any(unix,test))] fn real() { let sql=concat!("DELETE FROM ", "items WHERE 1"); }
        #[cfg(not(test))] fn runtime() { let sql="DELETE FROM items WHERE 1=1"; }
    "###;
    let tokens = production_tokens(
        &rust_tokens(source),
        Path::new("src/lib.rs"),
        &mut BTreeSet::new(),
    );
    let c = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(
        tokens
            .iter()
            .filter(|t| t.literal)
            .flat_map(|t| delete_violations(&c, &t.text))
            .count(),
        2
    );
}
#[test]
fn baseline_and_all_domain_migrations_have_no_without_rowid_tables() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    for set in steve_store_sqlite::migrate::CATALOG {
        for migration in set.migrations {
            c.execute_batch(migration.sql).unwrap();
            let names = c
                .prepare(
                    "SELECT name FROM pragma_table_list WHERE type='table' AND wr=1 ORDER BY name",
                )
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                names.is_empty(),
                "G2-R2: {} / {} has WITHOUT ROWID: {names:?}",
                set.domain,
                migration.name
            );
        }
    }
    // FTS shadow tables with wr=1 are intentionally excluded by SQLite's type.
    c.execute_batch("CREATE VIRTUAL TABLE fts_probe USING fts5(text)")
        .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM pragma_table_list WHERE type='table' AND wr=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        c.query_row(
            "SELECT count(*) FROM pragma_table_list WHERE type='shadow' AND wr=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
            > 0
    );
}
