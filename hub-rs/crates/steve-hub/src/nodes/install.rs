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
//! 3. the archive goes over the tunnel and is unpacked under `~/.steve-node-install`;
//! 4. `steve setup node --hub <this Hub>` then `steve node start`, the pairing code on stdin. The
//!    code is made `confirmed`: the owner authorised this install, so the Node is active once it
//!    pairs, with no fingerprint to compare by hand.
//!
//! The Hub runs the system `ssh` with `tailcat <address> 22` as its proxy (what `tailcat ssh` does,
//! with this install's own key). The key exists for this install only and is deleted when it ends.
//! The target authenticates the Hub by that key; the Hub reaches the target by the address, which
//! names the target's tunnel key, so no host key is checked. Neither address nor key is kept.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::Caller;
use steve_node_wire::tailcat;
use tokio::io::AsyncWriteExt;

use super::{NodeService, now};
use crate::error::json_response;

/// An install not run within this long is forgotten (its key with it).
const KEEP_MS: i64 = 60 * 60 * 1000;
/// The installer archive is a few tens of MB.
const ARCHIVE_MAX: u64 = 256 * 1024 * 1024;
/// Where on the target the archive is unpacked; `steve setup` copies what it keeps out of it.
const REMOTE_DIR: &str = "$HOME/.steve-node-install";

pub struct Installs {
    dir: PathBuf,
    version: String,
    /// The Hub's HTTPS address, for a Node when tailcat is off.
    public_url: String,
    ops: Mutex<HashMap<String, Arc<Mutex<Op>>>>,
}

#[derive(Clone)]
struct Op {
    id: String,
    owner: String,
    created: i64,
    pubkey: String,
    state: &'static str,
    steps: Vec<Value>,
    node_id: Option<String>,
    error: Option<String>,
}

impl Op {
    fn view(&self) -> Value {
        json!({
            "id": self.id,
            "state": self.state,
            "command": format!("tailcat serve --ssh-authorized-keys='{}' ssh", self.pubkey),
            "steps": self.steps,
            "node_id": self.node_id,
            "error": self.error,
        })
    }
}

impl Installs {
    pub fn new(home: &Path, version: &str, public_url: &str) -> Arc<Installs> {
        Arc::new(Installs {
            dir: home.join("tailcat/installs"),
            version: version.to_string(),
            public_url: public_url.to_string(),
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

fn step(op: &Arc<Mutex<Op>>, text: &str) {
    tracing::info!("node install: {text}");
    op.lock()
        .unwrap()
        .steps
        .push(json!({ "at": now(), "text": text }));
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
        let mut o = op.lock().unwrap();
        if o.state == "running" || o.state == "done" {
            return json_response(200, &o.view());
        }
        o.state = "running";
        o.error = None;
        o.steps.clear();
    }
    let (svc2, op2) = (svc.clone(), op.clone());
    // its steps and errors in the language of whoever started it
    let lang = steve_i18n::current();
    tokio::spawn(steve_i18n::scope(lang, async move {
        let r = install(&svc2, &installs, &op2, &address).await;
        let mut o = op2.lock().unwrap();
        match r {
            Ok(node) => {
                o.state = "done";
                o.node_id = Some(node);
                let _ = std::fs::remove_dir_all(installs.key_dir(&o.id));
            }
            Err(e) => {
                tracing::warn!("node install: {e:#}");
                o.state = "failed";
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
    let ssh = Ssh {
        key: installs.key_dir(&id).join("key"),
        proxy: format!("{} {address} 22", sh_quote(&program.to_string_lossy())),
    };

    step(op, &steve_i18n::t!("hub.nodes.install_step_probe"));
    let uname = ssh.run("uname -sm", None).await?;
    let target = target_of(&uname).with_context(|| {
        steve_i18n::t!("hub.nodes.install_unsupported", platform = uname.trim())
    })?;

    step(
        op,
        &steve_i18n::t!("hub.nodes.install_step_fetch", target = target),
    );
    let archive = installer(&installs.version, target).await?;

    step(op, &steve_i18n::t!("hub.nodes.install_step_send"));
    ssh.run(
        &format!(
            "umask 077; d=\"{REMOTE_DIR}\"; rm -rf \"$d\" && mkdir -p \"$d\" && tar xzf - -C \"$d\""
        ),
        Some(&archive),
    )
    .await?;

    step(op, &steve_i18n::t!("hub.nodes.install_step_setup"));
    let code = super::pairing::new_code(&svc.store, &owner, true).await?;
    let out = ssh
        .run(
            &format!(
                "IFS= read -r STEVE_PAIR_CODE && export STEVE_PAIR_CODE && d=\"{REMOTE_DIR}\" && \
                 \"$d/bin/steve\" setup node --hub {} && \"$d/bin/steve\" node start && rm -rf \"$d\"",
                sh_quote(&hub)
            ),
            Some(format!("{code}\n").as_bytes()),
        )
        .await?;
    tracing::debug!("node install: {out}");

    let hash = super::pairing::code_hash(&code);
    let store = svc.store.clone();
    let node = tokio::task::spawn_blocking(move || {
        store.reader(|c| {
            use rusqlite::OptionalExtension;
            c.query_row(
                "SELECT node_id FROM node_pairings WHERE code_hash=? AND claimed_at IS NOT NULL",
                [&hash],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(steve_store::StoreError::from)
        })
    })
    .await??
    .context(steve_i18n::t!("hub.nodes.install_not_paired"))?;
    step(op, &steve_i18n::t!("hub.nodes.install_step_done"));
    Ok(node)
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
    key: PathBuf,
    proxy: String,
}

impl Ssh {
    /// `cmd` on the target with `input` on its stdin; its stdout, or why it failed.
    async fn run(&self, cmd: &str, input: Option<&[u8]>) -> anyhow::Result<String> {
        // STEVE_SSH: another OpenSSH-compatible client (tests stand one in)
        let mut child =
            tokio::process::Command::new(std::env::var_os("STEVE_SSH").unwrap_or("ssh".into()))
                .arg("-i")
                .arg(&self.key)
                .args([
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
                ])
                .arg("-o")
                .arg(format!("ProxyCommand={}", self.proxy))
                .args(["-T", "steve-install", "--", cmd])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| anyhow::anyhow!("ssh: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let input = input.map(<[u8]>::to_vec);
        let writer = tokio::spawn(async move {
            if let Some(b) = input {
                let _ = stdin.write_all(&b).await;
            }
            drop(stdin);
        });
        let out = tokio::time::timeout(Duration::from_secs(15 * 60), child.wait_with_output())
            .await
            .map_err(|_| anyhow::anyhow!("ssh: no answer within 15 minutes"))??;
        let _ = writer.await;
        if !out.status.success() {
            let tail: String = String::from_utf8_lossy(&out.stderr)
                .lines()
                .chain(String::from_utf8_lossy(&out.stdout).lines())
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let tail: String = tail
                .chars()
                .rev()
                .take(800)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            anyhow::bail!(
                "{} ({}): {tail}",
                cmd.split_whitespace().next().unwrap_or("ssh"),
                out.status
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The installer archive of `version` for `target`, checked against the release's manifest.
async fn installer(version: &str, target: &str) -> anyhow::Result<Vec<u8>> {
    use anyhow::Context;
    let v = semver::Version::parse(version)
        .ok()
        .context(steve_i18n::t!(
            "hub.nodes.install_dev_build",
            version = version
        ))?;
    let local = std::env::var_os("STEVE_NODE_INSTALLERS").map(PathBuf::from);
    let base = match &local {
        Some(_) => String::new(),
        None => format!(
            "https://github.com/{}/releases/download/v{v}",
            crate::upgrades::repository()?
        ),
    };
    let fetch = |name: String, limit: u64| {
        let (local, base) = (local.clone(), base.clone());
        async move {
            match local {
                Some(dir) => {
                    let p = dir.join(&name);
                    let b = tokio::fs::read(&p)
                        .await
                        .with_context(|| p.display().to_string())?;
                    anyhow::ensure!(b.len() as u64 <= limit, "{name}: too large");
                    Ok::<_, anyhow::Error>(b)
                }
                None => {
                    let r = reqwest::Client::builder()
                        .timeout(Duration::from_secs(600))
                        .build()?
                        .get(format!("{base}/{name}"))
                        .send()
                        .await?
                        .error_for_status()?;
                    anyhow::ensure!(
                        r.content_length().is_none_or(|n| n <= limit),
                        "{name}: too large"
                    );
                    Ok(r.bytes().await?.to_vec())
                }
            }
        }
    };
    let manifest: Value =
        serde_json::from_slice(&fetch("steve-installer.json".into(), 1 << 20).await?)
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
    let size = asset["size"].as_u64().context("asset size")?;
    let bytes = fetch(name.to_string(), size.min(ARCHIVE_MAX)).await?;
    anyhow::ensure!(
        bytes.len() as u64 == size,
        "{name}: size differs from the manifest"
    );
    let got = hex::encode(Sha256::digest(&bytes));
    anyhow::ensure!(got == sha, "{name}: SHA256 differs from the manifest");
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
}
