use std::path::{Path, PathBuf};

use steve_toolchains::{DirectoryError, Places, Scope, Toolchains};

fn places() -> Places {
    Places {
        item: "/work/.steve/build".into(),
        person: "/node/cache/person".into(),
    }
}

fn custom(dirs: &str) -> Toolchains {
    Toolchains::parse(&format!(
        "[[toolchain]]\nid = '陌生=工具'\ndetect = ['project.xyz']\ndirs = [{dirs}]"
    ))
    .unwrap()
}

#[test]
fn discovers_all_locations_in_definition_then_project_order_and_detect_deduplicates() {
    let root = tempfile::tempdir().unwrap();
    let defs = Toolchains::parse(
        r#"
[[toolchain]]
id = "second-alphabetically"
detect = ["project.*", "*.xyz"]
dirs = [{name="output", scope="item", in_tree="out/nested"}]
[[toolchain]]
id = "first-alphabetically"
detect = ["other.marker"]
"#,
    )
    .unwrap();
    for dir in ["z", "a", ".hidden", "out", "deep/b"] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    for dir in ["", "z", "a", ".hidden", "out", "deep/b"] {
        std::fs::write(root.path().join(dir).join("project.xyz"), "").unwrap();
    }
    std::fs::write(root.path().join("a/other.marker"), "").unwrap();
    let found = defs.discover(root.path());
    let rows: Vec<_> = found
        .iter()
        .map(|f| (f.toolchain.id.as_str(), f.project.as_path()))
        .collect();
    assert_eq!(
        rows,
        [
            ("second-alphabetically", root.path()),
            ("second-alphabetically", root.path().join("a").as_path()),
            ("second-alphabetically", root.path().join("z").as_path()),
            ("first-alphabetically", root.path().join("a").as_path()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn discovery_ignores_linked_root_children_and_markers_but_keeps_directory_markers() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let defs = custom("");
    std::fs::write(outside.path().join("project.xyz"), "").unwrap();
    symlink(outside.path(), root.path().join("linked-project")).unwrap();
    symlink(
        outside.path().join("project.xyz"),
        root.path().join("project.xyz"),
    )
    .unwrap();
    symlink(root.path(), outside.path().join("linked-root")).unwrap();
    assert!(defs.discover(root.path()).is_empty());
    assert!(
        defs.discover(&outside.path().join("linked-root"))
            .is_empty()
    );
    std::fs::create_dir_all(root.path().join("real/project.xyz")).unwrap();
    let found = defs.discover(root.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].project, root.path().join("real"));
}

#[cfg(unix)]
#[test]
fn discovery_ignores_non_utf8_markers() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let defs = Toolchains::parse("[[toolchain]]\nid='unknown'\ndetect=['*.xyz']").unwrap();
    let filename = OsString::from_vec(vec![b'p', 0xff, b'.', b'x', b'y', b'z']);
    if let Err(error) = std::fs::write(root.path().join(filename), "") {
        eprintln!("filesystem does not support this non-UTF-8 fixture: {error}");
        return;
    }
    assert!(defs.discover(root.path()).is_empty());
}

#[test]
fn declarations_keep_scope_project_hints_and_shared_paths_for_the_measurement_layer() {
    let defs = Toolchains::parse(
        r#"
[[toolchain]]
id="one"
detect=["project.xyz"]
dirs=[{name="out", scope="seed", in_tree="deps", disposable=true}]
[[toolchain]]
id="two"
detect=["project.xyz"]
dirs=[{name="deps", scope="item", in_tree="deps"}]
"#,
    )
    .unwrap();
    let a = defs.all()[0].directory_plan(Path::new("/work/web"), &places());
    let b = defs.all()[1].directory_plan(Path::new("/work/web"), &places());
    assert_eq!(a[0].project, Path::new("/work/web"));
    assert_eq!(a[0].declaration.scope, Scope::Seed);
    assert!(a[0].declaration.disposable);
    assert_eq!(a[0].path, Ok(PathBuf::from("/work/web/deps")));
    assert_eq!(a[0].path, b[0].path, "don't lose either declaration");
}

#[test]
fn equals_are_path_characters_and_environment_semantics_are_unchanged() {
    let defs = custom(
        "{name='cache', scope='person', env='STRANGE_CACHE', value='{person}/dir=with=value', concurrent_safe=true}",
    );
    let tc = &defs.all()[0];
    let expected = PathBuf::from("/node/cache/person/陌生=工具/dir=with=value");
    assert_eq!(
        tc.directory_plan(Path::new("/work"), &places())[0].path,
        Ok(expected.clone())
    );
    assert_eq!(
        tc.env(&places(), &Default::default()).unwrap(),
        [("STRANGE_CACHE".into(), expected.display().to_string())]
    );
}

#[test]
fn placeholders_are_expanded_only_in_templates_never_in_inserted_places_or_ids() {
    let defs = custom(
        "{name='out', scope='seed', env='OUT', value='{item}/dir=with=value'}, \
         {name='cache', scope='person', env='CACHE', value='{person}/dir=with=value', concurrent_safe=true}",
    );
    let mut tc = defs.all()[0].clone();
    tc.id = "tool{person}{item}=x".into();
    let at = Places {
        item: "/work/{person}/{item}/build=1".into(),
        person: "/node/{item}/{person}/cache=2".into(),
    };
    assert_eq!(tc.validate_places(&at), Ok(()));
    let plan = tc.directory_plan(Path::new("/work"), &at);
    let env: std::collections::BTreeMap<_, _> = tc
        .env(&at, &Default::default())
        .unwrap()
        .into_iter()
        .collect();
    let expected_item = at.item.join(&tc.id).join("dir=with=value");
    let expected_person = at.person.join(&tc.id).join("dir=with=value");
    assert_eq!(plan[0].path, Ok(expected_item.clone()));
    assert_eq!(plan[1].path, Ok(expected_person.clone()));
    assert_eq!(env["OUT"], expected_item.to_str().unwrap());
    assert_eq!(env["CACHE"], expected_person.to_str().unwrap());
}

#[test]
fn builtins_all_resolve_without_changing_their_definitions() {
    for tc in Toolchains::builtin().all() {
        for dir in tc.directory_plan(Path::new("/work"), &places()) {
            assert!(
                dir.path.is_ok(),
                "{}/{:?}: {:?}",
                tc.id,
                dir.declaration,
                dir.path
            );
        }
    }
}

#[test]
fn malformed_and_unknown_declarations_are_retained_as_errors() {
    let cases = [
        ("{name='x', scope='item'}", DirectoryError::MissingPath),
        (
            "{name='x', scope='item', env='CACHE'}",
            DirectoryError::EnvValueMismatch,
        ),
        (
            "{name='x', scope='item', value='{item}/x'}",
            DirectoryError::EnvValueMismatch,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{item}/x', in_tree='x'}",
            DirectoryError::ConflictingPaths,
        ),
        (
            "{name='x', scope='item', in_tree='/outside'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', in_tree='../outside'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', in_tree='sub/../../outside'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', in_tree='C:/outside'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', in_tree='..\\outside'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='person', in_tree='shared'}",
            DirectoryError::ScopeMismatch,
        ),
        (
            "{name='x', scope='person', env='CACHE', value='{item}/cache'}",
            DirectoryError::ScopeMismatch,
        ),
        (
            "{name='x', scope='seed', env='CACHE', value='{person}/cache'}",
            DirectoryError::ScopeMismatch,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='--cache={item}/cache'}",
            DirectoryError::InvalidTemplate,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{item}suffix'}",
            DirectoryError::InvalidTemplate,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{future}/cache'}",
            DirectoryError::InvalidTemplate,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{item}/{future}'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{item}/../escape'}",
            DirectoryError::InvalidRelativePath,
        ),
        (
            "{name='x', scope='item', env='CACHE', value='{item}//escape'}",
            DirectoryError::InvalidRelativePath,
        ),
    ];
    for (definition, error) in cases {
        let defs = custom(definition);
        let plan = defs.all()[0].directory_plan(Path::new("/work"), &places());
        assert_eq!(plan.len(), 1, "{definition}");
        assert_eq!(plan[0].path, Err(error), "{definition}");
    }
}

#[test]
fn toolchain_ids_places_and_projects_cannot_escape_their_roots() {
    for id in [
        "",
        ".",
        "..",
        "../escape",
        "/escape",
        "a/b",
        "a\\b",
        "C:drive",
    ] {
        let defs = custom("{name='x', scope='item', env='CACHE', value='{item}/x'}");
        // Exercise constructed Toolchain data too; parsing is deliberately not a
        // new startup rejection or schema migration.
        let mut tc = defs.all()[0].clone();
        tc.id = id.into();
        assert_eq!(
            tc.directory_plan(Path::new("/work"), &places())[0].path,
            Err(DirectoryError::InvalidToolchainId),
            "{id}"
        );
    }
    let defs = custom("{name='x', scope='item', in_tree='cache'}");
    for project in ["relative", "/work/../outside"] {
        assert_eq!(
            defs.all()[0].directory_plan(Path::new(project), &places())[0].path,
            Err(DirectoryError::InvalidProject)
        );
    }
    let defs = custom("{name='x', scope='item', env='CACHE', value='{item}/cache'}");
    for item in ["relative", "/work/../outside"] {
        let at = Places {
            item: item.into(),
            ..places()
        };
        assert_eq!(
            defs.all()[0].directory_plan(Path::new("/work"), &at)[0].path,
            Err(DirectoryError::InvalidPlace)
        );
    }
}

#[test]
fn empty_directory_plans_still_require_valid_ids_and_places() {
    let defs = custom("");
    let mut tc = defs.all()[0].clone();
    assert!(tc.directory_plan(Path::new("/work"), &places()).is_empty());
    for id in [
        "../escape",
        ".",
        "line\nbreak",
        "tab\tname",
        "nul\0name",
        "esc\u{1b}",
    ] {
        tc.id = id.into();
        assert_eq!(
            tc.validate_places(&places()),
            Err(DirectoryError::InvalidToolchainId),
            "{id:?}"
        );
    }
    tc.id = "正常=工具".into();
    assert_eq!(tc.validate_places(&places()), Ok(()));
    let at = Places {
        item: "relative".into(),
        ..places()
    };
    assert_eq!(tc.validate_places(&at), Err(DirectoryError::InvalidPlace));
}

#[cfg(unix)]
#[test]
fn directory_paths_and_environment_reject_non_utf8() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let invalid = PathBuf::from(OsString::from_vec(b"/work/non-utf8-\xff".to_vec()));
    let defs = custom("{name='out', scope='item', env='OUT', value='{item}/target'}");
    let tc = &defs.all()[0];
    for at in [
        Places {
            item: invalid.clone(),
            ..places()
        },
        Places {
            person: invalid.clone(),
            ..places()
        },
    ] {
        assert_eq!(tc.validate_places(&at), Err(DirectoryError::NonUtf8Path));
        assert_eq!(
            tc.directory_plan(Path::new("/work"), &at)[0].path,
            Err(DirectoryError::NonUtf8Path)
        );
    }
    let at = Places {
        item: invalid.clone(),
        ..places()
    };
    assert_eq!(
        tc.env(&at, &Default::default()),
        Err(DirectoryError::NonUtf8Path)
    );
    let empty = custom("");
    assert_eq!(
        empty.all()[0].validate_places(&at),
        Err(DirectoryError::NonUtf8Path)
    );
    let in_tree = custom("{name='out', scope='item', in_tree='target'}");
    assert_eq!(
        in_tree.all()[0].directory_plan(&invalid, &places())[0].path,
        Err(DirectoryError::NonUtf8Path)
    );
}
