//! The single-file backup plan: verified database first, optional files best-effort,
//! durable manifest last, then atomic publication of an exclusively owned partial directory.
use super::{
    manifest::{Artifact, Files, Manifest, Usage},
    offline::{self, BackupError, CancelFlag, CopyOptions, Progress},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    Daily,
    PreUpgrade { from: String, to: String },
    PreDestructive { what: String },
    Diagnostic,
}
impl Reason {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::PreUpgrade { .. } => "pre-upgrade",
            Self::PreDestructive { .. } => "pre-destructive",
            Self::Diagnostic => "diagnostic",
        }
    }
    fn prefix(&self) -> String {
        match self {
            Self::PreDestructive { what } if what == "purge" => "pre-purge".into(),
            _ => self.kind().into(),
        }
    }
}
/// One record per purpose: all automatic retirement and host archive choices use this data.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Rule {
    pub kind: &'static str,
    pub days: u64,
    pub copies: usize,
    pub bytes: u64,
    pub with_files: bool,
}
const DAY_MS: u64 = 86_400_000;
const GIB: u64 = 1024 * 1024 * 1024;
pub const TOTAL_BYTES: u64 = 64 * GIB;
pub const RULES: [Rule; 4] = [
    Rule {
        kind: "daily",
        days: 14,
        copies: 14,
        bytes: 32 * GIB,
        with_files: true,
    },
    Rule {
        kind: "pre-upgrade",
        days: 90,
        copies: 8,
        bytes: 16 * GIB,
        with_files: true,
    },
    Rule {
        kind: "pre-destructive",
        days: 30,
        copies: 8,
        bytes: 16 * GIB,
        with_files: true,
    },
    Rule {
        kind: "diagnostic",
        days: 7,
        copies: 4,
        bytes: 2 * GIB,
        with_files: false,
    },
];
impl Reason {
    pub fn rule(&self) -> &'static Rule {
        // The enum has exactly the purposes described above.
        RULES.iter().find(|r| r.kind == self.kind()).unwrap()
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Keep {
    Default,
    All,
}
/// Archive implementation is supplied by the host, so storage has no mandatory tar/process
/// dependency. The callback returns a path in the supplied output directory, or None.
#[derive(Clone)]
pub struct FileSet {
    pub home: PathBuf,
    pub archive: fn(&Path, &Path) -> io::Result<Option<PathBuf>>,
}
pub struct BackupPlan {
    pub reason: Reason,
    pub dest_root: PathBuf,
    pub keep: Keep,
    pub deadline: Option<Duration>,
    pub with_files: Option<FileSet>,
}
impl BackupPlan {
    pub fn new(dest_root: PathBuf, reason: Reason) -> Self {
        Self {
            reason,
            dest_root,
            keep: Keep::Default,
            deadline: Some(Duration::from_secs(300)),
            with_files: None,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Copy(#[from] BackupError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
/// UTC is formatted by SQLite's date implementation rather than a host locale/timezone.
fn stamp(ms: u64) -> Result<String, rusqlite::Error> {
    Connection::open_in_memory()?.query_row(
        "SELECT strftime('%Y%m%dT%H%M%S',?1/1000,'unixepoch')",
        [i64::try_from(ms).unwrap_or(i64::MAX)],
        |r| r.get(0),
    )
}
fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("backup root must be a real directory"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
fn check(
    start: Instant,
    deadline: Option<Duration>,
    cancel: &CancelFlag,
) -> Result<(), BackupError> {
    if cancel.is_cancelled() {
        return Err(BackupError::Cancelled);
    }
    if deadline.is_some_and(|d| start.elapsed() >= d) {
        return Err(BackupError::DeadlineExceeded);
    }
    Ok(())
}
pub fn artifact(path: &Path) -> io::Result<Artifact> {
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok(Artifact {
        name: path
            .file_name()
            .ok_or_else(|| io::Error::other("artifact has no name"))?
            .to_string_lossy()
            .into_owned(),
        bytes,
        sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        quick_check: None,
    })
}
/// Schema metadata comes from the verified copy, hence describes its exact snapshot.
fn schema(path: &Path) -> rusqlite::Result<Option<i64>> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='schema_version' AND type='table')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let has_v: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('schema_version') WHERE name='v')",
        [],
        |r| r.get(0),
    )?;
    if !has_v {
        return Ok(None);
    }
    let value: Option<rusqlite::types::Value> = c
        .query_row("SELECT v FROM schema_version LIMIT 1", [], |r| r.get(0))
        .optional()?;
    // Application metadata is optional: an unfamiliar/older header does not prevent a
    // physically verified diagnostic copy. Actual SQLite I/O errors still propagate.
    Ok(match value {
        Some(rusqlite::types::Value::Integer(v)) => Some(v),
        Some(rusqlite::types::Value::Text(v)) => v.parse().ok(),
        _ => None,
    })
}
pub fn backup(
    db_path: &Path,
    plan: &BackupPlan,
    progress: &dyn Fn(Progress),
    cancel: &CancelFlag,
) -> Result<Manifest, PlanError> {
    let name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("database filename must be UTF-8"))?;
    if matches!(name, "manifest.json" | "files.tar" | "files.tar.partial") {
        return Err(io::Error::other("database filename conflicts with backup metadata").into());
    }
    let start = Instant::now();
    check(start, plan.deadline, cancel)?;
    private_dir(&plan.dest_root)?;
    let created_ms = now_ms();
    let created_at = stamp(created_ms)?;
    let id = format!(
        "{}-{}-{}",
        plan.reason.prefix(),
        created_at,
        uuid::Uuid::new_v4().simple()
    );
    let directory = plan.dest_root.join(&id);
    let partial = plan.dest_root.join(format!("{id}.partial"));
    fs::DirBuilder::new().mode(0o700).create(&partial)?;
    let _cleanup = Partial(partial.clone());
    let copied = offline::backup_file(
        db_path,
        &partial,
        &CopyOptions {
            deadline: plan.deadline.map(|d| d.saturating_sub(start.elapsed())),
            ..CopyOptions::default()
        },
        progress,
        cancel,
    )?;
    let schema_version = schema(&copied.path)?;
    let database = Artifact {
        name: copied
            .path
            .file_name()
            .ok_or_else(|| io::Error::other("database has no name"))?
            .to_string_lossy()
            .into_owned(),
        bytes: copied.bytes,
        sha256: copied.sha256,
        quick_check: Some(copied.quick_check),
    };
    let files = archive_files(plan, &partial, start, cancel)?;
    check(start, plan.deadline, cancel)?;
    let manifest = Manifest {
        format: 1,
        id,
        reason: plan.reason.clone(),
        created_at,
        created_ms,
        source: db_path.to_owned(),
        directory: directory.clone(),
        schema_version,
        database,
        files,
        completed: true,
        keep_all: matches!(plan.keep, Keep::All),
    };
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(partial.join("manifest.json"))?;
    output.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    output.sync_all()?;
    File::open(&partial)?.sync_all()?;
    check(start, plan.deadline, cancel)?;
    if fs::symlink_metadata(&directory).is_ok() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
    }
    fs::rename(&partial, &directory)?;
    if let Err(error) = File::open(&plan.dest_root).and_then(|f| f.sync_all()) {
        let _ = fs::remove_dir_all(&directory);
        return Err(error.into());
    }
    if let Err(error) = check(start, plan.deadline, cancel) {
        let _ = fs::remove_dir_all(&directory);
        let _ = File::open(&plan.dest_root).and_then(|f| f.sync_all());
        return Err(error.into());
    }
    Ok(manifest)
}
fn archive_files(
    plan: &BackupPlan,
    partial: &Path,
    start: Instant,
    cancel: &CancelFlag,
) -> Result<Files, PlanError> {
    let mut files = Files {
        consistency: "best_effort".into(),
        complete: true,
        artifacts: vec![],
        error: None,
    };
    if let Some(set) = &plan.with_files {
        for attempt in 0..3 {
            check(start, plan.deadline, cancel)?;
            match (set.archive)(&set.home, partial)
                .and_then(|path| path.map(|p| artifact(&p)).transpose())
            {
                Ok(file) => {
                    files.artifacts.extend(file);
                    files.error = None;
                    break;
                }
                Err(error) => {
                    files.complete = false;
                    files.error = Some(error.to_string());
                    if attempt == 2 {
                        break;
                    }
                }
            }
        }
        if files.error.is_none() {
            files.complete = true;
        }
    }
    Ok(files)
}

fn single_name(name: &str) -> bool {
    Path::new(name).file_name().is_some_and(|n| n == name) && name != "." && name != ".."
}
/// Inventory only completed, recognizable directories, never symlinked or foreign paths.
pub fn list(root: &Path) -> io::Result<Vec<Manifest>> {
    inventory(root, true)
}
fn inventory(root: &Path, require_database: bool) -> io::Result<Vec<Manifest>> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut all = vec![];
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".partial") {
            continue;
        }
        let path = entry.path();
        let meta = path.join("manifest.json");
        if !fs::symlink_metadata(&meta).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let Ok(mut m) = serde_json::from_slice::<Manifest>(&fs::read(meta)?) else {
            continue;
        };
        if m.files.artifacts.iter().any(|a| !single_name(&a.name)) {
            continue;
        }
        if m.format != 1
            || !m.completed
            || m.id != name
            || !name.starts_with(&format!("{}-{}-", m.reason.prefix(), m.created_at))
            || !single_name(&m.database.name)
            || m.database.quick_check.as_deref() != Some("ok")
        {
            continue;
        }
        if require_database
            && !fs::symlink_metadata(path.join(&m.database.name)).is_ok_and(|f| f.is_file())
        {
            continue;
        }
        m.directory = path;
        all.push(m);
    }
    all.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
    Ok(all)
}
/// Actual on-disk logical bytes, rather than the historical sizes in the manifest.
/// Missing artifacts are expected after operator intervention; other I/O errors propagate.
struct DirectoryState {
    bytes: u64,
    owned: bool,
}
fn directory_state(m: &Manifest) -> io::Result<DirectoryState> {
    let mut state = DirectoryState {
        bytes: 0,
        owned: true,
    };
    let mut expected = vec!["manifest.json".to_owned(), m.database.name.clone()];
    expected.extend(m.files.artifacts.iter().map(|a| a.name.clone()));
    for entry in fs::read_dir(&m.directory)? {
        let entry = entry?;
        let file = entry.file_type()?.is_file();
        if file {
            state.bytes = state.bytes.saturating_add(entry.metadata()?.len());
        }
        state.owned &= file && expected.contains(&entry.file_name().to_string_lossy().into_owned());
    }
    Ok(state)
}
pub fn usage(root: &Path) -> io::Result<BTreeMap<String, Usage>> {
    let mut result: BTreeMap<String, Usage> = RULES
        .iter()
        .map(|r| (r.kind.into(), Usage::default()))
        .collect();
    for m in inventory(root, false)? {
        let bytes = directory_state(&m)?.bytes;
        let u = result.get_mut(m.reason.kind()).unwrap();
        u.count += 1;
        u.bytes = u.bytes.saturating_add(bytes);
        // Inventory is oldest first: the final value describes the protected latest copy.
        u.latest_over_limit = bytes > m.reason.rule().bytes;
    }
    for rule in RULES {
        let u = result.get_mut(rule.kind).unwrap();
        u.over_limit = u.bytes > rule.bytes || u.count > rule.copies as u64;
    }
    Ok(result)
}
#[derive(Debug, Default, Serialize)]
pub struct RetentionStatus {
    pub purposes: BTreeMap<String, Usage>,
    pub bytes: u64,
    pub max_bytes: u64,
    pub over_limit: bool,
}
pub fn retention_status(root: &Path) -> io::Result<RetentionStatus> {
    let purposes = usage(root)?;
    let bytes = purposes
        .values()
        .fold(0u64, |sum, u| sum.saturating_add(u.bytes));
    let over_limit = bytes > TOTAL_BYTES || purposes.values().any(|u| u.over_limit);
    Ok(RetentionStatus {
        purposes,
        bytes,
        max_bytes: TOTAL_BYTES,
        over_limit,
    })
}
pub fn prune(root: &Path, reason: &Reason) -> io::Result<usize> {
    prune_at(root, reason, now_ms())
}
/// Scoped compatibility entry, interpreted by the same cleaner as the scheduled sweep.
pub fn prune_at(root: &Path, reason: &Reason, now: u64) -> io::Result<usize> {
    Ok(prune_impl(root, Some(reason), now, &RULES, TOTAL_BYTES, usize::MAX)?.removed)
}
#[derive(Debug, Default)]
pub struct Pruned {
    pub removed: usize,
    pub more: bool,
}
/// Resource hook for storage.retention. No SQLite connection/write grant is held here.
/// Bound deletions per turn; the existing Source continues immediately when more remain.
pub fn retain_at(root: &Path, now: u64) -> io::Result<Pruned> {
    prune_impl(
        root,
        None,
        now,
        &RULES,
        TOTAL_BYTES,
        crate::retention::MAX_SLICE_ROWS as usize,
    )
}
/// Single interpreter: evaluate age/count/byte pressure in oldest-first order.
/// Every purpose's newest copy and explicit Keep::All copies survive every rule, even when
/// those protected copies alone exceed the cap. Status reports that irreducible excess.
fn prune_impl(
    root: &Path,
    reason: Option<&Reason>,
    now: u64,
    rules: &[Rule],
    max_bytes: u64,
    limit: usize,
) -> io::Result<Pruned> {
    let all = inventory(root, false)?;
    let mut remaining: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    let mut newest = BTreeMap::new();
    let mut sizes = Vec::with_capacity(all.len());
    let mut total = 0u64;
    for m in &all {
        let bytes = directory_state(m)?.bytes;
        sizes.push(bytes);
        total = total.saturating_add(bytes);
        let state = remaining.entry(m.reason.kind()).or_default();
        state.0 += 1;
        state.1 = state.1.saturating_add(bytes);
        newest.insert(m.reason.kind(), m.id.as_str());
    }
    let mut report = Pruned::default();
    for (m, bytes) in all.iter().zip(sizes) {
        let kind = m.reason.kind();
        if reason.is_some_and(|r| r.kind() != kind)
            || m.keep_all
            || newest.get(kind).copied() == Some(m.id.as_str())
        {
            continue;
        }
        let rule = rules
            .iter()
            .find(|r| r.kind == kind)
            .ok_or_else(|| io::Error::other("backup purpose has no retention rule"))?;
        let state = remaining.get_mut(kind).unwrap();
        let expired = now.saturating_sub(m.created_ms) > rule.days.saturating_mul(DAY_MS);
        if !(expired || state.0 > rule.copies || state.1 > rule.bytes || total > max_bytes) {
            continue;
        }
        // Recheck ownership immediately before deletion. Missing declared artifacts are harmless.
        if !directory_state(m)?.owned {
            continue;
        }
        if report.removed == limit {
            report.more = true;
            break;
        }
        fs::remove_dir_all(&m.directory)?;
        report.removed += 1;
        state.0 -= 1;
        state.1 = state.1.saturating_sub(bytes);
        total = total.saturating_sub(bytes);
    }
    if report.removed > 0 {
        File::open(root)?.sync_all()?;
    }
    Ok(report)
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn copy(root: &Path, reason: Reason, at: u64, keep_all: bool) -> Manifest {
        let id = format!("{}-20260101T000000-{at:04}", reason.prefix());
        let directory = root.join(&id);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("db.sqlite3"), [0u8; 1024]).unwrap();
        let artifact = Artifact {
            name: "db.sqlite3".into(),
            bytes: 1024,
            sha256: "test".into(),
            quick_check: Some("ok".into()),
        };
        let m = Manifest {
            format: 1,
            id,
            reason,
            created_at: "20260101T000000".into(),
            created_ms: at,
            source: PathBuf::from("source.sqlite3"),
            directory,
            schema_version: None,
            database: artifact,
            files: Files {
                consistency: "best_effort".into(),
                complete: true,
                artifacts: vec![],
                error: None,
            },
            completed: true,
            keep_all,
        };
        save(&m);
        m
    }
    fn save(m: &Manifest) {
        fs::write(
            m.directory.join("manifest.json"),
            serde_json::to_vec(m).unwrap(),
        )
        .unwrap();
    }
    fn purposes() -> [Reason; 4] {
        [
            Reason::Daily,
            Reason::PreUpgrade {
                from: "a".into(),
                to: "b".into(),
            },
            Reason::PreDestructive {
                what: "test".into(),
            },
            Reason::Diagnostic,
        ]
    }

    #[test]
    fn every_purpose_has_age_count_and_bytes_limits_but_keeps_its_latest() {
        for reason in purposes() {
            let root = tempfile::tempdir().unwrap();
            let rule = reason.rule();
            assert!(rule.days > 0 && rule.copies > 0 && rule.bytes > 0);
            for at in 0..=rule.copies as u64 {
                copy(root.path(), reason.clone(), at, false);
            }
            assert_eq!(
                retain_at(root.path(), rule.copies as u64).unwrap().removed,
                1
            );
            assert_eq!(list(root.path()).unwrap().len(), rule.copies);
            let report = retain_at(root.path(), u64::MAX).unwrap();
            assert_eq!(report.removed, rule.copies - 1);
            assert_eq!(list(root.path()).unwrap()[0].created_ms, rule.copies as u64);
            assert_eq!(retain_at(root.path(), u64::MAX).unwrap().removed, 0);
        }
    }

    #[test]
    fn every_purpose_enforces_its_own_byte_cap() {
        for reason in purposes() {
            let root = tempfile::tempdir().unwrap();
            let old = copy(root.path(), reason.clone(), 1, false);
            let latest = copy(root.path(), reason.clone(), 2, false);
            let mut rules = RULES;
            rules
                .iter_mut()
                .find(|r| r.kind == reason.kind())
                .unwrap()
                .bytes = directory_state(&latest).unwrap().bytes;
            assert_eq!(
                prune_impl(root.path(), None, 2, &rules, TOTAL_BYTES, 500)
                    .unwrap()
                    .removed,
                1
            );
            assert!(!old.directory.exists() && latest.directory.exists());
        }
    }

    #[test]
    fn age_boundary_is_strict_and_keep_all_survives_even_extreme_pressure() {
        let root = tempfile::tempdir().unwrap();
        let old = copy(root.path(), Reason::Daily, 1, false);
        let manual = copy(root.path(), Reason::Daily, 2, true);
        let latest = copy(root.path(), Reason::Daily, 3, false);
        let deadline = 1 + Reason::Daily.rule().days * DAY_MS;
        assert_eq!(retain_at(root.path(), deadline).unwrap().removed, 0);
        assert_eq!(retain_at(root.path(), deadline + 1).unwrap().removed, 1);
        assert!(!old.directory.exists());
        assert_eq!(
            prune_impl(root.path(), None, u64::MAX, &RULES, 0, 500)
                .unwrap()
                .removed,
            0
        );
        assert!(manual.directory.exists() && latest.directory.exists());
    }

    #[test]
    fn byte_caps_remove_oldest_globally_and_never_the_latest_of_any_purpose() {
        let root = tempfile::tempdir().unwrap();
        let old_daily = copy(root.path(), Reason::Daily, 1, false);
        let old_diagnostic = copy(root.path(), Reason::Diagnostic, 2, false);
        let daily = copy(root.path(), Reason::Daily, 3, false);
        let diagnostic = copy(root.path(), Reason::Diagnostic, 4, false);
        let bytes = usage(root.path())
            .unwrap()
            .values()
            .map(|u| u.bytes)
            .sum::<u64>();
        let cap = bytes - directory_state(&old_daily).unwrap().bytes;
        let report = prune_impl(root.path(), None, 4, &RULES, cap, 500).unwrap();
        assert_eq!(report.removed, 1);
        assert!(!old_daily.directory.exists());
        assert!(old_diagnostic.directory.exists());
        assert_eq!(
            prune_impl(root.path(), None, 4, &RULES, 0, 500)
                .unwrap()
                .removed,
            1
        );
        assert!(!old_diagnostic.directory.exists());
        assert!(daily.directory.exists() && diagnostic.directory.exists());
    }

    #[test]
    fn purpose_bytes_are_independent_and_slices_resume_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let old = copy(root.path(), Reason::Daily, 1, false);
        let middle = copy(root.path(), Reason::Daily, 2, false);
        let latest = copy(root.path(), Reason::Daily, 3, false);
        let other = copy(root.path(), Reason::Diagnostic, 4, false);
        let mut rules = RULES;
        rules[0].bytes = directory_state(&latest).unwrap().bytes;
        let report = prune_impl(root.path(), None, 4, &rules, TOTAL_BYTES, 1).unwrap();
        assert_eq!(report.removed, 1);
        assert!(report.more);
        assert!(!old.directory.exists() && middle.directory.exists());
        let report = prune_impl(root.path(), None, 4, &rules, TOTAL_BYTES, 1).unwrap();
        assert_eq!(report.removed, 1);
        assert!(!report.more);
        assert!(!middle.directory.exists());
        assert!(latest.directory.exists() && other.directory.exists());
        assert_eq!(
            prune_impl(root.path(), None, 4, &rules, TOTAL_BYTES, 1)
                .unwrap()
                .removed,
            0
        );
    }

    #[test]
    fn missing_archives_and_database_are_retired_but_foreign_children_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let mut no_archive = copy(root.path(), Reason::Daily, 1, false);
        let mut artifact = no_archive.database.clone();
        artifact.name = "files.tar".into();
        artifact.bytes = u64::MAX; // historical size must not count as current disk usage
        no_archive.files.artifacts.push(artifact);
        save(&no_archive);
        let no_database = copy(root.path(), Reason::Daily, 2, false);
        fs::remove_file(no_database.database_path()).unwrap();
        let foreign = copy(root.path(), Reason::Daily, 3, false);
        fs::write(foreign.directory.join("operator-note"), "keep").unwrap();
        let linked = copy(root.path(), Reason::Daily, 4, false);
        std::os::unix::fs::symlink("outside", linked.directory.join("files.tar")).unwrap();
        let newest = copy(root.path(), Reason::Daily, 5, false);
        assert!(usage(root.path()).unwrap()["daily"].bytes < 20_000);
        assert_eq!(retain_at(root.path(), u64::MAX).unwrap().removed, 2);
        assert!(!no_archive.directory.exists() && !no_database.directory.exists());
        assert!(
            foreign.directory.exists() && linked.directory.exists() && newest.directory.exists()
        );
    }

    #[test]
    fn protected_oversized_copy_is_visible_in_status() {
        let root = tempfile::tempdir().unwrap();
        let latest = copy(root.path(), Reason::Diagnostic, 1, false);
        // Sparse logical file: no large allocation, copy, archive or hashing.
        File::options()
            .write(true)
            .open(latest.database_path())
            .unwrap()
            .set_len(Reason::Diagnostic.rule().bytes + 1)
            .unwrap();
        let status = retention_status(root.path()).unwrap();
        assert!(status.over_limit);
        assert!(status.purposes["diagnostic"].latest_over_limit);
        assert!(status.purposes["diagnostic"].over_limit);
        assert_eq!(retain_at(root.path(), u64::MAX).unwrap().removed, 0);
        assert!(latest.directory.exists());
    }
}
