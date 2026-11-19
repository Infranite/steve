//! Positional inserts cannot survive a logically equivalent physical column reorder.
//! This scans source literals, not migration SQL or test-only Rust module trees.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
struct Token {
    text: String,
    literal: bool,
    line: usize,
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
        let mut p = i;
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
        let mut literal = false;
        let text;
        if bytes.get(p) == Some(&b'"') {
            literal = true;
            let hash_count = p - hashes;
            i = p + 1;
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
                            decoded.push(
                                u8::from_str_radix(&source[i + 1..i + 3], 16).unwrap() as char
                            );
                            i += 2;
                        }
                        b'u' => {
                            let end = source[i..].find('}').unwrap() + i;
                            let hex = source[i + 2..end].replace('_', "");
                            decoded.push(
                                char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap(),
                            );
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
            text = decoded;
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
fn module_name(tokens: &[Token], mut start: usize) -> Option<usize> {
    if tokens.get(start)?.text == "pub" {
        start += 1;
        if tokens.get(start)?.text == "(" {
            start = group_end(tokens, start);
        }
    }
    (tokens.get(start)?.text == "mod").then_some(start + 1)
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

/// Gather all source literals outside explicitly test-only items. Out-of-line
/// test modules are recorded so their entire source trees can be excluded too.
fn production_tokens(
    tokens: &[Token],
    file: &Path,
    excluded: &mut BTreeSet<PathBuf>,
) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].text == "#"
            && tokens.get(i + 1).is_some_and(|t| t.text == "!")
            && tokens.get(i + 2).is_some_and(|t| t.text == "[")
        {
            let end = group_end(tokens, i + 2);
            let attr = &tokens[i + 3..end - 1];
            if attr.first().is_some_and(|t| t.text == "cfg")
                && attr.len() >= 3
                && test_cfg(&attr[2..attr.len() - 1])
            {
                return Vec::new();
            }
            i = end;
            continue;
        }
        if tokens[i].text == "#" && tokens.get(i + 1).is_some_and(|t| t.text == "[") {
            let mut end = i;
            let mut test = false;
            let mut path = None;
            while tokens.get(end).is_some_and(|t| t.text == "#")
                && tokens.get(end + 1).is_some_and(|t| t.text == "[")
            {
                let next = group_end(tokens, end + 1);
                let attr = &tokens[end + 2..next - 1];
                if attr.first().is_some_and(|t| t.text == "cfg") && attr.len() >= 3 {
                    test |= test_cfg(&attr[2..attr.len() - 1]);
                }
                if attr.first().is_some_and(|t| t.text == "path") {
                    path = attr.iter().find(|t| t.literal).map(|t| t.text.as_str());
                }
                end = next;
            }
            if test {
                let mut j = end;
                let module = module_name(tokens, end);
                while j < tokens.len() && !["{", ";"].contains(&tokens[j].text.as_str()) {
                    j += 1;
                }
                if let Some(name) = module {
                    excluded.insert(module_file(file, &tokens[name].text, path));
                    // Inline test modules can themselves contain out-of-line children.
                    if tokens.get(j).is_some_and(|t| t.text == "{") {
                        excluded.insert(module_dir(file).join(&tokens[name].text).join("mod.rs"));
                    }
                }
                i = if tokens.get(j).is_some_and(|t| t.text == "{") {
                    group_end(tokens, j)
                } else {
                    j + 1
                };
                continue;
            }
            // Ignore attribute strings themselves; e.g. documentation and cfg values.
            i = end;
            continue;
        }
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
                "<literal>".into()
            } else {
                sql[start..i].to_owned()
            });
            i = (i + 1).min(b.len());
        } else if b[i] == b'{' {
            let start = i;
            while i < b.len() && b[i] != b'}' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(sql[start..i].to_owned());
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128) {
                i += 1;
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

fn violations(sql: &str) -> Vec<String> {
    let tokens = sql_tokens(sql);
    let mut found = Vec::new();
    for (start, token) in tokens.iter().enumerate() {
        if !["INSERT", "REPLACE"].contains(&token.as_str()) {
            continue;
        }
        let mut i = start + 1;
        if tokens.get(i).is_some_and(|s| s == "OR") {
            i += 2;
        }
        if !tokens.get(i).is_some_and(|s| s == "INTO") {
            continue;
        }
        i += 2; // INTO and the table identifier (possibly a format placeholder).
        while tokens.get(i).is_some_and(|s| s == ".") {
            i += 2;
        }
        if tokens.get(i).is_some_and(|s| s == "AS") {
            i += 2;
        }
        if tokens.get(i).is_some_and(|s| s == "VALUES") {
            found.push("VALUES without column list".into());
        }
        let tail = &tokens[i.min(tokens.len())..];
        let end = tail.iter().position(|s| s == ";").unwrap_or(tail.len());
        for j in 0..end {
            if tail[j] != "SELECT" {
                continue;
            }
            let k = j
                + 1
                + usize::from(
                    tail.get(j + 1)
                        .is_some_and(|s| ["ALL", "DISTINCT"].contains(&s.as_str())),
                );
            if tail.get(k).is_some_and(|s| s == "*")
                || (tail.get(k + 1).is_some_and(|s| s == ".")
                    && tail.get(k + 2).is_some_and(|s| s == "*"))
            {
                found.push("SELECT wildcard".into());
            }
        }
    }
    found
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

#[test]
fn production_inserts_name_their_columns() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(workspace.join("crates")).unwrap() {
        source_files(&entry.unwrap().path().join("src"), &mut files);
    }
    files.sort();
    let mut excluded = BTreeSet::new();
    let parsed = files
        .iter()
        .map(|file| {
            let source = std::fs::read_to_string(file).unwrap();
            let tokens = production_tokens(&rust_tokens(&source), file, &mut excluded);
            (file, tokens)
        })
        .collect::<Vec<_>>();
    let mut actual = BTreeMap::new();
    let mut scanned = 0;
    let mut locations = Vec::new();
    for (file, tokens) in parsed {
        if excluded_file(file, &excluded) {
            continue;
        }
        scanned += 1;
        for token in tokens.iter().filter(|t| t.literal) {
            for kind in violations(&token.text) {
                let path = file
                    .strip_prefix(&workspace)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let key = (path.clone(), token.text.trim().to_owned(), kind.clone());
                *actual.entry(key).or_insert(0usize) += 1;
                locations.push(format!("{path}:{}: {kind}", token.line));
            }
        }
    }
    println!(
        "SQL_COLUMN_LISTS scanned_files={scanned} findings={} temporary_exceptions=1",
        locations.len()
    );
    // G2-W2a6q-SQL: group 4 owns this Node-journal fix; remove the entry once fixed.
    // Exact equality ratchets both ways: new violations and vanished exceptions fail.
    let expected = BTreeMap::from([(
        (
            "crates/steve-node/src/journal.rs".into(),
            "INSERT INTO command_specs VALUES (?1, ?2)".into(),
            "VALUES without column list".into(),
        ),
        1,
    )]);
    assert_eq!(
        actual,
        expected,
        "positional SQL changed; fix new inserts or tighten the exception list:\n{}",
        locations.join("\n")
    );
}

#[test]
fn matches_conflict_modes_raw_strings_comments_and_qualified_names() {
    for sql in [
        "iNsErT\nOR IGNORE INTO main.\"table\"\nVALUES(?)",
        "REPLACE INTO [table] VALUES(1)",
        "INSERT OR ROLLBACK INTO {table:?} VALUES(1)",
        "INSERT OR ABORT INTO 事项 VALUES(1)",
        "INSERT /* comment */ OR REPLACE INTO `table` VALUES(1)",
        "INSERT INTO t(a) SELECT * FROM other",
        "INSERT INTO t(a) SELECT DISTINCT src.* FROM other src",
        "WITH c AS (SELECT 1) INSERT INTO t SELECT * FROM c",
    ] {
        assert!(!violations(sql).is_empty(), "{sql}");
    }
    let rust = "let q = r###\"InSeRt OR FAIL INTO t VALUES(1)\"###;";
    assert_eq!(
        rust_tokens(rust)
            .iter()
            .filter(|t| t.literal)
            .flat_map(|t| violations(&t.text))
            .count(),
        1
    );
    for sql in [
        "INSERT INTO t(a) VALUES(1)",
        "INSERT INTO t(a) SELECT a FROM other",
        "SELECT 'INSERT INTO t VALUES(1)'",
        "-- INSERT INTO t VALUES(1)\nSELECT 1",
    ] {
        assert!(violations(sql).is_empty(), "{sql}");
    }
}

#[test]
fn excludes_test_items_but_keeps_non_test_cfg_and_raw_sql() {
    let source = r####"
        #[cfg(test)] mod tests { fn f() { let s = "INSERT INTO skipped VALUES(1)"; } }
        #[cfg(all(unix, test))] mod also_test { const S: &str = "INSERT INTO skipped VALUES(1)"; }
        #[cfg(any(test, unix))] fn production() { let s = r#"INSERT INTO kept VALUES(1)"#; }
        #[cfg(not(test))] fn production_too() { let s = "INSERT INTO kept VALUES(1)"; }
        // "INSERT INTO skipped VALUES(1)"
        const QUOTE: char = '"';
    "####;
    let tokens = production_tokens(
        &rust_tokens(source),
        Path::new("src/lib.rs"),
        &mut BTreeSet::new(),
    );
    assert_eq!(
        tokens
            .iter()
            .filter(|t| t.literal)
            .flat_map(|t| violations(&t.text))
            .count(),
        2
    );
}

#[test]
fn tracks_visible_out_of_line_and_nested_test_modules_and_concat_literals() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("src/area.rs");
    std::fs::create_dir_all(d.path().join("src/area/outer")).unwrap();
    let child = d.path().join("src/area/child.rs");
    let nested = d.path().join("src/area/outer/tests.rs");
    std::fs::write(&child, "").unwrap();
    std::fs::write(&nested, "").unwrap();
    let source = r###"
        #[cfg(test)] pub(crate) mod child;
        #[cfg(test)] #[path="mocks.rs"] mod alternate;
        mod outer { #[cfg(test)] mod tests; }
        const Q: &str = concat!("INSERT OR ", "ABORT INTO t ", r#"VALUES(1)"#);
    "###;
    let mut excluded = BTreeSet::new();
    let tokens = production_tokens(&rust_tokens(source), &file, &mut excluded);
    assert!(excluded_file(&child, &excluded));
    assert!(excluded_file(
        &d.path().join("src/area/child/grandchild.rs"),
        &excluded
    ));
    assert!(excluded_file(&nested, &excluded));
    assert!(!excluded_file(
        &d.path().join("src/area/alternate/production.rs"),
        &excluded
    ));
    assert!(!excluded_file(
        &d.path().join("src/area/production.rs"),
        &excluded
    ));
    assert_eq!(
        tokens
            .iter()
            .filter(|t| t.literal)
            .flat_map(|t| violations(&t.text))
            .count(),
        1
    );
    assert!(
        production_tokens(
            &rust_tokens("#![cfg(test)] const S: &str = \"INSERT INTO t VALUES(1)\";"),
            &file,
            &mut excluded
        )
        .is_empty()
    );
}
