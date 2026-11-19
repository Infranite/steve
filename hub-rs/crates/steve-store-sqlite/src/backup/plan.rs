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
    let mut files = Files {
        consistency: "best_effort".into(),
        complete: true,
        artifacts: vec![],
        error: None,
    };
    if let Some(set) = &plan.with_files {
        for attempt in 0..3 {
            check(start, plan.deadline, cancel)?;
            match (set.archive)(&set.home, &partial)
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
    if matches!(plan.keep, Keep::Default)
        && let Err(error) = prune_impl(&plan.dest_root, &plan.reason, now_ms(), Some(&manifest.id))
    {
        tracing::warn!("backup retention failed: {error}");
    }
    Ok(manifest)
}
fn single_name(name: &str) -> bool {
    Path::new(name).file_name().is_some_and(|n| n == name) && name != "." && name != ".."
}
/// Inventory only completed, recognizable directories, never symlinked or foreign paths.
pub fn list(root: &Path) -> io::Result<Vec<Manifest>> {
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
        if !fs::symlink_metadata(path.join(&m.database.name)).is_ok_and(|f| f.is_file()) {
            continue;
        }
        m.directory = path;
        all.push(m);
    }
    all.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
    Ok(all)
}
pub fn usage(root: &Path) -> io::Result<BTreeMap<String, Usage>> {
    let mut result: BTreeMap<String, Usage> =
        ["daily", "pre-upgrade", "pre-destructive", "diagnostic"]
            .into_iter()
            .map(|k| (k.into(), Usage::default()))
            .collect();
    for m in list(root)? {
        let total = m.database.bytes
            + m.files.artifacts.iter().map(|a| a.bytes).sum::<u64>()
            + fs::metadata(m.directory.join("manifest.json"))?.len();
        let u = result.entry(m.reason.kind().into()).or_default();
        u.count += 1;
        u.bytes += total;
    }
    Ok(result)
}
pub fn prune(root: &Path, reason: &Reason) -> io::Result<usize> {
    prune_at(root, reason, now_ms())
}
/// Explicit clock keeps retention tests deterministic. Foreign files and partials are retained.
pub fn prune_at(root: &Path, reason: &Reason, now: u64) -> io::Result<usize> {
    prune_impl(root, reason, now, None)
}
fn prune_impl(
    root: &Path,
    reason: &Reason,
    now: u64,
    protected: Option<&str>,
) -> io::Result<usize> {
    let mut candidates: Vec<_> = list(root)?
        .into_iter()
        .filter(|m| m.reason.kind() == reason.kind())
        .collect();
    candidates.reverse();
    // Clock corrections or equal timestamps must never retire the copy just returned.
    if let Some(position) = candidates
        .iter()
        .position(|m| Some(m.id.as_str()) == protected)
    {
        let current = candidates.remove(position);
        candidates.insert(0, current);
    }
    let mut removed = 0;
    for (n, m) in candidates.into_iter().enumerate() {
        let old = match reason {
            Reason::Daily => now.saturating_sub(m.created_ms) > 14 * 86400000,
            Reason::PreUpgrade { .. } => n >= 8,
            _ => false,
        };
        if old && Some(m.id.as_str()) != protected {
            let mut expected = vec!["manifest.json".to_owned(), m.database.name.clone()];
            expected.extend(m.files.artifacts.iter().map(|a| a.name.clone()));
            if fs::read_dir(&m.directory)?.any(|e| {
                e.map_or(true, |e| {
                    !e.file_type().is_ok_and(|f| f.is_file())
                        || !expected.contains(&e.file_name().to_string_lossy().into_owned())
                })
            }) {
                continue;
            }
            fs::remove_dir_all(&m.directory)?;
            removed += 1;
        }
    }
    if removed > 0 {
        File::open(root)?.sync_all()?;
    }
    Ok(removed)
}
