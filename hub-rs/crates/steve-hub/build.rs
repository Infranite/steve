mod identity {
    include!("../../build-identity.rs");
}

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn executable_regular_file(path: &Path) -> Option<PathBuf> {
    let resolved = fs::canonicalize(path).ok()?;
    let meta = fs::symlink_metadata(&resolved).ok()?;
    (resolved.is_absolute() && meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .then_some(resolved)
}

/// For local development and `cargo test` only (never release builds), locate an installed
/// `restic` executable at build time so unit/integration tests do not depend on manually
/// copying `restic` next to `target/debug/deps/<test-binary>`.
fn discover_dev_backup_engine() {
    println!("cargo:rerun-if-env-changed=STEVE_BACKUP_ENGINE");
    println!("cargo:rerun-if-env-changed=PATH");
    if std::env::var_os("STEVE_RELEASE_VERSION").is_some() {
        return;
    }

    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("STEVE_BACKUP_ENGINE").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(configured));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("restic"));
        }
    }
    for standard in [
        "/opt/homebrew/bin/restic",
        "/usr/local/bin/restic",
        "/usr/bin/restic",
    ] {
        candidates.push(PathBuf::from(standard));
    }

    if let Some(found) = candidates.iter().find_map(|p| executable_regular_file(p)) {
        println!(
            "cargo:rustc-env=STEVE_DEV_BACKUP_ENGINE={}",
            found.display()
        );
    }
}

fn main() {
    identity::main();
    discover_dev_backup_engine();
}
