//! A text guard deliberately checks all Rust source, including test-only source and imports.
use std::{fs, path::Path};

fn violations(source: &str, raw_module: bool) -> Vec<&'static str> {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let mut found = vec![];
    for forbidden in ["serde_json::Value", "serde_json::Map", "json!"] {
        if compact.contains(forbidden) {
            found.push(forbidden);
        }
    }
    // Braced imports can span lines, contain aliases and nested groups.
    for (_, tail) in compact
        .match_indices("serde_json::{")
        .map(|(i, _)| (i, &compact[i + 13..]))
    {
        let mut depth = 1;
        let end = tail
            .char_indices()
            .find_map(|(i, c)| {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then_some(i)
            })
            .unwrap_or(tail.len());
        let group = &tail[..end];
        // Whitespace removal joins `Value as V`; starts_with also rejects these aliases.
        if group
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|token| token.starts_with("Value") || token.starts_with("Map"))
        {
            found.push("braced JSON tree import");
        }
    }
    if !raw_module && compact.contains("RawValue") {
        found.push("RawValue outside raw.rs");
    }
    found
}

fn scan(dir: &Path, failures: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, failures);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let raw_module = path == Path::new(env!("CARGO_MANIFEST_DIR")).join("src/raw.rs");
            for violation in violations(&fs::read_to_string(&path).unwrap(), raw_module) {
                failures.push(format!("{}: {violation}", path.display()));
            }
        }
    }
}

#[test]
fn contracts_keep_json_opaque() {
    let mut failures = vec![];
    scan(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut failures,
    );
    assert!(
        failures.is_empty(),
        "use concrete contracts or RawJson:\n{}",
        failures.join("\n")
    );
}

#[test]
fn guard_catches_paths_macros_and_braced_imports() {
    for source in [
        "serde_json::Value",
        "serde_json :: Map",
        "serde_json::json!({})",
        "json ! ({})",
        "use serde_json::{Deserialize,\n Value as Tree};",
        "use serde_json::{value::{RawValue}, Map as Object};",
        "serde_json::value::RawValue",
    ] {
        assert!(!violations(source, false).is_empty(), "{source}");
    }
    assert!(violations("use serde_json::value::RawValue;", true).is_empty());
    assert!(violations("use serde_json::{from_str, to_string};", false).is_empty());
}
