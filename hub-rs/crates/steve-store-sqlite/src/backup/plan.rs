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
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    Manual,
    Daily,
    PreUpgrade { from: String, to: String },
    PreDestructive { what: String },
    Diagnostic,
}
impl Reason {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
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
    /// None means this purpose is never automatically retired.
    pub limits: Option<Limits>,
    pub with_files: bool,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Limits {
    /// None resolves the installation's daily retention setting each sweep.
    pub days: Option<u64>,
    pub copies: usize,
    pub bytes: u64,
}
pub const DEFAULT_DAILY_KEEP_DAYS: u64 = 7;
pub const MAX_DAILY_KEEP_DAYS: u64 = 365;
const DAY_MS: u64 = 86_400_000;
const GIB: u64 = 1024 * 1024 * 1024;
pub const TOTAL_BYTES: u64 = 64 * GIB;
pub const RULES: [Rule; 5] = [
    Rule {
        kind: "manual",
        limits: None,
        with_files: true,
    },
    Rule {
        kind: "daily",
        limits: Some(Limits {
            days: None,
            copies: MAX_DAILY_KEEP_DAYS as usize + 1,
            bytes: 32 * GIB,
        }),
        with_files: true,
    },
    Rule {
        kind: "pre-upgrade",
        limits: Some(Limits {
            days: Some(90),
            copies: 8,
            bytes: 16 * GIB,
        }),
        with_files: true,
    },
    Rule {
        kind: "pre-destructive",
        limits: Some(Limits {
            days: Some(30),
            copies: 8,
            bytes: 16 * GIB,
        }),
        with_files: true,
    },
    Rule {
        kind: "diagnostic",
        limits: Some(Limits {
            days: Some(7),
            copies: 4,
            bytes: 2 * GIB,
        }),
        with_files: false,
    },
];
impl Reason {
    pub fn rule(&self) -> &'static Rule {
        &RULES[self.rule_index()]
    }
    fn rule_index(&self) -> usize {
        match self {
            Self::Manual => 0,
            Self::Daily => 1,
            Self::PreUpgrade { .. } => 2,
            Self::PreDestructive { .. } => 3,
            Self::Diagnostic => 4,
        }
    }
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
    pub deadline: Option<Duration>,
    pub with_files: Option<FileSet>,
}
impl BackupPlan {
    pub fn new(dest_root: PathBuf, reason: Reason) -> Self {
        Self {
            reason,
            dest_root,
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
    // This is our fully closed, verified snapshot. A normal read-only WAL connection
    // can leave -wal/-shm files that make the owned directory look foreign to retention.
    // Immutable mode reads only the snapshot; encode all path bytes so ?, #, % and Unicode
    // in installation/database paths cannot change SQLite's URI parameters.
    let encoded: String = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    let c = Connection::open_with_flags(
        format!("file:{encoded}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
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
    // All Hub/offline writers publish under the installation owner. Serialize only the
    // publication phase in-process, then derive a durable sequence independent of wall time.
    static PUBLISH: Mutex<()> = Mutex::new(());
    let _publish = PUBLISH.lock().unwrap_or_else(|e| e.into_inner());
    let published_order = inventory(&plan.dest_root, false)?
        .manifests
        .iter()
        .map(|m| m.published_order)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| io::Error::other("backup publication order exhausted"))?;
    let manifest = Manifest {
        format: 1,
        id,
        reason: plan.reason.clone(),
        created_at,
        created_ms,
        published_order,
        source: db_path.to_owned(),
        directory: directory.clone(),
        schema_version,
        database,
        files,
        completed: true,
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
    rename(&partial, &directory)?;
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

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}
fn single_name(name: &str) -> bool {
    Path::new(name).file_name().is_some_and(|n| n == name) && name != "." && name != ".."
}
/// Inventory only completed, recognizable directories, never symlinked or foreign paths.
pub fn list(root: &Path) -> io::Result<Vec<Manifest>> {
    Ok(inventory(root, true)?.manifests)
}
#[derive(Default)]
struct Inventory {
    manifests: Vec<Manifest>,
    retiring: Vec<PathBuf>,
    failures: usize,
}
fn directory_failure(path: &Path, error: &io::Error) {
    tracing::warn!(path = %path.display(), %error, "backup retention skipped directory");
}
fn read_manifest(path: &Path, id: &str, require_database: bool) -> io::Result<Option<Manifest>> {
    let meta = path.join("manifest.json");
    match fs::symlink_metadata(&meta) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let Ok(mut m) = serde_json::from_slice::<Manifest>(&fs::read(meta)?) else {
        return Ok(None);
    };
    if m.files.artifacts.iter().any(|a| !single_name(&a.name))
        || m.format != 1
        || !m.completed
        || m.id != id
        || !id.starts_with(&format!("{}-{}-", m.reason.prefix(), m.created_at))
        || !single_name(&m.database.name)
        || m.database.quick_check.as_deref() != Some("ok")
    {
        return Ok(None);
    }
    if require_database {
        match fs::symlink_metadata(path.join(&m.database.name)) {
            Ok(f) if f.is_file() => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    m.directory = path.to_owned();
    Ok(Some(m))
}
fn inventory(root: &Path, require_database: bool) -> io::Result<Inventory> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Inventory::default()),
        Err(e) => return Err(e),
    };
    let mut all = Inventory::default();
    for entry in entries {
        let result = (|| -> io::Result<Option<Manifest>> {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Ok(None);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".partial") {
                return Ok(None);
            }
            let path = entry.path();
            if name.ends_with(".retiring") {
                all.retiring.push(path);
                return Ok(None);
            }
            let read = read_manifest(&path, &name, require_database);
            if let Err(ref error) = read {
                directory_failure(&path, error);
            }
            read
        })();
        match result {
            Ok(Some(m)) => all.manifests.push(m),
            Ok(None) => {}
            Err(error) => {
                all.failures += 1;
                // An unreadable root entry has no usable directory path.
                tracing::warn!(%error, "backup inventory entry failed");
            }
        }
    }
    all.manifests.sort_by(|a, b| {
        a.published_order
            .cmp(&b.published_order)
            .then(a.created_ms.cmp(&b.created_ms))
            .then(a.id.cmp(&b.id))
    });
    all.retiring.sort();
    Ok(all)
}
/// Actual on-disk logical bytes, rather than the historical sizes in the manifest.
/// Missing artifacts are expected after operator intervention; other I/O errors propagate.
struct DirectoryState {
    bytes: u64,
    owned: bool,
    files: Vec<PathBuf>,
}
fn directory_state(m: &Manifest) -> io::Result<DirectoryState> {
    directory_contents(&m.directory, Some(m))
}
fn directory_contents(path: &Path, manifest: Option<&Manifest>) -> io::Result<DirectoryState> {
    let mut state = DirectoryState {
        bytes: 0,
        owned: true,
        files: Vec::new(),
    };
    let mut expected = Vec::new();
    if let Some(m) = manifest {
        expected.extend(["manifest.json".to_owned(), m.database.name.clone()]);
        expected.extend(m.files.artifacts.iter().map(|a| a.name.clone()));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file = entry.file_type()?.is_file();
        if file {
            state.bytes = state.bytes.saturating_add(entry.metadata()?.len());
            state.files.push(entry.path());
        }
        state.owned &= file && expected.contains(&entry.file_name().to_string_lossy().into_owned());
    }
    Ok(state)
}
fn retiring_state(path: &Path) -> io::Result<DirectoryState> {
    let id = path.file_name().unwrap().to_string_lossy();
    let id = id.strip_suffix(".retiring").unwrap();
    let manifest = read_manifest(path, id, false)?;
    let mut state = directory_contents(path, manifest.as_ref())?;
    state.owned &= match manifest {
        Some(m) => m.reason.rule().limits.is_some(),
        // Manifest is unlinked last. A crash after that leaves only an empty directory.
        None => retired_id(id),
    };
    Ok(state)
}
fn retired_id(id: &str) -> bool {
    let Some((head, nonce)) = id.rsplit_once('-') else {
        return false;
    };
    let Some((reason, stamp)) = head.rsplit_once('-') else {
        return false;
    };
    (RULES.iter().any(|r| r.limits.is_some() && r.kind == reason)
        || reason
            == Reason::PreDestructive {
                what: "purge".into(),
            }
            .prefix())
        && nonce.len() == 32
        && nonce.bytes().all(|b| b.is_ascii_hexdigit())
        && stamp.len() == 15
        && stamp.as_bytes()[8] == b'T'
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit())
}
fn remove_retiring(path: &Path, mut state: DirectoryState) -> io::Result<()> {
    if !state.owned {
        return Err(io::Error::other("retiring backup contains unowned entries"));
    }
    // Retain ownership metadata until all artifacts are gone; retry never needs to guess
    // which remaining files belonged to a partially deleted backup.
    state
        .files
        .sort_by_key(|p| p.file_name().is_some_and(|n| n == "manifest.json"));
    for file in state.files {
        if file.file_name().is_some_and(|n| n == "manifest.json") {
            File::open(path)?.sync_all()?;
        }
        fs::remove_file(file)?;
    }
    fs::remove_dir(path)
}
pub fn usage(root: &Path) -> io::Result<BTreeMap<String, Usage>> {
    Ok(retention_status(root)?.purposes)
}
#[derive(Debug, Default, Serialize)]
pub struct RetentionStatus {
    pub purposes: BTreeMap<String, Usage>,
    /// Automatic copies only; manual backups cannot displace bounded purposes.
    pub bytes: u64,
    pub manual_bytes: u64,
    pub retiring_bytes: u64,
    pub retiring_count: u64,
    pub failures: usize,
    pub max_bytes: u64,
    pub over_limit: bool,
}
pub fn retention_status(root: &Path) -> io::Result<RetentionStatus> {
    let inventory = inventory(root, false)?;
    let mut status = RetentionStatus {
        failures: inventory.failures,
        max_bytes: TOTAL_BYTES,
        ..RetentionStatus::default()
    };
    let mut result: BTreeMap<String, Usage> = RULES
        .iter()
        .map(|r| (r.kind.into(), Usage::default()))
        .collect();
    for m in inventory.manifests {
        let bytes = match directory_state(&m) {
            Ok(state) => state.bytes,
            Err(error) => {
                directory_failure(&m.directory, &error);
                status.failures += 1;
                continue;
            }
        };
        let u = result.get_mut(m.reason.kind()).unwrap();
        u.count += 1;
        u.bytes = u.bytes.saturating_add(bytes);
        // Inventory is oldest first: the final value describes the protected latest copy.
        u.latest_over_limit = m.reason.rule().limits.is_some_and(|l| bytes > l.bytes);
    }
    for rule in RULES {
        let u = result.get_mut(rule.kind).unwrap();
        u.over_limit = rule
            .limits
            .is_some_and(|l| u.bytes > l.bytes || u.count > l.copies as u64);
    }
    for path in inventory.retiring {
        status.retiring_count += 1;
        match retiring_state(&path) {
            Ok(state) => {
                status.retiring_bytes = status.retiring_bytes.saturating_add(state.bytes);
                status.failures += usize::from(!state.owned);
            }
            Err(error) => {
                directory_failure(&path, &error);
                status.failures += 1;
            }
        }
    }
    status.manual_bytes = result["manual"].bytes;
    status.bytes = RULES
        .iter()
        .filter(|r| r.limits.is_some())
        .fold(status.retiring_bytes, |sum, r| {
            sum.saturating_add(result[r.kind].bytes)
        });
    status.over_limit = status.bytes > TOTAL_BYTES || result.values().any(|u| u.over_limit);
    status.purposes = result;
    Ok(status)
}
#[derive(Debug, Default)]
pub struct Pruned {
    pub failures: usize,
    pub removed: usize,
    pub more: bool,
}
/// Resource hook for storage.retention. No SQLite connection/write grant is held here.
/// Bound deletions per turn; the existing Source continues immediately when more remain.
pub fn retain_at(root: &Path, now: u64, daily_keep_days: u64) -> io::Result<Pruned> {
    prune_impl(
        root,
        now,
        &RULES,
        daily_keep_days,
        TOTAL_BYTES,
        crate::retention::MAX_SLICE_ROWS as usize,
    )
}
/// Resume retired directories before considering any newly eligible backups. They
/// remain part of the automatic budget until their remaining files are removed.
/// Returns the bytes still on disk and whether any directory was touched.
fn resume_retiring(retiring: Vec<PathBuf>, limit: usize, report: &mut Pruned) -> (u64, bool) {
    let (mut total, mut changed) = (0u64, false);
    for path in retiring {
        let state = match retiring_state(&path) {
            Ok(state) => state,
            Err(error) => {
                directory_failure(&path, &error);
                report.failures += 1;
                continue;
            }
        };
        let bytes = state.bytes;
        if state.owned && report.removed == limit {
            report.more = true;
            total = total.saturating_add(bytes);
            continue;
        }
        changed = true;
        match remove_retiring(&path, state) {
            Ok(()) => report.removed += 1,
            Err(error) => {
                directory_failure(&path, &error);
                report.failures += 1;
                total = total
                    .saturating_add(directory_contents(&path, None).map_or(bytes, |s| s.bytes));
            }
        }
    }
    (total, changed)
}
/// Every purpose's newest copy survives every rule; an oversized one is only reported.
fn warn_if_latest_oversized(kind: &str, bytes: u64, max_bytes: u64) {
    if bytes > max_bytes {
        tracing::warn!(
            reason = kind,
            bytes,
            max_bytes,
            "latest backup exceeds retention limit; retained"
        );
    }
}
/// A backup may be retired only while every entry in it is still ours.
fn owned_state(m: &Manifest) -> io::Result<DirectoryState> {
    let state = directory_state(m)?;
    if !state.owned {
        return Err(io::Error::other("backup contains unowned entries"));
    }
    Ok(state)
}
/// Rename one backup to `retired`, sync the root, then remove its files. The flag says
/// whether the rename happened, so the caller's budget moves even if removal failed.
fn retire(
    root: &Path,
    m: &Manifest,
    contents: DirectoryState,
    retired: &Path,
) -> (bool, io::Result<()>) {
    if fs::symlink_metadata(retired).is_ok() {
        let exists = io::Error::new(io::ErrorKind::AlreadyExists, "retired backup path exists");
        return (false, Err(exists));
    }
    if let Err(error) = rename(&m.directory, retired) {
        return (false, Err(error));
    }
    let files = contents
        .files
        .iter()
        .map(|p| retired.join(p.file_name().unwrap()))
        .collect();
    let result = File::open(root)
        .and_then(|f| f.sync_all())
        .and_then(|()| remove_retiring(retired, DirectoryState { files, ..contents }));
    (true, result)
}
/// Single interpreter: evaluate age/count/byte pressure in publication order, oldest first.
/// Every purpose's newest copy survives every rule. Manual copies are outside the
/// automatic byte budget and never retired. Status reports any irreducible excess.
fn prune_impl(
    root: &Path,
    now: u64,
    rules: &[Rule; 5],
    daily_keep_days: u64,
    max_bytes: u64,
    limit: usize,
) -> io::Result<Pruned> {
    let inventory = inventory(root, false)?;
    let all = inventory.manifests;
    let mut remaining: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    let mut newest = BTreeMap::new();
    let mut sizes = Vec::with_capacity(all.len());
    let mut report = Pruned {
        failures: inventory.failures,
        ..Pruned::default()
    };
    let (mut total, mut changed) = resume_retiring(inventory.retiring, limit, &mut report);
    for m in &all {
        let bytes = match directory_state(m) {
            Ok(state) => Some(state.bytes),
            Err(error) => {
                directory_failure(&m.directory, &error);
                report.failures += 1;
                None
            }
        };
        sizes.push(bytes);
        if rules[m.reason.rule_index()].limits.is_some() {
            total = total.saturating_add(bytes.unwrap_or(0));
        }
        let state = remaining.entry(m.reason.kind()).or_default();
        state.0 += 1;
        state.1 = state.1.saturating_add(bytes.unwrap_or(0));
        newest.insert(m.reason.kind(), m.id.as_str());
    }
    for (m, bytes) in all.iter().zip(sizes) {
        let kind = m.reason.kind();
        let rule = &rules[m.reason.rule_index()];
        let Some(limits) = rule.limits else {
            continue;
        };
        let Some(bytes) = bytes else {
            continue;
        };
        if newest.get(kind).copied() == Some(m.id.as_str()) {
            warn_if_latest_oversized(kind, bytes, limits.bytes);
            continue;
        }
        let state = remaining.get_mut(kind).unwrap();
        let expired = now.saturating_sub(m.created_ms)
            > limits
                .days
                .unwrap_or(daily_keep_days)
                .saturating_mul(DAY_MS);
        if !(expired || state.0 > limits.copies || state.1 > limits.bytes || total > max_bytes) {
            continue;
        }
        // Recheck ownership immediately before retirement; directory errors never stop others.
        let contents = match owned_state(m) {
            Ok(state) => state,
            Err(error) => {
                directory_failure(&m.directory, &error);
                report.failures += 1;
                continue;
            }
        };
        if report.removed == limit {
            report.more = true;
            break;
        }
        let retired = root.join(format!("{}.retiring", m.id));
        let (moved, result) = retire(root, m, contents, &retired);
        changed |= moved;
        if moved {
            state.0 -= 1;
            state.1 = state.1.saturating_sub(bytes);
            total = total.saturating_sub(bytes);
        }
        if let Err(error) = result {
            directory_failure(&m.directory, &error);
            report.failures += 1;
            if moved {
                total = total
                    .saturating_add(directory_contents(&retired, None).map_or(bytes, |s| s.bytes));
            }
            continue;
        }
        report.removed += 1;
    }
    if changed && let Err(error) = File::open(root).and_then(|f| f.sync_all()) {
        directory_failure(root, &error);
        report.failures += 1;
    }
    if total > max_bytes {
        tracing::warn!(
            bytes = total,
            max_bytes,
            "backup retention remains over total limit; protected or inaccessible copies retained"
        );
    }
    Ok(report)
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn copy(root: &Path, reason: Reason, at: u64) -> Manifest {
        let id = format!("{}-20260101T000000-{at:032x}", reason.prefix());
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
            published_order: at,
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
            let rule = reason.rule().limits.unwrap();
            assert!(
                rule.days.unwrap_or(DEFAULT_DAILY_KEEP_DAYS) > 0
                    && rule.copies > 0
                    && rule.bytes > 0
            );
            for at in 0..=rule.copies as u64 {
                copy(root.path(), reason.clone(), at);
            }
            assert_eq!(
                retain_at(root.path(), rule.copies as u64, DEFAULT_DAILY_KEEP_DAYS)
                    .unwrap()
                    .removed,
                1
            );
            assert_eq!(list(root.path()).unwrap().len(), rule.copies);
            let report = retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS).unwrap();
            assert_eq!(report.removed, rule.copies - 1);
            assert_eq!(list(root.path()).unwrap()[0].created_ms, rule.copies as u64);
            assert_eq!(
                retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS)
                    .unwrap()
                    .removed,
                0
            );
        }
    }

    #[test]
    fn every_purpose_enforces_its_own_byte_cap() {
        for reason in purposes() {
            let root = tempfile::tempdir().unwrap();
            let old = copy(root.path(), reason.clone(), 1);
            let latest = copy(root.path(), reason.clone(), 2);
            let mut rules = RULES;
            rules
                .iter_mut()
                .find(|r| r.kind == reason.kind())
                .unwrap()
                .limits
                .as_mut()
                .unwrap()
                .bytes = directory_state(&latest).unwrap().bytes;
            assert_eq!(
                prune_impl(
                    root.path(),
                    2,
                    &rules,
                    DEFAULT_DAILY_KEEP_DAYS,
                    TOTAL_BYTES,
                    500
                )
                .unwrap()
                .removed,
                1
            );
            assert!(!old.directory.exists() && latest.directory.exists());
        }
    }

    #[test]
    fn age_boundary_is_strict_and_manual_survives_even_extreme_pressure() {
        let root = tempfile::tempdir().unwrap();
        let old = copy(root.path(), Reason::Daily, 1);
        let manual = copy(root.path(), Reason::Manual, 2);
        let newest_manual = copy(root.path(), Reason::Manual, 4);
        let latest = copy(root.path(), Reason::Daily, 3);
        let deadline = 1 + DEFAULT_DAILY_KEEP_DAYS * DAY_MS;
        assert_eq!(
            retain_at(root.path(), deadline, DEFAULT_DAILY_KEEP_DAYS)
                .unwrap()
                .removed,
            0
        );
        assert_eq!(
            retain_at(root.path(), deadline + 1, DEFAULT_DAILY_KEEP_DAYS)
                .unwrap()
                .removed,
            1
        );
        assert!(!old.directory.exists());
        assert_eq!(
            prune_impl(
                root.path(),
                u64::MAX,
                &RULES,
                DEFAULT_DAILY_KEEP_DAYS,
                0,
                500
            )
            .unwrap()
            .removed,
            0
        );
        assert!(
            manual.directory.exists()
                && newest_manual.directory.exists()
                && latest.directory.exists()
        );
    }

    #[test]
    fn byte_caps_remove_oldest_globally_and_never_the_latest_of_any_purpose() {
        let root = tempfile::tempdir().unwrap();
        let old_daily = copy(root.path(), Reason::Daily, 1);
        let old_diagnostic = copy(root.path(), Reason::Diagnostic, 2);
        let daily = copy(root.path(), Reason::Daily, 3);
        let diagnostic = copy(root.path(), Reason::Diagnostic, 4);
        let bytes = usage(root.path())
            .unwrap()
            .values()
            .map(|u| u.bytes)
            .sum::<u64>();
        let cap = bytes - directory_state(&old_daily).unwrap().bytes;
        let report = prune_impl(root.path(), 4, &RULES, DEFAULT_DAILY_KEEP_DAYS, cap, 500).unwrap();
        assert_eq!(report.removed, 1);
        assert!(!old_daily.directory.exists());
        assert!(old_diagnostic.directory.exists());
        assert_eq!(
            prune_impl(root.path(), 4, &RULES, DEFAULT_DAILY_KEEP_DAYS, 0, 500)
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
        let old = copy(root.path(), Reason::Daily, 1);
        let middle = copy(root.path(), Reason::Daily, 2);
        let latest = copy(root.path(), Reason::Daily, 3);
        let other = copy(root.path(), Reason::Diagnostic, 4);
        let mut rules = RULES;
        rules[1].limits.as_mut().unwrap().bytes = directory_state(&latest).unwrap().bytes;
        let report = prune_impl(
            root.path(),
            4,
            &rules,
            DEFAULT_DAILY_KEEP_DAYS,
            TOTAL_BYTES,
            1,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
        assert!(report.more);
        assert!(!old.directory.exists() && middle.directory.exists());
        let report = prune_impl(
            root.path(),
            4,
            &rules,
            DEFAULT_DAILY_KEEP_DAYS,
            TOTAL_BYTES,
            1,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
        assert!(!report.more);
        assert!(!middle.directory.exists());
        assert!(latest.directory.exists() && other.directory.exists());
        assert_eq!(
            prune_impl(
                root.path(),
                4,
                &rules,
                DEFAULT_DAILY_KEEP_DAYS,
                TOTAL_BYTES,
                1
            )
            .unwrap()
            .removed,
            0
        );
    }

    #[test]
    fn missing_archives_and_database_are_retired_but_foreign_children_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let mut no_archive = copy(root.path(), Reason::Daily, 1);
        let mut artifact = no_archive.database.clone();
        artifact.name = "files.tar".into();
        artifact.bytes = u64::MAX; // historical size must not count as current disk usage
        no_archive.files.artifacts.push(artifact);
        save(&no_archive);
        let no_database = copy(root.path(), Reason::Daily, 2);
        fs::remove_file(no_database.database_path()).unwrap();
        let foreign = copy(root.path(), Reason::Daily, 3);
        fs::write(foreign.directory.join("operator-note"), "keep").unwrap();
        let linked = copy(root.path(), Reason::Daily, 4);
        std::os::unix::fs::symlink("outside", linked.directory.join("files.tar")).unwrap();
        let newest = copy(root.path(), Reason::Daily, 5);
        assert!(usage(root.path()).unwrap()["daily"].bytes < 20_000);
        assert_eq!(
            retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS)
                .unwrap()
                .removed,
            2
        );
        assert!(!no_archive.directory.exists() && !no_database.directory.exists());
        assert!(
            foreign.directory.exists() && linked.directory.exists() && newest.directory.exists()
        );
    }

    #[test]
    fn unreadable_manifest_is_reported_without_blocking_other_directories() {
        let root = tempfile::tempdir().unwrap();
        let bad = copy(root.path(), Reason::Daily, 1);
        let good = copy(root.path(), Reason::Daily, 2);
        let latest = copy(root.path(), Reason::Daily, 3);
        let path = bad.directory.join("manifest.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let denied = fs::read(&path).is_err();
        let report = retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(denied, "fixture must actually deny manifest access");
        let report = report.unwrap();
        assert_eq!(report.failures, 1);
        assert_eq!(report.removed, 1);
        assert!(!report.more);
        assert!(bad.directory.exists() && latest.directory.exists());
        assert!(!good.directory.exists());
        assert_eq!(
            retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS)
                .unwrap()
                .removed,
            1
        );
    }

    #[test]
    fn retirement_error_is_reported_without_blocking_later_backups() {
        let root = tempfile::tempdir().unwrap();
        let bad = copy(root.path(), Reason::Daily, 1);
        let good = copy(root.path(), Reason::Daily, 2);
        let latest = copy(root.path(), Reason::Daily, 3);
        let interrupted = root.path().join(format!("{}.retiring", bad.id));
        fs::write(&interrupted, "foreign collision").unwrap();
        let report = retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS).unwrap();
        assert_eq!(report.failures, 1);
        assert_eq!(report.removed, 1);
        assert!(!report.more);
        assert!(bad.directory.exists() && interrupted.exists() && latest.directory.exists());
        assert!(!good.directory.exists());
        assert!(!root.path().join(format!("{}.retiring", good.id)).exists());
    }

    #[test]
    fn manual_bytes_cannot_displace_any_automatic_purpose() {
        let root = tempfile::tempdir().unwrap();
        let mut automatic = Vec::new();
        for (i, reason) in purposes().into_iter().enumerate() {
            automatic.push(copy(root.path(), reason.clone(), (2 * i + 1) as u64));
            automatic.push(copy(root.path(), reason, (2 * i + 2) as u64));
        }
        let manual = copy(root.path(), Reason::Manual, 10);
        File::options()
            .write(true)
            .open(manual.database_path())
            .unwrap()
            .set_len(TOTAL_BYTES + 1)
            .unwrap();
        let status = retention_status(root.path()).unwrap();
        assert_eq!(
            status.bytes,
            automatic
                .iter()
                .map(|m| directory_state(m).unwrap().bytes)
                .sum::<u64>()
        );
        assert_eq!(status.manual_bytes, status.purposes["manual"].bytes);
        assert!(status.manual_bytes > TOTAL_BYTES);
        assert!(!status.over_limit);
        let report = retain_at(root.path(), 10, DEFAULT_DAILY_KEEP_DAYS).unwrap();
        assert_eq!((report.removed, report.failures), (0, 0));
        assert!(automatic.iter().all(|m| m.directory.exists()));
        assert!(manual.directory.exists());
    }

    #[test]
    // The fixture makes removal fail with a read-only copy. Some macOS releases also refuse to
    // rename a directory without write permission, so the failure would come a step earlier;
    // the accounting under test is the same on every platform.
    #[cfg_attr(target_os = "macos", ignore)]
    fn failed_retirement_is_accounted_and_retried_before_new_candidates() {
        let root = tempfile::tempdir().unwrap();
        let old = copy(root.path(), Reason::Daily, 1);
        let middle = copy(root.path(), Reason::Daily, 2);
        let latest = copy(root.path(), Reason::Daily, 3);
        fs::set_permissions(&old.directory, fs::Permissions::from_mode(0o500)).unwrap();
        let denied = fs::write(old.directory.join("denied"), "denied").is_err();
        let first = retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS);
        let retired = root.path().join(format!("{}.retiring", old.id));
        fs::set_permissions(&retired, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(denied, "fixture must deny file removal");
        let first = first.unwrap();
        assert_eq!((first.removed, first.failures), (1, 1));
        assert!(!old.directory.exists() && !middle.directory.exists() && latest.directory.exists());
        assert!(retired.join("manifest.json").is_file());
        let status = retention_status(root.path()).unwrap();
        assert_eq!(status.retiring_count, 1);
        assert_eq!(
            status.retiring_bytes,
            directory_contents(&retired, None).unwrap().bytes
        );
        assert_eq!(
            status.bytes,
            status.retiring_bytes + status.purposes["daily"].bytes
        );
        // Emulate interruption after removing one artifact. Manifest remains readable.
        fs::remove_file(retired.join("db.sqlite3")).unwrap();
        let next = retain_at(root.path(), 0, DEFAULT_DAILY_KEEP_DAYS).unwrap();
        assert_eq!((next.removed, next.failures), (1, 0));
        assert!(!retired.exists() && latest.directory.exists());
        let status = retention_status(root.path()).unwrap();
        assert_eq!((status.retiring_count, status.retiring_bytes), (0, 0));
    }

    #[test]
    fn retirement_retries_are_sliced_and_preserve_partial_foreign_and_linked_paths() {
        let root = tempfile::tempdir().unwrap();
        let first = copy(root.path(), Reason::Daily, 1);
        let second = copy(root.path(), Reason::Daily, 2);
        let partial = copy(root.path(), Reason::Daily, 3);
        let first_retired = root.path().join(format!("{}.retiring", first.id));
        let second_retired = root.path().join(format!("{}.retiring", second.id));
        let writing = root.path().join(format!("{}.partial", partial.id));
        fs::rename(&first.directory, &first_retired).unwrap();
        fs::rename(&second.directory, &second_retired).unwrap();
        fs::rename(&partial.directory, &writing).unwrap();
        let foreign = root.path().join("foreign.retiring");
        fs::create_dir(&foreign).unwrap();
        let linked = root.path().join("linked.retiring");
        std::os::unix::fs::symlink(&writing, &linked).unwrap();
        let report = prune_impl(
            root.path(),
            0,
            &RULES,
            DEFAULT_DAILY_KEEP_DAYS,
            TOTAL_BYTES,
            1,
        )
        .unwrap();
        assert_eq!((report.removed, report.failures, report.more), (1, 1, true));
        assert!(!first_retired.exists() && second_retired.exists());
        // Crash between unlinking the manifest and removing the empty directory is retryable.
        fs::remove_file(second_retired.join("db.sqlite3")).unwrap();
        fs::remove_file(second_retired.join("manifest.json")).unwrap();
        let report = prune_impl(
            root.path(),
            0,
            &RULES,
            DEFAULT_DAILY_KEEP_DAYS,
            TOTAL_BYTES,
            1,
        )
        .unwrap();
        assert_eq!(
            (report.removed, report.failures, report.more),
            (1, 1, false)
        );
        assert!(!second_retired.exists());
        assert!(writing.exists() && foreign.exists() && linked.is_symlink());
    }

    #[test]
    fn protected_oversized_copy_is_visible_in_status() {
        let root = tempfile::tempdir().unwrap();
        let latest = copy(root.path(), Reason::Diagnostic, 1);
        // Sparse logical file: no large allocation, copy, archive or hashing.
        File::options()
            .write(true)
            .open(latest.database_path())
            .unwrap()
            .set_len(Reason::Diagnostic.rule().limits.unwrap().bytes + 1)
            .unwrap();
        let status = retention_status(root.path()).unwrap();
        assert!(status.over_limit);
        assert!(status.purposes["diagnostic"].latest_over_limit);
        assert!(status.purposes["diagnostic"].over_limit);
        assert_eq!(
            retain_at(root.path(), u64::MAX, DEFAULT_DAILY_KEEP_DAYS)
                .unwrap()
                .removed,
            0
        );
        assert!(latest.directory.exists());
    }
}
