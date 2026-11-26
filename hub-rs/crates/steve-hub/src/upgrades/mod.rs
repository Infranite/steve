//! Instance updates. The journal is deliberately outside SQLite: rollback must not erase its receipt.
pub mod cli;
mod client;
pub mod install;
pub mod maintenance;
pub mod runner;
pub mod signing;
pub mod supervisor;
#[cfg(test)]
mod tests;
use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub use supervisor::{run as supervise, startup_gate};

/// How long a version check waits for `check.lock` (tries × wait) before it says another check runs.
const CHECK_LOCK_TRIES: u32 = 40;
const CHECK_LOCK_WAIT: Duration = Duration::from_millis(50);

/// Per-request HTTP timeout during an update check, bounded so retries finish within the UI timeout.
pub(crate) const CHECK_HTTP_TIMEOUT: Duration = Duration::from_secs(8);
/// Retries after the initial attempt on transient GitHub check errors (5xx, timeout, connect).
pub(crate) const CHECK_RETRY_ATTEMPTS: u32 = 2;
/// Initial exponential backoff delay between update-check retries.
pub(crate) const CHECK_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
/// Maximum exponential backoff delay between update-check retries.
pub(crate) const CHECK_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
/// Exponential backoff multiplier for update-check retries.
pub(crate) const CHECK_RETRY_FACTOR: u32 = 2;
/// Cache TTL (ms) for healthy check results and GitHub rate-limit responses.
const CHECK_CACHE_TTL_MS: u64 = 600_000;
/// Shorter cache TTL (ms) for transient upstream failures and permanent check errors.
const CHECK_FAILURE_CACHE_TTL_MS: u64 = 60_000;
/// Delay after Hub startup before the first background update check.
const AUTO_CHECK_INITIAL_DELAY: Duration = Duration::from_secs(30);
/// Normal interval between background update checks.
const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Initial background retry delay when GitHub is temporarily unavailable.
const AUTO_CHECK_TRANSIENT_RETRY_BASE: Duration = Duration::from_secs(5 * 60);
/// Upper bound on background retry delay while GitHub remains unavailable.
const AUTO_CHECK_TRANSIENT_RETRY_MAX: Duration = Duration::from_secs(60 * 60);
/// Maximum historical upgrade/deploy records returned by `status()`.
const UPDATE_HISTORY_LIMIT: usize = 10;
/// Clock/second-truncation tolerance when correlating pre-`operation_id` local deploy receipts.
const RECEIPT_START_SLACK_MS: u64 = 2_000;
/// Maximum finish-time delta when correlating pre-`operation_id` local deploy receipts.
const RECEIPT_FINISH_SLACK_MS: u64 = 5_000;
/// Lifetime of an unapproved agent upgrade request (24 hours).
const APPROVAL_TTL_MS: u64 = 86_400_000;

pub(crate) fn default_check_backoff() -> steve_runtime::policy::Backoff {
    steve_runtime::policy::Backoff {
        base: CHECK_RETRY_BASE_DELAY,
        max: CHECK_RETRY_MAX_DELAY,
        factor: CHECK_RETRY_FACTOR,
        jitter: steve_runtime::policy::Jitter::Equal,
        attempts: Some(CHECK_RETRY_ATTEMPTS),
        reset_after: None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub target: String,
    pub name: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schema {
    pub min_read: u32,
    pub max_read: u32,
    pub target: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub version: String,
    pub commit: String,
    pub hub_version: String,
    pub web_version: String,
    pub api_protocol: u32,
    pub schema: Schema,
    pub assets: Vec<Asset>,
}

/// Strongly typed outcome of a GitHub release check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Available,
    UpToDate,
    Ahead,
    UnknownCurrent,
    NoRelease,
    UnsupportedPlatform,
    /// Temporary upstream outage (5xx, timeout, connection failure, rate limit); retries automatically.
    Unavailable,
    /// Permanent or semantic failure (for example, missing or invalid release manifest).
    Error,
}

impl CheckStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::UpToDate => "up_to_date",
            Self::Ahead => "ahead",
            Self::UnknownCurrent => "unknown_current",
            Self::NoRelease => "no_release",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
        }
    }
}

/// Strongly typed classification of an update-check failure for localized UI rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckErrorKind {
    UpstreamServer,
    Timeout,
    Connect,
    RateLimited,
    InvalidManifest,
    InvalidRelease,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub status: CheckStatus,
    pub checked_at: u64,
    pub version: Option<String>,
    pub release_url: Option<String>,
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<CheckErrorKind>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedCheck {
    repository: String,
    current_hub: String,
    current_web: String,
    result: Check,
}
impl CachedCheck {
    fn matches(&self, u: &Updates) -> bool {
        self.repository == u.repository
            && self.current_hub == u.current_hub
            && self.current_web == u.current_web
    }
    fn ttl(&self) -> u64 {
        match (self.result.status, self.result.error_kind) {
            (CheckStatus::Unavailable, Some(CheckErrorKind::RateLimited)) => CHECK_CACHE_TTL_MS,
            (CheckStatus::Unavailable | CheckStatus::Error, _) => CHECK_FAILURE_CACHE_TTL_MS,
            _ => CHECK_CACHE_TTL_MS,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub received_bytes: u64,
    pub total_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub step: String,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadProgress>,
}
#[derive(Clone, Debug, Serialize)]
pub struct BackupStatus {
    pub snapshot_id: Option<crate::backup::SnapshotId>,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct CurrentVersions {
    pub hub: String,
    pub web: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Capability {
    pub managed: bool,
    pub can_execute: bool,
    pub owner: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstallView {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl InstallView {
    pub fn owner(install: install::Install) -> Self {
        Self {
            kind: install.kind,
            home: Some(install.home),
            release: install.release,
            repo: install.repo,
        }
    }

    pub fn redacted(kind: &'static str) -> Self {
        Self {
            kind,
            home: None,
            release: None,
            repo: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    pub repository: String,
    pub current: CurrentVersions,
    pub capability: Capability,
    pub install: InstallView,
    pub check: Option<Check>,
    pub operation: Option<Operation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Operation>,
    pub backup: Option<BackupStatus>,
}

/// Origin of an upgrade or deployment receipt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationSource {
    #[default]
    InApp,
    TerminalDeploy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub idempotency_key: String,
    pub version: String,
    pub phase: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub requested_by: String,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub approved_at: Option<u64>,
    pub error: Option<String>,
    pub previous: Option<String>,
    /// Version that was running before this upgrade (and restored on rollback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    pub target: Option<String>,
    #[serde(default)]
    pub source: OperationSource,
    /// A local_release upgrade: what the one-shot runner job downloads and deploys (`runner.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<runner::Spec>,
    /// The verified repository snapshot taken before switching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_snapshot: Option<crate::backup::SnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// The last actual drain observation. Kept across reconnects, never guessed from item status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain: Option<Value>,
}
impl Operation {
    pub fn terminal(&self) -> bool {
        matches!(self.phase.as_str(), "succeeded" | "failed" | "rolled_back")
    }
    pub fn is_failure(&self) -> bool {
        matches!(self.phase.as_str(), "failed" | "rolled_back")
    }
    pub fn redact_paths(&mut self) {
        self.previous = None;
        self.target = None;
        self.runner = None;
        self.backup_snapshot = None;
        self.drain = None;
    }
}
#[derive(Clone)]
pub struct Updates {
    pub home: PathBuf,
    pub repository: String,
    pub current_hub: String,
    pub current_web: String,
    pub managed: bool,
    pub blocked_reason: Option<String>,
    pub client: reqwest::Client,
    pub api_base: String,
    /// github.com, for the rate-limit fallback and the tag a local runner fetches.
    pub web_base: String,
    /// STEVE_REPO: the checkout release scripts run from.
    pub repo: PathBuf,
    /// The installation kind, when not detected from where this process runs (tests).
    pub kind: Option<&'static str>,
    pub launch: runner::Launch,
    /// Per-request timeout for update checks.
    pub check_timeout: Duration,
    /// Bounded backoff policy for transient upstream update-check failures.
    pub check_backoff: steve_runtime::policy::Backoff,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(
        &fs::read(p).with_context(|| p.display().to_string())?,
    )?)
}
pub fn atomic<T: Serialize>(p: &Path, value: &T) -> Result<()> {
    let parent = p.parent().context("file needs parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&tmp, p)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn lock(p: &Path) -> Result<File> {
    fs::create_dir_all(p.parent().unwrap())?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p)?;
    f.lock()?;
    Ok(f)
}
pub fn platform() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else if cfg!(target_os = "linux") {
            "unknown-linux-gnu"
        } else {
            "unsupported"
        }
    )
}
/// STEVE_UPDATE_REPOSITORY (`owner/name`), the only place releases are taken from.
pub(crate) fn repository() -> Result<String> {
    let repository =
        std::env::var("STEVE_UPDATE_REPOSITORY").unwrap_or_else(|_| "Infranite/steve".into());
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || p.starts_with('.')
                || !p
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
    {
        bail!("invalid STEVE_UPDATE_REPOSITORY");
    }
    Ok(repository)
}

/// The newer release the last version check found for exactly this Hub and Web, if any: what the
/// page's update notice shows (`/api/shell`). Reads the cached check only, never the network.
pub fn available(cfg: &Config) -> Option<String> {
    let cached: CachedCheck = read(&cfg.home.join("updates/check.json")).ok()?;
    (cached.repository == repository().ok()?
        && cached.current_hub == cfg.version
        && cached.current_web == crate::web::web_version(cfg)
        && cached.result.status == CheckStatus::Available)
        .then_some(cached.result.version)
        .flatten()
}

/// Checks for a newer release shortly after start and then every six hours (with bounded backoff
/// retries on transient upstream failures), so the page can say so without anyone opening Settings.
/// Off in debug builds unless STEVE_UPDATE_CHECK=1; STEVE_UPDATE_CHECK=0 turns it off.
pub fn auto_check_task(cfg: Config) -> steve_runtime::Task {
    let on = match std::env::var("STEVE_UPDATE_CHECK").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => !cfg!(debug_assertions),
    };
    if !on {
        return steve_runtime::spawn("upgrades.check", async {});
    }
    let clock: std::sync::Arc<dyn steve_runtime::Clock> =
        std::sync::Arc::new(steve_runtime::SystemClock);
    let task_clock = clock.clone();
    steve_runtime::jobs::supervise("upgrades.check", clock, move || {
        let cfg = cfg.clone();
        let clock = task_clock.clone();
        async move {
            let transient_backoff = steve_runtime::policy::Backoff {
                base: AUTO_CHECK_TRANSIENT_RETRY_BASE,
                max: AUTO_CHECK_TRANSIENT_RETRY_MAX,
                factor: CHECK_RETRY_FACTOR,
                jitter: steve_runtime::policy::Jitter::Equal,
                attempts: None,
                reset_after: None,
            };
            let mut transient_failures = 0u32;
            clock.sleep(AUTO_CHECK_INITIAL_DELAY).await;
            loop {
                let status =
                    steve_runtime::jobs::round("upgrades.check", check_release(&cfg)).await;
                let wait = if status == Some(CheckStatus::Unavailable) {
                    let delay = transient_backoff.sample_delay(transient_failures);
                    transient_failures = transient_failures.saturating_add(1);
                    delay
                } else {
                    transient_failures = 0;
                    AUTO_CHECK_INTERVAL
                };
                clock.sleep(wait).await;
            }
        }
    })
}

pub async fn run_auto_check(cfg: Config) {
    auto_check_task(cfg).await;
}

async fn check_release(cfg: &Config) -> Option<CheckStatus> {
    match Updates::new(cfg) {
        Ok(u) => match u.check().await {
            Ok(c) => {
                tracing::info!("update check: {} {:?}", c.status.as_str(), c.version);
                Some(c.status)
            }
            Err(e) => {
                tracing::info!("update check: {e:#}");
                None
            }
        },
        Err(e) => {
            tracing::warn!("update check: {e:#}");
            None
        }
    }
}

pub(super) fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

fn same_release_version(left: &str, right: &str) -> bool {
    let left = normalize_version(left);
    let right = normalize_version(right);
    !left.is_empty() && left == right
}

/// Extract a release version from a directory or label such as `0.11.0-local-51826fc`,
/// `0.11.0-<uuid>`, `v0.11.0`, or `0.11.0`. Dated `ops/release.sh` directories return `None`.
fn extract_version_from_label(label: &str) -> Option<String> {
    let trimmed = normalize_version(label);
    let parsed = semver::Version::parse(&trimmed).ok()?;
    if parsed.pre.as_str().starts_with("local-")
        || uuid::Uuid::parse_str(parsed.pre.as_str()).is_ok()
    {
        return Some(format!(
            "{}.{}.{}",
            parsed.major, parsed.minor, parsed.patch
        ));
    }
    Some(parsed.to_string())
}

fn resolve_version_from_release_path(raw: &str) -> Option<String> {
    if let Some(version) = extract_version_from_label(raw) {
        return Some(version);
    }
    let path = Path::new(raw);
    if let Ok(manifest) = read::<Value>(&path.join("release.json"))
        && let Some(version) = manifest["version"].as_str()
    {
        let normalized = normalize_version(version);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(extract_version_from_label)
}

fn parse_local_stamp_ms(stamp: &str) -> Option<u64> {
    let dt = jiff::civil::DateTime::strptime("%Y%m%d-%H%M%S", stamp).ok()?;
    let zoned = dt.to_zoned(jiff::tz::TimeZone::system()).ok()?;
    u64::try_from(zoned.timestamp().as_millisecond()).ok()
}

fn file_mtime_ms(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    u64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

fn dir_entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
        .collect()
}

#[derive(Clone, Debug, Deserialize)]
struct LocalDeployReceipt {
    version: String,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    previous: Option<String>,
    #[serde(default)]
    previous_version: Option<String>,
    #[serde(default)]
    started: Option<String>,
    #[serde(default)]
    finished: Option<String>,
    #[serde(default)]
    started_at: Option<u64>,
    #[serde(default)]
    finished_at: Option<u64>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    recovered: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    backup_snapshot: Option<crate::backup::SnapshotId>,
    #[serde(default)]
    operation_id: Option<String>,
}

struct ParsedLocalReceipt {
    operation_id: Option<String>,
    has_structured_time: bool,
    op: Operation,
}

fn parse_local_receipt(path: &Path) -> Option<ParsedLocalReceipt> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with("local-") || !name.ends_with(".json") {
        return None;
    }
    let receipt: LocalDeployReceipt = read(path).ok()?;
    let ok = receipt.ok?;
    let started_ms = receipt
        .started_at
        .or_else(|| receipt.started.as_deref().and_then(parse_local_stamp_ms));
    let finished_ms = receipt
        .finished_at
        .or_else(|| receipt.finished.as_deref().and_then(parse_local_stamp_ms));
    let mtime_ms = file_mtime_ms(path);
    let created_at = started_ms.or(finished_ms).or(mtime_ms).unwrap_or(0);
    let updated_at = finished_ms
        .or(started_ms)
        .or(mtime_ms)
        .unwrap_or(created_at);
    let phase = if ok {
        "succeeded"
    } else if receipt.recovered == Some(true) {
        "rolled_back"
    } else {
        "failed"
    };
    let previous_version = receipt
        .previous_version
        .as_deref()
        .map(normalize_version)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            receipt
                .previous
                .as_deref()
                .and_then(resolve_version_from_release_path)
        });
    let id = name.strip_suffix(".json")?.to_owned();
    let op = Operation {
        id,
        idempotency_key: String::new(),
        version: normalize_version(&receipt.version),
        phase: phase.into(),
        created_at,
        updated_at,
        requested_by: "terminal".into(),
        approved_by: None,
        approved_at: None,
        error: receipt.error,
        previous: receipt.previous,
        previous_version,
        target: receipt.release,
        source: OperationSource::TerminalDeploy,
        runner: None,
        backup_snapshot: receipt.backup_snapshot,
        progress: None,
        drain: None,
    };
    Some(ParsedLocalReceipt {
        operation_id: receipt.operation_id,
        has_structured_time: receipt.started_at.is_some(),
        op,
    })
}

fn receipt_corresponds_to_op(receipt: &ParsedLocalReceipt, op: &Operation) -> bool {
    if let Some(op_id) = receipt.operation_id.as_deref() {
        return op.id == op_id;
    }
    if receipt.has_structured_time {
        return false;
    }
    op.runner.is_some()
        && op.terminal()
        && op.phase == receipt.op.phase
        && same_release_version(&op.version, &receipt.op.version)
        && receipt.op.created_at.saturating_add(RECEIPT_START_SLACK_MS) >= op.created_at
        && receipt.op.created_at <= op.updated_at.saturating_add(RECEIPT_START_SLACK_MS)
        && receipt.op.updated_at <= op.updated_at.saturating_add(RECEIPT_FINISH_SLACK_MS)
}

fn enrich_op_from_receipt(op: &mut Operation, receipt: &Operation) {
    if op.previous.is_none() {
        op.previous = receipt.previous.clone();
    }
    if op.previous_version.is_none() {
        op.previous_version = receipt.previous_version.clone();
    }
    if op.backup_snapshot.is_none() {
        op.backup_snapshot = receipt.backup_snapshot.clone();
    }
}

impl Updates {
    pub fn new(cfg: &Config) -> Result<Self> {
        let repository = repository()?;
        Ok(Self {
            home: cfg.home.clone(),
            repository,
            current_hub: cfg.version.clone(),
            current_web: crate::web::web_version(cfg),
            managed: std::env::var("STEVE_MANAGED").as_deref() == Ok("1"),
            blocked_reason: if cfg!(target_os = "macos")
                && !matches!(
                    std::env::var("STEVE_VAULT_KEYSTORE").as_deref(),
                    Ok("dir") | Ok("file")
                ) {
                Some(steve_i18n::t!("hub.upgrade.reason.keychain"))
            } else {
                None
            },
            client: reqwest::Client::builder()
                .user_agent("Steve-updater/1")
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            api_base: "https://api.github.com".into(),
            web_base: "https://github.com".into(),
            repo: cfg.steve_repo.clone(),
            kind: None,
            launch: runner::Launch::Launchd,
            check_timeout: CHECK_HTTP_TIMEOUT,
            check_backoff: default_check_backoff(),
        })
    }
    /// Only the journal, for the runner process: no network, no installation probe.
    pub fn journal(home: &Path) -> Self {
        Self {
            home: home.into(),
            repository: String::new(),
            current_hub: String::new(),
            current_web: String::new(),
            managed: false,
            blocked_reason: None,
            client: reqwest::Client::new(),
            api_base: String::new(),
            web_base: String::new(),
            repo: PathBuf::new(),
            kind: None,
            launch: runner::Launch::Launchd,
            check_timeout: CHECK_HTTP_TIMEOUT,
            check_backoff: default_check_backoff(),
        }
    }
    /// Why this page can't upgrade this installation itself; `None` when it can.
    pub fn execution_reason(&self) -> Option<String> {
        if self.managed {
            return self.blocked_reason.clone();
        }
        let install = self.install(&self.repo);
        if install.kind == "local_release" {
            let missing = runner::local_missing(self);
            return (!missing.is_empty()).then(|| {
                steve_i18n::t!(
                    "hub.upgrade.reason.local_missing",
                    missing = missing.join(steve_i18n::t!("hub.error.list_sep").as_str())
                )
            });
        }
        install.reason()
    }
    pub fn dir(&self) -> PathBuf {
        self.home.join("updates")
    }
    pub fn operation(&self, id: &str) -> Result<Operation> {
        if uuid::Uuid::parse_str(id).is_err() {
            bail!("invalid operation id")
        };
        read(&self.dir().join("operations").join(format!("{id}.json")))
    }
    pub fn save(&self, op: &Operation) -> Result<()> {
        atomic(
            &self
                .dir()
                .join("operations")
                .join(format!("{}.json", op.id)),
            op,
        )
    }
    pub fn active(&self) -> Result<Option<Operation>> {
        let p = self.dir().join("active.json");
        if !p.exists() {
            return Ok(None);
        };
        let v: Value = read(&p)?;
        Ok(Some(self.operation(
            v["id"].as_str().context("invalid active receipt")?,
        )?))
    }
    pub fn phase(&self, op: &mut Operation, phase: &str, error: Option<String>) -> Result<()> {
        op.phase = phase.into();
        op.updated_at = now();
        op.error = error;
        let step = match phase {
            "staging" => "resolving",
            "ready" => "preflight",
            "draining" => "waiting",
            "stopping" => "stopping",
            "backing_up" => "backing_up",
            "restarting" => "starting",
            "verifying" | "committed" => "checking",
            "succeeded" => "complete",
            "rolling_back" | "rolled_back_pending_health" | "rolled_back" => "rollback",
            _ => return self.save(op),
        };
        self.progress(op, step, None)
    }
    pub(super) fn progress(
        &self,
        op: &mut Operation,
        step: &str,
        download: Option<DownloadProgress>,
    ) -> Result<()> {
        let at = now();
        let started_at = op
            .progress
            .as_ref()
            .filter(|p| p.step == step)
            .map_or(at, |p| p.started_at);
        op.updated_at = at;
        op.progress = Some(Progress {
            step: step.into(),
            started_at,
            updated_at: at,
            download,
        });
        self.save(op)
    }
    fn running_at_least(&self, target: &str) -> bool {
        let target_norm = normalize_version(target);
        let hub_norm = normalize_version(&self.current_hub);
        let hub_ok = match (
            semver::Version::parse(&hub_norm),
            semver::Version::parse(&target_norm),
        ) {
            (Ok(cur), Ok(tgt)) => cur >= tgt,
            _ => !hub_norm.is_empty() && hub_norm == target_norm,
        };
        if !hub_ok {
            return false;
        }
        let web_norm = normalize_version(&self.current_web);
        match (
            semver::Version::parse(&web_norm),
            semver::Version::parse(&target_norm),
        ) {
            (Ok(cur_web), Ok(tgt)) => cur_web >= tgt,
            _ => true,
        }
    }

    fn resolve_records(&self) -> Result<(Option<Operation>, Vec<Operation>)> {
        let mut active = self.active()?;
        if let Some(op) = active.as_mut() {
            runner::reap(self, op)?;
            if op.previous_version.is_none() {
                op.previous_version = op
                    .previous
                    .as_deref()
                    .and_then(resolve_version_from_release_path);
            }
        }
        let active_id = active.as_ref().map(|op| op.id.clone());
        let mut records: Vec<Operation> = Vec::new();
        for path in dir_entries(&self.dir().join("operations")) {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(mut op) = read::<Operation>(&path) else {
                continue;
            };
            if op.requested_by == "local-release" && op.created_at == 0 && op.updated_at == 0 {
                continue;
            }
            if active_id.as_deref() == Some(op.id.as_str()) {
                continue;
            }
            if !op.terminal() {
                continue;
            }
            if op.previous_version.is_none() {
                op.previous_version = op
                    .previous
                    .as_deref()
                    .and_then(resolve_version_from_release_path);
            }
            records.push(op);
        }
        if let Some(op) = active.as_ref().filter(|op| op.terminal()) {
            records.push(op.clone());
        }
        for path in dir_entries(&self.home.join("logs/release")) {
            let Some(parsed) = parse_local_receipt(&path) else {
                continue;
            };
            if let Some(op) = active
                .as_mut()
                .filter(|op| !op.terminal() && receipt_corresponds_to_op(&parsed, op))
            {
                enrich_op_from_receipt(op, &parsed.op);
                continue;
            }
            if let Some(existing) = records
                .iter_mut()
                .find(|op| receipt_corresponds_to_op(&parsed, op))
            {
                enrich_op_from_receipt(existing, &parsed.op);
                continue;
            }
            records.push(parsed.op);
        }
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| b.id.cmp(&a.id))
        });

        if let Some(op) = active.filter(|op| !op.terminal()) {
            records.truncate(UPDATE_HISTORY_LIMIT);
            return Ok((Some(op), records));
        }

        if let Some(head) = records.first_mut() {
            if head.phase == "rolled_back"
                && head.previous_version.is_none()
                && !self.current_hub.is_empty()
                && !same_release_version(&self.current_hub, &head.version)
            {
                head.previous_version = Some(normalize_version(&self.current_hub));
            }
            if head.is_failure() && self.running_at_least(&head.version) {
                records.truncate(UPDATE_HISTORY_LIMIT);
                return Ok((None, records));
            }
            let current = records.remove(0);
            records.truncate(UPDATE_HISTORY_LIMIT);
            return Ok((Some(current), records));
        }
        Ok((None, Vec::new()))
    }

    pub fn status(&self) -> Result<UpdateStatus> {
        let (operation, history) = self.resolve_records()?;
        let backup = operation.as_ref().map(|op| BackupStatus {
            snapshot_id: op.backup_snapshot.clone(),
            dir: crate::backup::root(&self.home),
        });
        let kind = self.install(&self.repo).kind;
        let ready = if kind == "local_release" {
            "local_release runner"
        } else {
            "managed supervisor"
        };
        let execution_reason = self.execution_reason();
        Ok(UpdateStatus {
            repository: self.repository.clone(),
            current: CurrentVersions {
                hub: self.current_hub.clone(),
                web: self.current_web.clone(),
            },
            capability: Capability {
                managed: self.managed,
                can_execute: execution_reason.is_none(),
                owner: true,
                reason: execution_reason.unwrap_or_else(|| ready.into()),
            },
            install: InstallView::redacted(kind),
            check: self.cached_check().map(|cached| cached.result),
            operation,
            history,
            backup,
        })
    }
    /// `repo`: STEVE_REPO, where release scripts run from.
    pub fn install(&self, repo: &Path) -> install::Install {
        let mut install = install::detect(&install::Probe {
            managed: self.managed,
            blocked: self.blocked_reason.is_some(),
            home: &self.home,
            exe: std::env::current_exe().ok(),
            repo,
            forced: std::env::var("STEVE_INSTALL_KIND").ok(),
        });
        if let Some(kind) = self.kind {
            install.kind = kind;
        }
        install
    }
    fn cached_check(&self) -> Option<CachedCheck> {
        read::<CachedCheck>(&self.dir().join("check.json"))
            .ok()
            .filter(|cached| cached.matches(self))
    }
    pub async fn check(&self) -> Result<Check> {
        fs::create_dir_all(self.dir())?;
        let guard = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir().join("check.lock"))?;
        // A busy lock is usually another check, but it can also be a child process forked while a
        // check held it: the child keeps a copy of the descriptor, and so the lock, until it execs. So
        // wait a little before saying a check is running (then its fresh result is in the cache).
        let mut tries = 0;
        while guard.try_lock().is_err() {
            tries += 1;
            if tries < CHECK_LOCK_TRIES {
                tokio::time::sleep(CHECK_LOCK_WAIT).await;
                continue;
            }
            return self
                .cached_check()
                .map(|cached| cached.result)
                .context(steve_i18n::t!("hub.upgrade.check_running"));
        }
        if let Some(cached) = self.cached_check()
            && now().saturating_sub(cached.result.checked_at) < cached.ttl()
        {
            return Ok(cached.result);
        }
        let mut c = Check {
            status: CheckStatus::Error,
            checked_at: now(),
            version: None,
            release_url: None,
            notes: None,
            error_kind: None,
            error: None,
        };
        // the unauthenticated API allows 60 calls an hour per address: on any failure ask github.com
        // which tag `latest` is, which isn't counted
        let fetched = match self.latest_checked().await {
            Ok(Some(release)) => Ok(Some((release, None))),
            Ok(None) => Ok(None),
            Err(api_err) => {
                if self.web_base.is_empty() {
                    Err(api_err)
                } else {
                    match self.latest_web_checked().await {
                        Ok(Some((release, manifest))) => Ok(Some((release, Some(manifest)))),
                        Ok(None) => Ok(None),
                        Err(web_err) => {
                            if api_err.kind == CheckErrorKind::RateLimited
                                || web_err.kind == CheckErrorKind::InvalidManifest
                            {
                                Err(web_err)
                            } else {
                                Err(api_err)
                            }
                        }
                    }
                }
            }
        };
        match fetched {
            Ok(None) => c.status = CheckStatus::NoRelease,
            Ok(Some((release, prefetched_manifest))) => {
                c.release_url = Some(release.html_url.clone());
                c.notes = Some(release.body.clone().unwrap_or_default());
                match release.version() {
                    Err(error) => {
                        c.status = CheckStatus::Error;
                        c.error_kind = Some(CheckErrorKind::InvalidRelease);
                        c.error = Some(error.to_string());
                    }
                    Ok(version) => {
                        c.version = Some(version.to_string());
                        match self
                            .inspect_release_checked(&release, prefetched_manifest.as_ref())
                            .await
                        {
                            Err(failure) => {
                                c.status = failure.status();
                                c.error_kind = Some(failure.kind);
                                c.error = Some(failure.detail);
                            }
                            // only the managed supervisor installs the prebuilt bundle
                            Ok(false) if self.managed => {
                                c.status = CheckStatus::UnsupportedPlatform;
                            }
                            Ok(_) => {
                                c.status = match semver::Version::parse(
                                    self.current_hub.trim_start_matches('v'),
                                ) {
                                    Ok(current)
                                        if version > current
                                            || (version == current
                                                && self.current_web.trim_start_matches('v')
                                                    != version.to_string()) =>
                                    {
                                        CheckStatus::Available
                                    }
                                    Ok(current) if version == current => CheckStatus::UpToDate,
                                    Ok(current) if version < current => CheckStatus::Ahead,
                                    _ => CheckStatus::UnknownCurrent,
                                };
                            }
                        }
                    }
                }
            }
            Err(failure) => {
                c.status = failure.status();
                c.error_kind = Some(failure.kind);
                c.error = Some(failure.detail);
            }
        }
        metrics::counter!("steve_update_check_total", "result" => c.status.as_str()).increment(1);
        atomic(
            &self.dir().join("check.json"),
            &CachedCheck {
                repository: self.repository.clone(),
                current_hub: self.current_hub.clone(),
                current_web: self.current_web.clone(),
                result: c.clone(),
            },
        )?;
        Ok(c)
    }
    pub fn request(
        &self,
        version: &str,
        key: &str,
        who: &str,
        approved: bool,
    ) -> Result<Operation> {
        if let Some(reason) = self.execution_reason() {
            bail!("{reason}")
        };
        let parsed = semver::Version::parse(version.trim_start_matches('v'))?;
        if !parsed.pre.is_empty() || !parsed.build.is_empty() {
            bail!("{}", steve_i18n::t!("hub.upgrade.stable_only"))
        }
        let version = parsed.to_string();
        if key.is_empty() || key.len() > 200 {
            bail!("idempotency_key required (1..200 bytes)")
        }
        let _guard = lock(&self.dir().join("journal.lock"))?;
        // Search durable history, not only the latest operation: retries after another update stay idempotent.
        for path in dir_entries(&self.dir().join("operations")) {
            if let Ok(op) = read::<Operation>(&path)
                && op.idempotency_key == key
            {
                if op.version != version || op.requested_by != who {
                    bail!("idempotency key conflict")
                };
                if !op.terminal() {
                    if self
                        .active()?
                        .is_some_and(|active| active.id != op.id && !active.terminal())
                    {
                        bail!("{}", steve_i18n::t!("hub.upgrade.orphan_conflict"))
                    }
                    atomic(&self.dir().join("active.json"), &json!({"id":op.id}))?;
                }
                return Ok(op);
            }
        }
        if let Some(mut active) = self.active()?
            && active.phase == "awaiting_approval"
            && (approved || now().saturating_sub(active.created_at) > APPROVAL_TTL_MS)
        {
            self.phase(
                &mut active,
                "failed",
                Some(
                    if approved {
                        "实例管理员发起了新的升级请求"
                    } else {
                        "升级审批请求已过期"
                    }
                    .into(),
                ),
            )?;
        }
        if self.active()?.is_some_and(|o| !o.terminal()) {
            bail!("{}", steve_i18n::t!("hub.upgrade.in_progress"))
        }
        let local = !self.managed && self.install(&self.repo).kind == "local_release";
        let runner = if local {
            Some(runner::prepare(self, &parsed)?)
        } else {
            None
        };
        let previous_version =
            (!self.current_hub.is_empty()).then(|| normalize_version(&self.current_hub));
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            idempotency_key: key.into(),
            version,
            phase: if approved {
                "staging"
            } else {
                "awaiting_approval"
            }
            .into(),
            created_at: now(),
            updated_at: now(),
            requested_by: who.into(),
            approved_by: approved.then(|| who.to_owned()),
            approved_at: approved.then(now),
            error: None,
            previous: None,
            previous_version,
            target: None,
            source: OperationSource::InApp,
            runner,
            backup_snapshot: None,
            progress: None,
            drain: None,
        };
        self.save(&op)?;
        atomic(&self.dir().join("active.json"), &json!({"id":op.id}))?;
        Ok(op)
    }
    pub fn approve(&self, id: &str, who: &str) -> Result<Operation> {
        if let Some(reason) = self.execution_reason() {
            bail!("{reason}")
        }
        let _guard = lock(&self.dir().join("journal.lock"))?;
        let mut op = self.operation(id)?;
        if self.active()?.as_ref().map(|active| active.id.as_str()) != Some(id) {
            bail!("{}", steve_i18n::t!("hub.upgrade.not_active"))
        }
        if op.phase == "awaiting_approval" {
            if now().saturating_sub(op.created_at) > APPROVAL_TTL_MS {
                self.phase(&mut op, "failed", Some("升级审批请求已过期".into()))?;
                bail!("{}", steve_i18n::t!("hub.upgrade.expired"))
            }

            op.approved_by = Some(who.into());
            op.approved_at = Some(now());
            self.phase(&mut op, "staging", None)?
        } else if op.phase != "staging" {
            bail!("{}", steve_i18n::t!("hub.upgrade.already_decided"))
        };
        Ok(op)
    }
    pub fn reject(&self, id: &str) -> Result<Operation> {
        let _guard = lock(&self.dir().join("journal.lock"))?;
        let mut op = self.operation(id)?;
        if op.phase != "awaiting_approval" {
            bail!("{}", steve_i18n::t!("hub.upgrade.reject_pending_only"))
        }
        self.phase(&mut op, "failed", Some("实例管理员拒绝了升级请求".into()))?;
        Ok(op)
    }
    /// Start an approved upgrade: a local build runs in its own launchd job (this Hub is the process the
    /// deploy replaces), a managed one is staged here for the supervisor.
    pub fn start(&self, op: Operation) {
        if op.runner.is_some() {
            let this = self.clone();
            tokio::spawn(async move {
                let mut op = op;
                if let Err(e) = runner::launch(&this, &mut op).await
                    && let Ok(mut now) = this.operation(&op.id)
                    && !now.terminal()
                {
                    let _ = this.phase(&mut now, "failed", Some(format!("{e:#}")));
                }
            });
        } else {
            self.spawn_stage(op)
        }
    }
    pub fn spawn_stage(&self, op: Operation) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut op = op;
            if let Err(e) = this.stage(&mut op).await {
                let _ = this.phase(&mut op, "failed", Some(format!("{e:#}")));
            }
        });
    }
    pub async fn agent(&self, session: &str, args: &Value) -> Result<String> {
        let out = match args["action"].as_str() {
            Some("check") => serde_json::to_value(self.check().await?)?,
            Some("status") => serde_json::to_value(self.status()?)?,
            Some("request") => serde_json::to_value(
                self.request(
                    args["version"].as_str().context("version required")?,
                    args["idempotency_key"]
                        .as_str()
                        .context("idempotency_key required")?,
                    &format!("agent:{session}"),
                    false,
                )?,
            )?,
            _ => bail!("action must be check, status or request"),
        };
        Ok(serde_json::to_string(&out)?)
    }
}

/// Held for the Hub process lifetime, including migration before any listener exists.
pub fn instance_lock(cfg: &Config) -> Result<File> {
    fs::create_dir_all(cfg.home.join("updates"))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cfg.home.join("updates/instance.lock"))?;
    file.try_lock()
        .context("another Hub still owns this installation")?;
    Ok(file)
}

/// A managed Hub is its process-group leader; remove research/flow descendants when its parent dies.
/// Never signal a shared foreground group in source/development execution.
pub fn exit_orphaned_managed_hub() -> ! {
    unsafe {
        let pid = libc::getpid();
        if libc::getpgrp() == pid {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    std::process::exit(70)
}
