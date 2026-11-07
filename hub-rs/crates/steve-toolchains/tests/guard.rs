//! The places that deal with builds and disks know no tool by name (path model §6.5.1): the
//! toolchains' directory names, lockfiles and variables are data, so none of them may be a string
//! literal in the item worktree's disk keeping, the merge lane, or the Node's workspace code.
//! Tests (`#[cfg(test)]` modules) may name them.

use std::path::Path;

const WATCHED: &[&str] = &[
    "../steve-core/src/workspace.rs",
    "../steve-core/src/lane",
    "../steve-node/src/workspace.rs",
    "../steve-node/src/toolchain.rs",
];

fn sources(p: &Path, out: &mut Vec<std::path::PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            sources(&e.path(), out);
        }
    } else if p.extension().is_some_and(|e| e == "rs") {
        out.push(p.to_path_buf());
    }
}

/// The source before its first test module.
fn production(text: &str) -> &str {
    text.find("#[cfg(test)]").map_or(text, |i| &text[..i])
}

#[test]
fn no_tool_is_named_where_builds_and_disks_are_handled() {
    let defs = steve_toolchains::Toolchains::builtin();
    let mut names: Vec<String> = vec![];
    for t in defs.all() {
        names.extend(t.lockfiles.iter().cloned());
        for d in &t.dirs {
            names.extend(d.env.iter().cloned());
            if d.disposable {
                names.extend(d.in_tree.iter().cloned());
            }
        }
        names.extend(t.env.keys().cloned());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![];
    for w in WATCHED {
        sources(&root.join(w), &mut files);
    }
    assert!(files.len() >= 4, "{files:?}");
    let mut found = vec![];
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for n in &names {
            if production(&text).contains(&format!("\"{n}\"")) {
                found.push(format!("{}: \"{n}\"", f.display()));
            }
        }
    }
    assert!(found.is_empty(), "name these in the toolchain data instead: {found:#?}");
}
