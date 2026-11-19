//! Workspace-wide file/count ratchet, including test fixture SQL. New files or larger counts
//! require review; the list can be shortened with STEVE_DIGEST_WRITE_SITES_BLESS=1.
use std::{collections::BTreeMap, path::Path};

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
fn is_writer(sql: &str) -> bool {
    let decoded = sql
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ");
    let s = decoded
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    for table in ["chan_messages", "mail_messages", "digest_receipts"] {
        for verb in [
            "insert into",
            "insert or ignore into",
            "insert or replace into",
            "replace into",
        ] {
            if let Some(tail) = s.strip_prefix(&format!("{verb} {table}"))
                && (tail.starts_with('(') || tail.starts_with(' '))
            {
                return true;
            }
        }
        if table == "digest_receipts" {
            continue;
        }
        if let Some(tail) = s.strip_prefix(&format!("delete from {table}"))
            && (tail.is_empty() || tail.starts_with(' '))
        {
            return true;
        }
        if let Some(set) = s.strip_prefix(&format!("update {table} set "))
            && set
                .split(" where ")
                .next()
                .unwrap()
                .split(',')
                .filter_map(|v| v.split('=').next())
                .map(str::trim)
                .any(|f| {
                    [
                        "status",
                        "workspace_id",
                        "space_id",
                        "thread_id",
                        "created",
                        "date",
                    ]
                    .contains(&f)
                })
        {
            return true;
        }
    }
    false
}
fn scan(dir: &Path, root: &Path, found: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if !["target", ".git"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                scan(&path, root, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = std::fs::read_to_string(&path).unwrap();
            let count = literals(&source)
                .into_iter()
                .flat_map(|s| s.split(';'))
                .filter(|sql| is_writer(sql))
                .count();
            if count > 0 {
                found.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                    count,
                );
            }
        }
    }
}
#[test]
fn workspace_writers_only_lose_files_and_sites() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/digest_write_sites.txt");
    let mut found = BTreeMap::new();
    scan(&root, &root, &mut found);
    let text = std::fs::read_to_string(&list);
    let bless = std::env::var_os("STEVE_DIGEST_WRITE_SITES_BLESS").is_some();
    if let Ok(text) = &text {
        let listed: BTreeMap<_, usize> = text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let (file, count) = l.split_once('\t').expect("file<TAB>count");
                (file.to_owned(), count.parse().unwrap())
            })
            .collect();
        for (file, count) in &found {
            assert!(
                *count <= listed.get(file).copied().unwrap_or(0),
                "new digest writer: {file} ({count}); review the source writer and its transactional maintenance"
            );
        }
    } else {
        assert!(bless, "missing digest write-site inventory");
    }
    if bless {
        let lines = found
            .iter()
            .map(|(file, count)| format!("{file}\t{count}\n"))
            .collect::<String>();
        std::fs::write(list,format!("# Workspace Rust SQL sites (production and fixtures); file counts may only shrink.\n{lines}")).unwrap();
    }
}
#[test]
fn scanner_finds_raw_multistatement_sql_and_filters_unrelated_columns() {
    let source = r###"// "DELETE FROM chan_messages"
/* "INSERT INTO mail_messages VALUES(1)" */
let sql=r#"BEGIN; INSERT INTO chan_messages(id) VALUES('a'); UPDATE mail_messages SET status='sent' WHERE id=?;"#;
let unrelated="UPDATE chan_messages SET text='body',purged=1 WHERE id=?";
let receipt="INSERT INTO digest_receipts VALUES(1)";
let quote='"';
"###;
    assert_eq!(
        literals(source)
            .into_iter()
            .flat_map(|s| s.split(';'))
            .filter(|s| is_writer(s))
            .count(),
        3
    );
    assert!(is_writer("DELETE FROM mail_messages WHERE id=?"));
    assert!(is_writer("\\nUPDATE chan_messages SET status='sent'"));
    assert!(is_writer(
        "UPDATE chan_messages SET workspace_id=?, space_id=? WHERE id=?"
    ));
}
