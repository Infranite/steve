//! Workspace-wide file/count ratchet, including test fixture SQL. New files or larger counts
//! require review; the list can be shortened with STEVE_DIGEST_WRITE_SITES_BLESS=1.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

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
// Deliberately conservative: even body-only updates, SQL comments and table-name fragments
// count. This protects format!/concat! paths without pretending to parse all of SQLite.
fn class(text: &str) -> Option<&'static str> {
    let s = text.to_ascii_lowercase();
    if !["chan_messages", "mail_messages", "digest_receipts"]
        .iter()
        .any(|t| s.contains(t))
    {
        return None;
    }
    Some(
        if ["insert", "update", "delete", "replace"]
            .iter()
            .any(|verb| s.contains(verb))
        {
            "write"
        } else {
            "table"
        },
    )
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
fn exempt(path: &str) -> bool {
    // Migrations/legacy/schema are immutable historical evidence or generated baselines,
    // not runtime writers. include_str! targets in these directories get the same exemption.
    path.starts_with("crates/steve-store/migrations/")
        || path.starts_with("crates/steve-store-sqlite/legacy/")
        || path.starts_with("crates/steve-store-sqlite/schema/")
}
type Inventory = BTreeMap<(String, String), usize>;
fn scan_file(path: &Path, root: &Path, found: &mut Inventory, seen: &mut BTreeSet<PathBuf>) {
    let path = path.canonicalize().unwrap();
    if !seen.insert(path.clone()) {
        return;
    }
    let relative = match path.strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => format!(
            "../{}",
            path.strip_prefix(root.parent().unwrap())
                .expect("include target outside checkout")
                .to_string_lossy()
                .replace('\\', "/")
        ),
    };
    if exempt(&relative) {
        return;
    }
    let source = std::fs::read_to_string(&path).unwrap();
    let is_rust = path.extension().is_some_and(|e| e == "rs");
    let parts = if is_rust {
        groups(&source)
    } else {
        source.split(';').map(str::to_owned).collect()
    };
    for text in parts {
        if let Some(kind) = class(&text) {
            *found.entry((kind.into(), relative.clone())).or_default() += 1;
        }
    }
    if is_rust {
        for (start, literal) in joined_literals(&source) {
            let prefix = &source[..start];
            if let Some(at) = prefix.rfind("include_str!")
                && prefix[at + "include_str!".len()..]
                    .replace("concat!", "")
                    .chars()
                    .all(|c| c.is_whitespace() || r##"("#r"##.contains(c))
            {
                let target = path.parent().unwrap().join(literal);
                if target.is_file() {
                    scan_file(&target, root, found, seen);
                }
            }
        }
    }
}
fn walk(dir: &Path, action: &mut impl FnMut(&Path)) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if !["target", ".git"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                walk(&path, action);
            }
        } else {
            action(&path);
        }
    }
}
#[test]
fn workspace_writers_only_lose_files_and_sites() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/digest_write_sites.txt");
    let mut found = Inventory::new();
    let mut seen = BTreeSet::new();
    walk(&root, &mut |path| {
        if path.extension().is_some_and(|e| e == "rs" || e == "sql") {
            scan_file(path, &root, &mut found, &mut seen);
        }
    });
    let listed = std::fs::read_to_string(&list).ok().map(|s| {
        s.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 3, "kind<TAB>file<TAB>count");
                (
                    (fields[0].into(), fields[1].into()),
                    fields[2].parse::<usize>().unwrap(),
                )
            })
            .collect::<Inventory>()
    });
    let bless = std::env::var_os("STEVE_DIGEST_WRITE_SITES_BLESS").is_some();
    if let Some(listed) = listed {
        for (key, count) in &found {
            assert!(
                *count <= listed.get(key).copied().unwrap_or(0),
                "new digest writer/table constant: {key:?} ({count})"
            );
        }
        if !bless {
            assert_eq!(
                found, listed,
                "digest writers decreased: 收紧清单 (bless after removal)"
            );
        }
    } else {
        assert!(bless, "missing digest inventory");
    }
    if bless {
        let rows = found
            .iter()
            .map(|((kind, file), count)| format!("{kind}\t{file}\t{count}\n"))
            .collect::<String>();
        std::fs::write(list,format!("# Conservative workspace digest inventory: write/table; exact equality, only shrink.\n{rows}")).unwrap();
    }
}
#[test]
fn conservative_matching_covers_advisory_examples() {
    for sql in [
        "WITH q AS (SELECT 1) UPDATE main.chan_messages SET (status,created)=('sent',(SELECT 1))",
        "UPDATE OR IGNORE \"mail_messages\" SET status='sent'",
        "UPDATE OR REPLACE [chan_messages] SET status='sent'",
        "INSERT OR ABORT INTO digest_receipts VALUES(1)",
        "INSERT OR FAIL INTO main.\"mail_messages\" VALUES(1)",
        "INSERT OR ROLLBACK INTO chan_messages VALUES(1)",
        "INSERT OR REPLACE INTO chan_messages VALUES(1)",
        "REPLACE INTO `digest_receipts` VALUES(1)",
        "WITH x AS (SELECT 1) INSERT INTO chan_messages SELECT * FROM x",
    ] {
        assert_eq!(class(sql), Some("write"), "{sql}");
    }
    assert_eq!(class("chan_messages"), Some("table"));
    assert_eq!(class("DELETE FROM {table}"), None);
    let split = r###"concat!("UP","DATE main.","chan_","messages SET status=?"); let tables=["mail_messages","digest_receipts"];"###;
    assert_eq!(
        groups(split)
            .iter()
            .filter_map(|s| class(s))
            .collect::<Vec<_>>(),
        ["write", "table", "table"]
    );
    assert!(exempt("crates/steve-store/migrations/114.sql"));
    assert!(!exempt("crates/another/runtime.sql"));
}

fn raw_constructors(source: &str) -> (usize, usize) {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let count = |name: &str| {
        compact
            .match_indices(&format!("{name}::"))
            .filter(|(at, _)| {
                *at == 0
                    || !compact.as_bytes()[at - 1].is_ascii_alphanumeric()
                        && compact.as_bytes()[at - 1] != b'_'
            })
            .count()
    };
    let mut env_names = vec!["TestEnv"];
    let mut store_names = vec!["Store"];
    for statement in source.split(';').filter(|s| s.contains("use ")) {
        let words = statement
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        for names in [&mut env_names, &mut store_names] {
            for triple in words.windows(3) {
                if triple[0] == names[0] && triple[1] == "as" {
                    names.push(triple[2]);
                }
            }
        }
    }
    let qualified_raw =
        compact.contains("::TestEnv::") && !compact.contains("digest_env::TestEnv::");
    let audited_import =
        compact.contains("usedigest_env::TestEnv;") || compact.contains("usesupport::TestEnv;");
    let env_count = env_names.iter().map(|name| count(name)).sum();
    (
        if qualified_raw || !audited_import {
            env_count
        } else {
            0
        },
        store_names.iter().map(|name| count(name)).sum(),
    )
}
#[test]
fn integration_fixtures_cannot_bypass_teardown_audits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut pending = Vec::new();
    for crate_name in ["steve-channels", "steve-mail", "steve-digest"] {
        walk(
            &root.join("crates").join(crate_name).join("tests"),
            &mut |path| {
                if path.extension().is_some_and(|e| e == "rs") {
                    pending.push(path.to_owned());
                }
            },
        );
    }
    let mut seen = BTreeSet::new();
    let mut found = BTreeMap::new();
    while let Some(path) = pending.pop() {
        let path = path.canonicalize().unwrap();
        if !seen.insert(path.clone()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let counts = raw_constructors(&source);
        if counts != (0, 0) {
            found.insert(
                path.strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                counts,
            );
        }
        // Follow shared fixtures included from examples or another crate, not just tests/**.
        for literal in literals(&source) {
            let start = literal.as_ptr() as usize - source.as_ptr() as usize;
            let before = &source[..start];
            if let Some(at) = before.rfind("#[path")
                && before[at + "#[path".len()..]
                    .chars()
                    .all(|c| c.is_whitespace() || r##"="r#"##.contains(c))
            {
                let target = path.parent().unwrap().join(literal);
                if target.is_file() && target.extension().is_some_and(|e| e == "rs") {
                    let target = target.canonicalize().unwrap();
                    let target_name = target
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    // These two external fixtures rely on auditing in their integration root.
                    // A new importer must not inherit the raw-constructor exception silently.
                    let owners = [
                        (
                            "crates/steve-channels/examples/support/personal_triggers.rs",
                            "crates/steve-channels/tests/personal_triggers.rs",
                        ),
                        (
                            "crates/steve-channels/examples/support/recovery_yolo.rs",
                            "crates/steve-channels/tests/c56_outbound.rs",
                        ),
                        (
                            "crates/steve-channels/examples/support/recovery_yolo.rs",
                            "crates/steve-channels/tests/support/reply_http.rs",
                        ),
                        (
                            "crates/steve-channels/tests/support/reply_http.rs",
                            "crates/steve-channels/tests/im_conversations.rs",
                        ),
                    ];
                    if owners.iter().any(|(fixture, _)| target_name == *fixture) {
                        let caller = path
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        assert!(owners.iter().any(|(fixture,owner)|target_name==*fixture && caller==*owner),
                            "shared raw fixture needs an explicit audited owner: {target_name} via {caller}");
                    }
                    pending.push(target);
                }
            }
        }
    }
    // Explicit exceptions are counted and tied to mandatory audit markers below.
    let allowed = BTreeMap::from([
        ("crates/steve-channels/tests/typed_events.rs".into(), (0, 1)), // custom publisher, explicit Audit guard
        (
            "crates/steve-channels/examples/support/recovery_yolo.rs".into(),
            (0, 1),
        ), // reopens an audited c56/im_conversations fixture file
        (
            "crates/steve-channels/tests/support/digest_env.rs".into(),
            (1, 0),
        ), // audited wrapper's underlying environment
        (
            "crates/steve-channels/examples/support/personal_triggers.rs".into(),
            (1, 0),
        ), // integration root implements audited Drop
        ("crates/steve-digest/tests/base.rs".into(), (0, 1)), // durability reopen, guarded by reopened_audit::Audit
    ]);
    assert_eq!(
        found, allowed,
        "raw TestEnv/Store construction must use an audited wrapper; tighten exceptions when removed"
    );
    let typed =
        std::fs::read_to_string(root.join("crates/steve-channels/tests/typed_events.rs")).unwrap();
    assert!(typed.contains("digest_audit::Audit(store.clone())"));
    let recovery = std::fs::read_to_string(
        root.join("crates/steve-channels/examples/support/recovery_yolo.rs"),
    )
    .unwrap();
    let outbound =
        std::fs::read_to_string(root.join("crates/steve-channels/tests/c56_outbound.rs")).unwrap();
    assert!(
        recovery.contains(r#"home.join("steve.sqlite3")"#)
            && outbound.contains("use digest_env::TestEnv;")
    );
    assert!(
        outbound
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .contains("recovery_yolo::engine(&h.env.modules,h.env.dir.path(),h.env.clock.clone())")
    );
    let reply =
        std::fs::read_to_string(root.join("crates/steve-channels/tests/support/reply_http.rs"))
            .unwrap();
    let im = std::fs::read_to_string(root.join("crates/steve-channels/tests/im_conversations.rs"))
        .unwrap();
    assert!(im.contains("use digest_env::TestEnv;"));
    assert!(
        reply
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .contains("actual_yolo::engine(&h.env.modules,h.env.dir.path(),h.env.clock.clone())")
    );
    let personal =
        std::fs::read_to_string(root.join("crates/steve-channels/tests/personal_triggers.rs"))
            .unwrap();
    assert!(
        personal.contains("impl Drop for Fixture")
            && personal.contains("digest_audit::check(self.env.store())")
    );
    let reopened = std::fs::read_to_string(root.join("crates/steve-digest/tests/base.rs")).unwrap();
    assert!(reopened.contains("reopened_audit::Audit(reopened.clone())"));
}
#[test]
fn raw_constructor_guard_detects_aliases_and_qualified_construction() {
    assert_ne!(
        raw_constructors("use steve_modules::testkit::TestEnv as E; E::new(|_|{});"),
        (0, 0)
    );
    assert_ne!(
        raw_constructors("steve_modules::testkit::TestEnv::new(|_|{});"),
        (0, 0)
    );
    assert_ne!(
        raw_constructors("use steve_store::Store as Db; Db::open(path);"),
        (0, 0)
    );
    assert_eq!(
        raw_constructors("use digest_env::TestEnv; TestEnv::new(|_|{});"),
        (0, 0)
    );
    assert_eq!(raw_constructors("FileKeyStore::open(path);"), (0, 0));
}
