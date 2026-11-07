//! A session's toolchains (path model §6.5, `steve-toolchains`): what its project uses, detected in
//! its working directory, and the variables that put those tools' outputs into the item's own
//! `.steve/build/<id>` (ignored by git) and their shared caches into the person's own cache on this
//! Node, instead of the Node owner's home. Off unless `STEVE_TOOLCHAINS=on` on the Node: moving a
//! build's output means its first build there starts cold.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use steve_node_api::HarnessState;
use steve_toolchains::{Places, Toolchains};

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("STEVE_TOOLCHAINS").is_ok_and(|v| v.trim() == "on"))
}

fn definitions() -> &'static Toolchains {
    static DEFS: OnceLock<Toolchains> = OnceLock::new();
    DEFS.get_or_init(Toolchains::builtin)
}

/// What a run in `cwd` gets for its toolchains: the variables, and the person's own cache
/// directory it may write (`None`: nothing). Only for a run whose principal is known (`state`):
/// without one there is no one's own cache to give it. A directory that cannot be made leaves its
/// toolchain out (the run goes on as before for it).
pub(crate) struct ForRun {
    pub env: Vec<(OsString, OsString)>,
    pub person: Option<PathBuf>,
}

pub(crate) fn for_run(node_home: &Path, cwd: &Path, state: Option<&HarnessState>) -> ForRun {
    if !on() {
        return ForRun {
            env: vec![],
            person: None,
        };
    }
    for_run_with(definitions(), node_home, cwd, state)
}

/// Each name a real directory made here (0700), never a link someone left on the way.
fn real_dirs(base: &Path, parts: &[&str]) -> Result<PathBuf, String> {
    let mut dir = base.to_path_buf();
    for part in parts {
        dir.push(part);
        crate::attachments::private_dir(&dir)?;
    }
    Ok(dir)
}

fn for_run_with(
    defs: &Toolchains,
    node_home: &Path,
    cwd: &Path,
    state: Option<&HarnessState>,
) -> ForRun {
    let none = ForRun {
        env: vec![],
        person: None,
    };
    // one person's caches per workspace: the first two names of their harness directory
    let Some(state) = state.filter(|s| s.valid()) else {
        return none;
    };
    let owner: Vec<&str> = state.dir.split('/').take(2).collect();
    if owner.len() != 2 {
        return none;
    }
    let found = defs.detect(cwd);
    if found.is_empty() {
        return none;
    }
    let build = cwd.join(".steve/build");
    let at = Places {
        item: build.clone(),
        person: node_home.join("cache").join(owner[0]).join(owner[1]),
    };
    let found: Vec<_> = found
        .into_iter()
        .filter_map(|toolchain| {
            // Validate even an empty plan. A malformed declaration must not disappear from
            // dirs_to_make while its environment still points at an unchecked location.
            let planned = toolchain.validate_places(&at).and_then(|()| {
                toolchain
                    .directory_plan(cwd, &at)
                    .into_iter()
                    .map(|dir| {
                        dir.path
                            .map(|path| (dir.declaration.env.is_some(), path))
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
            match planned {
                Ok(dirs) => Some((
                    toolchain,
                    dirs.into_iter()
                        .filter_map(|(moved, path)| moved.then_some(path))
                        .collect::<Vec<_>>(),
                )),
                Err(reason) => {
                    tracing::warn!(toolchain = ?toolchain.id, ?reason, "toolchain directory declaration skipped");
                    None
                }
            }
        })
        .collect();
    if found.is_empty() {
        return none;
    }
    let person = match real_dirs(node_home, &["cache", owner[0], owner[1]]) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("toolchains: {e}");
            return none;
        }
    };
    let made = crate::attachments::real_dir(&cwd.join(".steve"))
        .and_then(|()| crate::attachments::real_dir(&build))
        .and_then(|()| {
            let ign = build.join(".gitignore");
            if std::fs::symlink_metadata(&ign).is_ok_and(|m| m.file_type().is_file()) {
                Ok(())
            } else {
                crate::attachments::publish(&ign, b"*\n")
            }
        });
    if let Err(e) = made {
        tracing::warn!("toolchains: {e}");
        return none;
    }
    let mut env = vec![];
    for (t, dirs) in found {
        // each directory a real one under the item's or the person's own
        let made = dirs.iter().try_for_each(|d| {
            let (base, rel) = if let Ok(r) = d.strip_prefix(&person) {
                (&person, r)
            } else if let Ok(r) = d.strip_prefix(&build) {
                (&build, r)
            } else {
                return Err(format!("{} is outside its places", d.display()));
            };
            let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            real_dirs(base, &parts).map(|_| ())
        });
        if let Err(e) = made {
            tracing::warn!("toolchains: {}: {e}", t.id);
            continue;
        }
        for (k, v) in t.env(&at, &Default::default()) {
            // a definition from a Node or a workspace sets nothing Steve keeps for itself
            if steve_node_api::env::user_env_refusal(&k).is_some() {
                tracing::warn!("toolchains: {}: {k} is Steve's, not set", t.id);
                continue;
            }
            env.push((k.into(), v.into()));
        }
    }
    ForRun {
        env,
        person: Some(person),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn state() -> HarnessState {
        HarnessState {
            dir: "ws/user/bot/adapter".into(),
            import_owner: false,
        }
    }

    #[test]
    fn literal_template_text_and_equals_in_real_paths_are_not_reinterpreted() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("{item}=node");
        let work = temp.path().join("{person}=work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("Example.project"), "").unwrap();
        let defs = Toolchains::parse(
            r#"
[[toolchain]]
id = "future=工具"
detect = ["Example.project"]
dirs = [
  { name = "output", scope = "item", env = "EXAMPLE_BUILD_DIR", value = "{item}/out=cache" },
  { name = "cache", scope = "person", env = "EXAMPLE_CACHE_DIR", value = "{person}/cache" },
]
"#,
        )
        .unwrap();
        let run = for_run_with(&defs, &home, &work, Some(&state()));
        let env: std::collections::BTreeMap<_, _> = run.env.into_iter().collect();
        let output = work.join(".steve/build/future=工具/out=cache");
        let cache = home.join("cache/ws/user/future=工具/cache");
        assert_eq!(env[OsStr::new("EXAMPLE_BUILD_DIR")], output.as_os_str());
        assert_eq!(env[OsStr::new("EXAMPLE_CACHE_DIR")], cache.as_os_str());
        assert!(output.is_dir() && cache.is_dir());
        assert_eq!(run.person, Some(home.join("cache/ws/user")));
    }

    #[test]
    fn invalid_empty_plans_cannot_inject_environment_or_create_directories() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("node");
        let work = temp.path().join("work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("Example.project"), "").unwrap();
        let defs = Toolchains::parse(
            r#"
[[toolchain]]
id = "../escape"
detect = ["Example.project"]
env = { EXAMPLE_BAD_VALUE = "must-not-be-injected" }
"#,
        )
        .unwrap();
        let run = for_run_with(&defs, &home, &work, Some(&state()));
        assert!(run.env.is_empty() && run.person.is_none());
        assert!(!home.join("cache").exists());
        assert!(!work.join(".steve").exists());
    }

    #[test]
    fn one_invalid_directory_skips_the_whole_toolchain_but_not_other_tools() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("node");
        let work = temp.path().join("work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("Example.project"), "").unwrap();
        let defs = Toolchains::parse(
            r#"
[[toolchain]]
id = "bad"
detect = ["Example.project"]
env = { EXAMPLE_BAD_OPTION = "not-applied" }
dirs = [
  { name = "first", scope = "item", env = "EXAMPLE_BAD_DIR", value = "{item}/first" },
  { name = "second", scope = "item", in_tree = "../escape" },
]
[[toolchain]]
id = "good"
detect = ["Example.project"]
dirs = [
  { name = "output", scope = "item", env = "EXAMPLE_GOOD_DIR", value = "{item}/output" },
]
"#,
        )
        .unwrap();
        let run = for_run_with(&defs, &home, &work, Some(&state()));
        let env: std::collections::BTreeMap<_, _> = run.env.into_iter().collect();
        assert!(!env.contains_key(OsStr::new("EXAMPLE_BAD_OPTION")));
        assert!(!env.contains_key(OsStr::new("EXAMPLE_BAD_DIR")));
        assert_eq!(
            env[OsStr::new("EXAMPLE_GOOD_DIR")],
            work.join(".steve/build/good/output").as_os_str()
        );
        assert!(!work.join(".steve/build/bad").exists());
        assert!(work.join(".steve/build/good/output").is_dir());
    }

    #[test]
    fn concurrent_first_runs_keep_their_cache_redirection() {
        let t = tempfile::tempdir().unwrap();
        let (home, work) = (t.path().join("node"), t.path().join("work"));
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("go.mod"), "").unwrap();
        let state = HarnessState {
            dir: "ws-1/u1-2/dev-3/claude-code-4".into(),
            import_owner: false,
        };
        let defs = Toolchains::builtin();
        let barrier = std::sync::Barrier::new(12);
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    barrier.wait();
                    let run = for_run_with(&defs, &home, &work, Some(&state));
                    assert_eq!(run.person, Some(home.join("cache/ws-1/u1-2")));
                    assert!(run.env.iter().any(|(key, _)| key == "GOCACHE"));
                    assert!(run.env.iter().any(|(key, _)| key == "GOMODCACHE"));
                });
            }
        });
    }

    #[test]
    fn a_cargo_project_builds_in_its_item_and_caches_in_its_person() {
        let t = tempfile::tempdir().unwrap();
        let (home, work) = (t.path().join("node"), t.path().join("work"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(work.join("Cargo.toml"), "").unwrap();
        let state = HarnessState {
            dir: "ws-1/u1-2/dev-3/claude-code-4".into(),
            import_owner: false,
        };
        let run = for_run_with(&Toolchains::builtin(), &home, &work, Some(&state));
        assert_eq!(run.person, Some(home.join("cache/ws-1/u1-2")));
        let env: std::collections::BTreeMap<String, String> = run
            .env
            .into_iter()
            .map(|(k, v)| (k.into_string().unwrap(), v.into_string().unwrap()))
            .collect();
        assert_eq!(
            env["CARGO_TARGET_DIR"],
            work.join(".steve/build/cargo/target").display().to_string()
        );
        assert!(!env.contains_key("CARGO_HOME"));
        assert_eq!(
            std::fs::read_to_string(work.join(".steve/build/.gitignore")).unwrap(),
            "*\n"
        );
        // nothing to detect: nothing set, nothing made
        let empty = t.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let none = for_run_with(&Toolchains::builtin(), &home, &empty, Some(&state));
        assert!(none.env.is_empty() && none.person.is_none());
        assert!(!empty.join(".steve").exists());
        // whose run it is not known: no one's cache, nothing set
        let anon = for_run_with(&Toolchains::builtin(), &home, &work, None);
        assert!(anon.env.is_empty());
        // a person's cache reached through a planted link is refused
        let go = t.path().join("go");
        std::fs::create_dir_all(&go).unwrap();
        std::fs::write(go.join("go.mod"), "").unwrap();
        std::os::unix::fs::symlink(t.path(), home.join("cache/ws-1/x")).unwrap();
        let planted = HarnessState {
            dir: "ws-1/x/dev/claude-code".into(),
            import_owner: false,
        };
        assert!(
            for_run_with(&Toolchains::builtin(), &home, &go, Some(&planted))
                .env
                .is_empty()
        );
    }
}
