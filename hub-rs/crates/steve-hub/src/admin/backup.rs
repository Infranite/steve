//! Host archive adapter and management backup entry. Tar is kept here once so the
//! storage contract needs no additional dependency or process-based archiver.
use anyhow::{Result, bail};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use steve_store_sqlite::backup::{self, BackupPlan, CancelFlag, FileSet, Manifest, Reason};
const PARTS: [&str; 6] = [
    "data/files",
    "signing",
    "secrets",
    "workspaces",
    "deliveries",
    "steve.env",
];
const NOT_COPIED: &str = "keychain-pass";
pub(crate) fn home(db: &Path) -> PathBuf {
    let parent = db.parent().unwrap_or_else(|| Path::new("."));
    if parent.file_name().is_some_and(|n| n == "data") {
        parent.parent().unwrap_or(parent).to_owned()
    } else {
        parent.to_owned()
    }
}
pub(crate) fn files(home: &Path) -> FileSet {
    FileSet {
        home: home.to_owned(),
        archive,
    }
}
pub(crate) fn reason(args: &[String]) -> Result<Reason> {
    let mut chosen = "diagnostic";
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("backup") {
        bail!("expected backup command");
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => {}
            "--reason" => {
                chosen = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--reason needs pre-upgrade or diagnostic"))?
            }
            _ => bail!("unknown backup option: {arg}"),
        }
    }
    match chosen {
        "diagnostic" => Ok(Reason::Diagnostic),
        "pre-upgrade" => Ok(Reason::PreUpgrade {
            from: "unknown".into(),
            to: "unknown".into(),
        }),
        _ => bail!("backup reason must be pre-upgrade or diagnostic"),
    }
}
pub(crate) fn run(home: &Path, db: &Path, reason: Reason) -> Result<Manifest> {
    let mut plan = BackupPlan::new(home.join("backups"), reason);
    plan.deadline = Some(std::time::Duration::from_secs(25));
    plan.with_files = Some(files(home));
    Ok(backup::backup(db, &plan, &|_| {}, &CancelFlag::default())?)
}
pub(crate) fn command(home: &Path, db: &Path, args: &[String]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&run(
        home,
        db,
        reason(args)?,
    )?)?)
}
pub(crate) fn archive(home: &Path, out: &Path) -> std::io::Result<Option<PathBuf>> {
    let parts: Vec<&str> = PARTS
        .into_iter()
        .filter(|p| fs::symlink_metadata(home.join(p)).is_ok())
        .collect();
    if parts.is_empty() {
        return Ok(None);
    }
    let dest = out.join("files.tar");
    let partial = out.join("files.tar.partial");
    let _ = fs::remove_file(&partial);
    let written = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)?;
        let mut b = tar::Builder::new(file);
        b.follow_symlinks(false);
        for part in parts {
            let path = home.join(part);
            if !fs::symlink_metadata(&path)?.is_dir() {
                b.append_path_with_name(&path, part)?;
            } else if part == "signing" {
                for entry in fs::read_dir(&path)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    if name == NOT_COPIED {
                        continue;
                    }
                    let inside = Path::new(part).join(&name);
                    if entry.file_type()?.is_dir() {
                        b.append_dir_all(&inside, entry.path())?;
                    } else {
                        b.append_path_with_name(entry.path(), &inside)?;
                    }
                }
            } else {
                b.append_dir_all(part, &path)?;
            }
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
    Ok(Some(dest))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_excludes_keychain_password_and_never_follows_symlinks() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("signing")).unwrap();
        fs::write(home.path().join("signing/keychain-pass"), "excluded").unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("signing/link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("secrets")).unwrap();
        let path = archive(home.path(), out.path()).unwrap().unwrap();
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
        assert!(
            entries
                .iter()
                .any(|(name, link)| name == "secrets" && *link)
        );
        assert!(
            entries
                .iter()
                .any(|(name, link)| name == "signing/link" && *link)
        );
        assert!(
            !entries
                .iter()
                .any(|(name, _)| name.contains("keychain-pass") || name.ends_with("secret"))
        );
    }
    #[test]
    fn a_real_archive_error_marks_files_incomplete_and_keeps_the_database() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("signing")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(home.path().join("signing/socket")).unwrap();
        let db = home.path().join("db.sqlite3");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE record(v);")
            .unwrap();
        let manifest = run(home.path(), &db, Reason::Diagnostic).unwrap();
        assert!(manifest.completed && manifest.database_path().exists());
        assert!(!manifest.files.complete);
        assert!(manifest.files.error.is_some());
        assert!(!manifest.directory.join("files.tar.partial").exists());
    }
}
