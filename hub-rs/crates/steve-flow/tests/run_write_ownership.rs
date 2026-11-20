//! All production run writes belong to Flow. Outside Flow, every table-name literal is
//! counted per file: moving SQL's table name into a variable must still trip the ratchet.
use std::collections::BTreeMap;
use std::path::Path;
#[path = "run_write_ownership/syntax.rs"]
mod syntax;
use syntax::{groups, production};
type Inventory = BTreeMap<String, usize>;
fn mentions(sql: &str) -> bool {
    sql.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w.eq_ignore_ascii_case("flow_runs"))
}
fn writes(sql: &str) -> bool {
    mentions(sql)
        && sql
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|w| {
                matches!(
                    w.to_ascii_lowercase().as_str(),
                    "insert" | "update" | "delete" | "replace"
                )
            })
}
fn historical(path: &Path) -> bool {
    path.starts_with("crates/steve-store/migrations")
        || path.starts_with("crates/steve-store-sqlite/legacy")
        || path.starts_with("crates/steve-store-sqlite/schema")
}
fn observe(sql: &str, file: &str, writers: &mut Vec<String>, found: &mut Inventory) {
    if mentions(sql) {
        *found.entry(file.into()).or_default() += 1;
        if sql.trim().eq_ignore_ascii_case("flow_runs") {
            *found.entry(format!("bare:{file}")).or_default() += 1;
        }
    }
    if writes(sql) {
        writers.push(file.into());
    }
}
fn scan_inventory(path: &Path, root: &Path, writers: &mut Vec<String>, found: &mut Inventory) {
    let relative = path.strip_prefix(root).unwrap();
    if relative
        .components()
        .any(|p| matches!(p.as_os_str().to_str(), Some("tests" | "target" | ".git")))
        || historical(relative)
        || relative.starts_with("crates/steve-flow")
    {
        return;
    }
    let file = relative.to_string_lossy().to_string();
    let source = std::fs::read_to_string(path).unwrap();
    if path.extension().is_some_and(|x| x == "rs") {
        let source = production(&source);
        for text in groups(&source) {
            observe(&text, &file, writers, found);
        }
        for target in syntax::includes(&source, path) {
            let target = target
                .canonicalize()
                .unwrap_or_else(|e| panic!("{}: {e}", target.display()));
            let relative = target.strip_prefix(root).unwrap_or(&target);
            if !historical(relative) {
                for sql in std::fs::read_to_string(&target).unwrap().split(';') {
                    observe(sql, &file, writers, found);
                }
            }
        }
    } else {
        for sql in source.split(';') {
            observe(sql, &file, writers, found);
        }
    }
}
fn scan(path: &Path, root: &Path, writers: &mut Vec<String>) {
    scan_inventory(path, root, writers, &mut Inventory::new());
}
fn source_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if !["target", ".git", "tests"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                source_files(&path, files);
            }
        } else if path.extension().is_some_and(|x| x == "rs" || x == "sql") {
            files.push(path);
        }
    }
}
fn test_sources(files: &[std::path::PathBuf]) -> std::collections::BTreeSet<std::path::PathBuf> {
    let canonical = |p: std::path::PathBuf| p.canonicalize().unwrap();
    let mut tests = std::collections::BTreeSet::new();
    for file in files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "rs"))
    {
        for module in syntax::modules(&std::fs::read_to_string(file).unwrap(), file, true) {
            tests.insert(canonical(module));
        }
    }
    let mut queue: Vec<_> = tests.iter().cloned().collect();
    while let Some(file) = queue.pop() {
        for module in syntax::modules(&std::fs::read_to_string(&file).unwrap(), &file, false) {
            let module = canonical(module);
            if tests.insert(module.clone()) {
                queue.push(module);
            }
        }
    }
    // A module used by live code as well as tests is still production, as are its descendants.
    loop {
        let mut live = Vec::new();
        for file in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "rs") && !tests.contains(*f))
        {
            let source = production(&std::fs::read_to_string(file).unwrap());
            live.extend(
                syntax::modules(&source, file, false)
                    .into_iter()
                    .map(canonical),
            );
        }
        let mut removed = false;
        for file in live {
            removed |= tests.remove(&file);
        }
        if !removed {
            break;
        }
    }
    tests
}
fn walk(dir: &Path, root: &Path, writers: &mut Vec<String>, found: &mut Inventory) {
    let mut files = Vec::new();
    source_files(dir, &mut files);
    let excluded = test_sources(&files);
    for file in files {
        if !excluded.contains(&file) {
            scan_inventory(&file, root, writers, found);
        }
    }
}
fn compare(found: &Inventory, listed: &Inventory) -> Result<(), String> {
    for (file, count) in found {
        if *count > listed.get(file).copied().unwrap_or(0) {
            return Err(format!("new flow_runs table literal: {file} ({count})"));
        }
    }
    Ok(())
}
#[test]
fn flow_alone_owns_production_run_writes_across_the_workspace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut writers = Vec::new();
    let mut found = Inventory::new();
    walk(&root, &root, &mut writers, &mut found);
    assert!(
        writers.is_empty(),
        "flow_runs writes outside steve-flow (never exempt): {writers:?}"
    );
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run_write_ownership.txt");
    let init = std::env::var_os("STEVE_FLOW_RUN_TABLES_INIT").is_some();
    let bless = std::env::var_os("STEVE_FLOW_RUN_TABLES_BLESS").is_some();
    if let Ok(text) = std::fs::read_to_string(&list) {
        assert!(!init, "cannot reinitialize an existing run-table inventory");
        let mut listed = Inventory::new();
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 3, "file<TAB>total<TAB>bare subset");
            let total: usize = fields[1].parse().unwrap();
            let bare: usize = fields[2].parse().unwrap();
            assert!(bare <= total);
            listed.insert(fields[0].into(), total);
            if bare > 0 {
                listed.insert(format!("bare:{}", fields[0]), bare);
            }
        }
        compare(&found, &listed).unwrap();
        if !bless {
            assert_eq!(
                found, listed,
                "run table readers decreased; bless to shrink inventory"
            );
        }
    } else {
        assert!(!list.exists(), "cannot overwrite an unreadable inventory");
        assert!(
            init,
            "missing run table inventory; first import requires STEVE_FLOW_RUN_TABLES_INIT=1"
        );
    }
    if init || bless {
        let body = found
            .iter()
            .filter(|(file, _)| !file.starts_with("bare:"))
            .map(|(file, count)| {
                format!(
                    "{file}\t{count}\t{}\n",
                    found.get(&format!("bare:{file}")).copied().unwrap_or(0)
                )
            })
            .collect::<String>();
        std::fs::write(list,format!("# flow_runs literals outside Flow: total and bare-identifier subset; reads only, exact equality, only shrink.\n{body}")).unwrap();
    }
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
    assert_eq!(found, vec!["crates/other/src/module.rs"]);
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
        vec!["crates/other/src/module.rs"],
        "an outside executor cannot hide SQL in Flow's directory"
    );
}

#[test]
fn variable_table_names_are_counted_even_when_the_write_verb_is_elsewhere() {
    let source =
        r#"fn cleanup() { for t in ["flow_runs"] { execute(format!("DELETE FROM {t}")); } }"#;
    let mut found = Inventory::new();
    let mut writers = Vec::new();
    for text in groups(source) {
        observe(
            &text,
            "crates/other/src/cleanup.rs",
            &mut writers,
            &mut found,
        );
    }
    assert!(
        writers.is_empty(),
        "the literal scanner cannot resolve the variable's value"
    );
    assert_eq!(found["crates/other/src/cleanup.rs"], 1);
    assert!(
        compare(&found, &Inventory::new()).is_err(),
        "adding that table literal still fails the ratchet"
    );
    assert!(
        compare(
            &found,
            &BTreeMap::from([("crates/other/src/cleanup.rs".into(), 0)])
        )
        .is_err()
    );
}

#[test]
fn cfg_test_fields_and_match_arms_do_not_hide_subsequent_production_sql() {
    for source in [
        r#"struct S { #[cfg(test)] fixture: (fn(i32,i32)->[u8;2],), }
            impl S { fn write() { execute("UPDATE flow_runs SET status='done'"); } }"#,
        r#"struct S { #[cfg(test)] fixture: [u8;2] }
            impl S { fn write() { execute("UPDATE flow_runs SET status='done'"); } }"#,
        r#"fn f(x:E) { match x { #[cfg(test)] E::Test => fixture([1,2],(3,4)),
            E::Prod => execute("UPDATE flow_runs SET status='done'"), } }"#,
        r#"fn f(x:E) { match x { #[cfg(test)] E::Test => fixture([1,2],(3,4)) } }
            fn write() { execute("UPDATE flow_runs SET status='done'"); }"#,
    ] {
        let sql: Vec<_> = groups(&production(source))
            .into_iter()
            .filter(|s| writes(s))
            .collect();
        assert_eq!(sql, vec!["UPDATE flow_runs SET status='done'"], "{source}");
    }
}

#[test]
fn escaped_character_literals_and_manifest_concat_includes_are_recognized() {
    let source = r###"fn f<'a>(x:&'a str) { let quote='\"'; let brace='\u{7d}'; execute("UPDATE flow_runs SET status='done'"); }"###;
    assert_eq!(
        groups(&production(source))
            .into_iter()
            .filter(|s| writes(s))
            .count(),
        1
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let package = root.join("crates/other");
    let src = package.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(package.join("Cargo.toml"), "[package]\nname='other'\n").unwrap();
    std::fs::write(src.join("write.sql"), "DELETE FROM flow_runs;").unwrap();
    let file = src.join("module.rs");
    std::fs::write(
        &file,
        r#"const Q: &str=include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/", "write.sql"));"#,
    )
    .unwrap();
    let mut writers = Vec::new();
    scan(&file, &root, &mut writers);
    assert_eq!(writers, vec!["crates/other/src/module.rs"]);
    std::fs::write(&file, r#"macro_rules! q {($name:literal)=>{include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/src/",$name,".sql"))};} q!("write");"#).unwrap();
    writers.clear();
    scan(&file, &root, &mut writers);
    assert_eq!(writers, vec!["crates/other/src/module.rs"]);
}

#[test]
fn the_reader_inventory_can_only_shrink_and_never_licenses_a_writer() {
    let listed = BTreeMap::from([("reader.rs".into(), 2)]);
    assert!(compare(&BTreeMap::from([("reader.rs".into(), 1)]), &listed).is_ok());
    assert!(compare(&BTreeMap::from([("reader.rs".into(), 3)]), &listed).is_err());
    assert!(compare(&BTreeMap::from([("new.rs".into(), 1)]), &listed).is_err());
    let mut writers = Vec::new();
    let mut found = Inventory::new();
    observe(
        "DELETE FROM flow_runs",
        "reader.rs",
        &mut writers,
        &mut found,
    );
    assert!(compare(&found, &listed).is_ok());
    assert_eq!(
        writers,
        vec!["reader.rs"],
        "a listed table reader is still forbidden to write"
    );
}

#[test]
fn r4g_generic_test_fields_and_items_do_not_leak_test_sql_into_production() {
    for source in [
        r#"struct S { #[cfg(test)] fixture: Result<String,Vec<u8>>, live:u8 } fn read(){query("SELECT * FROM flow_runs");}"#,
        r#"#[cfg(test)] fn f<T,U>() {execute("DELETE FROM flow_runs");} fn read(){query("SELECT * FROM flow_runs");}"#,
        r#"struct S { #[cfg(test)] fixture: Map<String,Box<dyn Fn(i32,i32)->Result<i32,i32>>>, live:u8 } fn read(){query("SELECT * FROM flow_runs");}"#,
    ] {
        let text = production(source);
        let groups = groups(&text);
        assert_eq!(
            groups.iter().filter(|s| mentions(s)).collect::<Vec<_>>(),
            vec![&"SELECT * FROM flow_runs".to_owned()],
            "{text}"
        );
        assert!(
            !text.contains("String"),
            "the entire generic test field/item is removed: {text}"
        );
    }
}

#[test]
fn r4g_bare_table_constants_cannot_borrow_a_readers_inventory_allowance() {
    let known = "reader.rs";
    let mut reader = Inventory::new();
    observe("SELECT * FROM flow_runs", known, &mut vec![], &mut reader);
    let mut dynamic = Inventory::new();
    for text in groups(
        r#"const TABLE:&str="flow_runs";fn write(){execute(format!("DELETE FROM {TABLE}"));}"#,
    ) {
        observe(&text, known, &mut vec![], &mut dynamic);
    }
    assert!(
        compare(&dynamic, &reader).is_err(),
        "a bare identifier is a separate inventory kind from a read query"
    );
}

#[test]
fn r4g_external_cfg_test_modules_are_not_scanned_as_production() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let src = root.join("crates/other/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "#[cfg(test)] mod tests;\nmod live;").unwrap();
    std::fs::write(
        src.join("tests.rs"),
        r#"fn fixture(){execute("DELETE FROM flow_runs");}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("live.rs"),
        r#"fn live(){query("SELECT * FROM flow_runs");}"#,
    )
    .unwrap();
    let mut writers = vec![];
    let mut found = Inventory::new();
    walk(&root, &root, &mut writers, &mut found);
    assert!(writers.is_empty(), "{writers:?}");
    assert_eq!(
        found,
        BTreeMap::from([("crates/other/src/live.rs".into(), 1)])
    );
    // An explicitly named test module and its ordinary child are excluded, too.
    std::fs::write(
        src.join("lib.rs"),
        "#[cfg(test)] #[path=\"fixture.rs\"] mod tests;\nmod live;",
    )
    .unwrap();
    std::fs::write(src.join("fixture.rs"), "mod nested;").unwrap();
    std::fs::create_dir_all(src.join("fixture")).unwrap();
    std::fs::write(
        src.join("fixture/nested.rs"),
        r#"fn fixture(){execute("UPDATE flow_runs SET status='done'");}"#,
    )
    .unwrap();
    // Remove the first fixture, so the second layout has no undeclared standalone file.
    std::fs::remove_file(src.join("tests.rs")).unwrap();
    writers.clear();
    found.clear();
    walk(&root, &root, &mut writers, &mut found);
    assert!(writers.is_empty(), "{writers:?}");
    assert_eq!(
        found,
        BTreeMap::from([("crates/other/src/live.rs".into(), 1)])
    );
    // A live reference to that same module wins: test ownership cannot hide a writer.
    std::fs::write(src.join("lib.rs"),"#[cfg(test)] #[path=\"fixture.rs\"] mod tests;\n#[path=\"fixture.rs\"] mod shared; mod live;").unwrap();
    writers.clear();
    found.clear();
    walk(&root, &root, &mut writers, &mut found);
    assert_eq!(writers, vec!["crates/other/src/fixture/nested.rs"]);
}
