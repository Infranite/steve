//! Installing a Node on another machine from the Hub, over tailcat's SSH (A6b).
//!
//! The owner starts an install in Devices and gets one line to run on the target:
//! `tailcat serve --ssh-authorized-keys='<this install's key>' ssh`. It prints an address, which the
//! owner pastes back; from there the Hub works on its own, step by step:
//!
//! 1. `uname` on the target, for its platform;
//! 2. the installer archive of this Hub's own version for that platform, checked against the
//!    release's `steve-installer.json` (from the release repository, or `STEVE_NODE_INSTALLERS`, a
//!    directory holding the same files for a Hub without the internet);
//! 3. the target stages the whole archive over the tunnel before unpacking under `~/.steve-node-install`;
//! 4. `steve setup node --hub <this Hub>` then `steve node start`, the pairing code on stdin. The
//!    code is made `confirmed`: the owner authorised this install, so the Node is active once it
//!    pairs, with no fingerprint to compare by hand.
//!
//! The Hub runs the system `ssh` with `tailcat <address> 22` as its proxy (what `tailcat ssh` does,
//! with this install's own key). The key exists for this install only and is deleted when it ends.
//! The target authenticates the Hub by that key; the Hub reaches the target by the address, which
//! names the target's tunnel key, so no host key is checked. A private, per-attempt OpenSSH
//! control socket keeps that connection warm from the probe through setup. Address and key are kept in memory, for
//! a retry, until the install succeeds or is forgotten; neither is ever written to the database.
//!
//! Each step says how far it is (bytes fetched or sent and how fast, the phase of the setup on the
//! target), at most twice a second, in the install itself: a page opened later shows the same. A
//! step that moves no data for [`Limits::stall`] (setup: prints nothing for [`Limits::quiet`]) fails
//! and says so. An archive fetched once is kept (the last [`CACHE_KEEP`]) and, checked against the
//! release's manifest again, serves the next install of the same version and platform. A second
//! install to an address one is already installing to joins that one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_install::setup::{PROGRESS, PROGRESS_LINE};
use steve_modules::Caller;
use steve_node_wire::tailcat;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::{NodeService, now};
use crate::error::json_response;

/// An install not run within this long is forgotten (its key with it).
const KEEP_MS: i64 = 60 * 60 * 1000;
/// The installer archive is a few tens of MB.
const ARCHIVE_MAX: u64 = 256 * 1024 * 1024;
/// Where on the target the archive is unpacked; `steve setup` copies what it keeps out of it.
const REMOTE_DIR: &str = "$HOME/.steve-node-install";
/// How often at most a step's progress is written into the install.
const TICK: Duration = Duration::from_millis(500);
const RATE_FRESH: Duration = Duration::from_secs(3);
const RECEIVED: &str = "steve-install-received";
/// Installer archives kept for the next install, the most recently used ones.
pub const CACHE_KEEP: usize = 2;
/// The phases `steve setup node` names on the target, in order.
const SETUP_PHASES: [&str; 4] = ["install", "pair", "adapters", "start"];

/// When a step that moves nothing has stalled.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// No byte fetched, sent or answered for this long.
    pub stall: Duration,
    /// Nothing printed by the setup on the target for this long (it installs adapters with npm,
    /// quietly).
    pub quiet: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            stall: Duration::from_secs(60),
            quiet: Duration::from_secs(10 * 60),
        }
    }
}

pub struct Installs {
    dir: PathBuf,
    /// Installer archives fetched before, by platform and SHA256.
    cache: PathBuf,
    version: String,
    /// The Hub's HTTPS address, for a Node when tailcat is off.
    public_url: String,
    limits: Limits,
    ops: Mutex<HashMap<String, Arc<Mutex<Op>>>>,
}

#[derive(Clone)]
struct Op {
    id: String,
    owner: String,
    created: i64,
    pubkey: String,
    state: &'static str,
    steps: Vec<Step>,
    node_id: Option<String>,
    error: Option<String>,
    /// The kind of the step that failed.
    failed_step: Option<&'static str>,
    /// The target's tailcat address, from the run until the install succeeds (for a retry).
    address: Option<String>,
    /// Route evidence from the current attempt's tunnel, never from a separate ping.
    route: Option<Route>,
}

#[derive(Clone, Serialize)]
struct Step {
    /// probe, fetch, send, setup or done
    kind: &'static str,
    text: String,
    at: i64,
    ended: Option<i64>,
    progress: Option<Progress>,
    #[serde(skip)]
    updated: Option<Instant>,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    unpack_started: Option<Instant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unpack_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Route {
    DirectObserved,
    DerpObserved,
}

/// Best-effort evidence from the dedicated tailcat ProxyCommand. DERP bootstrap and netcheck
/// messages alone do not indicate peer contact. These are route observations, not packet-level
/// accounting: the selected path can change, and unknown log formats need no special handling.
fn route_in(line: &str) -> Option<Route> {
    let (_, msg) = line.split_once(" magicsock: ")?;
    if msg.starts_with("new contact: peer=") && msg.ends_with(" via=derp") {
        return Some(Route::DerpObserved);
    }
    let (_, rest) = msg
        .strip_prefix("disco: node ")?
        .split_once(" now using ")?;
    let (endpoint, mtu) = rest.split_once(" mtu=")?;
    let endpoint: std::net::SocketAddr = endpoint.parse().ok()?;
    (endpoint.port() != 0
        && endpoint.ip().to_string() != "127.3.3.40"
        && mtu.as_bytes().first().is_some_and(u8::is_ascii_digit))
    .then_some(Route::DirectObserved)
}

/// How far a step is.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Progress {
    /// fetch: download, cached (a copy fetched before) or verify; send: send or unpack; setup: one of
    /// [`SETUP_PHASES`]
    pub phase: &'static str,
    /// Bytes so far.
    pub done: u64,
    pub total: Option<u64>,
    /// Bytes per second since the phase began.
    pub rate: Option<u64>,
}

impl Op {
    fn view(&self) -> Value {
        let t = now();
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|s| {
                let mut v = serde_json::to_value(s).unwrap_or_default();
                v["elapsed_ms"] = json!((s.ended.unwrap_or(t) - s.at).max(0));
                if s.ended.is_none() {
                    if let Some(started) = s.unpack_started {
                        v["unpack_ms"] = json!(started.elapsed().as_millis() as u64);
                    }
                    if let Some(p) = &s.progress
                        && let Some(rate) = p.rate.filter(|&r| r > 0)
                        && s.updated.is_some_and(|u| u.elapsed() <= RATE_FRESH)
                        && matches!(p.phase, "download" | "send")
                    {
                        if let Some(total) = p.total.filter(|&n| n > p.done) {
                            v["progress"]["eta_secs"] = json!((total - p.done).div_ceil(rate));
                        } else if p.total.is_some() {
                            v["progress"]["rate"] = Value::Null;
                        }
                    } else if s.progress.is_some() {
                        v["progress"]["rate"] = Value::Null;
                    }
                }
                v
            })
            .collect();
        json!({
            "id": self.id,
            "state": self.state,
            "command": format!("tailcat serve --ssh-authorized-keys='{}' ssh", self.pubkey),
            "steps": steps,
            "node_id": self.node_id,
            "error": self.error,
            "failed_step": self.failed_step,
            "address": self.address,
            "transfer_route": self.route,
            "created": self.created,
        })
    }
}

impl Installs {
    pub fn new(home: &Path, version: &str, public_url: &str) -> Arc<Installs> {
        Self::with_limits(home, version, public_url, Limits::default())
    }

    pub fn with_limits(
        home: &Path,
        version: &str,
        public_url: &str,
        limits: Limits,
    ) -> Arc<Installs> {
        let dir = home.join("tailcat/installs");
        Arc::new(Installs {
            cache: dir.join("cache"),
            dir,
            version: version.to_string(),
            public_url: public_url.to_string(),
            limits,
            ops: Mutex::new(HashMap::new()),
        })
    }

    fn key_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// Forgets installs nobody ran in time, with their keys.
    fn sweep(&self) {
        let cutoff = now() - KEEP_MS;
        let mut ops = self.ops.lock().unwrap();
        ops.retain(|id, op| {
            let op = op.lock().unwrap();
            let keep = op.created > cutoff || op.state == "running";
            if !keep {
                let _ = std::fs::remove_dir_all(self.key_dir(id));
            }
            keep
        });
    }
}

fn err(status: u16, e: impl Into<String>) -> Response {
    json_response(status, &json!({ "error": e.into() }))
}

/// The next step begins; the one before has ended.
fn step(op: &Arc<Mutex<Op>>, kind: &'static str, text: &str) {
    tracing::info!("node install: {text}");
    let t = now();
    let mut o = op.lock().unwrap();
    if let Some(last) = o.steps.last_mut() {
        last.ended.get_or_insert(t);
    }
    o.steps.push(Step {
        kind,
        text: text.to_string(),
        at: t,
        ended: None,
        progress: None,
        updated: None,
        started: Instant::now(),
        unpack_started: None,
        transfer_ms: None,
        unpack_ms: None,
    });
}

/// Writes a step's progress into the install, at most every [`TICK`] unless its phase changes.
struct Meter {
    op: Arc<Mutex<Op>>,
    phase: &'static str,
    total: Option<u64>,
    since: Instant,
    last: Option<Instant>,
}

impl Meter {
    fn new(op: &Arc<Mutex<Op>>) -> Meter {
        Meter {
            op: op.clone(),
            phase: "",
            total: None,
            since: Instant::now(),
            last: None,
        }
    }

    fn update(&mut self, phase: &'static str, done: u64, total: Option<u64>) {
        if phase != self.phase {
            self.phase = phase;
            self.since = Instant::now();
            self.last = None;
        }
        self.total = total;
        if self.last.is_some_and(|l| l.elapsed() < TICK) {
            return;
        }
        self.put(done);
    }

    /// Now, whatever the time since the last write.
    fn put(&mut self, done: u64) {
        self.last = Some(Instant::now());
        let secs = self.since.elapsed().as_secs_f64();
        let progress = Progress {
            phase: self.phase,
            done,
            total: self.total,
            rate: (secs >= 1.0 && done > 0).then(|| (done as f64 / secs) as u64),
        };
        if let Some(s) = self.op.lock().unwrap().steps.last_mut() {
            s.progress = Some(progress);
            s.updated = Some(Instant::now());
        }
    }
}

pub async fn create(State(svc): State<Arc<NodeService>>, caller: Caller) -> Response {
    let Some(installs) = svc.installs.get().cloned() else {
        return err(404, "not available");
    };
    if !super::tailcat::is_owner(&svc.store, &caller.me).await {
        return err(403, steve_i18n::t!("hub.nodes.install_owner_only"));
    }
    if tailcat::program().is_none() {
        return err(409, steve_i18n::t!("hub.nodes.install_no_tailcat"));
    }
    installs.sweep();
    let Ok(raw) = steve_node_wire::identity::random::<8>() else {
        return err(500, "internal");
    };
    let id = format!("ni_{}", hex::encode(raw));
    let dir = installs.key_dir(&id);
    let made = async {
        tokio::fs::create_dir_all(&dir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let out = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "steve-install", "-f"])
            .arg(dir.join("key"))
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        tokio::fs::read_to_string(dir.join("key.pub")).await
    }
    .await;
    let pubkey = match made {
        Ok(k) => k.trim().to_string(),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return err(
                409,
                steve_i18n::t!("hub.nodes.install_no_ssh", error = e.to_string()),
            );
        }
    };
    let op = Op {
        id: id.clone(),
        owner: caller.me.clone(),
        created: now(),
        pubkey,
        state: "waiting",
        steps: vec![],
        node_id: None,
        error: None,
        failed_step: None,
        address: None,
        route: None,
    };
    let view = op.view();
    installs
        .ops
        .lock()
        .unwrap()
        .insert(id, Arc::new(Mutex::new(op)));
    json_response(200, &view)
}

fn find(svc: &NodeService, id: &str, me: &str) -> Option<(Arc<Installs>, Arc<Mutex<Op>>)> {
    let installs = svc.installs.get()?.clone();
    let op = installs.ops.lock().unwrap().get(id).cloned()?;
    let mine = op.lock().unwrap().owner == me;
    mine.then_some((installs, op))
}

/// The caller's installs, the newest first: what a page opened later picks up.
pub async fn list(State(svc): State<Arc<NodeService>>, caller: Caller) -> Response {
    let Some(installs) = svc.installs.get().cloned() else {
        return json_response(200, &json!({ "installs": [] }));
    };
    installs.sweep();
    let mut mine: Vec<Op> = installs
        .ops
        .lock()
        .unwrap()
        .values()
        .map(|op| op.lock().unwrap().clone())
        .filter(|op| op.owner == caller.me)
        .collect();
    mine.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
    let views: Vec<Value> = mine.iter().map(Op::view).collect();
    json_response(200, &json!({ "installs": views }))
}

pub async fn get(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Response {
    match find(&svc, &id, &caller.me) {
        Some((_, op)) => json_response(200, &op.lock().unwrap().view()),
        None => err(404, "not found"),
    }
}

/// Discards an inactive install and its temporary SSH key. Closing the dialog alone keeps it
/// resumable; cancellation is explicit, and never interrupts an install another page started.
pub async fn cancel(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let Some(installs) = svc.installs.get() else {
        return err(404, "not available");
    };
    // Use the same lock order as run, so cancellation and retry cannot both win.
    let mut ops = installs.ops.lock().unwrap();
    let Some(op) = ops.get(&id) else {
        return json_response(200, &json!({ "ok": true }));
    };
    {
        let o = op.lock().unwrap();
        if o.owner != caller.me {
            return err(404, "not found");
        }
        if o.state != "waiting" && o.state != "failed" {
            return err(409, steve_i18n::t!("hub.nodes.install_cancel_inactive"));
        }
        if let Err(e) = std::fs::remove_dir_all(installs.key_dir(&o.id))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!("node install: cannot remove cancelled install's key: {e}");
            return err(500, "internal");
        }
    }
    ops.remove(&id);
    json_response(200, &json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct RunBody {
    address: String,
}

pub async fn run(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RunBody>,
) -> Response {
    let Some((installs, op)) = find(&svc, &id, &caller.me) else {
        return err(404, "not found");
    };
    let Some(address) = tailcat::address_in(b.address.trim()).map(str::to_string) else {
        return err(400, steve_i18n::t!("hub.nodes.install_bad_address"));
    };
    // where the Node will reach this Hub: known before anything starts, or nothing starts
    if let Err(e) = hub_for_node(&svc, &installs) {
        return err(409, e.to_string());
    }
    {
        // one install at a time to an address: a second one joins the running one. Checked and
        // marked under the list's lock, so two at once cannot both start.
        let ops = installs.ops.lock().unwrap();
        // find may have returned just before a cancellation (or sweep) removed this install.
        if !ops.contains_key(&id) {
            return err(404, "not found");
        }
        for (other_id, other) in ops.iter().filter(|(k, _)| **k != id) {
            let other = other.lock().unwrap();
            if other.state == "running" && other.address.as_deref() == Some(address.as_str()) {
                if other.owner == caller.me {
                    return json_response(200, &other.view());
                }
                tracing::info!("node install: {other_id} already installs to this address");
                return err(409, steve_i18n::t!("hub.nodes.install_busy"));
            }
        }
        let mut o = op.lock().unwrap();
        if o.state == "running" || o.state == "done" {
            return json_response(200, &o.view());
        }
        o.state = "running";
        o.error = None;
        o.failed_step = None;
        o.address = Some(address.clone());
        o.route = None;
        o.steps.clear();
    }
    let (svc2, op2) = (svc.clone(), op.clone());
    // its steps and errors in the language of whoever started it
    let lang = steve_i18n::current();
    tokio::spawn(steve_i18n::scope(lang, async move {
        let r = install(&svc2, &installs, &op2, &address).await;
        let mut o = op2.lock().unwrap();
        let t = now();
        if let Some(last) = o.steps.last_mut() {
            last.ended.get_or_insert(t);
        }
        match r {
            Ok(node) => {
                o.state = "done";
                o.node_id = Some(node);
                o.address = None;
                let _ = std::fs::remove_dir_all(installs.key_dir(&o.id));
            }
            Err(e) => {
                tracing::warn!("node install: {e:#}");
                o.state = "failed";
                o.failed_step = o.steps.last().map(|s| s.kind);
                o.error = Some(format!("{e:#}"));
            }
        }
    }));
    json_response(200, &op.lock().unwrap().view())
}

/// Where the new Node reaches this Hub: over tailcat when it runs, else the HTTPS address.
fn hub_for_node(svc: &NodeService, installs: &Installs) -> anyhow::Result<String> {
    if let Some(url) = svc
        .tailcat
        .get()
        .and_then(|t| t.status()["hub_url"].as_str().map(str::to_string))
    {
        return Ok(url);
    }
    let url = installs.public_url.trim_end_matches('/');
    if url.starts_with("https://") && !url.contains(['\'', '"', ' ', '\\']) {
        return Ok(url.to_string());
    }
    anyhow::bail!(steve_i18n::t!("hub.nodes.install_no_route"))
}

/// What runs on the target to set the Node up, the pairing code on its stdin: the code goes into
/// `STEVE_PAIR_CODE`, which `steve setup node` hands to the pairing. In `sh`, whatever the login
/// shell; the Node is named after the machine.
fn setup_command(hub: &str) -> String {
    sh(&format!(
        "IFS= read -r STEVE_PAIR_CODE && export STEVE_PAIR_CODE && d=\"{REMOTE_DIR}\" && \
         HOSTNAME=\"${{HOSTNAME:-$(uname -n)}}\" && export HOSTNAME && \
         {PROGRESS}=1 \"$d/bin/steve\" setup node --hub {} && unset STEVE_PAIR_CODE && \
         \"$d/bin/steve\" node start && rm -rf \"$d\"",
        sh_quote(hub)
    ))
}

/// `script` run by `sh`, whatever the target's login shell is.
fn sh(script: &str) -> String {
    format!("exec /bin/sh -c {}", sh_quote(script))
}

async fn install(
    svc: &Arc<NodeService>,
    installs: &Installs,
    op: &Arc<Mutex<Op>>,
    address: &str,
) -> anyhow::Result<String> {
    use anyhow::Context;
    let hub = hub_for_node(svc, installs)?;
    let program = tailcat::program().context(steve_i18n::t!("hub.nodes.install_no_tailcat"))?;
    let (id, owner) = {
        let o = op.lock().unwrap();
        (o.id.clone(), o.owner.clone())
    };
    let program_ssh = std::env::var_os("STEVE_SSH").unwrap_or("ssh".into());
    let ssh = Ssh {
        program: if Path::new(&program_ssh)
            .parent()
            .is_some_and(|p| !p.as_os_str().is_empty())
        {
            std::path::absolute(&program_ssh)?.into_os_string()
        } else {
            program_ssh
        },
        key: std::path::absolute(installs.key_dir(&id).join("key"))?,
        proxy: format!(
            "{} --verbose {address} 22",
            sh_quote(&std::path::absolute(program)?.to_string_lossy())
        ),
        control: Some(format!(
            "s-{}",
            hex::encode(steve_node_wire::identity::random::<8>()?)
        )),
    };
    let Limits { stall, quiet } = installs.limits;
    let secs = |d: Duration| d.as_secs().max(1);

    step(op, "probe", &steve_i18n::t!("hub.nodes.install_step_probe"));
    let _tunnel = ssh
        .open(
            op,
            stall,
            steve_i18n::t!("hub.nodes.install_stalled_probe", secs = secs(stall)),
        )
        .await?;
    let uname = ssh
        .exec(
            "uname -sm",
            b"",
            stall,
            steve_i18n::t!("hub.nodes.install_stalled_probe", secs = secs(stall)),
            &mut |_| {},
            &mut |_| {},
        )
        .await?;
    let target = target_of(&uname).with_context(|| {
        steve_i18n::t!("hub.nodes.install_unsupported", platform = uname.trim())
    })?;

    step(
        op,
        "fetch",
        &steve_i18n::t!("hub.nodes.install_step_fetch", target = target),
    );
    let mut meter = Meter::new(op);
    let archive = fetch_installer(
        &installs.version,
        target,
        Some(&installs.cache),
        stall,
        &mut |phase, done, total| meter.update(phase, done, total),
    )
    .await?;
    meter.put(archive.len() as u64);

    step(op, "send", &steve_i18n::t!("hub.nodes.install_step_send"));
    let total = archive.len() as u64;
    let mut meter = Meter::new(op);
    meter.update("send", 0, Some(total));
    let result = ssh
        .exec(
            &receive_command(total),
            &archive,
            stall,
            steve_i18n::t!("hub.nodes.install_stalled_send", secs = secs(stall)),
            &mut |n| meter.update("send", n, Some(total)),
            &mut |line| {
                if line == RECEIVED
                    && let Some(s) = op.lock().unwrap().steps.last_mut()
                    && s.unpack_started.is_none()
                {
                    s.transfer_ms = Some(s.started.elapsed().as_millis() as u64);
                    s.unpack_started = Some(Instant::now());
                    s.progress = Some(Progress {
                        phase: "unpack",
                        done: total,
                        total: Some(total),
                        rate: None,
                    });
                }
            },
        )
        .await;
    let unpacking = {
        let mut o = op.lock().unwrap();
        let s = o.steps.last_mut().expect("send step");
        let started = s.unpack_started.take();
        s.unpack_ms = started.map(|t| t.elapsed().as_millis() as u64);
        started.is_some()
    };
    if unpacking {
        result.context(steve_i18n::t!("hub.nodes.install_unpack_failed"))?;
    } else {
        result?;
        anyhow::bail!(steve_i18n::t!("hub.nodes.install_no_receipt"));
    }

    step(op, "setup", &steve_i18n::t!("hub.nodes.install_step_setup"));
    let code = super::pairing::new_code(&svc.store, &owner, true).await?;
    let mut meter = Meter::new(op);
    meter.update(SETUP_PHASES[0], 0, None);
    let marker = format!("{PROGRESS_LINE} ");
    let out = ssh
        .exec(
            &setup_command(&hub),
            format!("{code}\n").as_bytes(),
            quiet,
            steve_i18n::t!("hub.nodes.install_stalled_setup", secs = secs(quiet)),
            &mut |_| {},
            &mut |line| {
                if let Some(phase) = line
                    .strip_prefix(&marker)
                    .and_then(|p| SETUP_PHASES.iter().find(|x| **x == p.trim()))
                {
                    meter.update(phase, 0, None);
                }
            },
        )
        .await;

    let hash = super::pairing::code_hash(&code);
    let store = svc.store.clone();
    let node = tokio::task::spawn_blocking(move || {
        store.transaction(|tx| {
            use rusqlite::OptionalExtension;
            let node = tx.query_row(
                "SELECT node_id FROM node_pairings WHERE code_hash=? AND claimed_at IS NOT NULL",
                [&hash],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
            // Once setup exits, a failed attempt must not leave an auto-confirming code usable.
            // A retry gets its own code; a Node that already paired is left alone.
            tx.execute(
                "UPDATE node_pairings SET expires=? WHERE code_hash=? AND claimed_at IS NULL",
                rusqlite::params![now(), hash],
            )?;
            Ok::<_, steve_store::StoreError>(node)
        })
    })
    .await??;
    let out = out?;
    tracing::debug!("node install: {out}");
    let node = node.context(steve_i18n::t!("hub.nodes.install_not_paired"))?;
    step(op, "done", &steve_i18n::t!("hub.nodes.install_step_done"));
    Ok(node)
}

/// Stage the verified bytes before extracting, so target disk/decompression can't be mistaken
/// for streaming network time. SSH authenticates and integrity-protects the bytes; the size check
/// prevents a prematurely closed stdin from being accepted. The receipt is emitted before tar.
fn receive_command(total: u64) -> String {
    sh(&format!(
        "umask 077; d=\"{REMOTE_DIR}\"; rm -rf \"$d\" && mkdir -p \"$d\" || exit 1; \
         a=$(mktemp \"$d/.archive.XXXXXXXX\") || exit 1; trap 'rm -f \"$a\"' 0; \
         trap 'exit 1' HUP INT TERM; cat > \"$a\" && \
         [ \"$(wc -c < \"$a\")\" -eq {total} ] && printf '%s\\n' {RECEIVED} && tar xzf \"$a\" -C \"$d\""
    ))
}

/// `uname -sm` → a release target.
fn target_of(uname: &str) -> Option<&'static str> {
    match uname.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["Darwin", "arm64"] => Some("aarch64-apple-darwin"),
        ["Linux", "x86_64"] => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

struct Ssh {
    /// `ssh`, or another OpenSSH-compatible client in `STEVE_SSH` (tests stand one in)
    program: std::ffi::OsString,
    key: PathBuf,
    proxy: String,
    /// Relative to the private key directory, so it also works with long STEVE_HOME paths.
    control: Option<String>,
}

/// The foreground master (no ControlPersist or detached process). Dropping it also stops users
/// of its channels; the operation directory owns the socket, including on abort or retry.
struct Tunnel {
    child: tokio::process::Child,
    diagnostics: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.diagnostics.abort();
    }
}

/// The last lines of what a command said, for its error.
fn tail_of(text: &str) -> String {
    let tail: Vec<char> = text.chars().rev().take(800).collect();
    tail.into_iter().rev().collect()
}

impl Ssh {
    fn command(&self, cmd: Option<&str>) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(&self.program);
        c.arg("-i")
            .arg(&self.key)
            .args([
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=60",
                "-o",
                "ServerAliveInterval=15",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(control) = &self.control {
            // Only the very same authenticated connection may run these commands. Falling back
            // implicitly could change the path and invalidate the route evidence.
            c.current_dir(self.key.parent().expect("key directory"))
                .arg("-S")
                .arg(format!("./{control}"))
                .args(["-o", "ControlPersist=no"]);
            if cmd.is_none() {
                c.args(["-M", "-N"]);
            } else {
                c.args(["-o", "ControlMaster=no"]);
            }
        }
        let proxy = if self.control.is_some() && cmd.is_some() {
            "false"
        } else {
            &self.proxy
        };
        c.arg("-o")
            .arg(format!("ProxyCommand={proxy}"))
            .args(["-T", "steve-install"]);
        if let Some(cmd) = cmd {
            c.args(["--", cmd]);
        } else {
            c.stdin(std::process::Stdio::null());
        }
        c
    }

    async fn open(
        &self,
        op: &Arc<Mutex<Op>>,
        limit: Duration,
        stalled: String,
    ) -> anyhow::Result<Tunnel> {
        let socket = self
            .key
            .with_file_name(self.control.as_ref().expect("a control socket"));
        let mut child = self.command(None).spawn()?;
        let stderr = child.stderr.take().expect("piped");
        let error = Arc::new(Mutex::new(String::new()));
        let (op, last_error) = (op.clone(), error.clone());
        let diagnostics = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(route) = route_in(&line) {
                    let mut o = op.lock().unwrap();
                    if !matches!(o.steps.last().map(|s| s.kind), Some("setup" | "done"))
                        && !o.steps.last().is_some_and(|s| s.transfer_ms.is_some())
                    {
                        o.route = Some(route);
                    }
                } else if line.starts_with("ssh:") || line.contains("Permission denied (") {
                    // Do not pass the verbose log, with peer addresses and keys, to the API.
                    *last_error.lock().unwrap() = tail_of(&line);
                }
            }
        });
        let mut tunnel = Tunnel { child, diagnostics };
        tokio::time::timeout(limit, async {
            loop {
                if let Some(status) = tunnel.child.try_wait()? {
                    let _ = (&mut tunnel.diagnostics).await;
                    anyhow::bail!(steve_i18n::t!(
                        "hub.nodes.install_remote_failed",
                        status = status.to_string(),
                        output = error.lock().unwrap().as_str()
                    ));
                }
                if socket.exists() {
                    return Ok(tunnel);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!(stalled))?
    }

    /// `cmd` on the target with `input` on its stdin; its stdout, or why it failed. `sent` hears
    /// how many bytes of `input` went out, `line` each line of stdout. Nothing written or read for
    /// `limit`: it has stalled, and fails with `stalled`.
    async fn exec(
        &self,
        cmd: &str,
        input: &[u8],
        limit: Duration,
        stalled: String,
        sent: &mut (dyn FnMut(u64) + Send),
        line: &mut (dyn FnMut(&str) + Send),
    ) -> anyhow::Result<String> {
        let mut child = self
            .command(Some(cmd))
            .spawn()
            .map_err(|e| anyhow::anyhow!("ssh: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let last = Mutex::new(Instant::now());
        let touch = || *last.lock().unwrap() = Instant::now();
        let write = async {
            let mut n = 0u64;
            for chunk in input.chunks(64 * 1024) {
                // the target stopped reading: its exit status says why
                if stdin.write_all(chunk).await.is_err() {
                    break;
                }
                n += chunk.len() as u64;
                touch();
                sent(n);
            }
            drop(stdin);
        };
        let read_out = async {
            let mut r = tokio::io::BufReader::new(stdout);
            let (mut all, mut buf) = (Vec::new(), Vec::new());
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        touch();
                        let text = String::from_utf8_lossy(&buf);
                        let text = text.trim_end();
                        line(text);
                        if !text.starts_with(PROGRESS_LINE) && text != RECEIVED {
                            all.extend_from_slice(&buf);
                        }
                    }
                }
            }
            all
        };
        let read_err = async {
            let mut r = tokio::io::BufReader::new(stderr);
            let (mut all, mut buf) = (Vec::new(), Vec::new());
            while let Ok(n) = r.read_until(b'\n', &mut buf).await {
                if n == 0 {
                    break;
                }
                touch();
                all.append(&mut buf);
            }
            all
        };
        let work = async {
            let ((), out, err) = tokio::join!(write, read_out, read_err);
            (out, err, child.wait().await)
        };
        let watch = async {
            let every = (limit / 4).min(Duration::from_secs(1));
            loop {
                tokio::time::sleep(every).await;
                if last.lock().unwrap().elapsed() >= limit {
                    return;
                }
            }
        };
        let (out, err, status) = tokio::select! {
            r = work => r,
            () = watch => anyhow::bail!(stalled),
        };
        let status = status?;
        if !status.success() {
            let said: String = String::from_utf8_lossy(&err)
                .lines()
                .chain(String::from_utf8_lossy(&out).lines())
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(steve_i18n::t!(
                "hub.nodes.install_remote_failed",
                status = status.to_string(),
                output = tail_of(&said)
            ));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Hears a fetch's phase (download, cached, verify), its bytes so far and their total.
type OnFetch<'a> = &'a mut (dyn FnMut(&'static str, u64, Option<u64>) + Send);

/// The installer archive of `version` for `target`, checked against the release's manifest.
pub(crate) async fn installer(version: &str, target: &str) -> anyhow::Result<Vec<u8>> {
    fetch_installer(
        version,
        target,
        None,
        Limits::default().stall,
        &mut |_, _, _| {},
    )
    .await
}

/// Where release files come from: the release repository, or a directory holding the same files.
enum Source {
    Local(PathBuf),
    Remote(String, reqwest::Client),
}

impl Source {
    fn remote(base: String, stall: Duration) -> anyhow::Result<Source> {
        Ok(Source::Remote(
            base,
            reqwest::Client::builder().connect_timeout(stall).build()?,
        ))
    }

    /// The file `name`, at most `limit` bytes; `on` hears the bytes so far. No byte for `stall`:
    /// it has stalled.
    async fn get(
        &self,
        name: &str,
        limit: u64,
        stall: Duration,
        on: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> anyhow::Result<Vec<u8>> {
        use anyhow::Context;
        let stalled = || {
            anyhow::anyhow!(steve_i18n::t!(
                "hub.nodes.install_stalled_fetch",
                secs = stall.as_secs().max(1)
            ))
        };
        match self {
            Source::Local(dir) => {
                let p = dir.join(name);
                let b = tokio::fs::read(&p)
                    .await
                    .with_context(|| p.display().to_string())?;
                anyhow::ensure!(b.len() as u64 <= limit, "{name}: too large");
                on(b.len() as u64, Some(b.len() as u64));
                Ok(b)
            }
            Source::Remote(base, client) => {
                let mut r =
                    tokio::time::timeout(stall, client.get(format!("{base}/{name}")).send())
                        .await
                        .map_err(|_| stalled())??
                        .error_for_status()?;
                let total = r.content_length();
                anyhow::ensure!(total.is_none_or(|n| n <= limit), "{name}: too large");
                let mut b = Vec::with_capacity(total.unwrap_or(0) as usize);
                on(0, total);
                while let Some(chunk) = tokio::time::timeout(stall, r.chunk())
                    .await
                    .map_err(|_| stalled())??
                {
                    b.extend_from_slice(&chunk);
                    anyhow::ensure!(b.len() as u64 <= limit, "{name}: too large");
                    on(b.len() as u64, total);
                }
                Ok(b)
            }
        }
    }
}

/// `bytes` with its SHA256, worked out off the runtime's threads.
async fn digest(bytes: Vec<u8>) -> anyhow::Result<(Vec<u8>, String)> {
    Ok(tokio::task::spawn_blocking(move || {
        let sha = hex::encode(Sha256::digest(&bytes));
        (bytes, sha)
    })
    .await?)
}

/// Keeps `bytes` as `path` in the cache, which then holds the [`CACHE_KEEP`] most recent.
async fn cache_put(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().expect("in the cache");
    tokio::fs::create_dir_all(dir).await?;
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    cache_prune(dir).await
}

async fn cache_prune(dir: &Path) -> std::io::Result<()> {
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(e) = entries.next_entry().await? {
        let m = e.metadata().await?;
        if m.is_file() {
            files.push((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), e.path()));
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(CACHE_KEEP) {
        let _ = tokio::fs::remove_file(p).await;
    }
    Ok(())
}

/// [`installer`], with an archive cache (`cache`, for a download from the release repository)
/// and its progress for `on`.
async fn fetch_installer(
    version: &str,
    target: &str,
    cache: Option<&Path>,
    stall: Duration,
    on: OnFetch<'_>,
) -> anyhow::Result<Vec<u8>> {
    use anyhow::Context;
    let v = semver::Version::parse(version)
        .ok()
        .context(steve_i18n::t!(
            "hub.nodes.install_dev_build",
            version = version
        ))?;
    let source = match std::env::var_os("STEVE_NODE_INSTALLERS").map(PathBuf::from) {
        Some(dir) => Source::Local(dir),
        None => Source::remote(
            format!(
                "https://github.com/{}/releases/download/v{v}",
                crate::upgrades::repository()?
            ),
            stall,
        )?,
    };
    fetch_from(&source, &v, target, cache, stall, on).await
}

async fn fetch_from(
    source: &Source,
    v: &semver::Version,
    target: &str,
    cache: Option<&Path>,
    stall: Duration,
    on: OnFetch<'_>,
) -> anyhow::Result<Vec<u8>> {
    use anyhow::Context;
    let manifest: Value = serde_json::from_slice(
        &source
            .get("steve-installer.json", 1 << 20, stall, &mut |_, _| {})
            .await?,
    )
    .context("steve-installer.json")?;
    anyhow::ensure!(
        manifest["version"] == v.to_string(),
        "steve-installer.json is for {}, not {v}",
        manifest["version"]
    );
    let asset = manifest["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["target"] == target))
        .with_context(|| steve_i18n::t!("hub.nodes.install_unsupported", platform = target))?;
    let name = asset["name"].as_str().context("asset name")?;
    anyhow::ensure!(
        !name.contains(['/', '\\']) && !name.starts_with('.'),
        "bad asset name {name}"
    );
    let sha = asset["sha256"].as_str().context("asset sha256")?;
    anyhow::ensure!(
        sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "{name}: bad sha256 in the manifest"
    );
    let size = asset["size"].as_u64().context("asset size")?;
    // a copy fetched before, checked again as if it had just come
    let cached = match source {
        Source::Remote(..) => {
            cache.map(|d| d.join(format!("{target}-{}.tar.gz", sha.to_lowercase())))
        }
        Source::Local(_) => None,
    };
    if let Some(path) = &cached
        && let Ok(bytes) = tokio::fs::read(path).await
    {
        on("cached", bytes.len() as u64, Some(size));
        let (bytes, got) = digest(bytes).await?;
        if bytes.len() as u64 == size && got.eq_ignore_ascii_case(sha) {
            // the most recently used is kept longest
            if let Ok(f) = std::fs::File::options().write(true).open(path) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Ok(bytes);
        }
        tracing::warn!(
            "node install: cached {} differs from the manifest",
            path.display()
        );
        let _ = tokio::fs::remove_file(path).await;
    }
    let bytes = source
        .get(name, size.min(ARCHIVE_MAX), stall, &mut |done, _| {
            on("download", done, Some(size))
        })
        .await?;
    on("verify", bytes.len() as u64, Some(size));
    anyhow::ensure!(
        bytes.len() as u64 == size,
        "{name}: size differs from the manifest"
    );
    let (bytes, got) = digest(bytes).await?;
    anyhow::ensure!(
        got.eq_ignore_ascii_case(sha),
        "{name}: SHA256 differs from the manifest"
    );
    if let Some(path) = &cached
        && let Err(e) = cache_put(path, &bytes).await
    {
        tracing::warn!("node install: keeping the installer for next time: {e}");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_quoting() {
        assert_eq!(target_of("Darwin arm64\n"), Some("aarch64-apple-darwin"));
        assert_eq!(target_of("Linux x86_64"), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(target_of("Linux aarch64"), None);
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    type Events = Arc<Mutex<Vec<(&'static str, u64, Option<u64>)>>>;

    /// An HTTP server for release files: `name` → body, sent in 16 KiB pieces a little apart;
    /// `stall` sends its headers and half its body, then nothing. Counts the requests per file.
    async fn releases(
        files: HashMap<String, Vec<u8>>,
    ) -> (String, Arc<Mutex<HashMap<String, usize>>>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(HashMap::new()));
        let (files, counts) = (Arc::new(files), asked.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut c, _)) = listener.accept().await else {
                    return;
                };
                let (files, counts) = (files.clone(), counts.clone());
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match c.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let path = String::from_utf8_lossy(&req)
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .trim_start_matches('/')
                        .to_string();
                    *counts.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
                    let Some(body) = files.get(&path) else {
                        let _ = c
                            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                            .await;
                        return;
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = c.write_all(head.as_bytes()).await;
                    if path == "stall" {
                        let _ = c.write_all(&body[..body.len() / 2]).await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    for piece in body.chunks(16 * 1024) {
                        if c.write_all(piece).await.is_err() {
                            return;
                        }
                        let _ = c.flush().await;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                });
            }
        });
        (base, asked)
    }

    fn release_files(archives: &[(&str, Vec<u8>)]) -> HashMap<String, Vec<u8>> {
        let assets: Vec<Value> = archives
            .iter()
            .map(|(target, b)| {
                json!({"target": target, "name": format!("steve-9.9.9-{target}.tar.gz"),
                       "sha256": hex::encode(Sha256::digest(b)), "size": b.len()})
            })
            .collect();
        let mut files: HashMap<String, Vec<u8>> = archives
            .iter()
            .map(|(target, b)| (format!("steve-9.9.9-{target}.tar.gz"), b.clone()))
            .collect();
        files.insert(
            "steve-installer.json".into(),
            json!({"version": "9.9.9", "assets": assets})
                .to_string()
                .into_bytes(),
        );
        files
    }

    async fn fetch(
        base: &str,
        target: &str,
        cache: &Path,
        events: &Events,
    ) -> anyhow::Result<Vec<u8>> {
        let source = Source::remote(base.to_string(), Duration::from_secs(5)).unwrap();
        let v = semver::Version::parse("9.9.9").unwrap();
        let ev = events.clone();
        fetch_from(
            &source,
            &v,
            target,
            Some(cache),
            Duration::from_secs(5),
            &mut |phase, done, total| ev.lock().unwrap().push((phase, done, total)),
        )
        .await
    }

    #[tokio::test]
    async fn fetching_reports_its_progress_and_reuses_a_bounded_cache() {
        let a: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let b = vec![1u8; 70_000];
        let c = vec![2u8; 50_000];
        let (base, asked) = releases(release_files(&[
            ("x86_64-unknown-linux-gnu", a.clone()),
            ("aarch64-apple-darwin", b.clone()),
            ("riscv64", c.clone()),
        ]))
        .await;
        let cache = tempfile::tempdir().unwrap();
        let events: Events = Default::default();
        let size = a.len() as u64;

        // the first time: downloaded piece by piece, then checked
        let got = fetch(&base, "x86_64-unknown-linux-gnu", cache.path(), &events)
            .await
            .unwrap();
        assert_eq!(got, a);
        let seen = std::mem::take(&mut *events.lock().unwrap());
        let downloads: Vec<u64> = seen
            .iter()
            .filter(|e| e.0 == "download")
            .map(|e| e.1)
            .collect();
        assert!(downloads.len() > 5, "{seen:?}");
        assert!(downloads.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert_eq!(downloads.last(), Some(&size));
        assert!(seen.iter().all(|e| e.0 == "verify" || e.2 == Some(size)));
        assert_eq!(seen.last(), Some(&("verify", size, Some(size))));
        let archive = "steve-9.9.9-x86_64-unknown-linux-gnu.tar.gz";
        assert_eq!(asked.lock().unwrap()[archive], 1);

        // again: the copy, checked again, and no download
        let got = fetch(&base, "x86_64-unknown-linux-gnu", cache.path(), &events)
            .await
            .unwrap();
        assert_eq!(got, a);
        let seen = std::mem::take(&mut *events.lock().unwrap());
        assert_eq!(seen, [("cached", size, Some(size))]);
        assert_eq!(asked.lock().unwrap()[archive], 1);
        assert_eq!(
            asked.lock().unwrap()["steve-installer.json"],
            2,
            "the manifest is always fetched"
        );

        // a copy that changed is not used, but fetched again
        let kept = std::fs::read_dir(cache.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(&kept, b"not the archive").unwrap();
        let got = fetch(&base, "x86_64-unknown-linux-gnu", cache.path(), &events)
            .await
            .unwrap();
        assert_eq!(got, a);
        assert_eq!(asked.lock().unwrap()[archive], 2);
        assert_eq!(std::fs::read(&kept).unwrap(), a);

        // the cache keeps the most recent few
        for target in ["aarch64-apple-darwin", "riscv64"] {
            tokio::time::sleep(Duration::from_millis(20)).await;
            fetch(&base, target, cache.path(), &events).await.unwrap();
        }
        let mut left: Vec<String> = std::fs::read_dir(cache.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), CACHE_KEEP, "{left:?}");
        assert!(left.iter().all(|f| !f.starts_with("x86_64")), "{left:?}");
    }

    #[tokio::test]
    async fn a_download_with_no_data_fails_as_stalled() {
        let mut files = HashMap::new();
        files.insert("stall".to_string(), vec![0u8; 100_000]);
        let (base, _) = releases(files).await;
        let source = Source::remote(base, Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        let e = source
            .get("stall", 1 << 20, Duration::from_millis(300), &mut |_, _| {})
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            e.to_string(),
            steve_i18n::t!("hub.nodes.install_stalled_fetch", secs = 1)
        );
    }

    /// An `ssh` that runs the command it is given here.
    fn local_ssh(dir: &Path) -> Ssh {
        let program = dir.join("ssh");
        std::fs::write(
            &program,
            "#!/bin/sh\nfor a; do cmd=\"$a\"; done\nexec sh -c \"$cmd\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Ssh {
            program: program.into(),
            key: dir.join("key"),
            proxy: "true".into(),
            control: None,
        }
    }

    #[tokio::test]
    async fn sending_reports_bytes_and_a_target_that_takes_none_has_stalled() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = local_ssh(dir.path());
        let input: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
        let out = dir.path().join("out");
        let mut sent = Vec::new();
        ssh.exec(
            &sh(&format!("cat > {}", sh_quote(&out.to_string_lossy()))),
            &input,
            Duration::from_secs(10),
            "stalled".into(),
            &mut |n| sent.push(n),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), input);
        assert!(sent.len() > 10 && sent.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(sent.last(), Some(&(input.len() as u64)));

        // it reads nothing: once the pipe is full no byte moves, and it is stopped
        let started = Instant::now();
        let e = ssh
            .exec(
                "sleep 30",
                &vec![0u8; 8 << 20],
                Duration::from_millis(500),
                "the target took nothing".into(),
                &mut |_| {},
                &mut |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "the target took nothing");
        assert!(started.elapsed() < Duration::from_secs(10));

        // its lines as they come, without setup's progress in what it said
        let mut lines = Vec::new();
        let said = ssh
            .exec(
                &format!("echo '{PROGRESS_LINE} pair'; echo hi"),
                b"",
                Duration::from_secs(10),
                "stalled".into(),
                &mut |_| {},
                &mut |l| lines.push(l.to_string()),
            )
            .await
            .unwrap();
        assert_eq!(lines, [format!("{PROGRESS_LINE} pair"), "hi".into()]);
        assert_eq!(said, "hi\n");

        // a failure says how it ended and what the target said
        let e = ssh
            .exec(
                "echo boom >&2; exit 3",
                b"",
                Duration::from_secs(10),
                "stalled".into(),
                &mut |_| {},
                &mut |_| {},
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("boom") && e.contains('3'), "{e}");
    }

    #[test]
    fn progress_is_written_at_most_every_tick_unless_its_phase_changes() {
        let op = Arc::new(Mutex::new(Op {
            id: "ni_x".into(),
            owner: "u".into(),
            created: 0,
            pubkey: String::new(),
            state: "running",
            steps: vec![],
            node_id: None,
            error: None,
            failed_step: None,
            address: None,
            route: None,
        }));
        step(&op, "fetch", "fetching");
        let mut m = Meter::new(&op);
        for n in 0..100 {
            m.update("download", n, Some(100));
        }
        let seen = |op: &Arc<Mutex<Op>>| op.lock().unwrap().steps[0].progress.clone().unwrap();
        assert_eq!((seen(&op).phase, seen(&op).done), ("download", 0));
        m.update("verify", 100, Some(100));
        assert_eq!((seen(&op).phase, seen(&op).done), ("verify", 100));
        step(&op, "send", "sending");
        m.since = Instant::now() - Duration::from_secs(2);
        m.phase = "send";
        m.total = Some(1000);
        m.put(400);
        let active = op.lock().unwrap().view();
        assert!(active["steps"][1]["progress"]["eta_secs"].is_u64());
        op.lock().unwrap().steps[1].updated = Some(Instant::now() - 2 * RATE_FRESH);
        let v = op.lock().unwrap().view();
        assert!(v["steps"][1]["progress"]["rate"].is_null());
        assert!(v["steps"][1]["progress"]["eta_secs"].is_null());
        assert!(
            v["steps"][0]["ended"].is_i64() && v["steps"][1]["ended"].is_null(),
            "{v}"
        );
        assert!(v["steps"][1]["elapsed_ms"].as_i64().unwrap() >= 0);
    }

    #[test]
    fn route_evidence_is_from_the_peer_not_relay_bootstrap_or_interface_ips() {
        let classify = |s: &str| route_in(&format!("2026/11/18 10:00:00 magicsock: {s}"));
        assert_eq!(classify("derp-1 connected; connGen=1"), None);
        assert_eq!(
            classify("new contact: control-netmap usec=600 cached=false"),
            None
        );
        assert_eq!(
            classify("new contact: peer=[p] usec=600 cached=false via=derp"),
            Some(Route::DerpObserved)
        );
        assert_eq!(
            classify("disco: node [p] d:abc now using [2001:db8::1]:5000 mtu=1360"),
            Some(Route::DirectObserved)
        );
        assert_eq!(
            classify("disco: node [p] d:abc now using 127.3.3.40:1 mtu=1360"),
            None
        );
    }

    #[tokio::test]
    async fn receiving_a_truncated_archive_neither_confirms_nor_unpacks_it() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = local_ssh(dir.path());
        let mut command = ssh.command(Some(&receive_command(100)));
        command
            .env("HOME", dir.path())
            .stdin(std::process::Stdio::null());
        let out = command.output().await.unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn the_setup_command_reads_the_code_from_stdin_into_the_environment() {
        let cmd = setup_command("https://hub.example");
        assert!(cmd.starts_with("exec /bin/sh -c '"), "{cmd}");
        for part in [
            "IFS= read -r STEVE_PAIR_CODE && export STEVE_PAIR_CODE",
            "STEVE_SETUP_PROGRESS=1",
            "setup node --hub '\\''https://hub.example'\\''",
            "node start",
        ] {
            assert!(cmd.contains(part), "{part}: {cmd}");
        }
    }
}
