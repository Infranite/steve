//! Maintenance of an existing Hub's literal configuration. Installation and identity creation
//! deliberately do not participate in this transaction.
use super::{Options, Prompts, ask_value, browser_url, listen_address, oidc_issuer_url};
use crate::common::*;
use crate::env_file::EnvDocument;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use steve_i18n::t;

const TRANSACTIONS: &str = "setup-reconfigure";
const EDITABLE: &[&str] = &[
    "STEVE_HOST",
    "STEVE_PORT",
    "STEVE_ALLOW_ANY_HOST",
    "STEVE_PUBLIC_URL",
    "STEVE_PUBLIC_URLS",
    "STEVE_OIDC_ISSUER",
    "STEVE_OIDC_CLIENT_ID",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Guard {
    File { sha256: String, mode: u32, uid: u32 },
    Link { target: PathBuf, uid: u32 },
    Directory { mode: u32, uid: u32 },
}

impl Guard {
    fn read(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.uid() == unsafe { libc::getuid() },
            "{}", t!("install.reconfigure.dep.not_owned", path = path.display()));
        let mode = meta.permissions().mode() & 0o777;
        if meta.file_type().is_symlink() {
            return Ok(Self::Link {
                target: fs::read_link(path)?,
                uid: meta.uid(),
            });
        }
        ensure!(
            mode & 0o022 == 0,
            "{}", t!("install.reconfigure.dep.writable_by_others", path = path.display()));
        if meta.is_file() {
            Ok(Self::File {
                sha256: digest(path)?,
                mode,
                uid: meta.uid(),
            })
        } else if meta.is_dir() {
            Ok(Self::Directory {
                mode,
                uid: meta.uid(),
            })
        } else {
            bail!("{}", t!("install.reconfigure.dep.not_regular", path = path.display()))
        }
    }
}

fn guard(guards: &mut BTreeMap<PathBuf, Guard>, path: &Path) -> Result<()> {
    let value = Guard::read(path)?;
    if let Some(previous) = guards.insert(path.into(), value.clone()) {
        ensure!(previous == value, "{}", t!("install.reconfigure.state_changed_reading"));
    }
    Ok(())
}

fn verify_guards(guards: &BTreeMap<PathBuf, Guard>) -> Result<()> {
    for (path, expected) in guards {
        ensure!(
            Guard::read(path)? == *expected,
            "{}", t!("install.reconfigure.changed_elsewhere", path = path.display()));
    }
    Ok(())
}

fn private_bytes(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    // Check the opened descriptor, not a pathname that could have been replaced before open.
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.permissions().mode() & 0o777 == 0o600,
        "{}", t!("install.reconfigure.file_mode", path = path.display()));
    ensure!(metadata.len() <= 1024 * 1024, "{}", t!("install.reconfigure.file_too_large"));
    let mut data = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= 1024 * 1024, "{}", t!("install.reconfigure.file_too_large"));
    Ok(data)
}

fn receipt(bytes: &[u8], home: &Path) -> Result<Value> {
    let record: Value = serde_json::from_slice(bytes).context(t!("install.reconfigure.receipt.not_json"))?;
    ensure!(
        record["format"] == 1
            && record["component"] == "hub"
            && record["home"].as_str() == home.to_str()
            && record["files"].is_object()
            && record["environment"].is_object()
            && record["steps"]
                .as_array()
                .is_some_and(|steps| steps.iter().any(|step| step == "installed")),
        "{}", t!("install.reconfigure.receipt.not_managed_hub"));
    let version = record["version"].as_str().context(t!("install.reconfigure.receipt.version_missing"))?;
    let parsed = semver::Version::parse(version)?;
    let commit = record["commit"].as_str().context(t!("install.reconfigure.receipt.commit_missing"))?;
    ensure!(
        parsed.to_string() == version
            && parsed.pre.is_empty()
            && parsed.build.is_empty()
            && commit.len() == 40
            && commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{}", t!("install.reconfigure.receipt.identity_invalid"));
    ensure!(
        record["files"]["steve.env"].is_string(),
        "{}", t!("install.reconfigure.receipt.env_not_owned"));
    for digest in record["files"].as_object().unwrap().values() {
        ensure!(
            digest.as_str().is_some_and(|value| value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "{}", t!("install.reconfigure.receipt.file_digest_invalid"));
    }
    Ok(record)
}

struct Runtime {
    release: PathBuf,
    label: String,
    guards: BTreeMap<PathBuf, Guard>,
}

fn inspect(home: &Path) -> Result<Runtime> {
    let releases = fs::canonicalize(home.join("releases"))?;
    let current = fs::canonicalize(home.join("current"))?;
    ensure!(
        current.parent() == Some(releases.as_path()),
        "{}", t!("install.reconfigure.current_outside"));
    let mut guards = BTreeMap::new();
    // Validate the managed path and pin its files before the service API executes --build-info.
    for path in [
        home.join("current"),
        current.join("release.json"),
        current.join("bin/steve-hub"),
    ] {
        guard(&mut guards, &path)?;
    }
    let context = crate::service::Service::new("hub", home, None)?.reconfiguration_context()?;
    ensure!(
        context.release == current,
        "{}", t!("install.reconfigure.current_changed_reading"));
    for path in [
        home.to_path_buf(),
        home.join("current"),
        home.join("web/current"),
        releases,
        context.release.clone(),
        context.record_path,
        context.config,
        context.release.join("release.json"),
        context.release.join("web/dist/release.json"),
        context.release.join("bin/steve-hub"),
    ] {
        guard(&mut guards, &path)?;
    }
    if cfg!(target_os = "macos") {
        for path in [
            home.join("signing"),
            home.join("signing/identity.json"),
            home.join("bin/steve-keyhelper"),
        ] {
            guard(&mut guards, &path)?;
        }
    }
    Ok(Runtime {
        release: context.release,
        label: context.label,
        guards,
    })
}

struct Snapshot {
    home: PathBuf,
    env: Vec<u8>,
    receipt: Vec<u8>,
    record: Value,
    document: EnvDocument,
    runtime: Runtime,
}

impl Snapshot {
    fn read(home: &Path, inspect: &impl Fn(&Path) -> Result<Runtime>) -> Result<Self> {
        owned(home, true).context(t!("install.reconfigure.home_missing"))?;
        let receipt_bytes = private_bytes(&home.join("setup.json"))?;
        let record = receipt(&receipt_bytes, home)?;
        let env = private_bytes(&home.join("steve.env"))?;
        let document = EnvDocument::parse(&env)?;
        ensure!(
            !document.values().contains_key("STEVE_OIDC_CLIENT_SECRET"),
            "{}", t!("install.reconfigure.oidc.inline_secret"));
        let mut runtime = inspect(home)?;
        for (name, expected) in record["files"].as_object().unwrap() {
            ensure!(
                !Path::new(name).is_absolute()
                    && Path::new(name)
                        .components()
                        .all(|p| matches!(p, Component::Normal(_))),
                "{}", t!("install.reconfigure.receipt.path_invalid"));
            if name == "steve.env" {
                continue;
            }
            let path = home.join(name);
            owned(&path, false)?;
            ensure!(
                digest(&path)? == expected.as_str().context(t!("install.reconfigure.receipt.file_digest_invalid"))?,
                "{}", t!("install.reconfigure.protected_file_modified", name = name));
            guard(&mut runtime.guards, &path)?;
        }
        let values = document.values();
        if let Some(configured_home) = values.get("STEVE_HOME") {
            ensure!(
                Path::new(configured_home) == home,
                "{}", t!("install.reconfigure.other_home"));
        }
        /* OIDC is optional since 2026-11-09 (a Hub set up without it signs in with access tokens):
         * when it is configured, it is kept whole and maintained as before */
        let oidc = ["STEVE_OIDC_ISSUER", "STEVE_OIDC_CLIENT_ID", "STEVE_OIDC_CLIENT_SECRET_FILE"]
            .iter()
            .any(|k| values.get(*k).is_some_and(|v| !v.is_empty()));
        if oidc {
            for name in ["STEVE_OIDC_ISSUER", "STEVE_OIDC_CLIENT_ID"] {
                ensure!(
                    values.get(name).is_some_and(|v| !v.is_empty()),
                    "{}", t!("install.reconfigure.oidc.field_missing", name = name));
            }
            let secret = Path::new(
                values
                    .get("STEVE_OIDC_CLIENT_SECRET_FILE")
                    .context(t!("install.reconfigure.oidc.secret_by_file"))?,
            );
            ensure!(secret.is_absolute(), "{}", t!("install.reconfigure.oidc.secret_path_relative"));
            owned(secret, false)?;
            guard(&mut runtime.guards, secret)?;
        }
        match values.get("STEVE_VAULT_KEYSTORE").map(String::as_str) {
            Some("dir") => {
                let keys = Path::new(
                    values
                        .get("STEVE_VAULT_KEY_DIR")
                        .context(t!("install.reconfigure.vault.dir_unset"))?,
                );
                ensure!(keys.is_absolute(), "{}", t!("install.reconfigure.vault.dir_relative"));
                owned(keys, true).context(t!("install.reconfigure.vault.dir_missing"))?;
                let resolved = fs::canonicalize(keys)?;
                ensure!(
                    !resolved.starts_with(home) && !home.starts_with(&resolved),
                    "{}", t!("install.reconfigure.vault.dir_inside_home"));
                guard(&mut runtime.guards, keys)?;
            }
            Some("file") => {
                let keys = home.join("vault-keys");
                owned(&keys, true).context(t!("install.reconfigure.vault.keys_missing"))?;
                guard(&mut runtime.guards, &keys)?;
            }
            Some("keychain") if cfg!(target_os = "macos") => {}
            _ => bail!(
                "{}", t!("install.reconfigure.vault.backend_unsupported")),
        }
        let snapshot = Self {
            home: home.into(),
            env,
            receipt: receipt_bytes,
            record,
            document,
            runtime,
        };
        snapshot.verify()?;
        Ok(snapshot)
    }

    fn verify(&self) -> Result<()> {
        verify_guards(&self.runtime.guards)?;
        ensure!(
            private_bytes(&self.home.join("steve.env"))? == self.env
                && private_bytes(&self.home.join("setup.json"))? == self.receipt,
            "{}", t!("install.reconfigure.changed_while_asking"));
        Ok(())
    }
}

struct Listening {
    hosts: String,
    port: String,
    any: bool,
}

fn listening(value: &str) -> Result<Listening> {
    let mut hosts = Vec::new();
    let mut selected_port = None;
    let mut any = false;
    for part in value.split(',') {
        let part = part.trim();
        let (host, port) = if let Some(port) = part.strip_prefix("localhost:") {
            let port: u16 = port.parse().context(t!("install.reconfigure.listen.localhost_port"))?;
            ensure!(port != 0, "{}", t!("install.reconfigure.listen.port_zero"));
            ("localhost".to_owned(), port)
        } else {
            let address = listen_address(part)?;
            any |= address.ip().is_unspecified();
            (address.ip().to_string(), address.port())
        };
        ensure!(
            selected_port.is_none_or(|p| p == port),
            "{}", t!("install.reconfigure.listen.one_port"));
        selected_port = Some(port);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(Listening {
        hosts: hosts.join(","),
        port: selected_port.context(t!("install.reconfigure.listen.empty"))?.to_string(),
        any,
    })
}

fn listen_default(values: &BTreeMap<String, String>) -> Result<String> {
    let port = values
        .get("STEVE_PORT")
        .map(String::as_str)
        .unwrap_or("8440");
    let hosts = values
        .get("STEVE_HOST")
        .map(String::as_str)
        .unwrap_or("127.0.0.1");
    let mut result = Vec::new();
    for host in hosts.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let host = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host);
        result.push(if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        });
    }
    if result.is_empty() {
        result.push(format!("127.0.0.1:{port}"));
    }
    Ok(result.join(","))
}

fn value<'a>(values: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    values.get(key).map(String::as_str).unwrap_or("")
}

fn changes(
    snapshot: &Snapshot,
    options: &Options,
    input: &mut dyn Prompts,
) -> Result<BTreeMap<String, String>> {
    let current = snapshot.document.values();
    let mut changes = BTreeMap::new();
    let old_listen = listen_default(current)?;
    let selected = match &options.listen {
        Some(explicit) => explicit.clone(),
        None => ask_value(
            input,
            &t!("install.reconfigure.listen.question"),
            Some(&old_listen),
            false,
            |answer| {
                listening(answer)?;
                Ok(answer.to_owned())
            },
        )?,
    };
    // A default answer preserves the operator's original spelling and whitespace.
    if selected != old_listen {
        let parsed = listening(&selected)?;
        changes.insert("STEVE_HOST".into(), parsed.hosts);
        changes.insert("STEVE_PORT".into(), parsed.port);
        changes.insert(
            "STEVE_ALLOW_ANY_HOST".into(),
            if parsed.any { "1" } else { "0" }.into(),
        );
    }
    let old_public = value(current, "STEVE_PUBLIC_URL");
    let public = ask_value(
        input,
        &t!("install.reconfigure.label.browser_address"),
        Some(old_public),
        false,
        |answer| {
            let parsed = browser_url(answer, &t!("install.reconfigure.label.access_address"))?;
            Ok(if answer == old_public {
                answer.to_owned()
            } else {
                parsed
            })
        },
    )?;
    if public != old_public {
        changes.insert("STEVE_PUBLIC_URL".into(), public.clone());
        if let Some(urls) = current.get("STEVE_PUBLIC_URLS") {
            let old_origin = old_public.trim_end_matches('/');
            let mut entries = vec![public.clone()];
            for entry in urls.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                if entry.trim_end_matches('/') != old_origin
                    && !entries
                        .iter()
                        .any(|v| v.trim_end_matches('/') == entry.trim_end_matches('/'))
                {
                    entries.push(entry.to_owned());
                }
            }
            changes.insert("STEVE_PUBLIC_URLS".into(), entries.join(","));
        }
    }
    let old_issuer = value(current, "STEVE_OIDC_ISSUER");
    if old_issuer.is_empty() {
        /* no OIDC to maintain: signing in is by access token */
        return Ok(changes);
    }
    let issuer = ask_value(input, "OIDC issuer", Some(old_issuer), false, |answer| {
        let parsed = oidc_issuer_url(answer)?;
        Ok(if answer == old_issuer {
            answer.to_owned()
        } else {
            parsed
        })
    })?;
    if issuer != old_issuer {
        changes.insert("STEVE_OIDC_ISSUER".into(), issuer);
    }
    let old_client = value(current, "STEVE_OIDC_CLIENT_ID");
    let client = input.line("OIDC client ID", Some(old_client), false)?;
    if client != old_client {
        changes.insert("STEVE_OIDC_CLIENT_ID".into(), client);
    }
    Ok(changes)
}

fn updated_receipt(original: &Value, candidate: &[u8]) -> Result<Value> {
    let document = EnvDocument::parse(candidate)?;
    ensure!(
        !document.values().contains_key("STEVE_OIDC_CLIENT_SECRET"),
        "{}", t!("install.reconfigure.oidc.inline_secret_receipt"));
    let mut next = original.clone();
    next["environment"] = serde_json::to_value(document.values())?;
    next["files"]["steve.env"] = bytes_digest(candidate).into();
    Ok(next)
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn confirm(input: &mut dyn Prompts, title: &str) -> Result<bool> {
    if input.rich() {
        return input.confirm(title.trim_end_matches("[y/N]").trim_end(), false);
    }
    let answer = ask_value(input, title, Some("N"), false, |answer| {
        match answer.to_ascii_lowercase().as_str() {
            "y" | "yes" => Ok(true),
            "n" | "no" => Ok(false),
            _ => bail!("{}", t!("install.reconfigure.answer_yes_no")),
        }
    })?;
    Ok(answer)
}

fn locks(home: &Path) -> Result<(File, File)> {
    let path = home.join(".setup.lock");
    let setup = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    setup
        .try_lock()
        .context(t!("install.reconfigure.busy"))?;
    owned(&path, false)?;
    Ok((setup, crate::release::lock_release(home)?))
}

fn check(runtime: &Runtime, home: &Path, values: &BTreeMap<String, String>) -> Result<()> {
    let mut command = Command::new(runtime.release.join("bin/steve-hub"));
    command
        .arg("--check-config")
        .env_clear()
        .stdin(Stdio::null());
    for name in ["HOME", "PATH", "USER", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (key, value) in values {
        if key.starts_with("STEVE_") && key != "STEVE_OIDC_CLIENT_SECRET" {
            command.env(key, value);
        }
    }
    command.env("STEVE_HOME", home);
    let output = crate::signing::output(&mut command, Duration::from_secs(30))?;
    let report: Value =
        serde_json::from_slice(&output.stdout).context(t!("install.reconfigure.check.no_result"))?;
    ensure!(
        output.status.success() && report["ok"] == true,
        "{}", t!("install.reconfigure.check.rejected"));
    if report["warnings"]
        .as_array()
        .is_some_and(|warnings| !warnings.is_empty())
    {
        crate::ui::info(&t!("install.reconfigure.check.passed_with_notes"));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Journal {
    format: u32,
    home: PathBuf,
    id: String,
    phase: String,
    before_env: String,
    after_env: String,
    before_receipt: String,
    after_receipt: String,
    guards: BTreeMap<PathBuf, Guard>,
}

struct Pending {
    directory: PathBuf,
    journal: Journal,
    before_env: Vec<u8>,
    after_env: Vec<u8>,
    before_receipt: Vec<u8>,
    after_receipt: Vec<u8>,
}

fn pending_path(home: &Path) -> PathBuf {
    home.join(TRANSACTIONS).join("pending")
}

fn verify_transition(
    home: &Path,
    before_env: &[u8],
    after_env: &[u8],
    before_receipt: &[u8],
    after_receipt: &[u8],
) -> Result<()> {
    let before = EnvDocument::parse(before_env)?;
    let after = EnvDocument::parse(after_env)?;
    for key in before.values().keys().chain(after.values().keys()) {
        ensure!(
            EDITABLE.contains(&key.as_str()) || before.values().get(key) == after.values().get(key),
            "{}", t!("install.reconfigure.txn.unsupported_field"));
    }
    let original = receipt(before_receipt, home)?;
    let candidate = receipt(after_receipt, home)?;
    ensure!(
        updated_receipt(&original, after_env)? == candidate,
        "{}", t!("install.reconfigure.txn.identity_changed"));
    Ok(())
}

impl Pending {
    fn prepare(snapshot: &Snapshot, env: &[u8], receipt: &[u8]) -> Result<Self> {
        verify_transition(
            &snapshot.home,
            &snapshot.env,
            env,
            &snapshot.receipt,
            receipt,
        )?;
        snapshot.verify()?;
        let base = snapshot.home.join(TRANSACTIONS);
        private_dir(&base)?;
        ensure!(
            !exists(&pending_path(&snapshot.home)),
            "{}", t!("install.reconfigure.txn.pending"));
        let id = uuid::Uuid::new_v4().to_string();
        let staging = base.join(format!(".prepare-{id}"));
        private_dir(&staging)?;
        let journal = Journal {
            format: 1,
            home: snapshot.home.clone(),
            id,
            phase: "prepared".into(),
            before_env: bytes_digest(&snapshot.env),
            after_env: bytes_digest(env),
            before_receipt: bytes_digest(&snapshot.receipt),
            after_receipt: bytes_digest(receipt),
            guards: snapshot.runtime.guards.clone(),
        };
        for (name, bytes) in [
            ("before.env", snapshot.env.as_slice()),
            ("after.env", env),
            ("before-setup.json", snapshot.receipt.as_slice()),
            ("after-setup.json", receipt),
        ] {
            private_file(&staging.join(name), bytes)?;
        }
        private_file(
            &staging.join("journal.json"),
            &serde_json::to_vec_pretty(&journal)?,
        )?;
        File::open(&staging)?.sync_all()?;
        // Before this durable rename, a crash can only leave an unused preparation directory.
        fs::rename(&staging, pending_path(&snapshot.home))?;
        File::open(&base)?.sync_all()?;
        Self::load(&snapshot.home)
    }

    fn load(home: &Path) -> Result<Self> {
        let directory = pending_path(home);
        owned(&home.join(TRANSACTIONS), true)?;
        owned(&directory, true)?;
        let journal: Journal =
            serde_json::from_slice(&private_bytes(&directory.join("journal.json"))?)?;
        ensure!(
            journal.format == 1
                && journal.home == home
                && uuid::Uuid::parse_str(&journal.id).is_ok_and(|id| id.to_string() == journal.id)
                && matches!(
                    journal.phase.as_str(),
                    "prepared" | "committed" | "rolled_back"
                ),
            "{}", t!("install.reconfigure.txn.record_invalid"));
        let before_env = private_bytes(&directory.join("before.env"))?;
        let after_env = private_bytes(&directory.join("after.env"))?;
        let before_receipt = private_bytes(&directory.join("before-setup.json"))?;
        let after_receipt = private_bytes(&directory.join("after-setup.json"))?;
        ensure!(
            bytes_digest(&before_env) == journal.before_env
                && bytes_digest(&after_env) == journal.after_env
                && bytes_digest(&before_receipt) == journal.before_receipt
                && bytes_digest(&after_receipt) == journal.after_receipt,
            "{}", t!("install.reconfigure.txn.backup_modified"));
        verify_transition(
            home,
            &before_env,
            &after_env,
            &before_receipt,
            &after_receipt,
        )?;
        Ok(Self {
            directory,
            journal,
            before_env,
            after_env,
            before_receipt,
            after_receipt,
        })
    }

    fn archive(mut self, phase: &str) -> Result<PathBuf> {
        self.journal.phase = phase.into();
        atomic(
            &self.directory.join("journal.json"),
            &serde_json::to_vec_pretty(&self.journal)?,
        )?;
        let archive = self.directory.parent().unwrap().join(&self.journal.id);
        ensure!(!exists(&archive), "{}", t!("install.reconfigure.txn.archive_exists"));
        fs::rename(&self.directory, &archive)?;
        File::open(archive.parent().unwrap())?
            .sync_all()
            .with_context(|| {
                t!("install.reconfigure.txn.archive_sync_failed", archive = archive.display())
            })?;
        Ok(archive)
    }
}

fn replace_if(path: &Path, expected: &[u8], candidate: &[u8]) -> Result<()> {
    // The setup/release locks serialize Steve operations. These extra checks catch edits made
    // outside those locks; ordinary filesystems do not offer conditional rename against such
    // writers, so this is intentionally not described as a filesystem-wide atomic CAS.
    ensure!(
        private_bytes(path)? == expected,
        "{}", t!("install.reconfigure.txn.file_changed", path = path.display()));
    if expected != candidate {
        atomic(path, candidate)?;
    }
    ensure!(
        private_bytes(path)? == candidate,
        "{}", t!("install.reconfigure.txn.readback_mismatch", path = path.display()));
    Ok(())
}

fn apply(pending: Pending, after_write: impl Fn(&str) -> Result<()>) -> Result<PathBuf> {
    let home = &pending.journal.home;
    verify_guards(&pending.journal.guards)?;
    ensure!(
        private_bytes(&home.join("setup.json"))? == pending.before_receipt,
        "{}", t!("install.reconfigure.txn.receipt_changed"));
    replace_if(
        &home.join("steve.env"),
        &pending.before_env,
        &pending.after_env,
    )?;
    after_write("env")?;
    verify_guards(&pending.journal.guards)?;
    ensure!(
        private_bytes(&home.join("steve.env"))? == pending.after_env,
        "{}", t!("install.reconfigure.txn.env_changed"));
    replace_if(
        &home.join("setup.json"),
        &pending.before_receipt,
        &pending.after_receipt,
    )?;
    after_write("receipt")?;
    verify_guards(&pending.journal.guards)?;
    ensure!(
        private_bytes(&home.join("steve.env"))? == pending.after_env
            && private_bytes(&home.join("setup.json"))? == pending.after_receipt,
        "{}", t!("install.reconfigure.txn.changed_during_commit"));
    pending.archive("committed")
}

fn recover(home: &Path) -> Result<(PathBuf, bool)> {
    let pending = Pending::load(home)?;
    verify_guards(&pending.journal.guards)?;
    let env_path = home.join("steve.env");
    let receipt_path = home.join("setup.json");
    let env = private_bytes(&env_path)?;
    let receipt = private_bytes(&receipt_path)?;
    if pending.journal.phase == "committed" {
        ensure!(
            env == pending.after_env && receipt == pending.after_receipt,
            "{}", t!("install.reconfigure.txn.committed_changed"));
        return Ok((pending.archive("committed")?, true));
    }
    ensure!(
        (env == pending.before_env || env == pending.after_env)
            && (receipt == pending.before_receipt || receipt == pending.after_receipt),
        "{}", t!("install.reconfigure.txn.external_edit"));
    replace_if(&env_path, &env, &pending.before_env)?;
    verify_guards(&pending.journal.guards)?;
    ensure!(
        private_bytes(&env_path)? == pending.before_env,
        "{}", t!("install.reconfigure.txn.env_changed_recovering"));
    replace_if(&receipt_path, &receipt, &pending.before_receipt)?;
    verify_guards(&pending.journal.guards)?;
    Ok((pending.archive("rolled_back")?, false))
}

pub(super) fn run(options: &Options, input: &mut dyn Prompts) -> Result<()> {
    crate::ui::intro(&t!("install.reconfigure.intro"));
    run_with(options, input, inspect, check)?;
    crate::ui::outro("");
    Ok(())
}

fn run_with(
    options: &Options,
    input: &mut dyn Prompts,
    inspect: impl Fn(&Path) -> Result<Runtime>,
    check: impl Fn(&Runtime, &Path, &BTreeMap<String, String>) -> Result<()>,
) -> Result<()> {
    ensure!(
        options.role.as_deref().is_none_or(|role| role == "hub")
            && options.hub.is_none()
            && options.pin.is_none(),
        "{}", t!("install.reconfigure.hub_only"));
    let home = canonical(&options.home.clone().unwrap_or(user_home()?.join(".steve")))?;
    owned(&home, true).context(t!("install.reconfigure.no_home"))?;
    if exists(&pending_path(&home)) {
        let pending = Pending::load(&home)?;
        crate::ui::info(&t!("install.reconfigure.pending_found", pending = pending.directory.display()));
        if !confirm(input, &t!("install.reconfigure.recover_question"))? {
            crate::ui::info(&t!("install.reconfigure.recover_cancelled"));
            return Ok(());
        }
        let _locks = locks(&home)?;
        // Also recheck the actual installation, rather than trusting paths in a journal alone.
        inspect(&home)?;
        let (archive, committed) = recover(&home)?;
        crate::ui::info(&t!("install.reconfigure.recovered", outcome = if committed {
                t!("install.reconfigure.recovered_committed")
            } else {
                t!("install.reconfigure.recovered_reverted")
            }, archive = archive.display()));
        return Ok(());
    }
    let snapshot = Snapshot::read(&home, &inspect)?;
    crate::ui::info(&t!("install.reconfigure.scope"));
    let changes = changes(&snapshot, options, input)?;
    let candidate = snapshot.document.update(&changes)?;
    let record = updated_receipt(&snapshot.record, &candidate)?;
    let candidate_receipt = if record == snapshot.record {
        snapshot.receipt.clone()
    } else {
        json_bytes(&record)?
    };
    snapshot.verify()?;
    if candidate == snapshot.env && candidate_receipt == snapshot.receipt {
        crate::ui::info(&t!("install.reconfigure.nothing_to_do"));
        return Ok(());
    }
    crate::ui::info(&t!("install.reconfigure.about_to_save"));
    for (name, new) in &changes {
        if snapshot.document.values().get(name) != Some(new) {
            crate::ui::info(&format!("  {name} = {new}"));
        }
    }
    if changes.is_empty() {
        crate::ui::info(&t!("install.reconfigure.sync_receipt_only"));
    }
    crate::ui::info(&t!("install.reconfigure.secret_kept"));
    if !confirm(input, &t!("install.reconfigure.save_question"))? {
        crate::ui::info(&t!("install.reconfigure.cancelled"));
        return Ok(());
    }
    let _locks = locks(&home)?;
    snapshot.verify()?;
    let verified_runtime = inspect(&home)?;
    ensure!(
        verified_runtime.release == snapshot.runtime.release
            && verified_runtime.label == snapshot.runtime.label
            && verified_runtime.guards.iter().all(|(path, guard)| snapshot
                .runtime
                .guards
                .get(path)
                == Some(guard)),
        "{}", t!("install.reconfigure.state_changed_asking"));
    let values = EnvDocument::parse(&candidate)?;
    check(&snapshot.runtime, &home, values.values())?;
    snapshot.verify()?;
    let pending = Pending::prepare(&snapshot, &candidate, &candidate_receipt)?;
    let archive = match apply(pending, |_| Ok(())) {
        Ok(archive) => archive,
        Err(error) => {
            return match recover(&home) {
                Ok((archive, false)) => Err(error.context(t!("install.reconfigure.save_reverted", archive = archive.display()))),
                Ok((archive, true)) => Err(error.context(t!("install.reconfigure.committed_archived", archive = archive.display()))),
                Err(recovery) => Err(error.context(t!("install.reconfigure.write_unfinished", pending = pending_path(&home).display(), recovery = format!("{:#}", recovery)))),
            };
        }
    };
    crate::ui::info(&t!("install.reconfigure.saved", archive = archive.display()));
    crate::ui::info(&t!("install.reconfigure.restart_hint", home = shell_quote(home.to_str().context(t!("install.reconfigure.path_not_utf8"))?)?, label = shell_quote(&snapshot.runtime.label)?));
    crate::ui::info(&t!("install.reconfigure.verify_later"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::os::unix::fs::symlink;

    struct Answers {
        values: VecDeque<String>,
        calls: usize,
        edit: Option<Box<dyn FnMut(usize)>>,
    }
    impl Answers {
        fn new(values: &[&str]) -> Self {
            Self {
                values: values.iter().map(|v| v.to_string()).collect(),
                calls: 0,
                edit: None,
            }
        }
    }
    impl Prompts for Answers {
        fn interactive(&self) -> bool {
            true
        }
        fn line(&mut self, _: &str, default: Option<&str>, _: bool) -> Result<String> {
            self.calls += 1;
            if let Some(edit) = &mut self.edit {
                edit(self.calls);
            }
            let value = self.values.pop_front().context("fixture cancelled input")?;
            Ok(if value.is_empty() {
                default.unwrap_or("").to_owned()
            } else {
                value
            })
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        home: PathBuf,
    }
    fn fixture_inspect(home: &Path) -> Result<Runtime> {
        let release = home.join("releases/1.0.0");
        let mut guards = BTreeMap::new();
        for path in [
            home.to_path_buf(),
            home.join("current"),
            home.join("web/current"),
            home.join("services/hub.json"),
            home.join("service-definition"),
            home.join("signing/identity.json"),
            home.join("bin/steve-keyhelper"),
            release.join("bin/steve-hub"),
            release.join("release.json"),
        ] {
            guard(&mut guards, &path)?;
        }
        Ok(Runtime {
            release,
            label: "test.custom.hub".into(),
            guards,
        })
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home with 'quote");
            private_dir(&home).unwrap();
            // macOS tempdir may spell this /var while the production entry resolves /private/var.
            let home = fs::canonicalize(&home).unwrap();
            for path in [
                "secrets",
                "services",
                "signing",
                "bin",
                "data",
                "vault-keys",
                "web",
                "releases/1.0.0/bin",
            ] {
                private_dir(&home.join(path)).unwrap();
            }
            for (name, bytes) in [
                ("secrets/oidc-client-secret", "fixture-private-secret"),
                ("services/hub.json", "{\"component\":\"hub\"}"),
                ("service-definition", "fixture registered service"),
                ("signing/identity.json", "{\"identity\":\"original\"}"),
                ("bin/steve-keyhelper", "original helper"),
                ("data/steve.sqlite3", "original data"),
                ("releases/1.0.0/bin/steve-hub", "original installed binary"),
                ("releases/1.0.0/release.json", "{}"),
            ] {
                private_file(&home.join(name), bytes.as_bytes()).unwrap();
            }
            symlink(home.join("releases/1.0.0"), home.join("current")).unwrap();
            symlink(
                home.join("releases/1.0.0/web/dist"),
                home.join("web/current"),
            )
            .unwrap();
            let env = format!(
                "# keep operator formatting\nSTEVE_HOST='127.0.0.1'\nSTEVE_PORT='8440'\nSTEVE_PUBLIC_URL='https://old.example'\nSTEVE_OIDC_ISSUER='https://issuer.example/tenant/'\nSTEVE_OIDC_CLIENT_ID='existing-client'\nSTEVE_OIDC_CLIENT_SECRET_FILE={}\nSTEVE_VAULT_KEYSTORE='file'\nSTEVE_CUSTOM='unchanged # literal' # retain comment\n",
                shell_quote(home.join("secrets/oidc-client-secret").to_str().unwrap()).unwrap()
            );
            private_file(&home.join("steve.env"), env.as_bytes()).unwrap();
            let record = json!({"format":1,"component":"hub","home":home,"version":"1.0.0","commit":"a".repeat(40),
                "steps":["copy","keychain","installed"],"files":{"steve.env":bytes_digest(env.as_bytes()),
                    "secrets/oidc-client-secret":digest(&home.join("secrets/oidc-client-secret")).unwrap()},
                "environment":EnvDocument::parse(env.as_bytes()).unwrap().values(),
                "identity_sha256":"unchanged fixture identity","custom_receipt_field":{"preserve":true}});
            private_file(&home.join("setup.json"), &json_bytes(&record).unwrap()).unwrap();
            Self { _root: root, home }
        }
        fn options(&self) -> Options {
            Options {
                role: Some("hub".into()),
                home: Some(self.home.clone()),
                reconfigure: true,
                ..Default::default()
            }
        }
        fn run(&self, answers: &mut Answers) -> Result<()> {
            run_with(&self.options(), answers, fixture_inspect, |_, _, _| Ok(()))
        }
        fn snapshot(&self) -> Snapshot {
            Snapshot::read(&self.home, &fixture_inspect).unwrap()
        }
        fn pending(&self) -> Pending {
            let snapshot = self.snapshot();
            let env = snapshot
                .document
                .update(&BTreeMap::from([(
                    "STEVE_PUBLIC_URL".into(),
                    "https://new.example".into(),
                )]))
                .unwrap();
            let receipt = json_bytes(&updated_receipt(&snapshot.record, &env).unwrap()).unwrap();
            Pending::prepare(&snapshot, &env, &receipt).unwrap()
        }
    }

    fn tree(home: &Path) -> BTreeMap<PathBuf, Guard> {
        let mut all = BTreeMap::new();
        let mut pending = vec![home.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
            all.insert(
                path.strip_prefix(home).unwrap().into(),
                Guard::read(&path).unwrap(),
            );
        }
        all
    }

    #[test]
    fn unknown_homes_node_and_cancelled_prompts_never_create_or_change_files() {
        let root = tempfile::tempdir().unwrap();
        let unknown = root.path().join("unknown");
        let options = Options {
            home: Some(unknown.clone()),
            reconfigure: true,
            ..Default::default()
        };
        assert!(
            run_with(
                &options,
                &mut Answers::new(&[]),
                fixture_inspect,
                |_, _, _| Ok(())
            )
            .is_err()
        );
        assert!(!exists(&unknown));
        private_dir(&unknown).unwrap();
        let before = tree(&unknown);
        assert!(
            run_with(
                &options,
                &mut Answers::new(&[]),
                fixture_inspect,
                |_, _, _| Ok(())
            )
            .is_err()
        );
        assert_eq!(tree(&unknown), before);
        let fixture = Fixture::new();
        let before = tree(&fixture.home);
        let mut options = fixture.options();
        options.role = Some("node".into());
        assert!(
            run_with(
                &options,
                &mut Answers::new(&[]),
                fixture_inspect,
                |_, _, _| Ok(())
            )
            .unwrap_err()
            .to_string()
            .contains("Node")
        );
        assert_eq!(tree(&fixture.home), before);
        for supplied in 0..5 {
            let answers = ["", "https://new.example", "", "", "y"];
            assert!(
                fixture
                    .run(&mut Answers::new(&answers[..supplied]))
                    .is_err()
            );
            assert_eq!(
                tree(&fixture.home),
                before,
                "cancelled after {supplied} inputs"
            );
        }
        fixture
            .run(&mut Answers::new(&["", "https://new.example", "", "", ""]))
            .unwrap();
        assert_eq!(tree(&fixture.home), before);
    }

    #[test]
    fn edited_environment_is_the_baseline_and_secret_identity_and_service_are_preserved() {
        let fixture = Fixture::new();
        let env_path = fixture.home.join("steve.env");
        let original = fs::read_to_string(&env_path).unwrap();
        fs::write(
            &env_path,
            original.replace("existing-client", "operator-client")
                + "STEVE_PUBLIC_URLS='https://old.example,https://other.example'\n",
        )
        .unwrap();
        let before = fixture.snapshot();
        fixture
            .run(&mut Answers::new(&[
                "",
                "https://new.example/",
                "",
                "",
                "y",
            ]))
            .unwrap();
        let after = fixture.snapshot();
        assert_eq!(
            after.document.values()["STEVE_OIDC_CLIENT_ID"],
            "operator-client"
        );
        assert_eq!(
            after.document.values()["STEVE_PUBLIC_URLS"],
            "https://new.example,https://other.example"
        );
        assert_eq!(
            after.document.values()["STEVE_OIDC_ISSUER"],
            "https://issuer.example/tenant/"
        );
        assert_eq!(
            after.document.values()["STEVE_OIDC_CLIENT_SECRET_FILE"],
            before.document.values()["STEVE_OIDC_CLIENT_SECRET_FILE"]
        );
        assert!(
            String::from_utf8(after.env.clone())
                .unwrap()
                .contains("STEVE_CUSTOM='unchanged # literal' # retain comment")
        );
        verify_guards(&before.runtime.guards).unwrap();
        let mut preserved = after.record.clone();
        preserved["environment"] = before.record["environment"].clone();
        preserved["files"]["steve.env"] = before.record["files"]["steve.env"].clone();
        assert_eq!(preserved, before.record);
        assert_eq!(after.record["files"]["steve.env"], bytes_digest(&after.env));
        let no_op = tree(&fixture.home);
        fixture.run(&mut Answers::new(&["", "", "", ""])).unwrap();
        assert_eq!(
            tree(&fixture.home),
            no_op,
            "second maintenance must be a complete no-op"
        );
    }

    #[test]
    fn default_answers_preserve_multilistener_spelling_and_sync_only_a_stale_receipt() {
        let fixture = Fixture::new();
        let path = fixture.home.join("steve.env");
        let env = fs::read_to_string(&path).unwrap().replace(
            "STEVE_HOST='127.0.0.1'",
            "STEVE_HOST='127.0.0.1, ::1, localhost'",
        );
        fs::write(&path, &env).unwrap();
        fixture
            .run(&mut Answers::new(&["", "", "", "", "y"]))
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), env);
        let snapshot = fixture.snapshot();
        let before = tree(&fixture.home);
        fixture.run(&mut Answers::new(&["", "", "", ""])).unwrap();
        assert_eq!(tree(&fixture.home), before);
        let parsed = listening("[::1]:8440,127.0.0.1:8440,localhost:8440").unwrap();
        assert_eq!(parsed.hosts, "::1,127.0.0.1,localhost");
        assert!(!parsed.any);
        assert!(listening("127.0.0.1:8440,[::1]:8441").is_err());
        assert!(listening("host.example:8440").is_err());
        assert!(listening("localhost:0").is_err());
        assert_eq!(
            listen_default(&BTreeMap::from([("STEVE_HOST".into(), " , ".into())])).unwrap(),
            "127.0.0.1:8440"
        );
        assert_eq!(
            snapshot.document.values()["STEVE_HOST"],
            "127.0.0.1, ::1, localhost"
        );
    }

    #[test]
    fn failed_preflight_missing_keystore_and_external_edits_preserve_live_configuration() {
        let fixture = Fixture::new();
        let before = fixture.snapshot();
        let result = run_with(
            &fixture.options(),
            &mut Answers::new(&["", "https://new.example", "", "", "y"]),
            fixture_inspect,
            |_, _, _| bail!("fixture bad config"),
        );
        assert!(result.is_err());
        before.verify().unwrap();
        assert!(!exists(&pending_path(&fixture.home)));
        assert!(!exists(&fixture.home.join(TRANSACTIONS)));
        let path = fixture.home.join("steve.env");
        let external = fs::read_to_string(&path)
            .unwrap()
            .replace("existing-client", "external-change");
        let mut answers = Answers::new(&["", "https://new.example", "", "", "y"]);
        let edited = external.clone();
        let edited_path = path.clone();
        answers.edit = Some(Box::new(move |call| {
            if call == 4 {
                fs::write(&edited_path, &edited).unwrap();
            }
        }));
        assert!(fixture.run(&mut answers).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        assert_eq!(
            fs::read(fixture.home.join("setup.json")).unwrap(),
            before.receipt
        );
        let missing_keys = fixture._root.path().join("missing-keys");
        fs::write(
            &path,
            external.replace(
                "STEVE_VAULT_KEYSTORE='file'",
                &format!(
                    "STEVE_VAULT_KEYSTORE='dir'\nSTEVE_VAULT_KEY_DIR={}",
                    shell_quote(missing_keys.to_str().unwrap()).unwrap()
                ),
            ),
        )
        .unwrap();
        assert!(fixture.run(&mut Answers::new(&[])).is_err());
        assert!(!exists(&missing_keys));
    }

    #[test]
    fn interrupted_writes_recover_original_bytes_and_committed_archives_are_private() {
        for fail_at in ["env", "receipt"] {
            let fixture = Fixture::new();
            let before = fixture.snapshot();
            let pending = fixture.pending();
            assert!(
                apply(pending, |stage| {
                    if stage == fail_at {
                        bail!("fixture crash");
                    }
                    Ok(())
                })
                .is_err()
            );
            let (archive, committed) = recover(&fixture.home).unwrap();
            assert!(!committed);
            before.verify().unwrap();
            assert_eq!(
                read_json(&archive.join("journal.json")).unwrap()["phase"],
                "rolled_back"
            );
            assert!(!exists(&pending_path(&fixture.home)));
            let pending = fixture.pending();
            let archive = apply(pending, |_| Ok(())).unwrap();
            owned(&archive, true).unwrap();
            for entry in fs::read_dir(&archive).unwrap() {
                owned(&entry.unwrap().path(), false).unwrap();
            }
            assert_eq!(
                read_json(&archive.join("journal.json")).unwrap()["phase"],
                "committed"
            );
            fixture.snapshot().verify().unwrap();
        }
    }

    #[test]
    fn recovery_refuses_external_edits_tampered_backups_and_identity_changes() {
        let fixture = Fixture::new();
        let original_receipt = private_bytes(&fixture.home.join("setup.json")).unwrap();
        let pending = fixture.pending();
        assert!(apply(pending, |_| bail!("fixture crash")).is_err());
        let env = fixture.home.join("steve.env");
        fs::write(&env, b"external edit\n").unwrap();
        assert!(recover(&fixture.home).is_err());
        assert_eq!(fs::read(&env).unwrap(), b"external edit\n");
        assert_eq!(
            fs::read(fixture.home.join("setup.json")).unwrap(),
            original_receipt
        );
        assert!(exists(&pending_path(&fixture.home)));
        let fixture = Fixture::new();
        let before = fixture.snapshot();
        let pending = fixture.pending();
        fs::write(pending.directory.join("before.env"), b"tampered backup").unwrap();
        assert!(recover(&fixture.home).is_err());
        before.verify().unwrap();
        let fixture = Fixture::new();
        let pending = fixture.pending();
        let identity = fixture.home.join("signing/identity.json");
        fs::write(&identity, b"external identity").unwrap();
        assert!(apply(pending, |_| Ok(())).is_err());
        assert!(recover(&fixture.home).is_err());
        assert_eq!(fs::read(identity).unwrap(), b"external identity");
    }

    #[test]
    fn pending_recovery_requires_confirmation_and_never_continues_to_new_configuration() {
        let fixture = Fixture::new();
        let before = fixture.snapshot();
        let pending = fixture.pending();
        assert!(apply(pending, |_| bail!("fixture crash")).is_err());
        let interrupted = tree(&fixture.home);
        fixture.run(&mut Answers::new(&[""])).unwrap();
        assert_eq!(tree(&fixture.home), interrupted);
        let mut answers = Answers::new(&["y"]);
        fixture.run(&mut answers).unwrap();
        assert_eq!(answers.calls, 1);
        before.verify().unwrap();
        assert!(!exists(&pending_path(&fixture.home)));
    }

    #[test]
    fn setup_lock_prevents_concurrent_maintenance_and_current_changes_fail_cas() {
        let fixture = Fixture::new();
        let before = fixture.snapshot();
        let held = locks(&fixture.home).unwrap();
        assert!(
            fixture
                .run(&mut Answers::new(&["", "https://new.example", "", "", "y"]))
                .is_err()
        );
        before.verify().unwrap();
        drop(held);
        let pending = fixture.pending();
        let current = fixture.home.join("current");
        fs::remove_file(&current).unwrap();
        symlink(fixture.home.join("releases/other"), &current).unwrap();
        assert!(apply(pending, |_| Ok(())).is_err());
        assert_eq!(
            fs::read(fixture.home.join("steve.env")).unwrap(),
            before.env
        );
        assert_eq!(
            fs::read(fixture.home.join("setup.json")).unwrap(),
            before.receipt
        );
    }

    #[test]
    fn private_reads_refuse_links_nonprivate_files_and_oversized_configuration() {
        let fixture = Fixture::new();
        let path = fixture.home.join("private-read-fixture");
        private_file(&path, b"safe").unwrap();
        assert_eq!(private_bytes(&path).unwrap(), b"safe");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_bytes(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(private_bytes(&path).is_err());
        let link = fixture.home.join("private-read-link");
        symlink(&path, &link).unwrap();
        assert!(private_bytes(&link).is_err());
        assert!(private_bytes(&fixture.home).is_err());
    }

    #[test]
    fn inline_secrets_and_unproven_keystore_backends_are_refused_without_writes() {
        for replacement in [
            "STEVE_VAULT_KEYSTORE='auto'",
            "STEVE_VAULT_KEYSTORE='unrecognized-backend'",
            "# no explicit Vault backend",
            "STEVE_VAULT_KEYSTORE='file'\nSTEVE_OIDC_CLIENT_SECRET='inline-private-sentinel'",
        ] {
            let fixture = Fixture::new();
            let path = fixture.home.join("steve.env");
            let env = fs::read_to_string(&path)
                .unwrap()
                .replace("STEVE_VAULT_KEYSTORE='file'", replacement);
            fs::write(&path, env).unwrap();
            let before = tree(&fixture.home);
            let error = fixture.run(&mut Answers::new(&[])).unwrap_err().to_string();
            assert!(!error.contains("inline-private-sentinel"));
            assert_eq!(tree(&fixture.home), before);
            assert!(
                !String::from_utf8(fs::read(fixture.home.join("setup.json")).unwrap())
                    .unwrap()
                    .contains("inline-private-sentinel")
            );
        }
    }
}
