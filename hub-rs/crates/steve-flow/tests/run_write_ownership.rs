//! Flow is the sole production owner of flow_runs writes. Test fixtures and historical schema
//! are excluded; the guard has no production allowlist. Runtime-built identifiers cannot be
//! resolved by this lexical check; literal/concat!/include_str! SQL is checked conservatively.
use std::path::Path;

fn literals(source: &str) -> Vec<&str> {
    let b = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            i += b[i..]
                .iter()
                .position(|x| *x == b'\n')
                .unwrap_or(b.len() - i);
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'r' {
            let start = i + 1;
            let mut end = start;
            while end < b.len() && b[end] == b'#' {
                end += 1;
            }
            if end < b.len() && b[end] == b'"' {
                let terminator = format!("\"{}", "#".repeat(end - start));
                let text = &source[end + 1..];
                if let Some(len) = text.find(&terminator) {
                    result.push(&text[..len]);
                    i = end + 1 + len + terminator.len();
                } else {
                    i = b.len();
                }
            } else {
                i += 1;
            }
        } else if b[i] == b'\'' && i + 2 < b.len() && b[i + 2] == b'\'' {
            i += 3; // A character literal such as '"', not a string delimiter.
        } else if b[i] == b'"' {
            let start = i + 1;
            i = start;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == b'"' {
                    result.push(&source[start..i]);
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    result
}
fn joined_literals(source: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut end = 0;
    let mut nesting = Vec::new();
    let mut token = String::new();
    for literal in literals(source) {
        let start = literal.as_ptr() as usize - source.as_ptr() as usize;
        let gap = &source[end..start];
        let bytes = gap.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"//") {
                i += bytes[i..]
                    .iter()
                    .position(|c| *c == b'\n')
                    .unwrap_or(bytes.len() - i);
            } else if bytes[i..].starts_with(b"/*") {
                i += 2;
                let mut depth = 1;
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
            } else if bytes[i] == b'\'' && i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
                i += 3;
            } else {
                let c = bytes[i] as char;
                if c.is_ascii_alphanumeric() || c == '_' || c == '!' {
                    token.push(c);
                } else if c == '(' {
                    nesting.push(token == "concat!");
                    token.clear();
                } else if c == ')' {
                    nesting.pop();
                    token.clear();
                } else if !c.is_whitespace() {
                    token.clear();
                }
                i += 1;
            }
        }
        let adjacent = !out.is_empty()
            && (!gap.contains(',') || nesting.contains(&true))
            && source[end..start]
                .chars()
                .all(|c| c.is_whitespace() || r##""#rb,+()"##.contains(c));
        if adjacent {
            out.last_mut().unwrap().1.push_str(literal);
        } else {
            out.push((start, literal.to_owned()));
        }
        end = start + literal.len();
    }
    out
}
fn groups(source: &str) -> Vec<String> {
    joined_literals(source)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

// Match whole SQL identifiers conservatively, including quoted/schema-qualified tables, CTEs,
// body-only updates, comments inside SQL and concat! literals, like digest_write_sites.
fn writes(sql: &str) -> bool {
    let words: Vec<_> = sql
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_ascii_lowercase)
        .collect();
    words.iter().any(|w| w == "flow_runs")
        && words
            .iter()
            .any(|w| matches!(w.as_str(), "insert" | "update" | "delete" | "replace"))
}

// SQL fixtures may write runs. Exclude cfg(test) items structurally, not everything after the
// first test attribute: a later production item must still be checked. Comments/strings/chars
// are masked while finding item braces; the SQL scanner gets the original production text.
fn production(source: &str) -> String {
    let mut code = source.as_bytes().to_vec();
    for text in literals(source) {
        let at = text.as_ptr() as usize - source.as_ptr() as usize;
        code[at..at + text.len()].fill(b' ');
    }
    let mut i = 0;
    while i + 1 < code.len() {
        let start = i;
        if code[i..].starts_with(b"//") {
            while i < code.len() && code[i] != b'\n' {
                i += 1;
            }
            code[start..i].fill(b' ');
        } else if code[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i + 1 < code.len() && depth > 0 {
                if code[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if code[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            code[start..i].fill(b' ');
        } else {
            i += 1;
        }
    }
    let mut code = String::from_utf8(code).unwrap();
    let chars =
        regex::Regex::new(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'").unwrap();
    let ranges: Vec<_> = chars.find_iter(&code).map(|m| m.range()).collect();
    for range in ranges {
        code.replace_range(range.clone(), &" ".repeat(range.len()));
    }
    let attrs = regex::Regex::new(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]").unwrap();
    let mut result = source.as_bytes().to_vec();
    for attr in attrs.find_iter(&code) {
        let start = attr.start();
        let mut end = attr.end();
        let mut depth = 0;
        for (offset, b) in code.as_bytes()[end..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end += offset + 1;
                        break;
                    }
                }
                b';' if depth == 0 => {
                    end += offset + 1;
                    break;
                }
                _ => {}
            }
        }
        result[start..end].fill(b' ');
    }
    String::from_utf8(result).unwrap()
}

fn historical(path: &Path) -> bool {
    path.starts_with("crates/steve-store/migrations")
        || path.starts_with("crates/steve-store-sqlite/legacy")
        || path.starts_with("crates/steve-store-sqlite/schema")
}

fn scan(path: &Path, root: &Path, found: &mut Vec<String>) {
    let relative = path.strip_prefix(root).unwrap();
    if relative
        .components()
        .any(|part| matches!(part.as_os_str().to_str(), Some("tests" | "target" | ".git")))
        || historical(relative)
        || relative.starts_with("crates/steve-flow")
    {
        return;
    }
    let source = std::fs::read_to_string(path).unwrap();
    if path.extension().is_some_and(|x| x == "rs") {
        let source = production(&source);
        for (at, sql) in joined_literals(&source) {
            if writes(&sql) {
                found.push(relative.display().to_string());
            }
            let prefix = &source[..at];
            if let Some(start) = prefix.rfind("include_str!")
                && prefix[start + "include_str!".len()..]
                    .replace("concat!", "")
                    .chars()
                    .all(|c| c.is_whitespace() || r##"("#r"##.contains(c))
            {
                let target = path.parent().unwrap().join(&sql);
                if target.is_file()
                    && !historical(target.canonicalize().unwrap().strip_prefix(root).unwrap())
                    && writes(&std::fs::read_to_string(target).unwrap())
                {
                    found.push(format!("{} (include_str!)", relative.display()));
                }
            }
        }
    } else if writes(&source) {
        found.push(relative.display().to_string());
    }
}
fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if !["target", ".git"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                walk(&path, root, found);
            }
        } else if path.extension().is_some_and(|x| x == "rs" || x == "sql") {
            scan(&path, root, found);
        }
    }
}

#[test]
fn flow_alone_owns_production_run_writes_across_the_workspace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut found = Vec::new();
    walk(&root, &root, &mut found);
    found.sort();
    found.dedup();
    assert!(
        found.is_empty(),
        "flow_runs writes outside steve-flow: {found:?}"
    );
}
#[test]
fn recognizer_covers_sql_shapes_and_test_exclusions_without_hiding_later_code() {
    for sql in [
        "INSERT INTO flow_runs(id) VALUES(?)",
        "WITH x AS (SELECT 1) UPDATE main.flow_runs SET status='failed'",
        "DELETE FROM \"flow_runs\"",
        "REPLACE INTO [flow_runs] VALUES(1)",
        "UPDATE flow_runs SET (status,error)=('failed',?)",
    ] {
        assert!(writes(sql), "{sql}");
    }
    for sql in [
        "SELECT * FROM flow_runs",
        "UPDATE flow_run_steps SET status='done'",
        "UPDATE flow_runs_backup SET status='failed'",
    ] {
        assert!(!writes(sql));
    }
    let source = r###"
        // #[cfg(test)] mod hidden { }
        const NOTE: &str = "#[cfg(test)] mod hidden { }";
        #[cfg(test)] mod tests { fn fixture() { execute("UPDATE flow_runs SET status='failed'"); } }
        #[cfg(test)] const FIXTURE: &str = "DELETE FROM flow_runs";
        fn production() { let brace = '{'; execute(concat!("UPDATE ", "flow_runs SET status='failed'")); }
    "###;
    let sql: Vec<_> = groups(&production(source))
        .into_iter()
        .filter(|s| writes(s))
        .collect();
    assert_eq!(sql, vec!["UPDATE flow_runs SET status='failed'"]);
}

#[test]
fn includes_preserve_the_historical_schema_boundary_but_not_runtime_writers() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let root = canonical.as_path();
    let src = root.join("crates/other/src");
    std::fs::create_dir_all(&src).unwrap();
    let migrations = root.join("crates/steve-store/migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let old = migrations.join("001.sql");
    std::fs::write(
        &old,
        "CREATE TABLE flow_runs(id); INSERT INTO another_table VALUES(1);",
    )
    .unwrap();
    let file = src.join("module.rs");
    std::fs::write(
        &file,
        r#"const OLD: &str=include_str!("../../steve-store/migrations/001.sql");"#,
    )
    .unwrap();
    let mut found = Vec::new();
    scan(&file, root, &mut found);
    assert!(
        found.is_empty(),
        "historical migrations stay historical through include_str!"
    );
    let runtime = src.join("write.sql");
    std::fs::write(&runtime, "UPDATE flow_runs SET status='done';").unwrap();
    std::fs::write(&file, r#"const WRITE: &str=include_str!("write.sql");"#).unwrap();
    scan(&file, root, &mut found);
    assert_eq!(found, vec!["crates/other/src/module.rs (include_str!)"]);
    found.clear();
    scan(&runtime, root, &mut found);
    assert_eq!(found, vec!["crates/other/src/write.sql"]);
    let shadow_schema = src.join("schema");
    std::fs::create_dir_all(&shadow_schema).unwrap();
    let shadow = shadow_schema.join("runtime.sql");
    std::fs::write(&shadow, "DELETE FROM flow_runs;").unwrap();
    found.clear();
    scan(&shadow, root, &mut found);
    assert_eq!(
        found,
        vec!["crates/other/src/schema/runtime.sql"],
        "a production folder named schema is not historical store schema"
    );
    let owned = root.join("crates/steve-flow/src");
    std::fs::create_dir_all(&owned).unwrap();
    std::fs::write(owned.join("write.sql"), "DELETE FROM flow_runs;").unwrap();
    std::fs::write(
        &file,
        r#"const WRITE: &str=include_str!("../../steve-flow/src/write.sql");"#,
    )
    .unwrap();
    found.clear();
    scan(&file, root, &mut found);
    assert_eq!(
        found,
        vec!["crates/other/src/module.rs (include_str!)"],
        "an outside executor cannot hide SQL in Flow's directory"
    );
}
