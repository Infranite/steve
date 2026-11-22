//! File backup writers for the installation and its management backup entry.
use anyhow::{Result, bail};
use std::{
    fs,
    io::{self, Read, Seek, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink},
    path::{Path, PathBuf},
};
use steve_store_sqlite::backup::{
    self, CancelFlag, FileOutput, FileScope, FileSet, Manifest, Reason,
};
/// Every file part of an installation.
pub(crate) const ALL: &[&str] = &[
    "data/files",
    "signing",
    "secrets",
    "workspaces",
    "deliveries",
    "steve.env",
];
/// The installation's own state, what an upgrade can change besides the database.
pub(crate) const STATE: &[&str] = &["signing", "secrets", "steve.env"];
// Setup may leave private staging material here. Only copy the installed identity.
const SIGNING_FILES: [&str; 2] = ["identity.json", "steve-signing.keychain-db"];
pub(crate) fn files(home: &Path, reason: &Reason) -> Option<FileSet> {
    let parts = match reason.rule().files {
        FileScope::All => ALL,
        FileScope::State => STATE,
        FileScope::None => return None,
    };
    Some(FileSet {
        home: home.to_owned(),
        parts,
        output: if reason == &Reason::Daily {
            FileOutput::Snapshot(snapshot)
        } else {
            FileOutput::Archive(archive)
        },
    })
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
    let plan = crate::backup::plan(home, reason);
    Ok(backup::backup(db, &plan, &|_| {}, &CancelFlag::default())?)
}
pub(crate) fn command(home: &Path, db: &Path, args: &[String]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&run(
        home,
        db,
        reason(args)?,
    )?)?)
}
pub(crate) fn archive(home: &Path, out: &Path, parts: &[&str]) -> std::io::Result<Option<PathBuf>> {
    let parts: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| home.join(p).exists())
        .collect();
    if parts.is_empty() {
        report_skipped(&[]);
        return Ok(None);
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
    Ok(Some(dest))
}

/// A complete tree with newly copied files and links to unchanged files in the last snapshot.
/// Every link is to backup-owned storage. Restoring copies these files out of the tree.
fn snapshot(home: &Path, out: &Path, parts: &[&str], previous: Option<&Path>) -> io::Result<u64> {
    private_directory(out)?;
    let previous = existing_directory(previous);
    let mut skipped = vec![];
    let mut bytes = 0u64;
    for &part in parts {
        if !home.join(part).try_exists()? {
            continue;
        }
        let target = out.join(part);
        let previous = previous.and_then(|root| {
            let rel = Path::new(part);
            let parent = root.join(rel.parent()?);
            existing_directory(Some(&parent)).map(|p| p.join(rel.file_name().unwrap()))
        });
        let parent = target.parent().unwrap();
        private_directory(parent)?;
        bytes = bytes.saturating_add(snapshot_entry(
            home,
            Path::new(part),
            &target,
            previous.as_deref(),
            &mut skipped,
            true,
            part == "workspaces",
        )?);
        fs::File::open(parent)?.sync_all()?;
    }
    fs::File::open(out)?.sync_all()?;
    report_skipped(&skipped);
    Ok(bytes)
}

fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
}

fn existing_directory(path: Option<&Path>) -> Option<&Path> {
    path.filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
}

fn snapshot_entry(
    home: &Path,
    relative: &Path,
    target: &Path,
    previous: Option<&Path>,
    skipped: &mut Vec<SkippedDir>,
    top_level: bool,
    skip_caches: bool,
) -> io::Result<u64> {
    let path = home.join(relative);
    let meta = entry_metadata(&path, top_level)?;
    if omit(&path, relative, &meta, skipped, skip_caches) {
        return Ok(0);
    }
    if meta.is_dir() {
        private_directory(target)?;
        let previous = existing_directory(previous);
        let mut bytes = 0u64;
        for entry in fs::read_dir(&path)? {
            let name = entry?.file_name();
            bytes = bytes.saturating_add(snapshot_entry(
                home,
                &relative.join(&name),
                &target.join(&name),
                previous.map(|p| p.join(&name)).as_deref(),
                skipped,
                false,
                skip_caches,
            )?);
        }
        fs::File::open(target)?.sync_all()?;
        return Ok(bytes);
    }
    if meta.is_symlink() {
        symlink(fs::read_link(path)?, target)?;
        return Ok(0);
    }
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "unsupported backup entry: {}",
            relative.display()
        )));
    }
    let mut source = open_regular(&path, top_level)?;
    let meta = source.metadata()?;
    if let Some(previous) = previous.filter(|p| {
        fs::symlink_metadata(p).is_ok_and(|prior| {
            prior.is_file()
                && prior.len() == meta.len()
                && prior.mode() == meta.mode()
                && prior.modified().ok() == meta.modified().ok()
        })
    }) {
        match equal_contents(&mut source, previous, meta.len()) {
            Ok(true) => match fs::hard_link(previous, target) {
                Ok(()) => return Ok(meta.len()),
                // A retirement can remove the prior path while this snapshot is staged.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            },
            Ok(false) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
                ) => {}
            Err(e) => return Err(e),
        }
    }
    source.rewind()?;
    let mut copied = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)?;
    let bytes = io::copy(&mut source, &mut copied)?;
    copied.set_permissions(meta.permissions())?;
    copied.set_modified(meta.modified()?)?;
    copied.sync_all()?;
    Ok(bytes)
}

fn open_regular(path: &Path, follow_root: bool) -> io::Result<fs::File> {
    let flags = libc::O_NONBLOCK | if follow_root { 0 } else { libc::O_NOFOLLOW };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("backup entry ceased to be a regular file"));
    }
    Ok(file)
}

fn equal_contents(left: &mut fs::File, right: &Path, mut remaining: u64) -> io::Result<bool> {
    let mut right = open_regular(right, false)?;
    let mut a = [0; 64 * 1024];
    let mut b = [0; 64 * 1024];
    while remaining > 0 {
        let n = remaining.min(a.len() as u64) as usize;
        left.read_exact(&mut a[..n])?;
        right.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
        remaining -= n as u64;
    }
    // A live source may have grown since its metadata was measured.
    Ok(left.read(&mut a[..1])? == 0 && right.read(&mut b[..1])? == 0)
}

#[derive(Debug)]
struct SkippedDir {
    path: PathBuf,
    bytes: u64,
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
    let metadata = entry_metadata(&path, top_level)?;
    if omit(&path, relative, &metadata, skipped, skip_caches) {
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

fn entry_metadata(path: &Path, top_level: bool) -> io::Result<fs::Metadata> {
    if top_level {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }
}

fn omit(
    path: &Path,
    relative: &Path,
    metadata: &fs::Metadata,
    skipped: &mut Vec<SkippedDir>,
    skip_caches: bool,
) -> bool {
    if let Ok(entry) = relative.strip_prefix("signing") {
        return !entry.as_os_str().is_empty()
            && !SIGNING_FILES.iter().any(|name| entry == Path::new(name));
    }
    if skip_caches && metadata.is_dir() && steve_core::workspace::is_disposable_dir(path, None) {
        skipped.push(SkippedDir {
            path: relative.to_owned(),
            bytes: logical_bytes(path),
        });
        return true;
    }
    false
}

/// Logging only: disappearing or unreadable cache entries contribute zero and never fail a backup.
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
    fn archive_keeps_only_installed_signing_paths_and_follows_only_part_roots() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("signing/keys")).unwrap();
        for path in SIGNING_FILES {
            fs::write(home.path().join("signing").join(path), "installed").unwrap();
        }
        fs::write(home.path().join("signing/.setup-password"), "excluded").unwrap();
        fs::write(home.path().join("signing/keys/id"), "excluded").unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        fs::create_dir_all(home.path().join("data/files")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("data/files/link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("secrets")).unwrap();
        let path = archive(home.path(), out.path(), ALL).unwrap().unwrap();
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
        // The backup follows a linked part root and stores its nested links as links.
        assert!(
            entries
                .iter()
                .any(|(name, link)| name.trim_end_matches('/') == "secrets" && !*link)
        );
        assert!(entries.iter().any(|(name, _)| name == "secrets/secret"));
        assert!(
            entries
                .iter()
                .any(|(name, link)| name == "data/files/link" && *link)
        );
        let mut signing: Vec<_> = entries
            .iter()
            .filter_map(|(name, _)| name.strip_prefix("signing/"))
            .collect();
        signing.sort_unstable();
        assert_eq!(signing, SIGNING_FILES);
        assert!(
            !entries
                .iter()
                .any(|(name, _)| name.starts_with("data/files/link/"))
        );
    }
    #[test]
    fn management_backup_uses_the_reason_archive_rule() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("steve.env"), "test fixture").unwrap();
        fs::create_dir(home.path().join("workspaces")).unwrap();
        fs::write(home.path().join("workspaces/note.txt"), "work").unwrap();
        let db = home.path().join("db.sqlite3");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE record(v);")
            .unwrap();
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
            let scope = reason.rule().files;
            assert_eq!(
                !manifest.files.artifacts.is_empty(),
                scope != FileScope::None
            );
            assert!(manifest.files.complete && manifest.files.error.is_none());
            if let Some(tar) = manifest.files.artifacts.first() {
                let names: Vec<String> =
                    tar::Archive::new(fs::File::open(manifest.directory.join(&tar.name)).unwrap())
                        .entries()
                        .unwrap()
                        .map(|e| e.unwrap().path().unwrap().display().to_string())
                        .collect();
                assert!(names.iter().any(|n| n == "steve.env"), "{names:?}");
                // an upgrade only needs what it can change: the database and the installation's state
                assert_eq!(
                    names.iter().any(|n| n.starts_with("workspaces")),
                    scope == FileScope::All,
                    "{reason:?}: {names:?}"
                );
            }
        }
    }
    #[test]
    fn a_real_archive_error_marks_files_incomplete_and_keeps_the_database() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("signing")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(
            home.path().join("signing/steve-signing.keychain-db"),
        )
        .unwrap();
        let db = home.path().join("db.sqlite3");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE record(v);")
            .unwrap();
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
        assert!(run(home.path(), &db, Reason::Daily).is_err());
        let copies = backup::list(&crate::backup::root(home.path())).unwrap();
        assert_eq!(
            copies.len(),
            1,
            "an incomplete daily tree was never published"
        );
    }
}
