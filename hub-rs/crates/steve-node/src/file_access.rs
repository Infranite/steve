//! Probe only configured scopes, from the Node that launchd actually started.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use steve_node_api::{
    FileAccessReport, FileAccessStatus, NODE_BOOT_SERVICE_ENV, ProtectedAccess, ProtectedLocation,
};

use crate::LocalNode;

impl LocalNode {
    pub(crate) async fn file_access(&self) -> Option<FileAccessReport> {
        if std::env::var_os(NODE_BOOT_SERVICE_ENV).as_deref() != Some("1".as_ref()) {
            return None;
        }
        let Ok(permit) = self.0.file_access.clone().try_acquire_owned() else {
            return Some(FileAccessReport::unknown());
        };
        let node = self.clone();
        let (result, read) = tokio::sync::oneshot::channel();
        // A stalled network mount may outlast the reply deadline. A detached worker
        // keeps that read from holding up the runtime's shutdown during an upgrade.
        let _ = std::thread::Builder::new()
            .name("node-file-access".into())
            .spawn(move || {
                let _permit = permit;
                let roots = node.0.roots();
                let cfg = &node.0.cfg;
                let acp = &cfg.acp;
                let configured = roots
                    .iter()
                    .map(|root| root.path.as_path())
                    .chain(acp.adapters.values().map(PathBuf::as_path))
                    .chain([
                        cfg.home.as_path(),
                        acp.steve_home.as_path(),
                        acp.codex_home.as_path(),
                        acp.node.as_path(),
                        acp.claude_executable.as_path(),
                    ])
                    .filter(|path| path.is_absolute())
                    .flat_map(|path| [Some(path.to_owned()), path.canonicalize().ok()])
                    .flatten()
                    .collect::<Vec<_>>();
                let _ = result.send(report(
                    &acp.user_home,
                    &configured,
                    &mounted_folders(),
                    read_folder,
                ));
            });
        Some(
            match tokio::time::timeout(Duration::from_secs(5), read).await {
                Ok(Ok(report)) => report,
                _ => FileAccessReport::unknown(),
            },
        )
    }
}

fn mounted_folders() -> Vec<PathBuf> {
    // MNT_NOWAIT uses the kernel's cached mount table, including disconnected network mounts.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let mut mounts = vec![unsafe { std::mem::zeroed::<libc::statfs>() }; count as usize + 16];
    let bytes = std::mem::size_of_val(mounts.as_slice()) as libc::c_int;
    let n = unsafe { libc::getfsstat(mounts.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    mounts.truncate(n.max(0) as usize);
    mounts
        .into_iter()
        .filter_map(|m| {
            let name = m.f_mntonname.map(|c| c as u8);
            let path = Path::new(std::ffi::OsStr::from_bytes(name.split(|&c| c == 0).next()?));
            (path.starts_with("/Volumes") || m.f_flags & libc::MNT_LOCAL as u32 == 0)
                .then(|| path.to_owned())
        })
        .collect()
}

fn read_folder(path: &Path) -> Option<FileAccessStatus> {
    use FileAccessStatus::*;
    match std::fs::read_dir(path).and_then(|mut entries| entries.next().transpose()) {
        Ok(_) => Some(Granted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // TCC denies a directory open with EPERM. Ordinary Unix permissions and
        // other failures do not establish which macOS authorization is needed.
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => Some(Missing),
        Err(_) => Some(Unknown),
    }
}

fn report(
    home: &Path,
    configured: &[PathBuf],
    mounts: &[PathBuf],
    read: impl Fn(&Path) -> Option<FileAccessStatus>,
) -> FileAccessReport {
    use FileAccessStatus::*;
    use ProtectedLocation::*;
    let volumes = Path::new("/Volumes");
    let mounts: BTreeSet<_> = mounts
        .iter()
        .cloned()
        .chain(configured.iter().filter_map(|path| {
            let volume = path.strip_prefix(volumes).ok()?.components().next()?;
            Some(volumes.join(volume))
        }))
        .collect();
    let candidates = [
        ("Desktop", Desktop),
        ("Documents", Documents),
        ("Downloads", Downloads),
        ("Library/Mail", ProtectedLibrary),
        ("Library/Messages", ProtectedLibrary),
        ("Library/Safari", ProtectedLibrary),
    ]
    .into_iter()
    .map(|(name, kind)| (home.join(name), kind))
    .chain(mounts.into_iter().map(|path| (path, Volume)));
    let mut locations = BTreeMap::new();
    let worst = |a, b| match (a, b) {
        (Missing, _) | (_, Missing) => Missing,
        (Unknown, _) | (_, Unknown) => Unknown,
        _ => Granted,
    };
    for (path, location) in candidates {
        let required = configured.iter().any(|root| root.starts_with(&path));
        if (required || configured.iter().any(|root| path.starts_with(root)))
            && let Some(status) = read(&path).or(required.then_some(Unknown))
        {
            let current = locations.entry(location).or_insert(Granted);
            *current = worst(*current, status);
        }
    }
    FileAccessReport {
        status: locations.values().copied().fold(Granted, worst),
        locations: locations
            .into_iter()
            .map(|(location, status)| ProtectedAccess { location, status })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_configured_protected_scopes_are_probed_and_missing_wins() {
        use FileAccessStatus::*;
        let home = Path::new("/account");
        let configured = [
            home.join("Documents/projects"),
            home.join("Library"),
            "/mnt/shared/project".into(),
            home.join("ordinary"),
        ];
        let found = report(
            home,
            &configured,
            &["/mnt/shared".into()],
            |path| match path.to_str().unwrap() {
                "/account/Documents" => Some(Missing),
                "/mnt/shared" => Some(Granted),
                "/account/Library/Mail" => Some(Unknown),
                "/account/Library/Safari" | "/account/Library/Messages" => None,
                path => panic!("unexpected probe: {path}"),
            },
        );
        assert_eq!(found.status, Missing);
        assert_eq!(found.locations.len(), 3);
        let clear = report(home, &configured[3..], &[], |_| panic!("no probe needed"));
        assert_eq!(clear.status, Granted);
        assert!(clear.locations.is_empty());
    }
}
