//! Upgrades the Hub asks of a Node installed as a service (`steve node install`, a release bundle
//! under `<home>/releases`).
//!
//! 1. `stage` ([`NodeUpgrade`]): the archive comes from the Hub over this Node's own link
//!    (`blob.fetch`, granted to that one operation), its size and SHA256 are checked, it is
//!    unpacked (plain files and directories only) and what it holds must say the version, commit
//!    and target the Hub asked for, both `bin/steve-node` and `bin/steve --build-info` included.
//!    Legacy Node-only archives remain supported when they do not promise a CLI. It is kept next to
//!    the current release as `<home>/releases/<version>`; nothing switches.
//! 2. `switch` ([`UpgradeSwitch`]): refused while the Node runs work, unless forced. A helper
//!    (`steve-node upgrade-switch <op>`, this binary, outside the service's process group and
//!    systemd unit) stops the service, points it at the new release (`steve node update`'s own
//!    path), starts it, and waits for `linked` ([`note_linked`] or a pre-migration claim).
//!    A migrating Node waits for the helper's receipt before opening its journal; the helper's
//!    success does not prove Hub connectivity. Without a claim the legacy helper rolls back.
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

/// Wait before opening an irreversible journal. `linked` tells an old helper to commit;
/// only its receipt, or verified absence AND the actual service release, permits migration.
/// No elapsed-time inference is safe: a paused helper can still roll back when resumed.
pub async fn claim_switch_before_journal_upgrade(home: &Path) -> anyhow::Result<()> {
    let Some(me) = own_build() else {
        // A dev build cannot identify a related operation. Only a verified empty record set
        // permits migration; uncertainty waits for a real stop just as a release build does.
        loop {
            if all_switch_records(home).is_ok_and(|rows| rows.is_empty()) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    wait_for_switch(
        home,
        &me,
        |r| helper_process::absent(home, r),
        || Ok(service_release(home).is_some()),
    )
    .await
}

async fn wait_for_switch(
    home: &Path,
    me: &BuildIdentity,
    mut absent: impl FnMut(&Record) -> anyhow::Result<bool>,
    mut current: impl FnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let mut reported = false;
    loop {
        // Read/query failures remain a wait, never an empty record set or missing process.
        if switch_permits_upgrade(home, me, &mut absent, &mut current).unwrap_or(false) {
            return Ok(());
        }
        if !reported {
            tracing::warn!("waiting for a verified Node upgrade handoff; journal remains unopened");
            reported = true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn all_switch_records(home: &Path) -> anyhow::Result<Vec<Record>> {
    let dir = match std::fs::read_dir(home.join(DIR)) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut related = vec![];
    for entry in dir {
        let path = entry?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let r: Record = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            r.state.operation == r.request.operation,
            "conflicting upgrade operation"
        );
        anyhow::ensure!(
            path == record_path(home, op_name(&r.state.operation)?),
            "conflicting upgrade filename"
        );
        anyhow::ensure!(
            r.state.version == r.request.build.version,
            "conflicting upgrade build"
        );
        anyhow::ensure!(
            matches!(
                r.state.phase.as_str(),
                "staging"
                    | "staged"
                    | "switching"
                    | "linked"
                    | "succeeded"
                    | "rolled_back"
                    | "failed"
                    | "abandoned"
            ),
            "unknown upgrade phase"
        );
        related.push(r);
    }
    anyhow::ensure!(
        related.iter().filter(|r| !r.state.is_terminal()).count() <= 1,
        "conflicting unfinished upgrades in this installation"
    );
    Ok(related)
}

fn switch_permits_upgrade(
    home: &Path,
    me: &BuildIdentity,
    absent: &mut impl FnMut(&Record) -> anyhow::Result<bool>,
    current: &mut impl FnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let rows = all_switch_records(home)?;
    // A manually installed repair has a different build. It may proceed only after any old
    // operation that can still roll this home back is proven gone AND this service is current.
    for other in rows.iter().filter(|r| {
        r.request.build != *me && matches!(r.state.phase.as_str(), "switching" | "linked")
    }) {
        if !current()? || !absent(other)? {
            return Ok(false);
        }
    }
    let related: Vec<_> = rows
        .into_iter()
        .filter(|r| r.request.build == *me)
        .collect();
    if related.is_empty() {
        return Ok(true);
    }
    let active: Vec<_> = related.iter().filter(|r| !r.state.is_terminal()).collect();
    anyhow::ensure!(
        active.len() <= 1,
        "conflicting unfinished upgrades for this build"
    );
    // A terminal record does not override a changed service definition or an active operation.
    let Some(r) = active.first() else {
        return Ok(current()?
            && related.iter().all(|r| {
                matches!(
                    r.state.phase.as_str(),
                    "succeeded" | "rolled_back" | "failed"
                )
            }));
    };
    let mut r = (*r).clone();
    if r.state.phase == "switching" {
        r.state.phase = "linked".into();
        r.state.error = None;
        r.state.updated = Timestamp(now());
        save(home, &r)?;
    }
    if !current()? || r.state.phase != "linked" || !absent(&r)? {
        return Ok(false);
    }
    // Observe the same operation AND service again after the process scan. Manual update with
    // a residual switching record must pass this same conjunction, never the automatic-path guess.
    let after: Vec<_> = all_switch_records(home)?
        .into_iter()
        .filter(|r| r.request.build == *me)
        .collect();
    Ok(after
        .iter()
        .any(|x| x.request == r.request && x.state.phase == "linked")
        && after.iter().filter(|x| !x.state.is_terminal()).count() == 1
        && current()?)
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

/// The release, Node and CLI report the offered build. Unmarked legacy Node-only packages can
/// still be staged; `cli_protocol` promises a CLI and may never silently degrade to Node-only.
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
    let mut binaries = vec!["bin/steve-node"];
    if manifest.get("cli_protocol").is_some()
        || steve_install::common::exists(&dir.join("bin/steve"))
    {
        binaries.push("bin/steve");
    }
    for name in binaries {
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
    tracing::info!("node upgrade {op}: stopping the service");
    if let Err(e) = svc.stop() {
        // still the old release, maybe still running: make sure it runs
        let _ = svc.start();
        set_phase(
            home,
            &mut r,
            "failed",
            Some(format!("stopping the node: {e:#}")),
        );
        return Ok(r.state);
    }
    tracing::info!("node upgrade {op}: switching to {}", new.display());
    if let Err(e) = svc.update(&new) {
        let _ = svc.start();
        set_phase(
            home,
            &mut r,
            "failed",
            Some(format!("switching the release: {e:#}")),
        );
        return Ok(r.state);
    }
    let started = svc.start();
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
    tracing::warn!("node upgrade {op}: {why}; going back to {}", old.display());
    let back = svc
        .stop()
        .and_then(|_| svc.update(&old))
        .and_then(|_| svc.start());
    match back {
        Ok(()) => set_phase(home, &mut r, "rolled_back", Some(why)),
        Err(e) => set_phase(
            home,
            &mut r,
            "failed",
            Some(format!("{why}; going back failed too: {e:#}")),
        ),
    }
    Ok(r.state)
}

/// Native, complete process inspection. Errors deliberately cannot prove absence. Only argv
/// structure and the installation home are retained; complete argv/environments are never logged.
mod helper_process {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    fn invalid() -> std::io::Error {
        std::io::Error::other("cannot establish upgrade helper identity")
    }

    pub(super) fn absent(home: &Path, record: &Record) -> anyhow::Result<bool> {
        let gone = scan(home, record, &mut inspect)?;
        // Linux absence is not yet validated against released helpers/systemd scopes.
        // Fail closed there; a real succeeded receipt still permits the normal upgrade.
        Ok(gone && !cfg!(target_os = "linux"))
    }

    fn scan(
        home: &Path,
        record: &Record,
        inspect: &mut impl FnMut(libc::pid_t, &Path) -> std::io::Result<Option<Process>>,
    ) -> anyhow::Result<bool> {
        let home = home.canonicalize()?;
        let old = record
            .previous_release
            .as_ref()
            .ok_or_else(invalid)?
            .join("bin/steve-node")
            .canonicalize()?;
        let op = op_name(&record.state.operation)?;
        let snapshot: std::collections::BTreeSet<_> = pids()?.into_iter().collect();
        for &pid in &snapshot {
            let inspected = inspect(pid, &old);
            let process = match inspected {
                Ok(process) => process,
                // ESRCH alone proves this enumerated process is gone, not an inspection error.
                Err(_)
                    if unsafe { libc::kill(pid, 0) } == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if let Some((exe, args, env)) = process
                && relevant(&exe, &args, &old, op)?
            {
                let homes: Vec<_> = env
                    .split(|b| *b == 0)
                    .filter_map(|e| e.strip_prefix(b"STEVE_HOME="))
                    .collect();
                anyhow::ensure!(
                    homes.len() == 1,
                    "cannot establish helper installation home"
                );
                let found = Path::new(OsStr::from_bytes(homes[0])).canonicalize()?;
                if found == home {
                    return Ok(false);
                }
            }
        }
        // A scope wrapper may have forked the helper during inspection. Recheck enumeration
        // before claiming absence; an unseen new process makes this scan inconclusive.
        if pids()?.iter().any(|pid| !snapshot.contains(pid)) {
            return Err(invalid().into());
        }
        Ok(true)
    }

    fn relevant(exe: &Path, args: &[OsString], old: &Path, op: &str) -> std::io::Result<bool> {
        let invoked = Path::new(args.first().ok_or_else(invalid)?);
        if invoked.as_os_str().is_empty()
            || (invoked.file_name() != exe.file_name() && invoked.canonicalize()? != exe)
        {
            return Err(invalid());
        }
        // A same-operation helper at another Node path is not absence: record/path
        // conflicts must still keep migration blocked. A canonical renamed binary also counts.
        if exe == old || exe.file_name() == Some(OsStr::new("steve-node")) {
            let command = args.get(1).ok_or_else(invalid)?;
            if command != "upgrade-switch" {
                return Ok(false);
            }
            let operation = args.get(2).ok_or_else(invalid)?;
            if operation != op {
                return Ok(false);
            }
            if args.len() != 3 {
                return Err(invalid());
            }
            return Ok(true);
        }
        if exe == Path::new("/usr/bin/systemd-run") {
            let unit = format!("--unit=steve-node-upgrade-{op}");
            if !args.iter().any(|a| a.as_os_str() == OsStr::new(&unit)) {
                return Ok(false);
            }
            let expected: Vec<OsString> = ["--user", "--scope", "--collect", "--quiet"]
                .into_iter()
                .map(Into::into)
                .chain([
                    unit.into(),
                    old.as_os_str().into(),
                    "upgrade-switch".into(),
                    op.into(),
                ])
                .collect();
            if args.get(1..) != Some(expected.as_slice()) {
                return Err(invalid());
            }
            return Ok(true); // wrapper not yet exec'd: it can still create the helper
        }
        Ok(false)
    }

    type Process = (PathBuf, Vec<OsString>, Vec<u8>);

    fn candidate(exe: &Path, old: &Path) -> bool {
        exe == old
            || matches!(
                exe.file_name().and_then(OsStr::to_str),
                Some("steve-node" | "systemd-run")
            )
    }

    #[cfg(any(target_os = "linux", test))]
    fn nul_fields(bytes: &[u8]) -> std::io::Result<Vec<OsString>> {
        if bytes.is_empty() || bytes.last() != Some(&0) {
            return Err(invalid());
        }
        Ok(bytes[..bytes.len() - 1]
            .split(|b| *b == 0)
            .map(|s| OsString::from_vec(s.into()))
            .collect())
    }

    #[cfg(target_os = "macos")]
    fn pids() -> std::io::Result<Vec<libc::pid_t>> {
        // PROC_UID_ONLY from libproc.h, not a command-name filter or a ps text snapshot.
        const UID_ONLY: u32 = 4;
        for capacity in [4096, 16384, 65536] {
            let mut pids = vec![0; capacity];
            let size = (pids.len() * std::mem::size_of::<libc::pid_t>()) as i32;
            let n = unsafe {
                libc::proc_listpids(UID_ONLY, libc::getuid(), pids.as_mut_ptr().cast(), size)
            };
            if n <= 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n == size {
                continue;
            } // possible truncation: grow, never call it absence
            if n % 4 != 0 || n > size {
                return Err(invalid());
            }
            pids.truncate(n as usize / 4);
            pids.retain(|pid| *pid > 0);
            return Ok(pids);
        }
        Err(invalid())
    }

    #[cfg(target_os = "macos")]
    fn stamp(pid: libc::pid_t) -> std::io::Result<(u64, u64)> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if n != size || info.pbi_uid != unsafe { libc::getuid() } {
            return Err(invalid());
        }
        Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
    }

    #[cfg(target_os = "macos")]
    fn inspect(pid: libc::pid_t, old: &Path) -> std::io::Result<Option<Process>> {
        let before = stamp(pid)?;
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if n <= 0 || n as usize >= path.len() {
            return Err(invalid());
        }
        let end = path.iter().position(|b| *b == 0).ok_or_else(invalid)?;
        let exe = PathBuf::from(OsStr::from_bytes(&path[..end]));
        if !exe.is_absolute() {
            return Err(invalid());
        }
        if !candidate(&exe, old) {
            return Ok(None);
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut bytes = vec![0u8; 1024 * 1024];
        let mut len = bytes.len();
        let n = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if n != 0 || len >= bytes.len() {
            return Err(invalid());
        }
        bytes.truncate(len);
        let (args, env) = mac_arguments(&bytes)?;
        if stamp(pid)? != before {
            return Err(invalid());
        }
        Ok(Some((exe, args, env)))
    }

    #[cfg(any(target_os = "macos", test))]
    fn mac_arguments(bytes: &[u8]) -> std::io::Result<(Vec<OsString>, Vec<u8>)> {
        let argc = i32::from_ne_bytes(bytes.get(..4).ok_or_else(invalid)?.try_into().unwrap());
        if argc <= 0 {
            return Err(invalid());
        }
        let mut rest = &bytes[4..];
        let end = rest.iter().position(|b| *b == 0).ok_or_else(invalid)?;
        rest = &rest[end..]; // kernel's executable path, followed by NUL padding
        while rest.first() == Some(&0) {
            rest = &rest[1..];
        }
        let mut args = vec![];
        for _ in 0..argc {
            let end = rest.iter().position(|b| *b == 0).ok_or_else(invalid)?;
            args.push(OsString::from_vec(rest[..end].into()));
            rest = &rest[end + 1..];
        }
        if rest.last() != Some(&0) {
            return Err(invalid());
        }
        Ok((args, rest.into()))
    }

    #[cfg(target_os = "linux")]
    fn pids() -> std::io::Result<Vec<libc::pid_t>> {
        use std::os::unix::fs::MetadataExt;
        let mut pids = vec![];
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            match entry.metadata() {
                Ok(meta) if meta.uid() == unsafe { libc::getuid() } => pids.push(pid),
                Ok(_) => {}
                Err(_)
                    if unsafe { libc::kill(pid, 0) } == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(pids)
    }

    #[cfg(target_os = "linux")]
    fn inspect(pid: libc::pid_t, old: &Path) -> std::io::Result<Option<Process>> {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let before = std::fs::read(dir.join("stat"))?;
        let exe = std::fs::read_link(dir.join("exe"))?;
        // A running, replaced/unlinked helper can still roll back; Linux appends this marker
        // to its executable link. Never skip it merely because the basename gained a suffix.
        let exe = PathBuf::from(OsStr::from_bytes(
            exe.as_os_str()
                .as_bytes()
                .strip_suffix(b" (deleted)")
                .unwrap_or(exe.as_os_str().as_bytes()),
        ));
        if !candidate(&exe, old) {
            return Ok(None);
        }
        let args = nul_fields(&std::fs::read(dir.join("cmdline"))?)?;
        let env = std::fs::read(dir.join("environ"))?;
        if env.last() != Some(&0) {
            return Err(invalid());
        }
        let after = std::fs::read(dir.join("stat"))?;
        // Compare starttime, not CPU/state fields that change while inspected.
        let start = |bytes: &[u8]| -> Option<Vec<u8>> {
            let end = bytes.iter().rposition(|b| *b == b')')?;
            bytes[end + 1..]
                .split(|b| b.is_ascii_whitespace())
                .filter(|s| !s.is_empty())
                .nth(19)
                .map(Into::into)
        };
        if start(&before).is_none() || start(&before) != start(&after) {
            return Err(invalid());
        }
        Ok(Some((exe, args, env)))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn pids() -> std::io::Result<Vec<libc::pid_t>> {
        Err(invalid())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn inspect(_: libc::pid_t, _: &Path) -> std::io::Result<Option<Process>> {
        Err(invalid())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn complete_native_arguments_distinguish_operations_and_systemd_scopes() {
            let old = Path::new("/fixture/releases/old/bin/steve-node");
            let args = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
            assert!(
                relevant(
                    old,
                    &args(&["steve-node", "upgrade-switch", "op_1"]),
                    old,
                    "op_1"
                )
                .unwrap()
            );
            assert!(
                !relevant(
                    old,
                    &args(&["steve-node", "upgrade-switch", "op_2"]),
                    old,
                    "op_1"
                )
                .unwrap()
            );
            assert!(relevant(old, &args(&["steve-node", "upgrade-switch"]), old, "op_1").is_err());
            assert!(relevant(old, &args(&["", "upgrade-switch", "op_1"]), old, "op_1").is_err());
            assert!(relevant(old, &args(&["upgrade-switch", "op_1"]), old, "op_1").is_err());
            let wrapper = Path::new("/usr/bin/systemd-run");
            let mut scope = args(&[
                "systemd-run",
                "--user",
                "--scope",
                "--collect",
                "--quiet",
                "--unit=steve-node-upgrade-op_1",
                "/fixture/releases/old/bin/steve-node",
                "upgrade-switch",
                "op_1",
            ]);
            assert!(relevant(wrapper, &scope, old, "op_1").unwrap());
            assert!(!relevant(wrapper, &scope, old, "op_2").unwrap());
            scope.pop();
            assert!(relevant(wrapper, &scope, old, "op_1").is_err());
        }

        #[test]
        fn truncated_or_ambiguous_native_snapshots_never_mean_absent() {
            assert!(nul_fields(b"steve-node\0upgrade-switch\0op_1").is_err());
            let mut bytes = 3i32.to_ne_bytes().to_vec();
            bytes.extend_from_slice(
                b"/fixture/steve-node\0\0steve-node\0upgrade-switch\0op_1\0STEVE_HOME=/fixture\0",
            );
            let (args, env) = mac_arguments(&bytes).unwrap();
            assert_eq!(args.len(), 3);
            assert!(env.starts_with(b"STEVE_HOME="));
            for end in 0..bytes.len() {
                assert!(mac_arguments(&bytes[..end]).is_err(), "truncated at {end}");
            }
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        #[ignore = "Ubuntu systemd scope, sudo -n and verified v0.5.0 STEVE_TEST_RELEASE_NODE required"]
        async fn published_linux_helper_in_systemd_scope_requires_complete_native_evidence() {
            use steve_runtime::testkit::{eventually_sync, process::Command};
            assert_eq!(std::env::var("STEVE_LINUX_HELPER_E2E").as_deref(), Ok("1"));
            assert_ne!(unsafe { libc::getuid() }, 0, "run nextest as the CI user");
            let binary = PathBuf::from(
                std::env::var_os("STEVE_TEST_RELEASE_NODE").expect("verified release fixture"),
            )
            .canonicalize()
            .unwrap();
            assert_eq!(binary.file_name().unwrap(), "steve-node");
            let home = tempfile::tempdir().unwrap();
            let op = format!("nup_linux_{}", std::process::id());
            let unit = format!("steve-node-upgrade-{op}");
            let record: Record = serde_json::from_value(serde_json::json!({
                "state":{"operation":op,"phase":"linked","version":"0.6.0","previous":"0.5.0","error":null,"updated":1},
                "request":{"operation":op,"build":{"version":"0.6.0","commit":"a".repeat(40),"target":"x86_64-unknown-linux-gnu"},
                    "artifact":"fixture","size":1,"sha256":"a".repeat(64),"health_secs":1},
                "previous_release":binary.parent().unwrap().parent().unwrap()
            })).unwrap();
            // The tracer stops the real ELF at exec's SIGTRAP, BEFORE main. Never continue it:
            // no old helper can perform an InstalledService action. EXITKILL also covers a
            // killed tracer; normal teardown kills/reaps only this tracer's own child.
            const TRACER: &str = r#"
import ctypes, os, pathlib, signal, sys, time
c = ctypes.CDLL(None, use_errno=True)
c.ptrace.argtypes = [ctypes.c_uint, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
c.ptrace.restype = ctypes.c_long
home, binary, op = map(str, sys.argv[1:])
p = os.fork()
if p == 0:
    if c.ptrace(0, 0, None, None) != 0: os._exit(98)
    os.execve(binary, [binary, 'upgrade-switch', op], os.environ)
try:
    _, status = os.waitpid(p, 0)
    assert os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGTRAP
    assert c.ptrace(0x4200, p, None, 0x00100000) == 0
    pathlib.Path(home, 'ready.tmp').write_text(f'{p} {os.getpid()}')
    os.replace(pathlib.Path(home, 'ready.tmp'), pathlib.Path(home, 'ready'))
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if pathlib.Path(home, 'stop').exists(): break
        if pathlib.Path(home, 'deny').exists():
            assert c.prctl(4, 0, 0, 0, 0) == 0
            pathlib.Path(home, 'denied').touch()
        time.sleep(0.02)
finally:
    try: os.kill(p, signal.SIGKILL)
    except ProcessLookupError: pass
    try: os.waitpid(p, 0)
    except ChildProcessError: pass
"#;
            struct Stop(PathBuf);
            impl Drop for Stop {
                fn drop(&mut self) {
                    let _ = std::fs::write(self.0.join("stop"), b"");
                }
            }
            let mut scope = Command::new("sudo")
                .args([
                    "-n",
                    "/usr/bin/systemd-run",
                    "--scope",
                    "--collect",
                    "--quiet",
                ])
                .arg(format!("--unit={unit}"))
                .args([
                    "/usr/bin/setpriv",
                    "--reuid",
                    &unsafe { libc::getuid() }.to_string(),
                    "--regid",
                    &unsafe { libc::getgid() }.to_string(),
                    "--clear-groups",
                    "/usr/bin/env",
                ])
                .arg(format!("HOME={}", home.path().display()))
                .arg(format!("STEVE_HOME={}", home.path().display()))
                .args([
                    "STEVE_VAULT_KEYSTORE=file",
                    "STEVE_BACKUP=off",
                    "STEVE_NO_RESUME=1",
                    "/usr/bin/python3",
                    "-c",
                    TRACER,
                ])
                .arg(home.path())
                .arg(&binary)
                .arg(&op)
                .stdin(std::process::Stdio::null())
                .spawn()
                .expect("sudo/systemd-run/setpriv fixture unavailable");
            // Dropped before `scope`, even on assertion failure. The tracer has its own bounded
            // cleanup as well because a system scope is outside testkit's process group.
            let stop = Stop(home.path().into());
            eventually_sync(
                "released helper exec-stopped inside the systemd scope",
                || {
                    assert!(
                        scope.try_wait().unwrap().is_none(),
                        "systemd scope/tracer failed"
                    );
                    home.path().join("ready").exists()
                },
            )
            .await;
            let ids: Vec<i32> = std::fs::read_to_string(home.path().join("ready"))
                .unwrap()
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            assert_eq!(ids.len(), 2);
            let old = binary.as_path();
            let (exe, args, env) = inspect(ids[0], old)
                .unwrap()
                .expect("actual released helper");
            assert!(relevant(&exe, &args, old, &op).unwrap());
            assert!(!relevant(&exe, &args, old, "nup_another_operation").unwrap());
            assert!(
                pids().unwrap().contains(&ids[0]),
                "helper belongs to the inspected user"
            );
            assert!(
                env.split(|b| *b == 0)
                    .any(|e| e == format!("STEVE_HOME={}", home.path().display()).as_bytes())
            );
            assert!(
                std::fs::read_to_string(format!("/proc/{}/cgroup", ids[0]))
                    .unwrap()
                    .contains(&format!("/{unit}.scope"))
            );
            assert!(!scan(home.path(), &record, &mut inspect).unwrap());
            let another_home = tempfile::tempdir().unwrap();
            eventually_sync("a different installation is distinct", || {
                scan(another_home.path(), &record, &mut inspect).is_ok_and(|gone| gone)
            })
            .await;
            // Query failures and truncated native data must propagate as unknown, including
            // at the same scan boundary that gates migration, rather than become absence.
            assert!(
                scan(home.path(), &record, &mut |_, _| Err(
                    std::io::Error::other("query failed")
                ))
                .is_err()
            );
            assert!(
                scan(home.path(), &record, &mut |_, _| Err(nul_fields(
                    b"steve-node\0upgrade-switch\0truncated"
                )
                .unwrap_err()))
                .is_err()
            );
            std::fs::write(home.path().join("deny"), b"").unwrap();
            eventually_sync(
                "same-user native query really denied by non-dumpable tracer",
                || home.path().join("denied").exists(),
            )
            .await;
            assert_eq!(
                std::fs::read_link(format!("/proc/{}/exe", ids[1]))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert!(inspect(ids[1], old).is_err());
            assert!(
                scan(home.path(), &record, &mut |pid, old| {
                    if pid == ids[1] {
                        inspect(pid, old)
                    } else {
                        Ok(None)
                    }
                })
                .is_err()
            );
            drop(stop);
            eventually_sync("scope tracer kills and reaps its released helper", || {
                scope.try_wait().unwrap().is_some()
            })
            .await;
            assert!(scope.wait().unwrap().success());
            assert_eq!(unsafe { libc::kill(ids[0], 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            eventually_sync(
                "native scan confirms absence after scope/helper exit",
                || scan(home.path(), &record, &mut inspect).is_ok_and(|gone| gone),
            )
            .await;
            // This CI evidence does not silently enable production: that needs a reviewed
            // follow-up once this opt-in test has actually passed on Linux.
            assert!(!absent(home.path(), &record).unwrap());
        }
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
        package(version, info, None, false, extra)
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
        assert!(
            !dir.join("bin/steve").exists(),
            "unmarked legacy Node-only package remains supported"
        );
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

    /// The service around a release that upgrades the journal (its schema version, as text) when
    /// it starts and then, like the unguarded old path, never links. This failure reproduction
    /// deliberately migrates without waiting; the previous release cannot read that journal.
    struct Upgrading {
        current: PathBuf,
        journal: PathBuf,
        old_started: Option<bool>,
    }

    impl ServiceOps for Upgrading {
        fn stop(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn update(&mut self, release: &Path) -> anyhow::Result<()> {
            self.current = release.to_owned();
            Ok(())
        }
        fn start(&mut self) -> anyhow::Result<()> {
            let version: u32 = std::fs::read_to_string(&self.journal)?.parse()?;
            if self.current.ends_with("new") {
                if version < 4 {
                    std::fs::write(&self.journal, "4")?;
                }
            } else {
                // "journal needs a newer Node", again and again
                self.old_started = Some(version <= 3);
            }
            Ok(())
        }
    }

    fn upgrading(home: &Path, op: &str) -> Upgrading {
        switching(home, op);
        let journal = home.join("journal-version");
        std::fs::write(&journal, "3").unwrap();
        Upgrading {
            current: home.join("releases/old"),
            journal,
            old_started: None,
        }
    }

    #[test]
    fn going_back_after_the_journal_upgrade_leaves_a_node_that_cannot_start() {
        // what a helper does when the new release does not claim the switch
        let home = tempfile::tempdir().unwrap();
        let mut svc = upgrading(home.path(), "nup_stuck");
        let s = run_switch(
            home.path(),
            "nup_stuck",
            &mut svc,
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(s.phase, "rolled_back");
        assert_eq!(std::fs::read_to_string(&svc.journal).unwrap(), "4");
        assert_eq!(svc.old_started, Some(false));
    }

    fn inert_harness(home: &Path) {
        std::fs::write(
            home.join("harness.json"),
            serde_json::to_vec(&serde_json::json!({
                "adapters":{"claude-code":"/usr/bin/false","codex":"/usr/bin/false"},
                "node":"/usr/bin/false","claude_executable":"/usr/bin/false"
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn fixture_sql(path: &Path, sql: &str) -> i64 {
        let output = steve_runtime::testkit::process::Command::new("python3")
            .arg("-c").arg("import sqlite3,sys; c=sqlite3.connect(sys.argv[1],timeout=0); c.executescript(sys.argv[2]); print(c.execute('PRAGMA user_version').fetchone()[0]); c.close()")
            .arg(path).arg(sql).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn the_real_journal_waits_for_the_helper_receipt_with_only_home_locked() {
        let temp = tempfile::tempdir().unwrap();
        let home = crate::config::Home(temp.path().into());
        inert_harness(temp.path());
        let path = home.local().join("node.sqlite3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let schema = include_str!("../../steve-node/src/journal/fixtures/v3.sql");
        assert_eq!(
            fixture_sql(
                &path,
                &format!(
                    "{schema}\nINSERT INTO meta(key,value) VALUES('journal_id',123),('incarnation',0); PRAGMA user_version=3;"
                )
            ),
            3
        );
        switching(temp.path(), "nup_sqlite");
        let record = load(temp.path(), "nup_sqlite").unwrap().unwrap();
        let config = || {
            let mut c = home
                .node_config(NodeId::new("node-journal-test").unwrap())
                .unwrap();
            c.acp.user_home = temp.path().into();
            c.acp.codex_home = temp.path().join("codex");
            c.acp.node = "/usr/bin/false".into();
            c.acp.claude_executable = "/usr/bin/false".into();
            c
        };
        let opening = LocalNode::open_upgrading(config(), async {
            wait_for_switch(
                temp.path(),
                &record.request.build,
                |_| Ok(false),
                || Ok(true),
            )
            .await
            .map_err(|_| err(ErrorCode::Offline, "handoff not verified"))
        });
        let acknowledge = async {
            steve_runtime::testkit::eventually(
                "linked is persisted before any migration",
                || async {
                    load(temp.path(), "nup_sqlite")
                        .unwrap()
                        .unwrap()
                        .state
                        .phase
                        == "linked"
                },
            )
            .await;
            // This would fail inside the prototype's long-lived SQLite write transaction.
            assert_eq!(fixture_sql(&path, "BEGIN IMMEDIATE; ROLLBACK;"), 3);
            assert!(matches!(LocalNode::open(config()), Err(e) if e.code == ErrorCode::Conflict));
            let mut receipt = load(temp.path(), "nup_sqlite").unwrap().unwrap();
            receipt.state.phase = "succeeded".into();
            save(temp.path(), &receipt).unwrap();
        };
        let (node, _) = tokio::join!(opening, acknowledge);
        let node = node.unwrap();
        assert_eq!(fixture_sql(&path, ""), 4);
        node.shutdown().await;
        drop(node);
    }

    #[test]
    fn linked_requires_both_a_gone_helper_and_the_actual_current_service() {
        for gone in [false, true] {
            for current in [false, true] {
                let home = tempfile::tempdir().unwrap();
                switching(home.path(), "nup_gate");
                let me = load(home.path(), "nup_gate")
                    .unwrap()
                    .unwrap()
                    .request
                    .build;
                let allowed =
                    switch_permits_upgrade(home.path(), &me, &mut |_| Ok(gone), &mut || {
                        assert_eq!(
                            load(home.path(), "nup_gate").unwrap().unwrap().state.phase,
                            "linked"
                        );
                        Ok(current)
                    })
                    .unwrap();
                assert_eq!(allowed, gone && current);
                assert_eq!(
                    load(home.path(), "nup_gate").unwrap().unwrap().state.phase,
                    "linked"
                );
            }
        }
    }

    #[test]
    fn receipts_and_terminal_failures_still_require_the_actual_service_build() {
        for phase in [
            "succeeded",
            "rolled_back",
            "failed",
            "abandoned",
            "staged",
            "staging",
        ] {
            for current in [false, true] {
                let home = tempfile::tempdir().unwrap();
                switching(home.path(), "nup_receipt");
                let mut r = load(home.path(), "nup_receipt").unwrap().unwrap();
                r.state.phase = phase.into();
                save(home.path(), &r).unwrap();
                assert_eq!(
                    switch_permits_upgrade(
                        home.path(),
                        &r.request.build,
                        &mut |_| panic!("terminal records are not an absence scan"),
                        &mut || Ok(current)
                    )
                    .unwrap(),
                    current && matches!(phase, "succeeded" | "rolled_back" | "failed")
                );
            }
        }
    }

    #[test]
    fn linked_restarts_recheck_and_never_rewrite_or_infer_absence_from_age() {
        let home = tempfile::tempdir().unwrap();
        switching(home.path(), "nup_restart");
        let mut r = load(home.path(), "nup_restart").unwrap().unwrap();
        r.state.phase = "linked".into();
        r.state.updated = Timestamp(i64::MIN / 2);
        save(home.path(), &r).unwrap();
        let before = std::fs::read(record_path(home.path(), "nup_restart")).unwrap();
        for _ in 0..2 {
            assert!(
                !switch_permits_upgrade(
                    home.path(),
                    &r.request.build,
                    &mut |_| Ok(false),
                    &mut || Ok(true)
                )
                .unwrap()
            );
        }
        assert!(
            switch_permits_upgrade(
                home.path(),
                &r.request.build,
                &mut |_| anyhow::bail!("process query failed"),
                &mut || Ok(true)
            )
            .is_err()
        );
        assert_eq!(
            before,
            std::fs::read(record_path(home.path(), "nup_restart")).unwrap()
        );
        assert!(
            switch_permits_upgrade(
                home.path(),
                &r.request.build,
                &mut |_| Ok(true),
                &mut || Ok(true)
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn receipts_and_verified_absence_allow_without_any_additional_wait() {
        let home = tempfile::tempdir().unwrap();
        switching(home.path(), "nup_ready");
        let mut r = load(home.path(), "nup_ready").unwrap().unwrap();
        for phase in ["switching", "linked", "succeeded"] {
            r.state.phase = phase.into();
            save(home.path(), &r).unwrap();
            tokio::time::timeout(
                Duration::ZERO,
                wait_for_switch(home.path(), &r.request.build, |_| Ok(true), || Ok(true)),
            )
            .await
            .unwrap()
            .unwrap();
        }
    }

    #[test]
    fn read_failures_and_conflicts_are_never_absence_including_manual_update() {
        let home = tempfile::tempdir().unwrap();
        let me = build("1.2.3");
        assert!(
            switch_permits_upgrade(home.path(), &me, &mut |_| Ok(true), &mut || Ok(true)).unwrap()
        );
        switching(home.path(), "nup_first");
        let mut r = load(home.path(), "nup_first").unwrap().unwrap();
        let me = r.request.build.clone();
        // A manual update to another build does not make this old helper harmless.
        let other = build("9.9.9");
        assert!(
            !switch_permits_upgrade(home.path(), &other, &mut |_| Ok(false), &mut || Ok(true))
                .unwrap()
        );
        assert!(
            !switch_permits_upgrade(home.path(), &other, &mut |_| Ok(true), &mut || Ok(false))
                .unwrap()
        );
        assert!(
            switch_permits_upgrade(home.path(), &other, &mut |_| Ok(true), &mut || Ok(true))
                .unwrap()
        );
        switching(home.path(), "nup_second");
        assert!(
            switch_permits_upgrade(home.path(), &me, &mut |_| Ok(true), &mut || Ok(true)).is_err()
        );
        assert!(
            switch_permits_upgrade(home.path(), &other, &mut |_| Ok(true), &mut || Ok(true))
                .is_err(),
            "foreign conflicts cannot become no record for this build"
        );
        std::fs::remove_file(record_path(home.path(), "nup_second")).unwrap();
        r.state.operation = OperationId::new("another_operation").unwrap();
        save(home.path(), &r).unwrap();
        assert!(
            switch_permits_upgrade(home.path(), &me, &mut |_| Ok(true), &mut || Ok(true)).is_err()
        );
        std::fs::write(record_path(home.path(), "broken"), b"{").unwrap();
        assert!(
            switch_permits_upgrade(home.path(), &me, &mut |_| Ok(true), &mut || Ok(true)).is_err()
        );
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires the checksum-verified v0.5.0 release in STEVE_TEST_RELEASE_NODE"]
    async fn published_helper_is_identified_by_operation_home_and_native_process_state() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let binary =
            PathBuf::from(std::env::var_os("STEVE_TEST_RELEASE_NODE").expect("release fixture"));
        let home = tempfile::tempdir().unwrap();
        switching(home.path(), "nup_native");
        let mut record = load(home.path(), "nup_native").unwrap().unwrap();
        record.previous_release = Some(binary.parent().unwrap().parent().unwrap().into());
        let current_release = home.path().join("releases/new");
        std::fs::create_dir_all(current_release.join("bin")).unwrap();
        // The actual running test executable stands in for this release's executable; this
        // tests service_release's real canonical-file comparison without executing a test alias.
        std::os::unix::fs::symlink(
            std::env::current_exe().unwrap(),
            current_release.join("bin/steve-node"),
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join("services")).unwrap();
        let point_service = |release: &Path| {
            std::fs::write(
                home.path().join("services/node.json"),
                serde_json::to_vec(&serde_json::json!({"release":release})).unwrap(),
            )
            .unwrap()
        };
        point_service(&current_release);
        save(home.path(), &record).unwrap();
        let path = CString::new(binary.as_os_str().as_bytes()).unwrap();
        let args = [
            path.clone(),
            CString::new("upgrade-switch").unwrap(),
            CString::new("nup_native").unwrap(),
        ];
        let mut argv: Vec<_> = args.iter().map(|s| s.as_ptr().cast_mut()).collect();
        argv.push(std::ptr::null_mut());
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(k, _)| k != "STEVE_HOME")
            .map(|(k, v)| CString::new([k.as_bytes(), b"=", v.as_bytes()].concat()).unwrap())
            .collect();
        env.push(
            CString::new([b"STEVE_HOME=", home.path().as_os_str().as_bytes()].concat()).unwrap(),
        );
        let mut envp: Vec<_> = env.iter().map(|s| s.as_ptr().cast_mut()).collect();
        envp.push(std::ptr::null_mut());
        struct Suspended(libc::pid_t);
        impl Drop for Suspended {
            fn drop(&mut self) {
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
                let mut status = 0;
                while unsafe { libc::waitpid(self.0, &mut status, 0) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                }
            }
        }
        // Start the actual released Mach-O suspended BEFORE main. Its argv/env are real, but
        // no InstalledService action can run; no installed service is created or stopped.
        let mut attr = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::posix_spawnattr_init(&mut attr) }, 0);
        assert_eq!(
            unsafe {
                libc::posix_spawnattr_setflags(&mut attr, libc::POSIX_SPAWN_START_SUSPENDED as i16)
            },
            0
        );
        let mut pid = 0;
        let result = unsafe {
            libc::posix_spawn(
                &mut pid,
                path.as_ptr(),
                std::ptr::null(),
                &attr,
                argv.as_ptr(),
                envp.as_ptr(),
            )
        };
        unsafe {
            libc::posix_spawnattr_destroy(&mut attr);
        }
        assert_eq!(result, 0);
        let process = Suspended(pid);
        assert!(
            !helper_process::absent(home.path(), &record).unwrap(),
            "paused is still present"
        );
        let permits = || {
            switch_permits_upgrade(
                home.path(),
                &record.request.build,
                &mut |r| helper_process::absent(home.path(), r),
                &mut || Ok(service_release(home.path()).is_some()),
            )
        };
        assert!(
            !permits().unwrap(),
            "current service alone cannot permit a paused helper"
        );
        let mut other = record.clone();
        other.state.operation = OperationId::new("nup_other").unwrap();
        other.request.operation = other.state.operation.clone();
        steve_runtime::testkit::eventually_sync("another operation is distinct", || {
            helper_process::absent(home.path(), &other).is_ok_and(|gone| gone)
        })
        .await;
        let other_home = tempfile::tempdir().unwrap();
        steve_runtime::testkit::eventually_sync("another home is distinct", || {
            helper_process::absent(other_home.path(), &record).is_ok_and(|gone| gone)
        })
        .await;
        drop(process);
        steve_runtime::testkit::eventually_sync("reaped helper is gone", || {
            helper_process::absent(home.path(), &record).is_ok_and(|gone| gone)
        })
        .await;
        point_service(record.previous_release.as_ref().unwrap());
        assert!(
            !permits().unwrap(),
            "helper absence alone cannot permit another service release"
        );
        point_service(&current_release);
        steve_runtime::testkit::eventually_sync(
            "native absence and the actual current service permit together",
            || permits().is_ok_and(|allowed| allowed),
        )
        .await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires the checksum-verified v0.5.0 release in STEVE_TEST_RELEASE_NODE"]
    async fn published_reader_accepts_retained_v3_and_rejects_the_real_v4_migration() {
        let binary =
            PathBuf::from(std::env::var_os("STEVE_TEST_RELEASE_NODE").expect("release fixture"));
        let home = tempfile::tempdir().unwrap();
        // Reaching its loopback connection proves LocalNode::open succeeded in that binary.
        let journal = home.path().join("node/local/node.sqlite3");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let schema = include_str!("../../steve-node/src/journal/fixtures/v3.sql");
        assert_eq!(
            fixture_sql(
                &journal,
                &format!(
                    "{schema}\nINSERT INTO meta(key,value) VALUES('journal_id',123),('incarnation',0); PRAGMA user_version=3;"
                )
            ),
            3
        );
        inert_harness(home.path());
        let identity = steve_node_wire::Identity::generate().unwrap();
        identity
            .save(&home.path().join("node/identity.key"))
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let pairing = serde_json::json!({"hub_url":format!("http://{}",listener.local_addr().unwrap()),
            "hub_id":"hub-native","node_id":"node-native","pin":null,"loopback":true,"hub_key":null,"name":"test"});
        std::fs::write(
            home.path().join("node/node.json"),
            serde_json::to_vec(&pairing).unwrap(),
        )
        .unwrap();
        let mut old = steve_runtime::testkit::process::Command::new(&binary)
            .arg("run")
            .env("HOME", home.path())
            .env("STEVE_HOME", home.path())
            .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut accepted = None;
        steve_runtime::testkit::eventually_sync(
            "released v0.5.0 Node opens the retained v3 journal",
            || {
                assert!(
                    old.try_wait().unwrap().is_none(),
                    "the old reader must start"
                );
                if let Ok((socket, _)) = listener.accept() {
                    accepted = Some(socket);
                }
                accepted.is_some()
            },
        )
        .await;
        assert_eq!(unsafe { libc::kill(old.id() as i32, libc::SIGTERM) }, 0);
        steve_runtime::testkit::eventually_sync("released old Node stops", || {
            old.try_wait().unwrap().is_some()
        })
        .await;
        assert!(old.wait().unwrap().success());
        assert_eq!(fixture_sql(&journal, ""), 3);
        // Reproduce the original unguarded open, using the actual migration rather than a
        // version-number stand-in. The new guarded dial path cannot do this before its receipt.
        {
            let mut cfg = crate::config::Home(home.path().into())
                .node_config(NodeId::new("node-native").unwrap())
                .unwrap();
            cfg.acp.user_home = home.path().into();
            cfg.acp.codex_home = home.path().join("codex");
            cfg.acp.node = "/usr/bin/false".into();
            cfg.acp.claude_executable = "/usr/bin/false".into();
            let node = LocalNode::open(cfg).unwrap();
            node.shutdown().await;
            drop(node);
        }
        assert_eq!(fixture_sql(&journal, ""), 4);
        let mut old_v4 = steve_runtime::testkit::process::Command::new(&binary)
            .arg("run")
            .env("HOME", home.path())
            .env("STEVE_HOME", home.path())
            .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        steve_runtime::testkit::eventually_sync(
            "released old Node rejects the migrated journal",
            || old_v4.try_wait().unwrap().is_some(),
        )
        .await;
        assert!(!old_v4.wait().unwrap().success());
        assert_eq!(fixture_sql(&journal, ""), 4);
    }
}
