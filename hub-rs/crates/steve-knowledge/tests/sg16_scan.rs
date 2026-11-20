//! SG16: filesystem behavior pinned on the original walker, using temporary roots only.
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use steve_knowledge::scan::{Held, MAX_FILE_BYTES, Rules, Walk, walk};

fn write(root: &Path, rel: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "content").unwrap();
}

fn names(walk: &Walk) -> Vec<&str> {
    walk.files.iter().map(|entry| entry.rel.as_str()).collect()
}

#[test]
fn internal_file_links_keep_aliases_and_linked_directories_are_not_entered() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    write(&root, "a.md");
    write(&root, "nested/source.md");
    symlink(root.join("nested"), root.join("0-linked-dir")).unwrap();
    symlink(root.join("nested/source.md"), root.join("b.md")).unwrap();

    let found = walk(&root, &Rules::new(vec![], vec![])).unwrap();
    assert_eq!(names(&found), ["a.md", "b.md", "nested/source.md"]);
    let alias = &found.files[1];
    let original = &found.files[2];
    assert_eq!(alias.path, root.join("nested/source.md"));
    assert_eq!((alias.size, alias.mtime), (original.size, original.mtime));
    assert_eq!(alias.size, 7);
    assert_eq!(found.links_refused, 0);
    assert!(found.held.is_empty() && found.too_big.is_empty());
    assert_eq!(found.over_limit, 0);
    assert!(found.complete());
}

#[test]
fn outside_and_dangling_links_preserve_refusal_and_held_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let root = base.join("root");
    let outside = base.join("root-other");
    fs::create_dir(&root).unwrap();
    write(&root, "safe.md");
    write(&outside, "secret.md");
    for (target, name) in [
        (outside.clone(), "out-dir.md"),
        (outside.join("secret.md"), "out-file.md"),
        (outside.join("secret.md"), "out.bin"),
        (root.join("missing.md"), "dangling.md"),
    ] {
        symlink(target, root.join(name)).unwrap();
    }

    let found = walk(&root, &Rules::new(vec![], vec![])).unwrap();
    assert_eq!(names(&found), ["safe.md"]);
    assert_eq!(found.links_refused, 4);
    assert_eq!(
        found.held,
        [
            ("out-dir.md".into(), Held::Outside),
            ("out-file.md".into(), Held::Outside),
        ]
    );
    assert!(found.complete(), "refused paths were seen, not missed");
}

#[test]
fn cyclic_links_are_held_but_a_link_back_to_the_root_is_not_entered() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    write(&root, "safe.md");
    symlink(&root, root.join("back")).unwrap();
    symlink(root.join("self.md"), root.join("self.md")).unwrap();
    symlink(root.join("b.md"), root.join("a.md")).unwrap();
    symlink(root.join("a.md"), root.join("b.md")).unwrap();

    let found = walk(&root, &Rules::new(vec![], vec![])).unwrap();
    assert_eq!(names(&found), ["safe.md"]);
    assert_eq!(found.links_refused, 3);
    assert_eq!(
        found.held,
        [
            ("a.md".into(), Held::Unreadable),
            ("b.md".into(), Held::Unreadable),
            ("self.md".into(), Held::Unreadable),
        ]
    );
    assert!(found.complete());
}

#[test]
fn fixed_and_hidden_directories_are_excluded_even_with_all_files_included() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    for prefix in ["", "nested/"] {
        for excluded in [".git", "node_modules", ".obsidian", ".trash", ".other"] {
            write(&root, &format!("{prefix}{excluded}/hidden.md"));
        }
    }
    write(&root, "visible.md");
    write(&root, "nested/visible.md");
    write(&root, ".visible.md"); // Only directories are unconditionally excluded.

    let found = walk(&root, &Rules::new(vec!["**".into()], vec![])).unwrap();
    assert_eq!(
        names(&found),
        [".visible.md", "visible.md", "nested/visible.md"]
    );
    assert_eq!(found.links_refused, 0);
    assert!(found.held.is_empty() && found.complete());
}

#[test]
fn include_and_custom_exclusions_match_relative_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    for path in [
        "a.txt",
        "data.bin",
        "drop.txt",
        "direct/hide.txt",
        "skipped/hide.txt",
        "trailing/hide.txt",
        "nested/b.txt",
        "nested/no.md",
    ] {
        write(&root, path);
    }
    symlink(root.join("data.bin"), root.join("alias.txt")).unwrap();
    symlink(root.join("a.txt"), root.join("skip-alias.txt")).unwrap();
    let rules = Rules::new(
        vec!["**/*.txt".into()],
        [
            "direct",
            "skipped/**",
            "trailing/",
            "drop.txt",
            "skip-alias.txt",
        ]
        .map(str::to_owned)
        .to_vec(),
    );

    let found = walk(&root, &rules).unwrap();
    assert_eq!(names(&found), ["a.txt", "alias.txt", "nested/b.txt"]);
    assert_eq!(found.files[1].path, root.join("data.bin"));
    assert_eq!(found.links_refused, 0);
    assert!(found.held.is_empty() && found.complete());
}

#[test]
fn size_boundary_is_inclusive_and_oversize_files_do_not_create_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::File::create(root.join("at.md"))
        .unwrap()
        .set_len(MAX_FILE_BYTES)
        .unwrap();
    fs::File::create(root.join("over.md"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();

    let found = walk(&root, &Rules::new(vec![], vec![])).unwrap();
    assert_eq!(names(&found), ["at.md"]);
    assert_eq!(found.files[0].size, MAX_FILE_BYTES);
    assert_eq!(found.too_big.len(), 1);
    assert_eq!(
        (found.too_big[0].rel.as_str(), found.too_big[0].size),
        ("over.md", MAX_FILE_BYTES + 1)
    );
    assert!(found.held.is_empty() && found.complete());
}
