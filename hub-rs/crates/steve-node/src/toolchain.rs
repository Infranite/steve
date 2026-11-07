//! A session's toolchains (path model §6.5, `steve-toolchains`): what its project uses, detected in
//! its working directory, and the variables that put those tools' outputs into the item's own
//! `.steve/build/<id>` (ignored by git) and their shared caches into the person's own cache on this
//! Node, instead of the Node owner's home. Off unless `STEVE_TOOLCHAINS=on` on the Node: moving a
//! build's output means its first build there starts cold.

use std::ffi::OsString;
use std::path::Path;
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

/// The variables for a run in `cwd` (`node_home`: this Node's home; `state`: whose run it is,
/// whose cache it uses). Empty when off or nothing is detected; a directory that cannot be made
/// leaves its toolchain out (the run goes on as before for it).
pub(crate) fn env(
    node_home: &Path,
    cwd: &Path,
    state: Option<&HarnessState>,
) -> Vec<(OsString, OsString)> {
    if !on() {
        return vec![];
    }
    env_with(definitions(), node_home, cwd, state)
}

fn env_with(
    defs: &Toolchains,
    node_home: &Path,
    cwd: &Path,
    state: Option<&HarnessState>,
) -> Vec<(OsString, OsString)> {
    let found = defs.detect(cwd);
    if found.is_empty() {
        return vec![];
    }
    // one person's caches per workspace: the first two names of their harness directory
    let person = match state.map(|s| s.dir.split('/').take(2).collect::<Vec<_>>()) {
        Some(p) if p.len() == 2 => node_home.join("cache").join(p[0]).join(p[1]),
        _ => node_home.join("cache").join("_node"),
    };
    let build = cwd.join(".steve/build");
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
        return vec![];
    }
    let at = Places {
        item: build,
        person,
    };
    let mut out = vec![];
    for t in found {
        let dirs = t.dirs_to_make(&at);
        if let Err(e) = dirs.iter().try_for_each(std::fs::create_dir_all) {
            tracing::warn!("toolchains: {}: {e}", t.id);
            continue;
        }
        out.extend(
            t.env(&at, &Default::default())
                .into_iter()
                .map(|(k, v)| (k.into(), v.into())),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cargo_project_builds_in_its_item_and_caches_in_its_person() {
        let t = tempfile::tempdir().unwrap();
        let (home, work) = (t.path().join("node"), t.path().join("work"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("Cargo.toml"), "").unwrap();
        let state = HarnessState {
            dir: "ws-1/u1-2/dev-3/claude-code-4".into(),
            import_owner: false,
        };
        let env: std::collections::BTreeMap<String, String> =
            env_with(&Toolchains::builtin(), &home, &work, Some(&state))
                .into_iter()
                .map(|(k, v)| (k.into_string().unwrap(), v.into_string().unwrap()))
                .collect();
        assert_eq!(
            env["CARGO_TARGET_DIR"],
            work.join(".steve/build/cargo/target").display().to_string()
        );
        assert_eq!(
            env["CARGO_HOME"],
            home.join("cache/ws-1/u1-2/cargo").display().to_string()
        );
        assert!(home.join("cache/ws-1/u1-2/cargo").is_dir());
        assert_eq!(
            std::fs::read_to_string(work.join(".steve/build/.gitignore")).unwrap(),
            "*\n"
        );
        // nothing to detect: nothing set, nothing made
        let empty = t.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(env_with(&Toolchains::builtin(), &home, &empty, None).is_empty());
        assert!(!empty.join(".steve").exists());
    }
}
