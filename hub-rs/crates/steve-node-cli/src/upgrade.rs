//! Upgrades the Hub asks of a Node installed as a service (`steve node install`, a release bundle
//! under `<home>/releases`).
//!
//! 1. `stage` ([`NodeUpgrade`]): the archive comes from the Hub over this Node's own link
//!    (`blob.fetch`, granted to that one operation), its size and SHA256 are checked, it is
//!    unpacked (plain files and directories only) and what it holds must say the version, commit
//!    and target the Hub asked for, both `bin/steve-node` and `bin/steve --build-info` included.
//!    It is kept next to the current release as `<home>/releases/<version>`; nothing switches.
//! 2. `switch` ([`UpgradeSwitch`]): refused while the Node runs work, unless forced. A helper
//!    (`steve-node upgrade-switch <op>`, this binary, outside the service's process group and
//!    systemd unit) stops the service, points it at the new release (`steve node update`'s own
//!    path), starts it, and waits for the new Node to link to the Hub ([`note_linked`]). If it does
//!    not within the time the Hub gave, it goes back to the release it switched from.
//!
//! Every step is written to `<home>/updates/node/<op>.json` before it is taken; asking again
//! answers what is written, never runs a step twice.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use steve_node::{LocalNode, Upgrader};
use steve_node_api::*;

/// Where the upgrade records are, under the Node's home.
pub const DIR: &str = "updates/node";
/// The largest archive a Node takes.
const ARCHIVE_MAX: u64 = 512 * 1024 * 1024;
/// The most an archive may unpack to.
const UNPACKED_MAX: u64 = 2 * 1024 * 1024 * 1024;
const CHUNK: u32 = 256 * 1024;
/// How long a fetch of one chunk may keep failing on a lost link before staging gives up.
const FETCH_PATIENCE: Duration = Duration::from_secs(120);
/// A helper that never finished leaves its record behind: past this (on top of the health time),
/// the next start of the Node settles it from what it runs.
const STRANDED: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    state: NodeUpgradeState,
    request: NodeUpgrade,
    /// The staged release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release: Option<PathBuf>,
    /// The release the service ran when the switch began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_release: Option<PathBuf>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn err(code: ErrorCode, m: impl Into<String>) -> ApiError {
    ApiError::new(code, m)
}

/// This binary's release identity; `None` for a development build, which is never upgraded.
pub fn own_build() -> Option<BuildIdentity> {
    Some(BuildIdentity {
        version: option_env!("STEVE_BUILD_VERSION")?.into(),
        commit: option_env!("STEVE_BUILD_COMMIT")?.into(),
        target: env!("STEVE_BUILD_TARGET").into(),
    })
}

/// An upgrade's operation names its record file: letters, digits, `_` and `-` only.
fn op_name(op: &OperationId) -> ApiResult<&str> {
    let s = op.as_str();
    if s.is_empty()
        || s.len() > 96
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(err(ErrorCode::Invalid, "bad upgrade operation id"));
    }
    Ok(s)
}

fn record_path(home: &Path, op: &str) -> PathBuf {
    home.join(DIR).join(format!("{op}.json"))
}

fn load(home: &Path, op: &str) -> std::io::Result<Option<Record>> {
    match std::fs::read(record_path(home, op)) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn save(home: &Path, r: &Record) -> std::io::Result<()> {
    let dir = home.join(DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = record_path(home, r.state.operation.as_str());
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    std::fs::File::open(&dir)?.sync_all()
}

fn records(home: &Path) -> Vec<Record> {
    let Ok(dir) = std::fs::read_dir(home.join(DIR)) else {
        return vec![];
    };
    dir.filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}

fn set_phase(home: &Path, r: &mut Record, phase: &str, error: Option<String>) {
    r.state.phase = phase.into();
    r.state.error = error.map(|mut error| {
        let mut end = error.len().min(2048);
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        error
    });
    r.state.updated = Timestamp(now());
    if let Err(e) = save(home, r) {
        tracing::error!(
            "node upgrade {}: writing {phase}: {e}",
            r.state.operation.as_str()
        );
    }
}

/// The new Node linked to its Hub: a switch to this very build is through ([`run_switch`] then
/// calls it done). Called on every link; nothing to do almost always.
pub fn note_linked(home: &Path) {
    let Some(me) = own_build() else { return };
    for mut r in records(home) {
        if r.state.phase == "switching" && r.request.build == me {
            set_phase(home, &mut r, "linked", None);
        }
    }
}

/// A Node installed as a service from a release bundle, running from the release its service
/// record names, can be upgraded by its Hub.
pub struct ServiceUpgrader {
    home: PathBuf,
    build: BuildIdentity,
    release: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl ServiceUpgrader {
    /// `None` when this process is not the service's (run by hand, a development build, another
    /// release than the installed one).
    pub fn detect(home: &Path) -> Option<Arc<ServiceUpgrader>> {
        let build = own_build()?;
        let release = service_release(home)?;
        settle(home, &build);
        Some(Arc::new(ServiceUpgrader {
            home: home.to_owned(),
            build,
            release,
            lock: tokio::sync::Mutex::new(()),
        }))
    }
}

/// The release the Node's service record (`steve node install`) names, when this very process runs
/// from it: `None` for a Node run by hand, or from another release than the installed one.
pub(crate) fn service_release(home: &Path) -> Option<PathBuf> {
    // System launchd is an admin boundary. A Hub operation can neither elevate
    // nor complete a stop/start here, including during migration from a LaunchAgent.
    if cfg!(target_os = "macos") {
        return None;
    }
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("services/node.json")).ok()?).ok()?;
    if record["uninstalled"] == true {
        return None;
    }
    let release = PathBuf::from(record["release"].as_str()?);
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let release = release.canonicalize().ok()?;
    if exe != release.join("bin/steve-node").canonicalize().ok()? {
        return None;
    }
    Some(release)
}

/// Records a restart left unfinished: staging dies with the process; a switch whose helper is long
/// gone ends as what this process runs says.
fn settle(home: &Path, me: &BuildIdentity) {
    let t = now();
    for mut r in records(home) {
        match r.state.phase.as_str() {
            "staging" => set_phase(
                home,
                &mut r,
                "failed",
                Some("the node restarted while staging".into()),
            ),
            "switching" | "linked"
                if t - r.state.updated.0 > STRANDED + r.request.health_secs as i64 * 1000 =>
            {
                if r.request.build == *me {
                    set_phase(home, &mut r, "succeeded", None)
                } else {
                    set_phase(
                        home,
                        &mut r,
                        "failed",
                        Some("the switch helper stopped before it finished".into()),
                    )
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Upgrader for ServiceUpgrader {
    fn maintenance_pending(&self) -> bool {
        records(&self.home).iter().any(|r| !r.state.is_terminal())
    }
    fn build(&self) -> BuildIdentity {
        self.build.clone()
    }

    async fn stage(&self, node: &LocalNode, u: NodeUpgrade) -> ApiResult<NodeUpgradeState> {
        let op = op_name(&u.operation)?.to_string();
        let _g = self.lock.lock().await;
        if let Some(r) =
            load(&self.home, &op).map_err(|e| err(ErrorCode::Internal, e.to_string()))?
        {
            if r.request != u {
                return Err(err(
                    ErrorCode::Conflict,
                    "another upgrade under this operation",
                ));
            }
            return Ok(r.state);
        }
        if records(&self.home).iter().any(|r| !r.state.is_terminal()) {
            return Err(err(ErrorCode::Busy, "another upgrade is unfinished"));
        }
        check_request(&self.build, &u)?;
        let files = node
            .hub_files()
            .ok_or_else(|| err(ErrorCode::Offline, "not linked to the hub"))?;
        let mut r = Record {
            state: NodeUpgradeState {
                operation: u.operation.clone(),
                phase: "staging".into(),
                version: u.build.version.clone(),
                previous: Some(self.build.version.clone()),
                error: None,
                updated: Timestamp(now()),
            },
            request: u,
            release: None,
            previous_release: None,
        };
        save(&self.home, &r).map_err(|e| err(ErrorCode::Internal, e.to_string()))?;
        let state = r.state.clone();
        let home = self.home.clone();
        tokio::spawn(async move {
            match stage_release(&home, files.as_ref(), &r.request).await {
                Ok(release) => {
                    r.release = Some(release);
                    set_phase(&home, &mut r, "staged", None);
                }
                Err(e) => {
                    tracing::warn!("node upgrade {op}: {e:#}");
                    set_phase(&home, &mut r, "failed", Some(format!("{e:#}")));
                }
            }
        });
        Ok(state)
    }

    async fn switch(&self, node: &LocalNode, s: UpgradeSwitch) -> ApiResult<NodeUpgradeState> {
        let op = op_name(&s.operation)?.to_string();
        let _g = self.lock.lock().await;
        let mut r = load(&self.home, &op)
            .map_err(|e| err(ErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| err(ErrorCode::Invalid, "no such upgrade"))?;
        if r.state.phase != "staged" {
            return Ok(r.state);
        }
        let staged = r.release.clone().unwrap_or_default();
        if s.abandon {
            if staged.starts_with(self.home.join("releases")) && staged != self.release {
                let _ = std::fs::remove_dir_all(&staged);
            }
            set_phase(&self.home, &mut r, "abandoned", None);
            return Ok(r.state);
        }
        let running = node.running_work()?;
        steve_node_api::upgrade_bridge::refuse_downgrade(
            &self.build.version,
            &r.request.build.version,
        )
        .map_err(ApiError::from)?;
        if running > 0 && !s.force {
            return Err(err(
                ErrorCode::Busy,
                format!("the node still runs {running} turns or commands"),
            ));
        }
        r.previous_release = Some(self.release.clone());
        set_phase(&self.home, &mut r, "switching", None);
        if let Err(e) = spawn_helper(&self.home, &op) {
            set_phase(
                &self.home,
                &mut r,
                "failed",
                Some(format!("starting the switch: {e}")),
            );
        }
        Ok(r.state)
    }

    fn status(&self, operation: &OperationId) -> ApiResult<NodeUpgradeState> {
        let op = op_name(operation)?;
        load(&self.home, op)
            .map_err(|e| err(ErrorCode::Internal, e.to_string()))?
            .map(|r| r.state)
            .ok_or_else(|| err(ErrorCode::Invalid, "no such upgrade"))
    }
}

/// Only the Hub's build for this very machine, and not the one it runs already.
fn check_request(me: &BuildIdentity, u: &NodeUpgrade) -> ApiResult<()> {
    if u.build.target != me.target {
        return Err(err(
            ErrorCode::Invalid,
            format!(
                "a release for {}, this node is {}",
                u.build.target, me.target
            ),
        ));
    }
    if u.build == *me {
        return Err(err(
            ErrorCode::Invalid,
            "this node already runs that release",
        ));
    }
    if steve_node_api::upgrade_bridge::release_version(&u.build.version).is_err()
        || u.build.commit.len() != 40
        || !u.build.commit.bytes().all(|b| b.is_ascii_hexdigit())
        || u.sha256.len() != 64
        || !u.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || u.size == 0
        || u.size > ARCHIVE_MAX
    {
        return Err(err(ErrorCode::Invalid, "not a release this node takes"));
    }
    Ok(())
}

/// Fetch, check, unpack and verify; the staged release's directory.
async fn stage_release(
    home: &Path,
    files: &dyn HubFiles,
    u: &NodeUpgrade,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let releases = home.join("releases");
    std::fs::create_dir_all(&releases)?;
    let op = u.operation.as_str();
    let archive = releases.join(format!(".upgrade-{op}.tar.gz"));
    let staging = releases.join(format!(".upgrade-{op}"));
    let result = async {
        fetch(files, u, &archive).await?;
        let (a, s, b) = (archive.clone(), staging.clone(), u.build.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            unpack(&a, &s)?;
            verify(&s, &b)
        })
        .await
        .context("unpacking")??;
        let mut dest = releases.join(&u.build.version);
        if dest.exists() {
            let short: String = op
                .chars()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            dest = releases.join(format!("{}-{short}", u.build.version));
        }
        std::fs::rename(&staging, &dest)?;
        std::fs::File::open(&releases)?.sync_all()?;
        Ok(dest)
    }
    .await;
    let _ = std::fs::remove_file(&archive);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

async fn fetch(files: &dyn HubFiles, u: &NodeUpgrade, path: &Path) -> anyhow::Result<()> {
    let mut out = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let mut hash = Sha256::new();
    let mut offset = 0u64;
    let mut failing_since: Option<Instant> = None;
    loop {
        let chunk = match files
            .fetch(
                FileAccess::Operation {
                    operation: u.operation.clone(),
                },
                u.artifact.clone(),
                offset,
                CHUNK,
            )
            .await
        {
            Ok(c) => {
                failing_since = None;
                c
            }
            // the link came and went: the same chunk again, for a while
            Err(e) if matches!(e.code, ErrorCode::Offline | ErrorCode::Expired) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                anyhow::ensure!(since.elapsed() < FETCH_PATIENCE, "fetching: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => anyhow::bail!("fetching: {e}"),
        };
        anyhow::ensure!(
            chunk.offset == offset,
            "fetching: the hub answered another offset"
        );
        offset += chunk.bytes.len() as u64;
        anyhow::ensure!(offset <= u.size, "the archive is larger than offered");
        hash.update(&chunk.bytes);
        out.write_all(&chunk.bytes)?;
        if chunk.eof {
            break;
        }
        anyhow::ensure!(
            !chunk.bytes.is_empty(),
            "fetching: an empty chunk before the end"
        );
    }
    out.sync_all()?;
    anyhow::ensure!(
        offset == u.size,
        "the archive's size differs from the offer"
    );
    anyhow::ensure!(
        hex::encode(hash.finalize()) == u.sha256.to_ascii_lowercase(),
        "the archive's SHA256 differs from the offer"
    );
    Ok(())
}

/// Plain files and directories under `dest`, nothing outside it, no links, no devices.
fn unpack(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(dest)?;
    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
    let mut total = 0u64;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        anyhow::ensure!(
            path.components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
            "an archive entry outside the release: {}",
            path.display()
        );
        let target = dest.join(&path);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                total += entry.size();
                anyhow::ensure!(total <= UNPACKED_MAX, "the archive unpacks too large");
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let exec = entry.header().mode().unwrap_or(0) & 0o111 != 0;
                let mut out = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(if exec { 0o755 } else { 0o644 })
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&target)?;
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                out.write_all(&buf)?;
                out.sync_all()?;
            }
            // the archive's own metadata
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader => {}
            other => anyhow::bail!(
                "an archive entry that is not a plain file: {} ({other:?})",
                path.display()
            ),
        }
    }
    Ok(())
}

/// The release, Node and CLI report the offered build. Every package carries both binaries.
fn verify(dir: &Path, want: &BuildIdentity) -> anyhow::Result<()> {
    use anyhow::Context;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("release.json")).context("release.json")?)
            .context("release.json")?;
    anyhow::ensure!(
        manifest["format_version"] == 1
            && manifest["version"] == want.version.as_str()
            && manifest["commit"] == want.commit.as_str(),
        "release.json is not the offered release"
    );
    for name in ["bin/steve-node", "bin/steve"] {
        let bin = dir.join(name);
        let meta = std::fs::symlink_metadata(&bin).with_context(|| {
            format!("missing {name}; use a complete verified installer or Node upgrade package")
        })?;
        anyhow::ensure!(
            meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            "{name} is not an executable file"
        );
        let out = std::process::Command::new(&bin)
            .arg("--build-info")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {name} --build-info"))?;
        anyhow::ensure!(out.status.success(), "{name} --build-info failed");
        let info: serde_json::Value =
            serde_json::from_slice(&out.stdout).with_context(|| format!("{name} --build-info"))?;
        anyhow::ensure!(
            info["version"] == want.version.as_str()
                && info["commit"] == want.commit.as_str()
                && info["target"] == want.target.as_str(),
            "{name} is not the offered build"
        );
    }
    Ok(())
}

/// `steve-node upgrade-switch <op>`, detached from the service so stopping it does not stop the
/// helper.
fn spawn_helper(home: &Path, op: &str) -> std::io::Result<()> {
    spawn_detached(home, &["upgrade-switch", op], &format!("upgrade-{op}"))
}

/// This binary with `args`, detached from the service so stopping the service does not stop it:
/// its own session, and its own systemd scope (`steve-node-<name>`) under systemd. Its output goes
/// to `<home>/logs/node-<name>.log`.
pub(crate) fn spawn_detached(home: &Path, args: &[&str], name: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(logs.join(format!("node-{name}.log")))?;
    let systemd =
        std::env::var_os("INVOCATION_ID").is_some() && Path::new("/usr/bin/systemd-run").exists();
    let mut cmd = if systemd {
        let mut c = std::process::Command::new("/usr/bin/systemd-run");
        c.args(["--user", "--scope", "--collect", "--quiet"])
            .arg(format!("--unit=steve-node-{name}"))
            .arg(&exe);
        c
    } else {
        std::process::Command::new(&exe)
    };
    cmd.args(args)
        .env("STEVE_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid only, between fork and exec
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn().map(|_| ())
}

/// What the switch does to the service, as `steve node stop|update|start` do it.
pub trait ServiceOps {
    fn stop(&mut self) -> anyhow::Result<()>;
    fn update(&mut self, release: &Path) -> anyhow::Result<()>;
    fn start(&mut self) -> anyhow::Result<()>;
}

/// The Node's own user service (launchd or systemd).
pub struct InstalledService(pub(crate) steve_install::service::Service);

impl InstalledService {
    pub fn open(home: &Path) -> anyhow::Result<InstalledService> {
        Ok(InstalledService(steve_install::service::Service::new(
            "node", home, None,
        )?))
    }
}

impl ServiceOps for InstalledService {
    fn stop(&mut self) -> anyhow::Result<()> {
        self.0.dispatch("stop", None, 0).map(|_| ())
    }
    fn update(&mut self, release: &Path) -> anyhow::Result<()> {
        // the service manager may need a moment to let the stopped process go
        self.0.dispatch("update", Some(release), 60).map(|_| ())
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.0.dispatch("start", None, 0).map(|_| ())
    }
}

/// The helper's whole job, for the record `op` in `switching`: switch, wait for the new Node to
/// link, else go back. `poll`: how often it looks at the record meanwhile.
pub fn run_switch(
    home: &Path,
    op: &str,
    svc: &mut dyn ServiceOps,
    poll: Duration,
) -> anyhow::Result<NodeUpgradeState> {
    use anyhow::Context;
    let mut r = load(home, op)?.context("no such upgrade")?;
    anyhow::ensure!(
        r.state.phase == "switching",
        "the upgrade is {}, not switching",
        r.state.phase
    );
    let new = r.release.clone().context("nothing staged")?;
    let old = r
        .previous_release
        .clone()
        .context("no release to go back to")?;
    // let the node's answer to the switch reach the Hub before the node stops
    std::thread::sleep(poll.min(Duration::from_secs(1)));
    let Some(started) = start_staged_release(home, op, &mut r, svc, &new) else {
        return Ok(r.state);
    };
    let deadline = Instant::now() + Duration::from_secs(r.request.health_secs.max(1) as u64);
    let mut linked = false;
    if started.is_ok() {
        while Instant::now() < deadline {
            if load(home, op)?.is_some_and(|x| x.state.phase == "linked") {
                linked = true;
                break;
            }
            std::thread::sleep(poll);
        }
    }
    if linked {
        r = load(home, op)?.context("the record went away")?;
        set_phase(home, &mut r, "succeeded", None);
        tracing::info!("node upgrade {op}: the new release linked");
        return Ok(r.state);
    }
    let why = match started {
        Err(e) => format!("the new release did not start: {e:#}"),
        Ok(()) => format!(
            "the new release did not link to the hub within {} s",
            r.request.health_secs
        ),
    };
    rollback_release(home, op, &mut r, svc, &old, why);
    Ok(r.state)
}

/// Stop and update before starting the staged build; failed preparation resumes the old one.
fn start_staged_release(
    home: &Path,
    op: &str,
    r: &mut Record,
    svc: &mut dyn ServiceOps,
    new: &Path,
) -> Option<anyhow::Result<()>> {
    tracing::info!("node upgrade {op}: stopping the service");
    if let Err(e) = svc.stop() {
        // still the old release, maybe still running: make sure it runs
        let _ = svc.start();
        set_phase(home, r, "failed", Some(format!("stopping the node: {e:#}")));
        return None;
    }
    tracing::info!("node upgrade {op}: switching to {}", new.display());
    if let Err(e) = svc.update(new) {
        let _ = svc.start();
        set_phase(
            home,
            r,
            "failed",
            Some(format!("switching the release: {e:#}")),
        );
        return None;
    }
    Some(svc.start())
}

/// Restore the old release after failed health admission, preserving short-circuit order.
fn rollback_release(
    home: &Path,
    op: &str,
    r: &mut Record,
    svc: &mut dyn ServiceOps,
    old: &Path,
    why: String,
) {
    tracing::warn!("node upgrade {op}: {why}; going back to {}", old.display());
    let back = svc
        .stop()
        .and_then(|_| svc.update(old))
        .and_then(|_| svc.start());
    match back {
        Ok(()) => set_phase(home, r, "rolled_back", Some(why)),
        Err(e) => set_phase(
            home,
            r,
            "failed",
            Some(format!("{why}; going back failed too: {e:#}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(v: &str) -> BuildIdentity {
        BuildIdentity {
            version: v.into(),
            commit: "a".repeat(40),
            target: env!("STEVE_BUILD_TARGET").into(),
        }
    }

    fn request(op: &str, v: &str, bytes: &[u8]) -> NodeUpgrade {
        NodeUpgrade {
            operation: OperationId::new(op).unwrap(),
            build: build(v),
            artifact: ArtifactId::new("node-upgrade-1").unwrap(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
            health_secs: 1,
        }
    }

    /// A release archive whose `bin/steve-node --build-info` says `info`.
    fn archive(version: &str, info: &str, extra: Option<(&str, tar::EntryType)>) -> Vec<u8> {
        let cli = format!("#!/bin/sh\nprintf '%s\\n' '{info}'\n");
        package(version, info, Some((&cli, 0o755)), false, extra)
    }

    fn package(
        version: &str,
        info: &str,
        cli: Option<(&str, u32)>,
        promised: bool,
        extra: Option<(&str, tar::EntryType)>,
    ) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut add = |name: &str, data: &[u8], mode: u32| {
            let mut h = tar::Header::new_ustar();
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_entry_type(tar::EntryType::Regular);
            b.append_data(&mut h, name, data).unwrap();
        };
        let mut manifest =
            serde_json::json!({"format_version":1,"version":version,"commit":"a".repeat(40)});
        if promised {
            manifest["cli_protocol"] = 2.into();
        }
        add("release.json", manifest.to_string().as_bytes(), 0o644);
        add(
            "bin/steve-node",
            format!("#!/bin/sh\necho '{info}'\n").as_bytes(),
            0o755,
        );
        if let Some((script, mode)) = cli {
            add("bin/steve", script.as_bytes(), mode);
        }
        if let Some((name, kind)) = extra {
            let mut h = tar::Header::new_ustar();
            h.set_size(0);
            h.set_entry_type(kind);
            h.set_link_name("/etc/passwd").unwrap();
            h.set_mode(0o644);
            b.append_data(&mut h, name, &[][..]).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    fn info(v: &str) -> String {
        serde_json::json!({"version":v,"commit":"a".repeat(40),"target":env!("STEVE_BUILD_TARGET")})
            .to_string()
    }

    /// The Hub's side of `blob.fetch` for one granted operation.
    struct Files {
        op: OperationId,
        bytes: Vec<u8>,
    }

    #[async_trait]
    impl HubFiles for Files {
        async fn fetch(
            &self,
            access: FileAccess,
            _id: ArtifactId,
            offset: u64,
            max: u32,
        ) -> ApiResult<ReadChunk> {
            match access {
                FileAccess::Operation { operation } if operation == self.op => {}
                _ => return Err(err(ErrorCode::Denied, "not granted")),
            }
            let end = (offset as usize + max.min(1000) as usize).min(self.bytes.len());
            Ok(ReadChunk {
                offset,
                eof: end == self.bytes.len(),
                bytes: self.bytes[offset as usize..end].to_vec(),
            })
        }
        async fn begin_upload(&self, _: BeginUpload) -> ApiResult<Transfer> {
            unimplemented!()
        }
        async fn upload(&self, _: BlobChunk) -> ApiResult<Transfer> {
            unimplemented!()
        }
        async fn upload_status(&self, _: TransferId) -> ApiResult<TransferState> {
            unimplemented!()
        }
        async fn finish_upload(&self, _: TransferId) -> ApiResult<ArtifactRef> {
            unimplemented!()
        }
        async fn abort_upload(&self, _: TransferId) -> ApiResult<TransferState> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn a_checked_release_is_staged_beside_the_current_one() {
        let home = tempfile::tempdir().unwrap();
        let bytes = archive("1.2.3", &info("1.2.3"), None);
        let u = request("nup_1", "1.2.3", &bytes);
        let files = Files {
            op: u.operation.clone(),
            bytes,
        };
        let dir = stage_release(home.path(), &files, &u).await.unwrap();
        assert_eq!(dir, home.path().join("releases/1.2.3"));
        assert!(dir.join("bin/steve-node").is_file());
        assert!(dir.join("bin/steve").is_file());
        // nothing left behind
        let left: Vec<_> = std::fs::read_dir(home.path().join("releases"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("1.2.3")]);
    }

    #[tokio::test]
    async fn a_cli_is_staged_with_the_node_and_must_report_the_same_build() {
        let home = tempfile::tempdir().unwrap();
        let cli = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", info("1.2.3"));
        let bytes = package("1.2.3", &info("1.2.3"), Some((&cli, 0o755)), true, None);
        let u = request("nup_cli", "1.2.3", &bytes);
        let files = Files {
            op: u.operation.clone(),
            bytes,
        };
        let dir = stage_release(home.path(), &files, &u).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("bin/steve")).unwrap(), cli);
        assert!(dir.join("bin/steve-node").is_file());
        // A retry cannot overwrite an existing release, including its CLI.
        let again = stage_release(home.path(), &files, &u).await.unwrap();
        assert_ne!(again, dir);
        assert_eq!(
            std::fs::read(dir.join("bin/steve")).unwrap(),
            std::fs::read(again.join("bin/steve")).unwrap()
        );
    }

    #[tokio::test]
    async fn a_promised_or_present_cli_cannot_be_missing_corrupt_or_from_another_release() {
        let good = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", info("1.2.3"));
        let other = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", info("9.9.9"));
        let home = tempfile::tempdir().unwrap();
        for (index, (cli, promised, why)) in [
            (None, true, "missing bin/steve"),
            (
                Some((other.as_str(), 0o755)),
                true,
                "bin/steve is not the offered build",
            ),
            (
                Some((other.as_str(), 0o755)),
                false,
                "bin/steve is not the offered build",
            ),
            (
                Some((good.as_str(), 0o644)),
                true,
                "bin/steve is not an executable",
            ),
            (
                Some(("#!/bin/sh\nexit 9\n", 0o755)),
                true,
                "bin/steve --build-info failed",
            ),
            (
                Some(("#!/bin/sh\nprintf broken\n", 0o755)),
                true,
                "bin/steve --build-info",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = package("1.2.3", &info("1.2.3"), cli, promised, None);
            let u = request(&format!("nup_bad_cli_{index}"), "1.2.3", &bytes);
            let files = Files {
                op: u.operation.clone(),
                bytes,
            };
            let error = stage_release(home.path(), &files, &u).await.unwrap_err();
            assert!(format!("{error:#}").contains(why), "{why}: {error:#}");
            assert_eq!(
                std::fs::read_dir(home.path().join("releases"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[tokio::test]
    async fn a_wrong_hash_size_build_or_entry_is_refused_and_cleaned_up() {
        let home = tempfile::tempdir().unwrap();
        let good = archive("1.2.3", &info("1.2.3"), None);
        let cases: Vec<(NodeUpgrade, Vec<u8>, &str)> = vec![
            {
                let mut u = request("nup_hash", "1.2.3", &good);
                u.sha256 = "0".repeat(64);
                (u, good.clone(), "SHA256")
            },
            {
                let mut u = request("nup_size", "1.2.3", &good);
                u.size += 1;
                (u, good.clone(), "size")
            },
            {
                // the binary says another build than the manifest and the offer
                let bytes = archive("1.2.3", &info("9.9.9"), None);
                (
                    request("nup_bin", "1.2.3", &bytes),
                    bytes,
                    "not the offered build",
                )
            },
            {
                let bytes = archive(
                    "1.2.3",
                    &info("1.2.3"),
                    Some(("bin/x", tar::EntryType::Symlink)),
                );
                (
                    request("nup_link", "1.2.3", &bytes),
                    bytes,
                    "not a plain file",
                )
            },
        ];
        for (u, bytes, why) in cases {
            let files = Files {
                op: u.operation.clone(),
                bytes,
            };
            let e = stage_release(home.path(), &files, &u).await.unwrap_err();
            assert!(format!("{e:#}").contains(why), "{why}: {e:#}");
        }
        assert_eq!(
            std::fs::read_dir(home.path().join("releases"))
                .unwrap()
                .count(),
            0,
            "a refused archive leaves nothing"
        );
    }

    #[tokio::test]
    async fn another_operations_grant_is_not_this_ones() {
        let home = tempfile::tempdir().unwrap();
        let bytes = archive("1.2.3", &info("1.2.3"), None);
        let u = request("nup_1", "1.2.3", &bytes);
        let files = Files {
            op: OperationId::new("nup_other").unwrap(),
            bytes,
        };
        let e = stage_release(home.path(), &files, &u).await.unwrap_err();
        assert!(format!("{e:#}").contains("not granted"), "{e:#}");
    }

    #[test]
    fn only_this_targets_other_release_is_taken() {
        let me = build("1.0.0");
        let u = request("nup_1", "1.0.0", b"x");
        assert!(check_request(&me, &u).is_err(), "the same build");
        let mut other = request("nup_1", "1.1.0", b"x");
        assert!(check_request(&me, &other).is_ok());
        other.build.target = "riscv64-unknown-none".into();
        assert!(check_request(&me, &other).is_err(), "another platform");
        let mut v = request("nup_1", "1.1", b"x");
        v.build.version = "1.1".into();
        assert!(check_request(&me, &v).is_err(), "not a release version");
        assert!(op_name(&OperationId::new("..").unwrap()).is_err());
        assert!(op_name(&OperationId::new("a:b").unwrap()).is_err());
    }

    #[test]
    fn persisted_upgrade_errors_fit_status_frames_without_splitting_utf8() {
        use steve_node_api::upgrade_v1 as v1;
        let home = tempfile::tempdir().unwrap();
        let request = request("nup_error", "1.2.3", b"unused");
        let mut record = Record {
            state: NodeUpgradeState {
                operation: request.operation.clone(),
                phase: "staging".into(),
                version: "1.2.3".into(),
                previous: Some("1.1.0".into()),
                error: None,
                updated: Timestamp(1),
            },
            request,
            release: None,
            previous_release: None,
        };
        for detail in [
            format!("{}中文", "x".repeat(2047)),
            "错误🙂".repeat(v1::MAX_FRAME),
        ] {
            set_phase(home.path(), &mut record, "failed", Some(detail.clone()));
            let saved = load(home.path(), "nup_error").unwrap().unwrap();
            let clipped = saved.state.error.as_ref().unwrap();
            assert!(clipped.len() <= 2048);
            assert!(detail.starts_with(clipped));
            assert_eq!(saved.state.error, record.state.error);
            let state = v1::StatusResponse::try_from(saved.state).unwrap();
            let response = v1::Frame {
                v: 1,
                t: v1::Kind::Res,
                id: "status".into(),
                re: Some("request".into()),
                m: String::new(),
                generation: 7,
                p: serde_json::to_value(v1::Outcome::Ok(serde_json::to_value(state).unwrap()))
                    .unwrap(),
            };
            assert!(response.encode().unwrap().len() <= v1::MAX_FRAME);
        }
    }

    #[tokio::test]
    async fn switching_a_staged_older_release_is_refused_before_service_or_record_changes() {
        let home = tempfile::tempdir().unwrap();
        let request = request("nup_older", "1.9.0", b"unused");
        let record = Record {
            state: NodeUpgradeState {
                operation: request.operation.clone(),
                phase: "staged".into(),
                version: "1.9.0".into(),
                previous: Some("1.10.0".into()),
                error: None,
                updated: Timestamp(1),
            },
            request,
            release: Some(home.path().join("releases/older")),
            previous_release: None,
        };
        save(home.path(), &record).unwrap();
        let upgrader = ServiceUpgrader {
            home: home.path().into(),
            build: build("1.10.0"),
            release: home.path().join("releases/current"),
            lock: tokio::sync::Mutex::new(()),
        };
        let root = home.path();
        let node = LocalNode::open(steve_node::NodeConfig {
            node: NodeId::new("fixture").unwrap(),
            home: root.join("journal"),
            roots: vec![],
            capacity: Capacity {
                sessions: 0,
                jobs: 0,
                execs: 0,
            },
            storage: Default::default(),
            ceiling: Default::default(),
            acp: steve_acp::AcpConfig {
                adapters: Default::default(),
                declared: Default::default(),
                node: "/usr/bin/false".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.into(),
                version: "test".into(),
            },
        })
        .unwrap();
        for force in [false, true] {
            let error = upgrader
                .switch(
                    &node,
                    UpgradeSwitch {
                        operation: record.request.operation.clone(),
                        abandon: false,
                        force,
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Downgrade);
            assert!(error.safe_detail.contains("refusing downgrade"));
            let after = load(root, "nup_older").unwrap().unwrap();
            assert_eq!(after.state.phase, "staged");
            assert!(after.previous_release.is_none());
            assert_eq!(after.state.updated, Timestamp(1));
        }
        node.shutdown().await;
    }

    /// The service as a test drives it: `link` says whether the release started links.
    struct Fake {
        home: PathBuf,
        op: String,
        calls: Vec<String>,
        links: bool,
        current: PathBuf,
    }

    impl ServiceOps for Fake {
        fn stop(&mut self) -> anyhow::Result<()> {
            self.calls.push("stop".into());
            Ok(())
        }
        fn update(&mut self, release: &Path) -> anyhow::Result<()> {
            self.calls.push(format!("update {}", release.display()));
            self.current = release.to_owned();
            Ok(())
        }
        fn start(&mut self) -> anyhow::Result<()> {
            self.calls.push("start".into());
            // the started release links (as `note_linked` would mark it)
            if self.links && self.current.ends_with("new") {
                let mut r = load(&self.home, &self.op).unwrap().unwrap();
                set_phase(&self.home, &mut r, "linked", None);
            }
            Ok(())
        }
    }

    fn switching(home: &Path, op: &str) {
        let u = request(op, "1.2.3", b"x");
        let r = Record {
            state: NodeUpgradeState {
                operation: u.operation.clone(),
                phase: "switching".into(),
                version: "1.2.3".into(),
                previous: Some("1.0.0".into()),
                error: None,
                updated: Timestamp(now()),
            },
            request: u,
            release: Some(home.join("releases/new")),
            previous_release: Some(home.join("releases/old")),
        };
        save(home, &r).unwrap();
    }

    #[test]
    fn a_switch_that_links_succeeds() {
        let home = tempfile::tempdir().unwrap();
        switching(home.path(), "nup_ok");
        let mut svc = Fake {
            home: home.path().into(),
            op: "nup_ok".into(),
            calls: vec![],
            links: true,
            current: home.path().join("releases/old"),
        };
        let s = run_switch(home.path(), "nup_ok", &mut svc, Duration::from_millis(10)).unwrap();
        assert_eq!(s.phase, "succeeded");
        let new = home.path().join("releases/new");
        assert_eq!(
            svc.calls,
            vec![
                "stop".into(),
                format!("update {}", new.display()),
                "start".into()
            ]
        );
        // done once: asked again, the helper changes nothing
        assert!(run_switch(home.path(), "nup_ok", &mut svc, Duration::from_millis(10)).is_err());
    }

    #[test]
    fn a_switch_that_never_links_goes_back_to_the_previous_release() {
        let home = tempfile::tempdir().unwrap();
        switching(home.path(), "nup_back");
        let mut svc = Fake {
            home: home.path().into(),
            op: "nup_back".into(),
            calls: vec![],
            links: false,
            current: home.path().join("releases/old"),
        };
        let s = run_switch(home.path(), "nup_back", &mut svc, Duration::from_millis(10)).unwrap();
        assert_eq!(s.phase, "rolled_back");
        assert!(s.error.unwrap().contains("did not link"));
        assert_eq!(svc.current, home.path().join("releases/old"));
        assert_eq!(svc.calls.last().unwrap(), "start");
    }

    #[test]
    fn a_restart_settles_what_it_left_unfinished() {
        let home = tempfile::tempdir().unwrap();
        let me = build("1.2.3");
        switching(home.path(), "nup_old");
        let mut r = load(home.path(), "nup_old").unwrap().unwrap();
        r.state.updated = Timestamp(now() - STRANDED - 60_000);
        save(home.path(), &r).unwrap();
        let mut staging = r.clone();
        staging.state.operation = OperationId::new("nup_staging").unwrap();
        staging.request.operation = staging.state.operation.clone();
        staging.state.phase = "staging".into();
        save(home.path(), &staging).unwrap();
        settle(home.path(), &me);
        assert_eq!(
            load(home.path(), "nup_old").unwrap().unwrap().state.phase,
            "succeeded"
        );
        assert_eq!(
            load(home.path(), "nup_staging")
                .unwrap()
                .unwrap()
                .state
                .phase,
            "failed"
        );
    }
}
