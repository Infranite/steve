use super::{FileScope, engine::Failure};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, ErrorKind},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink},
    path::{Path, PathBuf},
};

const STATE: &[&str] = &["secrets", "steve.env"];
const ASSETS: &[&str] = &["data/files", "workspaces", "deliveries"];
const IDENTITY: &str = "signing/identity.json";
const SIGNING: &[&str] = &[IDENTITY, "signing/steve-signing.keychain-db"];

#[derive(Default)]
pub(super) struct Staged {
    pub required: Vec<String>,
    pub files: u64,
    pub bytes: u64,
    pub skipped_caches: u64,
}

pub(super) fn stage(home: &Path, target: &Path, scope: FileScope) -> Result<Staged> {
    let mut staged = Staged::default();
    if scope == FileScope::None {
        return Ok(staged);
    }
    let home = home.canonicalize()?;
    let mut excluded = vec![home.join("backups"), home.join("vault-keys")];
    if let Some(dir) = std::env::var_os("STEVE_VAULT_KEY_DIR") {
        excluded.push(PathBuf::from(dir));
    }
    let excluded: Vec<_> = excluded
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let mut parts = STATE.to_vec();
    if fs::symlink_metadata(home.join(IDENTITY)).is_ok_and(|m| m.is_file()) {
        parts.extend(SIGNING);
    }
    if scope == FileScope::All {
        parts.extend(ASSETS);
    }
    for part in parts {
        let at = home.join(part);
        let source = match at.canonicalize() {
            Ok(path) => path,
            Err(e)
                if e.kind() == ErrorKind::NotFound
                    && !at.is_symlink()
                    && !part.starts_with("signing/") =>
            {
                continue;
            }
            Err(e) => return Err(e).context(Failure::Incomplete),
        };
        if part.starts_with("signing/") {
            // Only the installed identity and its known regular keychain file belong in the backup.
            ensure!(
                source == at && fs::symlink_metadata(&source)?.is_file(),
                Failure::Incomplete
            );
        }
        let destination = target.join(part);
        directory(destination.parent().context("missing staging parent")?)?;
        if copy(
            &source,
            &destination,
            &excluded,
            part == "workspaces",
            &mut staged,
        )
        .context(Failure::Incomplete)?
        {
            staged.required.push(format!("/files/{part}"));
        }
    }
    Ok(staged)
}

fn directory(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

fn copy(
    source: &Path,
    dest: &Path,
    excluded: &[PathBuf],
    skip_caches: bool,
    staged: &mut Staged,
) -> io::Result<bool> {
    if excluded.iter().any(|root| source.starts_with(root)) {
        return Ok(false);
    }
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() {
        if skip_caches && steve_core::workspace::is_disposable_dir(source, None) {
            staged.skipped_caches = staged.skipped_caches.saturating_add(1);
            return Ok(false);
        }
        directory(dest)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy(
                &entry.path(),
                &dest.join(entry.file_name()),
                excluded,
                skip_caches,
                staged,
            )?;
        }
    } else if meta.is_symlink() {
        symlink(fs::read_link(source)?, dest)?;
    } else if meta.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)?;
        if !input.metadata()?.is_file() {
            return Err(io::Error::other("backup source changed type"));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dest)?;
        let copied = io::copy(&mut input, &mut output)?;
        output.set_permissions(meta.permissions())?;
        output.set_modified(meta.modified()?)?;
        staged.files = staged.files.saturating_add(1);
        staged.bytes = staged.bytes.saturating_add(copied);
    } else {
        return Err(io::Error::other("unsupported backup source"));
    }
    Ok(true)
}
