//! Deterministic, non-destructive macOS TCC file-access probing for every Node
//! (both standalone services and the Hub's colocated local Node). Non-macOS
//! platforms return [`FileAccessStatus::NotApplicable`].

#[cfg(any(target_os = "macos", test))]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

use steve_node_api::FileAccessReport;
#[cfg(any(target_os = "macos", test))]
use steve_node_api::{FileAccessStatus, ProtectedAccess, ProtectedLocation};

use crate::LocalNode;

/// Bound each TCC probe pass so a stalled network mount cannot hold up discovery or shutdown.
#[cfg(target_os = "macos")]
const FILE_ACCESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Non-prompting macOS directories guarded exclusively by Full Disk Access.
/// Opening these read-only never triggers a per-folder TCC consent alert.
#[cfg(any(target_os = "macos", test))]
const FULL_DISK_ACCESS_CANARIES: &[&str] = &[
    "Library/Application Support/com.apple.TCC",
    "Library/Safari",
    "Library/Mail",
    "Library/Messages",
];

fn record_probe_metric(outcome: &'static str) {
    metrics::counter!("steve_node_file_access_probes_total", "outcome" => outcome).increment(1);
}

#[cfg(target_os = "macos")]
fn outcome_label(status: FileAccessStatus) -> &'static str {
    match status {
        FileAccessStatus::Granted => "granted",
        FileAccessStatus::Missing => "missing",
        FileAccessStatus::Unknown => "unknown",
        FileAccessStatus::NotApplicable => "not_applicable",
    }
}

fn resolved_executable_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let real = exe.canonicalize().unwrap_or(exe);
    Some(real.to_string_lossy().into_owned())
}

impl LocalNode {
    #[cfg(target_os = "macos")]
    pub(crate) async fn file_access(&self) -> FileAccessReport {
        let executable_path = resolved_executable_path();
        let Ok(permit) = self.0.file_access.clone().try_acquire_owned() else {
            record_probe_metric("busy");
            return FileAccessReport {
                executable_path,
                ..FileAccessReport::unknown()
            };
        };
        let node = self.clone();
        let exe_for_worker = executable_path.clone();
        let (result, read) = tokio::sync::oneshot::channel();
        // A stalled network mount may outlast the reply deadline. A detached worker
        // keeps that read from holding up the runtime's shutdown during an upgrade.
        let spawned = std::thread::Builder::new()
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
                    exe_for_worker,
                    read_folder,
                ));
            });
        if spawned.is_err() {
            record_probe_metric("unknown");
            return FileAccessReport {
                executable_path,
                ..FileAccessReport::unknown()
            };
        }
        match tokio::time::timeout(FILE_ACCESS_PROBE_TIMEOUT, read).await {
            Ok(Ok(report)) => {
                record_probe_metric(outcome_label(report.status));
                report
            }
            Err(_) => {
                record_probe_metric("timeout");
                FileAccessReport {
                    executable_path,
                    ..FileAccessReport::unknown()
                }
            }
            Ok(Err(_)) => {
                record_probe_metric("unknown");
                FileAccessReport {
                    executable_path,
                    ..FileAccessReport::unknown()
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) async fn file_access(&self) -> FileAccessReport {
        record_probe_metric("not_applicable");
        FileAccessReport {
            executable_path: resolved_executable_path(),
            ..FileAccessReport::not_applicable()
        }
    }
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(any(target_os = "macos", test))]
fn worst_status(a: FileAccessStatus, b: FileAccessStatus) -> FileAccessStatus {
    use FileAccessStatus::*;
    match (a, b) {
        (Missing, _) | (_, Missing) => Missing,
        (Unknown, _) | (_, Unknown) => Unknown,
        (NotApplicable, other) | (other, NotApplicable) => other,
        (Granted, Granted) => Granted,
    }
}

#[cfg(any(target_os = "macos", test))]
fn probe_full_disk_access(
    home: &Path,
    read: &impl Fn(&Path) -> Option<FileAccessStatus>,
) -> FileAccessStatus {
    use FileAccessStatus::*;
    let mut observed = None;
    for relative in FULL_DISK_ACCESS_CANARIES {
        if let Some(status) = read(&home.join(relative)) {
            if status == Missing {
                return Missing;
            }
            observed = Some(match observed {
                Some(prev) => worst_status(prev, status),
                None => status,
            });
        }
    }
    observed.unwrap_or(Unknown)
}

#[cfg(any(target_os = "macos", test))]
fn report(
    home: &Path,
    configured: &[PathBuf],
    mounts: &[PathBuf],
    executable_path: Option<String>,
    read: impl Fn(&Path) -> Option<FileAccessStatus>,
) -> FileAccessReport {
    use FileAccessStatus::*;
    use ProtectedLocation::*;
    let full_disk_access = probe_full_disk_access(home, &read);
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
    let mut locations: BTreeMap<(ProtectedLocation, PathBuf), FileAccessStatus> = BTreeMap::new();
    for (path, location) in candidates {
        let required = configured.iter().any(|root| root.starts_with(&path));
        if (required || configured.iter().any(|root| path.starts_with(root)))
            && let Some(status) = read(&path).or(required.then_some(Unknown))
        {
            let current = locations.entry((location, path)).or_insert(Granted);
            *current = worst_status(*current, status);
        }
    }
    FileAccessReport {
        status: locations.values().copied().fold(Granted, worst_status),
        full_disk_access,
        locations: locations
            .into_iter()
            .map(|((location, path), status)| ProtectedAccess {
                location,
                status,
                path: Some(path.to_string_lossy().into_owned()),
            })
            .collect(),
        executable_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TestCounterRecorder {
        counts: Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>,
    }

    struct VecCounter {
        name: String,
        labels: Vec<(String, String)>,
        counts: Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>,
    }

    impl metrics::CounterFn for VecCounter {
        fn increment(&self, value: u64) {
            let mut guard = self.counts.lock().unwrap();
            for _ in 0..value {
                guard.push((self.name.clone(), self.labels.clone()));
            }
        }

        fn absolute(&self, _value: u64) {}
    }

    impl metrics::Recorder for TestCounterRecorder {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn register_counter(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            let labels = key
                .labels()
                .map(|l| (l.key().to_owned(), l.value().to_owned()))
                .collect();
            metrics::Counter::from_arc(Arc::new(VecCounter {
                name: key.name().to_owned(),
                labels,
                counts: self.counts.clone(),
            }))
        }
        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }
        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }

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
            Some("/opt/steve/current/bin/steve-hub".into()),
            |path| match path.to_str().unwrap() {
                "/account/Library/Application Support/com.apple.TCC" => Some(Missing),
                "/account/Documents" => Some(Missing),
                "/mnt/shared" => Some(Granted),
                "/account/Library/Mail" => Some(Unknown),
                "/account/Library/Safari" | "/account/Library/Messages" => None,
                path => panic!("unexpected probe: {path}"),
            },
        );
        assert_eq!(found.status, Missing);
        assert_eq!(found.full_disk_access, Missing);
        assert_eq!(
            found.executable_path.as_deref(),
            Some("/opt/steve/current/bin/steve-hub")
        );
        assert_eq!(found.locations.len(), 3);
        assert_eq!(
            found.locations[0],
            ProtectedAccess {
                location: ProtectedLocation::Documents,
                status: Missing,
                path: Some("/account/Documents".into()),
            }
        );
        assert_eq!(
            found.locations[1],
            ProtectedAccess {
                location: ProtectedLocation::Volume,
                status: Granted,
                path: Some("/mnt/shared".into()),
            }
        );
        assert_eq!(
            found.locations[2],
            ProtectedAccess {
                location: ProtectedLocation::ProtectedLibrary,
                status: Unknown,
                path: Some("/account/Library/Mail".into()),
            }
        );

        let clear = report(home, &configured[3..], &[], None, |path| {
            match path.to_str().unwrap() {
                "/account/Library/Application Support/com.apple.TCC" => Some(Granted),
                "/account/Library/Safari"
                | "/account/Library/Mail"
                | "/account/Library/Messages" => None,
                other => panic!("no non-FDA scope probe needed: {other}"),
            }
        });
        assert_eq!(clear.status, Granted);
        assert_eq!(clear.full_disk_access, Granted);
        assert!(clear.locations.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_node_probes_without_boot_service_env_and_records_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let user_home = dir.path().join("user");
        let documents = user_home.join("Documents");
        let project = documents.join("demo");
        std::fs::create_dir_all(&project).unwrap();
        let node_home = dir.path().join("node");
        std::fs::create_dir_all(&node_home).unwrap();

        let acp = steve_acp::AcpConfig {
            adapters: Default::default(),
            declared: Default::default(),
            node: PathBuf::from("/usr/bin/false"),
            claude_executable: PathBuf::from("/usr/bin/false"),
            claude_config_dir: None,
            steve_home: dir.path().join("hub"),
            codex_home: dir.path().join("codex"),
            user_home: user_home.clone(),
            version: "test".into(),
        };
        let node = LocalNode::open(crate::NodeConfig {
            node: steve_node_api::NodeId::new("local").unwrap(),
            home: node_home,
            roots: vec![crate::Root {
                id: steve_node_api::RootId::new("r1").unwrap(),
                path: project,
                display: "~/Documents/demo".into(),
                access: steve_node_api::RootAccess::ReadWrite,
            }],
            acp,
            capacity: steve_node_api::Capacity {
                sessions: 1,
                jobs: 0,
                execs: 0,
            },
            storage: crate::Storage::default(),
            ceiling: crate::PolicyCeiling::unlimited(),
        })
        .unwrap();

        let counts = Arc::new(Mutex::new(Vec::new()));
        let recorder = TestCounterRecorder {
            counts: counts.clone(),
        };
        let host = metrics::with_local_recorder(&recorder, || {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(steve_node_api::Discovery::host(&node))
            })
        })
        .unwrap();

        let fa = host
            .file_access
            .expect("all Nodes must report FileAccessReport without STEVE_NODE_BOOT_SERVICE");
        assert!(
            fa.executable_path.as_deref().is_some_and(|p| !p.is_empty()),
            "real executable path must be populated: {fa:?}"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(fa.status, FileAccessStatus::Granted);
            assert!(fa.locations.iter().any(|loc| {
                loc.location == ProtectedLocation::Documents
                    && loc.status == FileAccessStatus::Granted
                    && loc.path.as_deref() == Some(documents.to_str().unwrap())
            }));
        } else {
            assert_eq!(fa.status, FileAccessStatus::NotApplicable);
            assert_eq!(fa.full_disk_access, FileAccessStatus::NotApplicable);
            assert!(fa.locations.is_empty());
        }

        let recorded = counts.lock().unwrap();
        assert_eq!(recorded.len(), 1, "expected 1 probe metric: {recorded:?}");
        assert_eq!(recorded[0].0, "steve_node_file_access_probes_total");
        let expected_outcome = if cfg!(target_os = "macos") {
            "granted"
        } else {
            "not_applicable"
        };
        assert_eq!(
            recorded[0].1,
            vec![("outcome".to_owned(), expected_outcome.to_owned())]
        );
    }
}
