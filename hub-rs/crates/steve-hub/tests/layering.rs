//! Layering guard, in ratchet mode: the workspace's crate graph may only lose edges. It reads
//! `cargo metadata` and compares two things with `tests/layering.txt`:
//!
//! - `edge a b`: crate `a` depends on workspace crate `b` (normal or build dependency; test-only
//!   dependencies don't shape production layering).
//! - `rusqlite a`: crate `a` depends on `rusqlite` in any way. SQL belongs to the storage layer;
//!   every other entry is a crate whose SQL still has to move there.
//!
//! A new edge or a new rusqlite user fails the test: depend on a lower layer's contract instead.
//! A listed one that is gone fails too, so the list shrinks with the code. Group 0 keeps the list;
//! `STEVE_LAYERING_BLESS=1 cargo test -p steve-hub --test layering` rewrites it, which a review
//! accepts only when it removes lines.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const LIST: &str = "tests/layering.txt";

fn found() -> BTreeSet<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let packages = meta["packages"].as_array().expect("packages");
    let members: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut lines = BTreeSet::new();
    for p in packages {
        let name = p["name"].as_str().expect("package name");
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dep = d["name"].as_str().expect("dependency name");
            if dep == "rusqlite" {
                lines.insert(format!("rusqlite\t{name}"));
            }
            let production = d["kind"].is_null() || d["kind"] == "build";
            if production && members.contains(dep) {
                lines.insert(format!("edge\t{name}\t{dep}"));
            }
        }
    }
    lines
}

#[test]
fn the_crate_graph_only_loses_edges() {
    let now = found();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    if std::env::var_os("STEVE_LAYERING_BLESS").is_some() {
        let head = "# The workspace's crate edges and rusqlite users (tests/layering.rs). The list only shrinks.\n";
        let body: String = now.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, format!("{head}{body}")).unwrap();
        return;
    }
    let listed: BTreeSet<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let mut problems = vec![];
    for l in now.difference(&listed) {
        let why = if l.starts_with("rusqlite") {
            "SQL belongs to the storage layer"
        } else {
            "depend on a lower layer's contract instead"
        };
        problems.push(format!("new: {} — {why}", l.replace('\t', " ")));
    }
    for l in listed.difference(&now) {
        problems.push(format!(
            "gone: {} — drop it from the list",
            l.replace('\t', " ")
        ));
    }
    assert!(problems.is_empty(), "layering:\n{}", problems.join("\n"));
}
