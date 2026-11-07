//! Instance updates. The journal is deliberately outside SQLite: rollback must not erase its receipt.
mod client;
pub mod install;
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub status: String,
    pub checked_at: u64,
    pub version: Option<String>,
    pub release_url: Option<String>,
    pub notes: Option<String>,
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
        if self.result.status == "error"
            && !self
                .result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("rate limited")
        {
            60_000
        } else {
            600_000
        }
    }
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
    pub target: Option<String>,
    /// A local_release upgrade: what the one-shot runner job builds and deploys (`runner.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<runner::Spec>,
    /// The verified database copy a local deploy took before switching (`ops/local-release.py`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}
impl Operation {
    pub fn terminal(&self) -> bool {
        matches!(self.phase.as_str(), "succeeded" | "failed" | "rolled_back")
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
    /// STEVE_REPO: the checkout a local_release upgrade builds from.
    pub repo: PathBuf,
    /// The installation kind, when not detected from where this process runs (tests).
    pub kind: Option<&'static str>,
    pub launch: runner::Launch,
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
fn repository() -> Result<String> {
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
/// page's update notice shows (`/api/state`). Reads the cached check only, never the network.
pub fn available(cfg: &Config) -> Option<String> {
    let cached: CachedCheck = read(&cfg.home.join("updates/check.json")).ok()?;
    (cached.repository == repository().ok()?
        && cached.current_hub == cfg.version
        && cached.current_web == crate::web::web_version(cfg)
        && cached.result.status == "available")
        .then_some(cached.result.version)
        .flatten()
}

/// Checks for a newer release shortly after start and then every six hours, so the page can say so
/// without anyone opening Settings. Off in debug builds unless STEVE_UPDATE_CHECK=1; STEVE_UPDATE_CHECK=0
/// turns it off. Uses the same cache and lock as a check from the page.
pub fn spawn_auto_check(cfg: Config) {
    let on = match std::env::var("STEVE_UPDATE_CHECK").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => !cfg!(debug_assertions),
    };
    if !on {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            match Updates::new(&cfg) {
                Ok(u) => match u.check().await {
                    Ok(c) => tracing::info!("update check: {} {:?}", c.status, c.version),
                    Err(e) => tracing::info!("update check: {e:#}"),
                },
                Err(e) => tracing::warn!("update check: {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
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
        self.save(op)
    }
    pub fn status(&self) -> Result<Value> {
        let mut operation = self.active()?;
        if let Some(op) = operation.as_mut() {
            runner::reap(self, op)?;
        }
        let backup = operation.as_ref().map(|op| self.backup(op));
        let kind = self.install(&self.repo).kind;
        let ready = if kind == "local_release" {
            "local_release runner"
        } else {
            "managed supervisor"
        };
        Ok(
            json!({"repository":self.repository,"current":{"hub":self.current_hub,"web":self.current_web},"capability":{"managed":self.managed,"reason":self.execution_reason().unwrap_or_else(||ready.into())},"install":{"kind":kind},"check":self.cached_check().map(|cached|cached.result),"operation":operation,"backup":backup}),
        )
    }
    /// Where what a failed upgrade is recovered from is kept: the verified database copy taken before
    /// the switch (`null` until it exists) and the directory beside it.
    fn backup(&self, op: &Operation) -> Value {
        if op.runner.is_some() {
            // ops/local-release.py backs up into $STEVE_HOME/backups and names the copy in the journal
            return json!({"database": op.backup, "dir": self.home.join("backups").to_string_lossy()});
        }
        let database = self.dir().join(format!("{}.sqlite3", op.id));
        json!({"database": database.exists().then(|| database.to_string_lossy().into_owned()), "dir": self.dir().to_string_lossy()})
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
            status: "error".into(),
            checked_at: now(),
            version: None,
            release_url: None,
            notes: None,
            error: None,
        };
        // the unauthenticated API allows 60 calls an hour per address: on any failure ask github.com
        // which tag `latest` is, which isn't counted
        let latest = match self.latest().await {
            Err(error) => self.latest_web().await.map_err(|_| error),
            found => found,
        };
        match latest {
            Ok(None) => c.status = "no_release".into(),
            Ok(Some(release)) => {
                c.release_url = Some(release.html_url.clone());
                c.notes = Some(release.body.clone().unwrap_or_default());
                match release.version() {
                    Err(error) => c.error = Some(error.to_string()),
                    Ok(version) => {
                        c.version = Some(version.to_string());
                        match self.inspect_release(&release).await {
                            Err(error) => c.error = Some(error.to_string()),
                            // only the managed supervisor installs the prebuilt bundle
                            Ok(false) if self.managed => c.status = "unsupported_platform".into(),
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
                                        "available"
                                    }
                                    Ok(current) if version == current => "up_to_date",
                                    Ok(current) if version < current => "ahead",
                                    _ => "unknown_current",
                                }
                                .into()
                            }
                        }
                    }
                }
            }
            Err(error) => c.error = Some(error.to_string()),
        }
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
        if let Ok(entries) = fs::read_dir(self.dir().join("operations")) {
            for entry in entries.flatten() {
                if let Ok(op) = read::<Operation>(&entry.path())
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
        }
        if let Some(mut active) = self.active()?
            && active.phase == "awaiting_approval"
            && (approved || now().saturating_sub(active.created_at) > 86_400_000)
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
            target: None,
            runner,
            backup: None,
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
            if now().saturating_sub(op.created_at) > 86_400_000 {
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
            Some("status") => self.status()?,
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
