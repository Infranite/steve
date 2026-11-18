//! Private, atomically replaced Bot caches and immutable skill views for individual session roots.
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use steve_node_api::*;

use crate::attachments::{private_dir, publish_private};

pub(crate) fn supported(props: &steve_acp::adapters::AdapterProperties) -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
        && props
            .state
            .as_ref()
            .is_some_and(|state| !state.env.is_empty() || state.xdg || state.home.is_some())
        && props.skills.as_deref().is_some_and(subdir_ok)
}

fn subdir_ok(dir: &str) -> bool {
    HarnessState {
        dir: dir.into(),
        import_owner: false,
        owner_context: false,
    }
    .valid()
}

fn cache_path(home: &Path, snap: &SkillSnapshot) -> ApiResult<PathBuf> {
    let id = derived_id("bot", &format!("{}\0{}", snap.workspace, snap.bot));
    Ok(home.join("managed-skills").join(format!("{id}.json")))
}

fn read_cache(path: &Path) -> ApiResult<Option<SyncSkills>> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() < 2 * 1024 * 1024 => {}
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        _ => return Err(cache_error()),
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| cache_error())?)
        .map(Some)
        .map_err(|_| cache_error())
}

fn cache_error() -> ApiError {
    ApiError::new(
        ErrorCode::Internal,
        "cannot read or write the node's private skill cache",
    )
}

/// Called under the Node's skill lock. No legacy harness directory can be changed here.
pub(crate) fn sync(home: &Path, target: &SyncSkills) -> ApiResult<SkillCacheReceipt> {
    let snap = &target.snapshot;
    snap.check()?;
    if target.generation < 1 {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "invalid skill generation",
        ));
    }
    let path = cache_path(home, snap)?;
    let root = path.parent().expect("cache directory");
    private_dir(root).map_err(|_| cache_error())?;
    let receipt = SkillCacheReceipt {
        generation: target.generation,
        digest: snap.digest.clone(),
    };
    let before = read_cache(&path)?;
    if let Some(before) = &before {
        if before.generation > target.generation
            || (before.generation == target.generation && before != target)
        {
            return Err(ApiError::new(
                ErrorCode::Stale,
                "obsolete skill cache target",
            ));
        }
    }
    if before.as_ref() != Some(target) {
        let bytes = serde_json::to_vec(target).map_err(crate::internal)?;
        publish_private(&path, &bytes).map_err(|_| cache_error())?;
    }
    sync_dir(root).map_err(|_| cache_error())?;
    Ok(receipt)
}

/// Open commands are their own durable delivery of a pinned snapshot. The proactive cache can
/// serve it when it matches, but may already have moved on (or been revoked) since this binding.
pub(crate) fn pinned(home: &Path, snapshot: &SkillSnapshot) -> ApiResult<SkillSnapshot> {
    snapshot.check()?;
    let path = cache_path(home, snapshot)?;
    let cache_root = path.parent().expect("cache directory");
    if fs::symlink_metadata(cache_root).is_ok_and(|m| m.is_dir())
        && let Ok(Some(hit)) = read_cache(&path)
        && hit.snapshot.digest == snapshot.digest
        && hit.snapshot.check().is_ok()
    {
        return Ok(hit.snapshot);
    }
    Ok(snapshot.clone())
}

/// Materialize only in this session's private directory, and do not overwrite even a local
/// regular file of the same name. A previously managed file must still match the original bytes.
pub(crate) fn place(root: &Path, subdir: &str, snap: &SkillSnapshot) -> ApiResult<SkillUseReceipt> {
    let fail = || {
        ApiError::new(
            ErrorCode::Internal,
            "cannot materialize the session's skill snapshot",
        )
    };
    if !subdir_ok(subdir) {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "invalid adapter skill directory",
        ));
    }
    let mut dir = root.to_path_buf();
    for part in subdir.split('/') {
        dir.push(part);
        private_dir(&dir).map_err(|_| fail())?;
    }
    let mut receipt = SkillUseReceipt {
        digest: snap.digest.clone(),
        materialized: vec![],
        conflicts: vec![],
    };
    for (entry, reference) in snap.entries.iter().zip(snap.refs()) {
        let path = dir.join(&entry.body.name);
        let file = path.join("SKILL.md");
        let marker = path.join(".steve-managed-skill");
        let bytes = entry.body.markdown();
        let digest = snap.digest.as_str().as_bytes();
        let regular_matches = |p: &Path, bytes: &[u8]| {
            fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() == bytes.len() as u64)
                && fs::read(p).is_ok_and(|b| b == bytes)
        };
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                if m.is_dir()
                    && regular_matches(&marker, digest)
                    && regular_matches(&file, bytes.as_bytes())
                {
                    receipt.materialized.push(reference);
                } else {
                    receipt.conflicts.push(reference);
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // Build the complete skill before making the directory visible. No other
                // managed operation writes this session's root; a planted name always wins.
                let tmp = tempfile::Builder::new()
                    .prefix(".steve-skill-")
                    .tempdir_in(&dir)
                    .map_err(|_| fail())?;
                publish_private(&tmp.path().join("SKILL.md"), bytes.as_bytes())
                    .map_err(|_| fail())?;
                publish_private(&tmp.path().join(".steve-managed-skill"), digest)
                    .map_err(|_| fail())?;
                sync_dir(tmp.path()).map_err(|_| fail())?;
                if let Err(e) = rename_skill(tmp.path(), &path) {
                    if e.kind() == ErrorKind::AlreadyExists {
                        receipt.conflicts.push(reference);
                        continue;
                    }
                    return Err(fail());
                }
                let _ = tmp.keep();
                receipt.materialized.push(reference);
            }
            Err(_) => return Err(fail()),
        }
    }
    sync_dir(&dir).map_err(|_| fail())?;
    Ok(receipt)
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Commit the whole skill without replacing an existing name (including an empty directory).
/// An interrupted write before this point leaves only a private staging directory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_skill(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Both C strings remain live for the syscall; neither call follows the final component.
    let result = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL)
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_skill(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from(ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(generation: i64, content: &str) -> SyncSkills {
        SyncSkills {
            generation,
            adapter: "fixture".into(),
            snapshot: SkillSnapshot::new(
                "workspace".into(),
                "bot".into(),
                vec![VersionedSkill {
                    id: "skill".into(),
                    version: generation,
                    publication: generation,
                    body: Skill {
                        name: "review".into(),
                        description: "Review a change".into(),
                        content: content.into(),
                    },
                }],
            )
            .unwrap(),
        }
    }

    #[test]
    fn failed_writes_are_retryable_and_stale_targets_cannot_replace_the_cache() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("managed-skills");
        fs::write(&root, "not a directory").unwrap();
        let first = target(1, "old");
        let latest = target(3, "new");
        assert_eq!(
            sync(home.path(), &first).unwrap_err().code,
            ErrorCode::Internal
        );
        fs::remove_file(root).unwrap();
        sync(home.path(), &first).unwrap();
        sync(home.path(), &latest).unwrap();
        assert_eq!(
            sync(home.path(), &first).unwrap_err().code,
            ErrorCode::Stale
        );
        assert_eq!(
            sync(home.path(), &target(3, "different")).unwrap_err().code,
            ErrorCode::Stale
        );
        assert_eq!(
            read_cache(&cache_path(home.path(), &latest.snapshot).unwrap()).unwrap(),
            Some(latest.clone())
        );
        assert_eq!(sync(home.path(), &latest).unwrap().generation, 3);
        assert_eq!(
            pinned(home.path(), &first.snapshot).unwrap(),
            first.snapshot
        );
    }

    #[test]
    fn old_views_are_unchanged_and_local_names_win() {
        let first = target(1, "old");
        let latest = target(3, "new");
        let before = tempfile::tempdir().unwrap();
        let after = tempfile::tempdir().unwrap();
        let conflict = tempfile::tempdir().unwrap();
        let file = "skills/review/SKILL.md";
        for _ in 0..2 {
            let r = place(before.path(), "skills", &first.snapshot).unwrap();
            assert_eq!(r.materialized, first.snapshot.refs());
        }
        place(after.path(), "skills", &latest.snapshot).unwrap();
        assert!(
            fs::read_to_string(before.path().join(file))
                .unwrap()
                .contains("old")
        );
        assert!(
            fs::read_to_string(after.path().join(file))
                .unwrap()
                .contains("new")
        );
        fs::create_dir_all(conflict.path().join("skills/review")).unwrap();
        fs::write(conflict.path().join(file), "local").unwrap();
        let r = place(conflict.path(), "skills", &latest.snapshot).unwrap();
        assert_eq!(r.conflicts, latest.snapshot.refs());
        assert_eq!(
            fs::read_to_string(conflict.path().join(file)).unwrap(),
            "local"
        );

        #[cfg(unix)]
        {
            fs::remove_file(conflict.path().join(file)).unwrap();
            fs::remove_dir(conflict.path().join("skills/review")).unwrap();
            std::os::unix::fs::symlink(
                before.path().join("skills/review"),
                conflict.path().join("skills/review"),
            )
            .unwrap();
            let r = place(conflict.path(), "skills", &latest.snapshot).unwrap();
            assert_eq!(r.conflicts, latest.snapshot.refs());
            assert!(
                fs::read_to_string(before.path().join(file))
                    .unwrap()
                    .contains("old")
            );
        }
    }
}
