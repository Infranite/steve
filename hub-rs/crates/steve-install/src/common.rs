//! Narrow filesystem primitives for the operator's own installation, never a session workspace.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn user_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    let path = PathBuf::from(home);
    if !path.is_absolute() {
        bail!("HOME must be absolute");
    }
    Ok(path)
}

pub fn canonical(path: &Path) -> Result<PathBuf> {
    let expanded = if let Ok(p) = path.strip_prefix("~") {
        user_home()?.join(p)
    } else {
        path.to_path_buf()
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    if absolute
        .components()
        .any(|p| matches!(p, Component::ParentDir))
    {
        bail!("path must not contain ..");
    }
    if exists(&absolute) && fs::symlink_metadata(&absolute)?.file_type().is_symlink() {
        bail!("path cannot be a symbolic link: {}", absolute.display());
    }
    let name = absolute
        .file_name()
        .context("installation home must not be a filesystem root")?;
    let parent = absolute.parent().context("path needs a parent")?;
    // Existing ancestors are resolved; a new path may have missing intermediate parents.
    let mut base = parent.to_path_buf();
    let mut suffix = Vec::new();
    while !base.exists() {
        suffix.push(
            base.file_name()
                .context("path has no existing ancestor")?
                .to_os_string(),
        );
        base = base
            .parent()
            .context("path has no existing ancestor")?
            .to_path_buf();
    }
    let mut result = fs::canonicalize(base)?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    result.push(name);
    Ok(result)
}

pub fn owned(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    let wanted = if directory { 0o700 } else { 0o600 };
    if m.uid() != unsafe { libc::getuid() }
        || m.permissions().mode() & 0o777 != wanted
        || if directory { !m.is_dir() } else { !m.is_file() }
    {
        bail!(
            "path must belong to this user, be {}, and have mode {:04o}: {}",
            if directory {
                "a directory"
            } else {
                "a regular file"
            },
            wanted,
            path.display()
        );
    }
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    if exists(path) {
        return owned(path, true);
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            private_dir(parent)?;
        }
        if fs::symlink_metadata(parent)?.file_type().is_symlink() {
            bail!("directory parent cannot be a symlink");
        }
    }
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    owned(path, true)
}

pub fn read_json(path: &Path) -> Result<Value> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        bail!("invalid metadata file: {}", path.display());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let value: Value = serde_json::from_reader(file)?;
    if !value.is_object() {
        bail!("metadata must be a JSON object: {}", path.display());
    }
    Ok(value)
}

pub fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("metadata needs parent")?;
    private_dir(parent)?;
    if exists(path) {
        owned(path, false)?;
    }
    let temporary = parent.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        private_file(&temporary, bytes)?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if exists(&temporary) {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic(path, &bytes)
}

pub fn digest(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("not a regular file: {}", path.display());
    }
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn bytes_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn shell_quote(value: &str) -> Result<String> {
    if value.contains(['\n', '\r', '\0']) {
        bail!("value contains a line break or NUL");
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn env_bytes(values: &serde_json::Map<String, Value>) -> Result<Vec<u8>> {
    let mut lines = String::from("# Managed by steve setup; secrets are referenced by file.\n");
    let mut keys: Vec<_> = values.keys().collect();
    keys.sort();
    for key in keys {
        if !key.starts_with("STEVE_")
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("invalid environment key");
        }
        lines.push_str(&format!(
            "{key}={}\n",
            shell_quote(
                values[key]
                    .as_str()
                    .context("environment value must be a string")?
            )?
        ));
    }
    Ok(lines.into_bytes())
}

pub fn clean_command(program: &Path, home: Option<&Path>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("STEVE_") {
            command.env_remove(key);
        }
    }
    if let Some(home) = home {
        command.env("STEVE_HOME", home);
    }
    command
}

pub fn regular_tree(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let m = fs::symlink_metadata(&path)?;
        if m.is_file() {
            result.push(path);
        } else if m.is_dir() {
            for child in fs::read_dir(path)? {
                pending.push(child?.path());
            }
        } else {
            bail!(
                "installation contains a link or special file: {}",
                path.display()
            );
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_does_not_follow_links_or_overwrite_unknown_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        private_dir(&home).unwrap();
        private_file(&home.join("owned.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(home.join("owned.json"), home.join("link.json")).unwrap();
        assert!(read_json(&home.join("link.json")).is_err());
        assert!(atomic(&home.join("link.json"), b"changed").is_err());
        assert_eq!(fs::read(home.join("owned.json")).unwrap(), b"{}");
    }
    #[test]
    fn shell_values_round_trip_without_interpreting_substitutions() {
        let value = "one ' quote $(touch should-not-exist) `true` $HOME";
        let quoted = shell_quote(value).unwrap();
        assert_eq!(shlex::split(&quoted).unwrap(), vec![value]);
        assert!(shell_quote("one\ntwo").is_err());
    }
}
