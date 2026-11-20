//! Host archive adapter and management backup entry. Tar is kept here once so the
//! storage contract needs no additional dependency or process-based archiver.
use anyhow::{Result, bail};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};
use steve_store_sqlite::backup::{
    self, CancelFlag, FileArchive, FileSet, Manifest, Reason, SkippedDir,
};
const PARTS: [&str; 6] = [
    "data/files",
    "signing",
    "secrets",
    "workspaces",
    "deliveries",
    "steve.env",
];
const NOT_COPIED: &str = "keychain-pass";
pub(crate) fn files(home: &Path) -> FileSet {
    FileSet {
        home: home.to_owned(),
        archive,
    }
}
pub(crate) fn reason(args: &[String]) -> Result<Reason> {
    let mut chosen = "manual";
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("backup") {
        bail!("expected backup command");
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => {}
            "--reason" => {
                chosen = it.next().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--reason needs manual, pre-upgrade, pre-destructive or diagnostic"
                    )
                })?
            }
            _ => bail!("unknown backup option: {arg}"),
        }
    }
    match chosen {
        "manual" => Ok(Reason::Manual),
        "diagnostic" => Ok(Reason::Diagnostic),
        "pre-upgrade" => Ok(Reason::PreUpgrade {
            from: "unknown".into(),
            to: "unknown".into(),
        }),
        "pre-destructive" => Ok(Reason::PreDestructive {
            what: "manual".into(),
        }),
        _ => bail!("backup reason must be manual, pre-upgrade, pre-destructive or diagnostic"),
    }
}
pub(crate) fn run(home: &Path, db: &Path, reason: Reason) -> Result<Manifest> {
    let mut plan = crate::backup::plan(home, reason);
    plan.deadline = Some(std::time::Duration::from_secs(25));
    Ok(backup::backup(db, &plan, &|_| {}, &CancelFlag::default())?)
}
pub(crate) fn command(home: &Path, db: &Path, args: &[String]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&run(
        home,
        db,
        reason(args)?,
    )?)?)
}
pub(crate) fn archive(home: &Path, out: &Path) -> std::io::Result<FileArchive> {
    let parts: Vec<&str> = PARTS
        .into_iter()
        .filter(|p| home.join(p).exists())
        .collect();
    if parts.is_empty() {
        report_skipped(&[]);
        return Ok(FileArchive::default());
    }
    let dest = out.join("files.tar");
    let partial = out.join("files.tar.partial");
    let _ = fs::remove_file(&partial);
    let mut skipped = vec![];
    let written = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)?;
        let mut b = tar::Builder::new(file);
        b.follow_symlinks(false);
        for part in parts {
            // Only item workspaces contain regenerable toolchain data. The other parts
            // hold durable user files even when their names or markers resemble caches.
            append(
                &mut b,
                home,
                Path::new(part),
                &mut skipped,
                true,
                part == "workspaces",
            )?;
        }
        let mut file = b.into_inner()?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    if let Err(error) = fs::rename(&partial, &dest) {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    report_skipped(&skipped);
    Ok(FileArchive {
        path: Some(dest),
        skipped_dirs: skipped,
    })
}

fn report_skipped(skipped: &[SkippedDir]) {
    let mut bytes = 0u64;
    for entry in skipped {
        bytes = bytes.saturating_add(entry.bytes);
        tracing::debug!(path = %entry.path.display(), bytes = entry.bytes,
            "backup: skipped regenerable directory");
    }
    tracing::info!(
        skipped_dirs = skipped.len(),
        skipped_bytes = bytes,
        "backup: regenerable directories skipped"
    );
}

/// A part's root is followed (it may be the owner's link to another disk); nothing below it is.
/// Only the workspace part enables the shared cache predicate, at any depth.
fn append(
    archive: &mut tar::Builder<fs::File>,
    home: &Path,
    relative: &Path,
    skipped: &mut Vec<SkippedDir>,
    top_level: bool,
    skip_caches: bool,
) -> std::io::Result<()> {
    let path = home.join(relative);
    let metadata = if top_level {
        fs::metadata(&path)?
    } else {
        fs::symlink_metadata(&path)?
    };
    if relative == Path::new("signing").join(NOT_COPIED) {
        return Ok(());
    }
    if skip_caches && metadata.is_dir() && steve_core::workspace::is_disposable_dir(&path, None) {
        skipped.push(SkippedDir {
            path: relative.to_owned(),
            bytes: logical_bytes(&path),
        });
        return Ok(());
    }
    if metadata.is_dir() {
        // Keep the signing layout: its children, without a root entry.
        if relative != Path::new("signing") {
            archive.append_dir(relative, &path)?;
        }
        for entry in fs::read_dir(&path)? {
            append(
                archive,
                home,
                &relative.join(entry?.file_name()),
                skipped,
                false,
                skip_caches,
            )?;
        }
    } else {
        archive.append_path_with_name(&path, relative)?;
    }
    Ok(())
}

/// Best-effort accounting: disappearing or unreadable cache entries contribute zero and never fail a backup.
/// Links are not followed while measuring a skipped directory.
fn logical_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    let mut bytes = if metadata.is_file() {
        metadata.len()
    } else {
        0
    };
    if metadata.is_dir() {
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            bytes = bytes.saturating_add(logical_bytes(&entry.path()));
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_excludes_keychain_password_and_follows_only_part_roots() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("signing")).unwrap();
        fs::write(home.path().join("signing/keychain-pass"), "excluded").unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("signing/link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("secrets")).unwrap();
        let path = archive(home.path(), out.path()).unwrap().path.unwrap();
        let mut reader = tar::Archive::new(fs::File::open(path).unwrap());
        let entries: Vec<_> = reader
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().to_string_lossy().into_owned(),
                    e.header().entry_type().is_symlink(),
                )
            })
            .collect();
        // A part root that is a link is followed (main's daily backup semantics since v0.5.0);
        // links below a part root are stored as links.
        assert!(
            entries
                .iter()
                .any(|(name, link)| name.trim_end_matches('/') == "secrets" && !*link)
        );
        assert!(entries.iter().any(|(name, _)| name == "secrets/secret"));
        assert!(
            entries
                .iter()
                .any(|(name, link)| name == "signing/link" && *link)
        );
        assert!(
            !entries.iter().any(
                |(name, _)| name.contains("keychain-pass") || name.starts_with("signing/link/")
            )
        );
    }
    fn current_database(path: &Path) {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        steve_store_sqlite::migrate::initialize(
            &mut connection,
            &steve_store_sqlite::layout::Layout::single_file(),
        )
        .unwrap();
        connection.execute_batch("CREATE TABLE record(v);").unwrap();
    }
    #[test]
    fn management_backup_uses_the_reason_archive_rule() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("steve.env"), "test fixture").unwrap();
        let db = home.path().join("db.sqlite3");
        current_database(&db);
        for reason in [
            Reason::Manual,
            Reason::Diagnostic,
            Reason::PreUpgrade {
                from: "a".into(),
                to: "b".into(),
            },
            Reason::PreDestructive {
                what: "test".into(),
            },
        ] {
            let manifest = run(home.path(), &db, reason.clone()).unwrap();
            assert_eq!(
                !manifest.files.artifacts.is_empty(),
                reason.rule().with_files
            );
            assert!(manifest.files.complete && manifest.files.error.is_none());
        }
    }
    #[test]
    fn a_real_archive_error_marks_files_incomplete_and_keeps_the_database() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("signing")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(home.path().join("signing/socket")).unwrap();
        let db = home.path().join("db.sqlite3");
        current_database(&db);
        let manifest = run(
            home.path(),
            &db,
            Reason::PreDestructive {
                what: "test".into(),
            },
        )
        .unwrap();
        assert!(manifest.completed && manifest.database_path().exists());
        assert!(!manifest.files.complete);
        assert!(manifest.files.error.is_some());
        assert!(!manifest.directory.join("files.tar.partial").exists());
    }
}
