//! Local signing of a verified private release copy. Passwords are supplied by the
//! signed Hub runner, never discovered by the ordinary installer CLI.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use zeroize::Zeroizing;

/// Upper bounds for native maintenance / signing helper subprocesses.
const BUILD_INFO_TIMEOUT: Duration = Duration::from_secs(15);
const CODESIGN_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
const CODESIGN_SIGN_TIMEOUT: Duration = Duration::from_secs(60);
const SECURITY_TIMEOUT: Duration = Duration::from_secs(30);
const OPENSSL_GENERATE_TIMEOUT: Duration = Duration::from_secs(60);
const OPENSSL_FINGERPRINT_TIMEOUT: Duration = Duration::from_secs(15);
const OPENSSL_EXPORT_TIMEOUT: Duration = Duration::from_secs(30);
const PROVISION_SIGNING_TIMEOUT: Duration = Duration::from_secs(30);
const SIGN_SETUP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct Tools {
    pub codesign: PathBuf,
    pub security: PathBuf,
    pub openssl: PathBuf,
    pub shell: PathBuf,
}
impl Default for Tools {
    fn default() -> Self {
        Self {
            codesign: "/usr/bin/codesign".into(),
            security: "/usr/bin/security".into(),
            openssl: "/usr/bin/openssl".into(),
            shell: "/bin/zsh".into(),
        }
    }
}

pub(crate) const NODE_CERTIFICATE_NAME: &str = "Steve Node";
pub(crate) const NODE_CLI_IDENTIFIER: &str = "io.github.infranite.steve.cli";
pub(crate) const NODE_RUNTIME_IDENTIFIER: &str = "io.github.infranite.steve.node";
pub(crate) const NODE_KEYSTORE_DIR: &str = "secrets/keystore";
pub(crate) const NODE_SIGNING_PASS_ENTRY: &str = "steve__signing__keychain-pass";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct NodeSigningIdentity {
    pub label: String,
    pub cli_identifier: String,
    pub node_identifier: String,
    pub certificate_name: String,
    pub certificate_sha1: String,
}

impl NodeSigningIdentity {
    pub(crate) fn validate(&self, expected_label: &str) -> Result<()> {
        ensure!(
            valid_identity(&self.label) && self.label == expected_label,
            "Node signing identity label differs from service label"
        );
        ensure!(
            valid_identity(&self.cli_identifier) && valid_identity(&self.node_identifier),
            "invalid Node signing identifier"
        );
        ensure!(
            !self.certificate_name.is_empty()
                && !self
                    .certificate_name
                    .chars()
                    .any(|c| c.is_control() || "\"'\\".contains(c)),
            "invalid Node signing certificate name"
        );
        ensure!(
            valid_sha1(&self.certificate_sha1),
            "invalid Node signing certificate fingerprint"
        );
        Ok(())
    }

    pub(crate) fn node_requirement(&self) -> String {
        self_signed_requirement(&self.node_identifier, &self.certificate_sha1)
    }

    pub(crate) fn cli_requirement(&self) -> String {
        self_signed_requirement(&self.cli_identifier, &self.certificate_sha1)
    }
}

pub(crate) fn valid_sha1(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn self_signed_requirement(identifier: &str, sha1: &str) -> String {
    format!("identifier \"{identifier}\" and certificate leaf = H\"{sha1}\"")
}

pub(crate) fn leaf_sha1_of(designated: &str) -> Option<String> {
    const MARK: &str = "certificate leaf = H\"";
    let start = designated.find(MARK)? + MARK.len();
    let rest = &designated[start..];
    let hex = &rest[..rest.find('"')?];
    (hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

pub(crate) fn parse_designated_requirement(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let from = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes).lines().find_map(|line| {
            line.strip_prefix("designated => ")
                .map(|s| s.trim().to_owned())
        })
    };
    from(stdout).or_else(|| from(stderr))
}

pub(crate) fn json_file(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}
pub(crate) fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.uid() == unsafe { libc::getuid() },
        "directory must be owned and not a link: {}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(crate) fn owned_file(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o022 == 0,
        "file must be owned, regular, and not writable by others: {}",
        path.display()
    );
    Ok(())
}
pub(crate) fn exclusive_json(path: &Path, value: &Value) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    File::open(path.parent().context("file parent missing")?)?.sync_all()?;
    Ok(())
}
pub(crate) fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("file parent missing")?;
    private_dir(parent)?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    exclusive_json(&tmp, value)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(crate) fn output(command: &mut Command, timeout: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    crate::common::run_bounded(
        command,
        &program,
        crate::common::BoundedOptions::capture(timeout),
    )
    .map_err(|err| match err {
        crate::common::CommandError::TimedOut { .. } => {
            anyhow::anyhow!("native maintenance command timed out")
        }
        crate::common::CommandError::Spawn { source, .. } => {
            anyhow::Error::new(source).context("start native maintenance command")
        }
        other => anyhow::Error::new(other),
    })
}
pub(crate) fn successful(command: &mut Command, timeout: Duration) -> Result<Output> {
    let result = output(command, timeout)?;
    // Do not include command arguments or captured output: signing commands can carry secrets on stdin.
    ensure!(
        result.status.success(),
        "native maintenance command failed ({})",
        result.status
    );
    Ok(result)
}
pub(crate) fn build_info(binary: &Path) -> Result<Value> {
    owned_file(binary)?;
    let result = successful(
        Command::new(binary)
            .arg("--build-info")
            .stdin(Stdio::null()),
        BUILD_INFO_TIMEOUT,
    )?;
    Ok(serde_json::from_slice(&result.stdout)?)
}
fn identifier(binary: &Path, tools: &Tools) -> Result<String> {
    owned_file(binary)?;
    let result = successful(
        Command::new(&tools.codesign)
            .arg("-dv")
            .arg(binary)
            .stdin(Stdio::null()),
        CODESIGN_VERIFY_TIMEOUT,
    )?;
    String::from_utf8_lossy(&result.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("Identifier=").map(str::to_owned))
        .context("code signature has no identifier")
}
pub(crate) fn verify_signature(binary: &Path, expected: &str, tools: &Tools) -> Result<()> {
    successful(
        Command::new(&tools.codesign)
            .args(["--verify", "--strict"])
            .arg(binary)
            .stdin(Stdio::null()),
        CODESIGN_VERIFY_TIMEOUT,
    )?;
    ensure!(
        identifier(binary, tools)? == expected,
        "installed code signature identifier differs from signing identity"
    );
    Ok(())
}

/// Verify that upgrading from `old_binary` to `new_binary` preserves the exact code signing
/// identifier and certificate-bound designated requirement so macOS TCC does not re-prompt.
pub(crate) fn verify_tcc_signing_continuity(
    old_binary: &Path,
    new_binary: &Path,
    expected_identifier: &str,
    tools: &Tools,
) -> Result<String> {
    verify_signature(old_binary, expected_identifier, tools)?;
    verify_signature(new_binary, expected_identifier, tools)?;
    let old_req = read_node_designated_requirement(old_binary, tools)?
        .context("previous release binary has no certificate-bound designated requirement")?;
    let new_req = read_node_designated_requirement(new_binary, tools)?
        .context("candidate release binary has no certificate-bound designated requirement")?;
    ensure!(
        old_req == new_req,
        "upgrade changed designated requirement: {old_req} != {new_req}"
    );
    let expected_prefix =
        format!("identifier \"{expected_identifier}\" and certificate leaf = H\"");
    ensure!(
        new_req.starts_with(&expected_prefix) && leaf_sha1_of(&new_req).is_some(),
        "designated requirement is not bound to expected identifier and certificate leaf: {new_req}"
    );
    Ok(new_req)
}
pub(crate) fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 201
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub(crate) fn identity(home: &Path, tools: &Tools) -> Result<Value> {
    let path = home.join("signing/identity.json");
    owned_file(&path)?;
    let value = json_file(&path)?;
    for key in [
        "label",
        "hub_identifier",
        "keyhelper_identifier",
        "vault_service",
    ] {
        ensure!(
            value[key].as_str().is_some_and(valid_identity),
            "invalid installation identity field: {key}"
        );
    }
    for (relative, key) in [
        ("bin/steve-keyhelper", "keyhelper_identifier"),
        ("current/bin/steve-hub", "hub_identifier"),
    ] {
        let binary = home.join(relative);
        if binary.exists() {
            verify_signature(&binary, value[key].as_str().unwrap(), tools)?;
        }
    }
    Ok(value)
}
fn stable(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|s| {
            !s.is_empty()
                && (s.len() == 1 || !s.starts_with('0'))
                && s.bytes().all(|b| b.is_ascii_digit())
        })
}
fn no_links(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        !m.file_type().is_symlink()
            && (m.is_file() || m.is_dir())
            && m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o022 == 0,
        "release contains a link, special file, or untrusted permissions: {}",
        path.display()
    );
    if m.is_dir() {
        for entry in fs::read_dir(path)? {
            no_links(&entry?.path())?;
        }
    }
    Ok(())
}
pub(crate) fn validate_bundle(
    home: &Path,
    candidate: &Path,
    version: &str,
    commit: &str,
) -> Result<Value> {
    ensure!(
        candidate.is_absolute() && candidate.is_dir() && fs::canonicalize(candidate)? == candidate,
        "bundle must be a real absolute directory"
    );
    ensure!(
        candidate.parent() == Some(fs::canonicalize(home.join("releases"))?.as_path()),
        "bundle must be a direct child of STEVE_HOME/releases"
    );
    for relative in ["current", "web/current"] {
        if let Ok(current) = fs::canonicalize(home.join(relative)) {
            ensure!(
                !current.starts_with(candidate),
                "cannot prepare the running release"
            );
        }
    }
    ensure!(
        stable(version)
            && commit.len() == 40
            && commit
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "expected stable version and full lowercase Git commit required"
    );
    no_links(candidate)?;
    let manifest = json_file(&candidate.join("release.json"))?;
    let web = json_file(&candidate.join("web/dist/release.json"))?;
    let info = build_info(&candidate.join("bin/steve-hub"))?;
    for value in [&manifest, &web, &info] {
        ensure!(
            value.is_object() && value["version"] == version && value["commit"] == commit,
            "bundle component identity differs from expected version/commit"
        );
    }
    ensure!(
        manifest["format_version"] == 1
            && manifest["hub_version"] == version
            && manifest["web_version"] == version
            && info["target"] == "aarch64-apple-darwin"
            && manifest["schema"]["target"] == info["schema"],
        "release metadata does not match the macOS Hub build"
    );
    let html = fs::read_to_string(candidate.join("web/dist/index.html"))?;
    let marker = regex::Regex::new(&format!(
        r#"<meta\s+name=["']steve-web-version["']\s+content=["']{}["']"#,
        regex::escape(version)
    ))?
    .is_match(&html);
    ensure!(
        marker && candidate.join("ops/run.sh").is_file(),
        "bundle is missing matching Web identity or its service launcher"
    );
    Ok(info)
}
struct Unlocked<'a> {
    keychain: &'a Path,
    tools: &'a Tools,
}
impl Drop for Unlocked<'_> {
    fn drop(&mut self) {
        let _ = output(
            Command::new(&self.tools.security)
                .arg("lock-keychain")
                .arg(self.keychain)
                .stdin(Stdio::null()),
            SECURITY_TIMEOUT,
        );
    }
}
fn check_security_token(value: &str, what: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value
                .chars()
                .any(|c| c.is_whitespace() || "\"'\\".contains(c)),
        "{what} cannot be represented safely"
    );
    Ok(())
}

fn check_security_path(path: &Path, what: &str) -> Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("{what} must be UTF-8"))?;
    ensure!(
        !text.is_empty() && !text.contains(['"', '\\', '\n', '\r']),
        "{what} cannot be represented safely"
    );
    Ok(text.to_owned())
}

fn run_security_stdin(
    security: &Path,
    input: &Zeroizing<String>,
    timeout: Duration,
    timeout_msg: &str,
    failure_msg: &str,
) -> Result<()> {
    let mut command = Command::new(security);
    command.arg("-i");
    let program = security.to_string_lossy();
    let result = crate::common::run_bounded(
        &mut command,
        &program,
        crate::common::BoundedOptions {
            stdin: crate::common::BoundedStdin::Bytes(input.as_bytes()),
            ..crate::common::BoundedOptions::silent(timeout)
        },
    )
    .map_err(|err| match err {
        crate::common::CommandError::TimedOut { .. } => anyhow::anyhow!("{timeout_msg}"),
        crate::common::CommandError::StdinConfigure { .. } => {
            anyhow::anyhow!("could not configure keychain input")
        }
        other => anyhow::Error::new(other),
    })?;
    ensure!(result.status.success(), "{failure_msg}");
    Ok(())
}

fn unlock<'a>(
    keychain: &'a Path,
    password: &str,
    tools: &'a Tools,
    timeout: Duration,
) -> Result<Unlocked<'a>> {
    check_security_token(password, "signing password")?;
    let keychain_text = check_security_path(keychain, "signing keychain path")?;
    // Even a failed or interrupted command may have unlocked the keychain.
    let guard = Unlocked { keychain, tools };
    let input = Zeroizing::new(format!(
        "unlock-keychain -p {password} \"{keychain_text}\"\n"
    ));
    run_security_stdin(
        &tools.security,
        &input,
        timeout,
        "signing keychain unlock timed out",
        "signing keychain did not unlock",
    )?;
    Ok(guard)
}
fn sign(binary: &Path, keychain: &Path, password: &str, id: &str, tools: &Tools) -> Result<()> {
    sign_with_timeout(binary, keychain, password, id, tools, SECURITY_TIMEOUT)
}
fn sign_with_timeout(
    binary: &Path,
    keychain: &Path,
    password: &str,
    id: &str,
    tools: &Tools,
    unlock_timeout: Duration,
) -> Result<()> {
    let _lock = unlock(keychain, password, tools, unlock_timeout)?;
    successful(
        Command::new(&tools.security)
            .args(["set-keychain-settings", "-l", "-u", "-t", "300"])
            .arg(keychain)
            .stdin(Stdio::null()),
        SECURITY_TIMEOUT,
    )?;
    successful(
        Command::new(&tools.codesign)
            .args([
                "--force",
                "--sign",
                "Steve Hub",
                "--identifier",
                id,
                "--keychain",
            ])
            .arg(keychain)
            .arg(binary)
            .stdin(Stdio::null()),
        CODESIGN_SIGN_TIMEOUT,
    )?;
    verify_signature(binary, id, tools)
}

pub fn prepare_bundle(
    home: &Path,
    candidate: &Path,
    version: &str,
    commit: &str,
    password: &str,
) -> Result<Value> {
    let _lock = crate::release::lock_release(home)?;
    prepare_with(
        home,
        candidate,
        version,
        commit,
        password,
        &Tools::default(),
    )
}
fn prepare_with(
    home: &Path,
    candidate: &Path,
    version: &str,
    commit: &str,
    password: &str,
    tools: &Tools,
) -> Result<Value> {
    let info = validate_bundle(home, candidate, version, commit)?;
    let ident = identity(home, tools)?;
    let helper = home.join("bin/steve-keyhelper");
    owned_file(&helper)?;
    let helper_hash = hash(&helper)?;
    let keychain = home.join("signing/steve-signing.keychain-db");
    owned_file(&keychain)?;
    let hub_id = ident["hub_identifier"].as_str().unwrap();
    let candidate_hub = candidate.join("bin/steve-hub");
    sign(&candidate_hub, &keychain, password, hub_id, tools)?;
    let current_hub = home.join("current/bin/steve-hub");
    if current_hub.is_file() && read_node_designated_requirement(&current_hub, tools)?.is_some() {
        verify_tcc_signing_continuity(&current_hub, &candidate_hub, hub_id, tools)?;
    }
    ensure!(
        hash(&helper)? == helper_hash && build_info(&candidate_hub)? == info,
        "installed helper or prepared Hub identity changed"
    );
    Ok(json!({"candidate":candidate,"build":info,"source":"release-bundle"}))
}

/// Finish first-install provisioning through the signed Hub and the installed helper.
fn provision_signing_password(home: &Path, candidate: &Path, staging: &Path) -> Result<()> {
    owned_file(staging)?;
    let mut command = crate::common::clean_command(&candidate.join("bin/steve-hub"), Some(home));
    let env = home.join("steve.env");
    if env.exists() {
        owned_file(&env)?;
        let env = crate::env_file::EnvDocument::parse(&fs::read(env)?)?;
        for key in ["STEVE_VAULT_KEYSTORE", "STEVE_VAULT_KEY_DIR"] {
            if let Some(value) = env.values().get(key) {
                command.env(key, value);
            }
        }
    }
    command
        .arg("--local-release")
        .arg("provision-signing")
        .arg(home)
        .stdin(File::open(staging)?);
    let result = crate::common::run_bounded(
        &mut command,
        "steve-hub",
        crate::common::BoundedOptions {
            stdin: crate::common::BoundedStdin::Preserve,
            ..crate::common::BoundedOptions::capture(PROVISION_SIGNING_TIMEOUT)
        },
    )?;
    ensure!(
        result.status.success(),
        "store first-install signing password: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    fs::remove_file(staging)?;
    Ok(())
}

pub fn setup_keychain(home: &Path, candidate: &Path, label: &str) -> Result<Value> {
    ensure!(
        cfg!(target_os = "macos"),
        "setup-keychain is only for a new macOS Hub"
    );
    let _lock = crate::release::lock_release(home)?;
    setup_with(home, candidate, label, &Tools::default())
}
fn setup_with(home: &Path, candidate: &Path, label: &str, tools: &Tools) -> Result<Value> {
    ensure!(valid_identity(label), "invalid service label");
    let ident = json!({"label":label,"hub_identifier":"io.github.infranite.steve.hub","keyhelper_identifier":"io.github.infranite.steve.keyhelper","vault_service":"io.github.infranite.steve.vault"});
    let signing = home.join("signing");
    let setup = signing.join("bundle-setup.json");
    let staging = signing.join(".setup-password");
    let identity_path = signing.join("identity.json");
    let helper = home.join("bin/steve-keyhelper");
    if setup.is_file() && identity_path.is_file() && helper.is_file() {
        owned_file(&setup)?;
        owned_file(&identity_path)?;
        let record = json_file(&setup)?;
        ensure!(
            record["release"].as_str() == candidate.to_str() && json_file(&identity_path)? == ident,
            "existing setup belongs to a different installation"
        );
        verify_signature(
            &helper,
            ident["keyhelper_identifier"].as_str().unwrap(),
            tools,
        )?;
        verify_signature(
            &candidate.join("bin/steve-hub"),
            ident["hub_identifier"].as_str().unwrap(),
            tools,
        )?;
        ensure!(
            record["helper_sha256"] == hash(&helper)?
                && record["hub_sha256"] == hash(&candidate.join("bin/steve-hub"))?
                && record["build"] == build_info(&candidate.join("bin/steve-hub"))?,
            "completed signing receipt no longer matches installed files"
        );
        if staging.exists() {
            provision_signing_password(home, candidate, &staging)?;
        }
        return Ok(json!({"initialized":true,"changed":false,"release":candidate}));
    }
    for path in [&identity_path, &setup, &helper, &home.join("current")] {
        ensure!(
            fs::symlink_metadata(path).is_err(),
            "home already has an identity, helper or Hub; inspect interrupted setup instead of replacing it"
        );
    }
    let data = home.join("data");
    if data.exists() {
        ensure!(
            fs::read_dir(data)?.next().is_none(),
            "home has existing Hub data"
        );
    }
    let manifest = json_file(&candidate.join("release.json"))?;
    let info = validate_bundle(
        home,
        candidate,
        manifest["version"]
            .as_str()
            .context("release version missing")?,
        manifest["commit"]
            .as_str()
            .context("release commit missing")?,
    )?;
    let generic = candidate.join("bin/steve-keyhelper");
    owned_file(&generic)?;
    for name in ["sign-setup.sh", "release-lib.sh"] {
        owned_file(&candidate.join("ops").join(name))?;
    }
    private_dir(&signing)?;
    let started = signing.join("bundle-setup-started.json");
    let keychain = signing.join("steve-signing.keychain-db");
    let started_value =
        json!({"release":candidate,"identity":ident,"generic_helper_sha256":hash(&generic)?});
    if started.exists() {
        owned_file(&started)?;
        ensure!(
            json_file(&started)? == started_value,
            "unfinished signing setup belongs to another candidate"
        );
    } else {
        ensure!(
            fs::symlink_metadata(&keychain).is_err() && fs::symlink_metadata(&staging).is_err(),
            "unowned signing resources already exist; refusing to adopt them"
        );
        exclusive_json(&started, &started_value)?;
    }
    let mut command = Command::new(&tools.shell);
    command
        .arg(candidate.join("ops/sign-setup.sh"))
        .env_clear()
        .env("STEVE_HOME", home);
    for key in ["HOME", "PATH", "USER", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    /* what the script says is kept off the terminal and shown only when it fails */
    let output = output(&mut command, SIGN_SETUP_TIMEOUT)?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let last = said
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!(
            "first-install signing identity setup failed: {}",
            last.trim()
        );
    }
    owned_file(&keychain)?;
    owned_file(&staging)?;
    let password = Zeroizing::new(fs::read_to_string(&staging)?.trim().to_owned());
    private_dir(helper.parent().unwrap())?;
    let temporary = helper
        .parent()
        .unwrap()
        .join(format!(".steve-keyhelper-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<Value> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&temporary)?;
        std::io::copy(&mut File::open(&generic)?, &mut file)?;
        file.sync_all()?;
        ensure!(
            hash(&temporary)? == started_value["generic_helper_sha256"].as_str().unwrap(),
            "helper private copy hash changed"
        );
        sign(
            &temporary,
            &keychain,
            &password,
            ident["keyhelper_identifier"].as_str().unwrap(),
            tools,
        )?;
        sign(
            &candidate.join("bin/steve-hub"),
            &keychain,
            &password,
            ident["hub_identifier"].as_str().unwrap(),
            tools,
        )?;
        ensure!(
            build_info(&candidate.join("bin/steve-hub"))? == info,
            "Hub build identity changed while signing"
        );
        fs::hard_link(&temporary, &helper)
            .context("helper appeared during setup; it was not overwritten")?;
        File::open(helper.parent().unwrap())?.sync_all()?;
        exclusive_json(&identity_path, &ident)?;
        exclusive_json(
            &setup,
            &json!({"release":candidate,"build":info,"helper_sha256":hash(&helper)?,"hub_sha256":hash(&candidate.join("bin/steve-hub"))?}),
        )?;
        Ok(json!({"initialized":true,"changed":true,"release":candidate,"build":info}))
    })();
    let _ = fs::remove_file(&temporary);
    let result = result?;
    provision_signing_password(home, candidate, &staging)?;
    Ok(result)
}

pub(crate) fn node_signing_identity_path(home: &Path) -> PathBuf {
    home.join("signing/identity.json")
}

pub(crate) fn node_signing_keychain(home: &Path) -> PathBuf {
    home.join("signing/steve-signing.keychain-db")
}

pub(crate) fn node_signing_password_path(home: &Path) -> PathBuf {
    home.join(NODE_KEYSTORE_DIR).join(NODE_SIGNING_PASS_ENTRY)
}

fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>> {
    let mut raw = Zeroizing::new([0u8; N]);
    let mut source = File::open("/dev/urandom").context("open random source")?;
    source.read_exact(raw.as_mut())?;
    Ok(raw)
}

fn strict_private_file(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o077 == 0,
        "key store entry must be a regular 0600 file owned by this user: {}",
        path.display()
    );
    Ok(())
}

fn store_node_signing_password(home: &Path, raw: &[u8; 32]) -> Result<()> {
    private_dir(&home.join("secrets"))?;
    let store = home.join(NODE_KEYSTORE_DIR);
    private_dir(&store)?;
    let path = store.join(NODE_SIGNING_PASS_ENTRY);
    let encoded = Zeroizing::new(data_encoding::BASE64.encode(raw));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .context("store Node signing keychain password")?;
    file.write_all(encoded.as_bytes())?;
    file.sync_all()?;
    File::open(&store)?.sync_all()?;
    let readback = load_node_signing_password(home)?;
    let expected = Zeroizing::new(hex::encode(raw));
    ensure!(
        readback.as_str() == expected.as_str(),
        "Node signing keychain password did not read back"
    );
    Ok(())
}

fn load_node_signing_password(home: &Path) -> Result<Zeroizing<String>> {
    let path = node_signing_password_path(home);
    strict_private_file(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .context("read Node signing keychain password")?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(1025).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024,
        "Node signing key store entry is too large"
    );
    let raw = Zeroizing::new(
        data_encoding::BASE64
            .decode(bytes.trim_ascii())
            .map_err(|_| anyhow::anyhow!("Node signing key store entry is malformed"))?,
    );
    ensure!(raw.len() == 32, "Node signing key store entry is malformed");
    Ok(Zeroizing::new(hex::encode(raw.as_slice())))
}

struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}
impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct NodeInitCleanup<'a> {
    keychain: &'a Path,
    pass_path: &'a Path,
    tools: &'a Tools,
    armed: bool,
}
impl Drop for NodeInitCleanup<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if fs::symlink_metadata(self.keychain).is_ok() {
            let _ = output(
                Command::new(&self.tools.security)
                    .arg("delete-keychain")
                    .arg(self.keychain)
                    .stdin(Stdio::null()),
                SECURITY_TIMEOUT,
            );
            let _ = fs::remove_file(self.keychain);
        }
        let _ = fs::remove_file(self.pass_path);
    }
}

fn parse_openssl_sha1(stdout: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(stdout);
    let (_, value) = text
        .split_once('=')
        .context("certificate fingerprint missing")?;
    let hex: String = value
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    ensure!(valid_sha1(&hex), "invalid certificate SHA-1 fingerprint");
    Ok(hex)
}

/// Load and validate an existing Node signing identity, keychain and key store password.
pub(crate) fn load_node_identity(home: &Path, expected_label: &str) -> Result<NodeSigningIdentity> {
    let identity_path = node_signing_identity_path(home);
    let keychain_path = node_signing_keychain(home);
    owned_file(&identity_path)?;
    let identity: NodeSigningIdentity = serde_json::from_value(json_file(&identity_path)?)
        .context("invalid Node signing identity")?;
    identity.validate(expected_label)?;
    owned_file(&keychain_path)?;
    let _ = load_node_signing_password(home)?;
    Ok(identity)
}

/// Load the Node's existing local self-signed identity, or create it once inside `home`.
/// Partial or foreign signing state is refused rather than silently replaced.
pub(crate) fn ensure_node_identity(
    home: &Path,
    label: &str,
    tools: &Tools,
) -> Result<NodeSigningIdentity> {
    ensure!(valid_identity(label), "invalid service label");
    let identity_path = node_signing_identity_path(home);
    let keychain_path = node_signing_keychain(home);
    let pass_path = node_signing_password_path(home);
    let present = [
        fs::symlink_metadata(&identity_path).is_ok(),
        fs::symlink_metadata(&keychain_path).is_ok(),
        fs::symlink_metadata(&pass_path).is_ok(),
    ];
    if present.iter().all(|exists| *exists) {
        return load_node_identity(home, label);
    }
    ensure!(
        present.iter().all(|exists| !*exists),
        "incomplete Node signing identity; inspect {}/signing instead of replacing it",
        home.display()
    );

    let keychain_text = check_security_path(&keychain_path, "signing keychain path")?;
    let codesign_text = check_security_path(&tools.codesign, "codesign path")?;
    let signing_dir = home.join("signing");
    private_dir(&signing_dir)?;
    let work_dir = signing_dir.join(format!(".init-{}", uuid::Uuid::new_v4()));
    private_dir(&work_dir)?;
    let _work_guard = TempDirGuard(work_dir.clone());
    let mut cleanup = NodeInitCleanup {
        keychain: &keychain_path,
        pass_path: &pass_path,
        tools,
        armed: true,
    };

    let cert_cnf = work_dir.join("cert.cnf");
    let key_pem = work_dir.join("key.pem");
    let cert_pem = work_dir.join("cert.pem");
    let id_p12 = work_dir.join("id.p12");
    let p12_text = check_security_path(&id_p12, "temporary signing bundle path")?;
    {
        let mut cnf = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&cert_cnf)?;
        write!(
            cnf,
            "[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN={NODE_CERTIFICATE_NAME}\n[v3]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n"
        )?;
    }

    successful(
        Command::new(&tools.openssl)
            .args([
                "req", "-x509", "-newkey", "rsa:3072", "-nodes", "-days", "36500",
            ])
            .arg("-keyout")
            .arg(&key_pem)
            .arg("-out")
            .arg(&cert_pem)
            .arg("-config")
            .arg(&cert_cnf)
            .stdin(Stdio::null()),
        OPENSSL_GENERATE_TIMEOUT,
    )
    .context("generate local Node signing certificate")?;

    let fingerprint = successful(
        Command::new(&tools.openssl)
            .args(["x509", "-noout", "-fingerprint", "-sha1", "-in"])
            .arg(&cert_pem)
            .stdin(Stdio::null()),
        OPENSSL_FINGERPRINT_TIMEOUT,
    )
    .context("read local Node signing certificate fingerprint")?;
    let certificate_sha1 = parse_openssl_sha1(&fingerprint.stdout)?;

    let raw_password = random_bytes::<32>()?;
    let password = Zeroizing::new(hex::encode(raw_password.as_slice()));
    let p12_pass = Zeroizing::new(hex::encode(random_bytes::<16>()?.as_slice()));
    check_security_token(&password, "signing password")?;
    check_security_token(&p12_pass, "temporary export password")?;

    let export_legacy = output(
        Command::new(&tools.openssl)
            .args(["pkcs12", "-export", "-legacy", "-passout", "env:P12"])
            .arg("-inkey")
            .arg(&key_pem)
            .arg("-in")
            .arg(&cert_pem)
            .arg("-out")
            .arg(&id_p12)
            .env("P12", p12_pass.as_str())
            .stdin(Stdio::null()),
        OPENSSL_EXPORT_TIMEOUT,
    )?;
    if !export_legacy.status.success() {
        successful(
            Command::new(&tools.openssl)
                .args(["pkcs12", "-export", "-passout", "env:P12"])
                .arg("-inkey")
                .arg(&key_pem)
                .arg("-in")
                .arg(&cert_pem)
                .arg("-out")
                .arg(&id_p12)
                .env("P12", p12_pass.as_str())
                .stdin(Stdio::null()),
            OPENSSL_EXPORT_TIMEOUT,
        )
        .context("export local Node signing identity")?;
    }

    let create_cmd = Zeroizing::new(format!(
        "create-keychain -p {} \"{keychain_text}\"\n",
        password.as_str()
    ));
    run_security_stdin(
        &tools.security,
        &create_cmd,
        SECURITY_TIMEOUT,
        "Node signing keychain creation timed out",
        "create Node signing keychain",
    )?;
    {
        let _lock = unlock(&keychain_path, &password, tools, SECURITY_TIMEOUT)?;
        successful(
            Command::new(&tools.security)
                .args(["set-keychain-settings", "-l", "-u", "-t", "300"])
                .arg(&keychain_path)
                .stdin(Stdio::null()),
            SECURITY_TIMEOUT,
        )?;
        let import_cmd = Zeroizing::new(format!(
            "import \"{p12_text}\" -k \"{keychain_text}\" -P {} -T \"{codesign_text}\"\n",
            p12_pass.as_str()
        ));
        run_security_stdin(
            &tools.security,
            &import_cmd,
            SECURITY_TIMEOUT,
            "Node signing identity import timed out",
            "import local Node signing identity",
        )?;
        let partition_cmd = Zeroizing::new(format!(
            "set-key-partition-list -S apple-tool:,apple: -s -k {} \"{keychain_text}\"\n",
            password.as_str()
        ));
        run_security_stdin(
            &tools.security,
            &partition_cmd,
            SECURITY_TIMEOUT,
            "Node signing keychain partition update timed out",
            "configure local Node signing keychain access",
        )?;
        let listed = successful(
            Command::new(&tools.security)
                .args(["find-identity", "-p", "codesigning"])
                .arg(&keychain_path)
                .stdin(Stdio::null()),
            SECURITY_TIMEOUT,
        )?;
        let found = String::from_utf8_lossy(&listed.stdout).to_ascii_lowercase();
        ensure!(
            found.contains(&certificate_sha1)
                || found.contains(&NODE_CERTIFICATE_NAME.to_ascii_lowercase()),
            "local Node signing identity did not verify in keychain"
        );
    }

    owned_file(&keychain_path)?;
    store_node_signing_password(home, &raw_password)?;
    let identity = NodeSigningIdentity {
        label: label.to_owned(),
        cli_identifier: NODE_CLI_IDENTIFIER.to_owned(),
        node_identifier: NODE_RUNTIME_IDENTIFIER.to_owned(),
        certificate_name: NODE_CERTIFICATE_NAME.to_owned(),
        certificate_sha1,
    };
    exclusive_json(&identity_path, &serde_json::to_value(&identity)?)?;
    cleanup.armed = false;
    Ok(identity)
}

/// Read a binary's embedded certificate-bound designated requirement (`None` for unsigned/ad-hoc).
pub(crate) fn read_node_designated_requirement(
    binary: &Path,
    tools: &Tools,
) -> Result<Option<String>> {
    let result = output(
        Command::new(&tools.codesign)
            .args(["--display", "-r-"])
            .arg(binary)
            .stdin(Stdio::null()),
        CODESIGN_VERIFY_TIMEOUT,
    )?;
    if !result.status.success() {
        return Ok(None);
    }
    let Some(designated) = parse_designated_requirement(&result.stdout, &result.stderr) else {
        return Ok(None);
    };
    Ok(leaf_sha1_of(&designated).map(|_| designated))
}

/// Verify that `binary` has a valid signature satisfying `requirement` AND embeds the matching
/// certificate-bound designated requirement so macOS TCC preserves grants across upgrades.
pub(crate) fn verify_node_binary(binary: &Path, requirement: &str, tools: &Tools) -> Result<()> {
    owned_file(binary)?;
    ensure!(
        leaf_sha1_of(requirement).is_some(),
        "Node code requirement must bind a certificate leaf"
    );
    successful(
        Command::new(&tools.codesign)
            .args(["--verify", "--strict", "-R"])
            .arg(format!("={requirement}"))
            .arg(binary)
            .stdin(Stdio::null()),
        CODESIGN_VERIFY_TIMEOUT,
    )
    .context("executable failed strict code-signature requirement verification")?;
    let actual = read_node_designated_requirement(binary, tools)?
        .context("signed executable has no certificate-bound designated requirement")?;
    ensure!(
        actual == requirement,
        "executable designated requirement differs from installation signing identity"
    );
    Ok(())
}

/// Sign `binary` with the Node's local self-signed identity via an atomic sibling copy,
/// skipping keychain unlock when `binary` already satisfies the exact requirement.
pub(crate) fn sign_node_binary(
    home: &Path,
    binary: &Path,
    identity: &NodeSigningIdentity,
    identifier: &str,
    tools: &Tools,
) -> Result<()> {
    let requirement = self_signed_requirement(identifier, &identity.certificate_sha1);
    if verify_node_binary(binary, &requirement, tools).is_ok() {
        return Ok(());
    }
    owned_file(binary)?;
    let keychain = node_signing_keychain(home);
    owned_file(&keychain)?;
    let password = load_node_signing_password(home)?;
    let mode = fs::symlink_metadata(binary)?.mode() & 0o755;
    let parent = binary.parent().context("binary has no parent directory")?;
    let name = binary
        .file_name()
        .and_then(|n| n.to_str())
        .context("binary name must be UTF-8")?;
    let staged = parent.join(format!(".{name}.sign-{}", uuid::Uuid::new_v4().simple()));
    let mut staged_guard = TempFileGuard::new(staged.clone());
    {
        let mut src = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(binary)?;
        let mut dst = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.set_permissions(fs::Permissions::from_mode(mode))?;
        dst.sync_all()?;
    }
    let sign_ref = identity.certificate_sha1.to_ascii_uppercase();
    {
        let _lock = unlock(&keychain, &password, tools, SECURITY_TIMEOUT)?;
        successful(
            Command::new(&tools.security)
                .args(["set-keychain-settings", "-l", "-u", "-t", "300"])
                .arg(&keychain)
                .stdin(Stdio::null()),
            SECURITY_TIMEOUT,
        )?;
        successful(
            Command::new(&tools.codesign)
                .args(["--force", "--sign", &sign_ref, "--identifier", identifier])
                .arg("--keychain")
                .arg(&keychain)
                .arg("-r")
                .arg(format!("=designated => {requirement}"))
                .arg(&staged)
                .stdin(Stdio::null()),
            CODESIGN_SIGN_TIMEOUT,
        )
        .context("sign macOS Node executable with local identity")?;
    }
    verify_node_binary(&staged, &requirement, tools)?;
    fs::rename(&staged, binary)?;
    File::open(parent)?.sync_all()?;
    staged_guard.disarm();
    Ok(())
}

/// Copy `source` into `destination_dir` as a mode-0700 temporary file signed with the Node's
/// local CLI identity and verified against its designated requirement.
pub(crate) fn stage_signed_node_runner(
    home: &Path,
    source: &Path,
    destination_dir: &Path,
    identity: &NodeSigningIdentity,
    tools: &Tools,
) -> Result<PathBuf> {
    private_dir(destination_dir)?;
    let staged = destination_dir.join(format!(
        ".node-boot-runner.sign-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut staged_guard = TempFileGuard::new(staged.clone());
    {
        let mut src = File::open(source)?;
        let mut dst = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.set_permissions(fs::Permissions::from_mode(0o700))?;
        dst.sync_all()?;
    }
    sign_node_binary(home, &staged, identity, &identity.cli_identifier, tools)?;
    staged_guard.disarm();
    Ok(staged)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Instant;
    pub(crate) fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, Tools) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let candidate = home.join("releases/1.2.3-test");
        for name in ["bin", "ops", "web/dist"] {
            fs::create_dir_all(candidate.join(name)).unwrap();
        }
        let candidate = fs::canonicalize(candidate).unwrap();
        let manifest = json!({"format_version":1,"version":"1.2.3","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hub_version":"1.2.3","web_version":"1.2.3","schema":{"target":54}});
        fs::write(
            candidate.join("release.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            candidate.join("web/dist/release.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            candidate.join("web/dist/index.html"),
            "<html><head><meta name=\"steve-web-version\" content=\"1.2.3\"></head></html>",
        )
        .unwrap();
        executable(
            &candidate.join("bin/steve-hub"),
            "#!/bin/sh\nif [ \"$2\" = provision-signing ]; then\n  test \"$(cat)\" = test-password\n  exit $?\nfi\nprintf '%s\\n' '{\"version\":\"1.2.3\",\"commit\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"target\":\"aarch64-apple-darwin\",\"schema\":54}'\n",
        );
        executable(
            &candidate.join("bin/steve-keyhelper"),
            "#!/bin/sh\nexit 0\n",
        );
        executable(&candidate.join("ops/run.sh"), "#!/bin/sh\nexit 0\n");
        executable(&candidate.join("ops/release-lib.sh"), "# fixture\n");
        executable(
            &candidate.join("ops/sign-setup.sh"),
            "#!/bin/sh\nset -eu\numask 077\nprintf keychain > \"$STEVE_HOME/signing/steve-signing.keychain-db\"\nprintf test-password > \"$STEVE_HOME/signing/.setup-password\"\n",
        );
        let codesign = home.join("codesign");
        executable(
            &codesign,
            "#!/bin/sh\nif [ \"$1\" = '-dv' ]; then\ncase \"$2\" in *keyhelper*) printf 'Identifier=io.github.infranite.steve.keyhelper\\n' >&2;; *) printf 'Identifier=io.github.infranite.steve.hub\\n' >&2;; esac\nfi\nexit 0\n",
        );
        let security = home.join("security");
        executable(
            &security,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nif [ \"$1\" = '-i' ]; then cat >/dev/null; fi\nexit 0\n",
                home.join("security.log").display()
            ),
        );
        (
            dir,
            candidate,
            Tools {
                codesign,
                security,
                openssl: "/usr/bin/false".into(),
                shell: "/bin/sh".into(),
            },
        )
    }
    #[test]
    fn first_setup_is_exclusive_repeatable_and_checks_completed_hashes() {
        let (dir, candidate, tools) = fixture();
        let home = dir.path();
        let value = setup_with(home, &candidate, "io.github.infranite.steve.hub", &tools).unwrap();
        assert_eq!(value["changed"], true);
        assert!(!home.join("signing/.setup-password").exists());
        let record = json_file(&home.join("signing/bundle-setup.json")).unwrap();
        assert_eq!(
            record["helper_sha256"],
            hash(&home.join("bin/steve-keyhelper")).unwrap()
        );
        assert_eq!(
            fs::metadata(home.join("signing/identity.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        let before = fs::read(home.join("security.log")).unwrap();
        assert_eq!(
            setup_with(home, &candidate, "io.github.infranite.steve.hub", &tools).unwrap()["changed"],
            false
        );
        assert_eq!(fs::read(home.join("security.log")).unwrap(), before);
        fs::write(home.join("bin/steve-keyhelper"), "changed").unwrap();
        assert!(setup_with(home, &candidate, "io.github.infranite.steve.hub", &tools).is_err());
    }
    #[test]
    fn interrupted_helper_is_never_adopted_or_overwritten() {
        let (dir, candidate, tools) = fixture();
        let home = dir.path();
        fs::create_dir(home.join("bin")).unwrap();
        fs::write(home.join("bin/steve-keyhelper"), "existing").unwrap();
        assert!(setup_with(home, &candidate, "io.github.infranite.steve.hub", &tools).is_err());
        assert_eq!(
            fs::read_to_string(home.join("bin/steve-keyhelper")).unwrap(),
            "existing"
        );
        assert!(!home.join("signing/identity.json").exists());
        assert!(!home.join("security.log").exists());
    }
    #[test]
    fn failed_codesign_relocks_and_preserves_the_installed_helper() {
        let (dir, candidate, tools) = fixture();
        let home = dir.path();
        setup_with(home, &candidate, "io.github.infranite.steve.hub", &tools).unwrap();
        let before = hash(&home.join("bin/steve-keyhelper")).unwrap();
        executable(
            &tools.codesign,
            "#!/bin/sh\nif [ \"$1\" = '--force' ]; then exit 7; fi\nif [ \"$1\" = '-dv' ]; then case \"$2\" in *keyhelper*) echo Identifier=io.github.infranite.steve.keyhelper >&2;; *) echo Identifier=io.github.infranite.steve.hub >&2;; esac; fi\n",
        );
        assert!(
            prepare_with(
                home,
                &candidate,
                "1.2.3",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "test-password",
                &tools
            )
            .is_err()
        );
        assert_eq!(hash(&home.join("bin/steve-keyhelper")).unwrap(), before);
        let events = fs::read_to_string(home.join("security.log")).unwrap();
        assert!(events.ends_with("lock-keychain\n"));
        assert!(!events.contains("test-password"));
    }
    #[test]
    fn failed_unlock_relocks_without_continuing_to_sign() {
        let (dir, _candidate, tools) = fixture();
        let log = dir.path().join("security.log");
        executable(
            &tools.security,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nif [ \"$1\" = '-i' ]; then cat >/dev/null; exit 7; fi\n",
                log.display()
            ),
        );
        assert!(
            sign(
                &dir.path().join("unused-binary"),
                &dir.path().join("keychain"),
                "test-password",
                "unused-id",
                &tools,
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(log).unwrap(), "-i\nlock-keychain\n");
    }
    #[test]
    fn stalled_unlock_is_killed_and_relocks_within_its_deadline() {
        let (dir, _candidate, tools) = fixture();
        let log = dir.path().join("security.log");
        let pid_file = dir.path().join("unlock.pid");
        let signed = dir.path().join("signed");
        executable(
            &tools.security,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = '-i' ]; then printf '%s\\n' \"$$\" > '{}'; fi\nprintf '%s\\n' \"$1\" >> '{}'\nif [ \"$1\" = '-i' ]; then while :; do :; done; fi\n",
                pid_file.display(),
                log.display()
            ),
        );
        executable(
            &tools.codesign,
            &format!("#!/bin/sh\nprintf signed > '{}'\n", signed.display()),
        );
        let keychain = dir.path().join("keychain");
        let start = Instant::now();
        // Larger than the pipe capacity: the deadline must cover stdin writing too.
        let password = Zeroizing::new("x".repeat(1024 * 1024));
        let (error, pid) = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                sign_with_timeout(
                    &dir.path().join("unused-binary"),
                    &keychain,
                    &password,
                    "unused-id",
                    &tools,
                    Duration::from_secs(3),
                )
            });
            // Observe a started, deliberately stalled process before testing timeout.
            // 100ms included scheduler startup and could kill it before this evidence existed.
            steve_runtime::testkit::eventually_blocking("unlock fixture logs -i", || {
                if fs::read_to_string(&log).is_ok_and(|events| events.contains("-i\n")) {
                    return true;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "unlock fixture did not become ready"
                );
                false
            });
            let pid: libc::pid_t = fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            (worker.join().unwrap().unwrap_err(), pid)
        });
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(6));
        assert_eq!(fs::read_to_string(log).unwrap(), "-i\nlock-keychain\n");
        assert!(
            !signed.exists(),
            "timed-out unlock must never reach codesign"
        );
        // SAFETY: signal 0 only queries this fixture's process; waitpid checks it was reaped.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    #[test]
    fn preparation_rejects_live_or_linked_candidates_before_signing() {
        let (dir, candidate, _tools) = fixture();
        let home = dir.path();
        std::os::unix::fs::symlink(&candidate, home.join("current")).unwrap();
        assert!(
            validate_bundle(
                home,
                &candidate,
                "1.2.3",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        fs::remove_file(home.join("current")).unwrap();
        std::os::unix::fs::symlink("/tmp", candidate.join("unexpected")).unwrap();
        assert!(
            validate_bundle(
                home,
                &candidate,
                "1.2.3",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
    }

    /// Fake openssl/security/codesign stand-ins that persist synthetic signatures inside a
    /// sidecar directory, check secret hygiene (stdin/env only, never argv), and enforce
    /// keychain lock/unlock ordering without touching any real macOS keychain.
    fn node_signing_fixture() -> (tempfile::TempDir, PathBuf, Tools) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("node-home");
        fs::create_dir(&home).unwrap();
        let state = root.join("fake-state");
        fs::create_dir(&state).unwrap();
        let argv_log = root.join("argv.log");
        let events_log = root.join("events.log");
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let sha1_colon = "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67";

        let openssl = root.join("fake-openssl");
        executable(
            &openssl,
            &format!(
                r#"#!/bin/sh
set -eu
printf 'openssl %s\n' "$*" >> '{argv}'
printf 'openssl:%s\n' "$1" >> '{events}'
case "$1" in
  req)
    keyout=''; out=''
    shift
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -keyout) keyout="$2"; shift 2 ;;
        -out) out="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    printf 'PRIVATE KEY\n' > "$keyout"
    printf 'CERT\n' > "$out"
    ;;
  x509)
    printf 'SHA1 Fingerprint={sha1_colon}\n'
    ;;
  pkcs12)
    [ -n "${{P12:-}}" ] || exit 11
    out=''
    shift
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -out) out="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    printf 'P12\n' > "$out"
    ;;
  *) exit 99 ;;
esac
"#,
                argv = argv_log.display(),
                events = events_log.display(),
            ),
        );

        let security = root.join("fake-security");
        executable(
            &security,
            &format!(
                r#"#!/bin/sh
set -eu
printf 'security %s\n' "$*" >> '{argv}'
if [ "$1" = "-i" ]; then
  line=$(cat)
  cmd=$(printf '%s' "$line" | awk '{{print $1}}')
  printf 'security:-i:%s\n' "$cmd" >> '{events}'
  case "$cmd" in
    create-keychain)
      kc=$(printf '%s' "$line" | sed -n 's/^create-keychain -p [^ ]* "\(.*\)"$/\1/p')
      [ -n "$kc" ] || exit 21
      umask 077
      printf 'keychain\n' > "$kc"
      ;;
    unlock-keychain)
      printf 'unlocked' > '{state}/unlocked'
      ;;
    import|set-key-partition-list)
      [ -f '{state}/unlocked' ] || exit 22
      ;;
    *) exit 23 ;;
  esac
  exit 0
fi
printf 'security:%s\n' "$1" >> '{events}'
case "$1" in
  set-keychain-settings) ;;
  lock-keychain) rm -f '{state}/unlocked' ;;
  delete-keychain) rm -f "$2" ;;
  find-identity)
    printf '  1) %s "Steve Node"\n     1 identities found\n' "$(printf '%s' '{sha1}' | tr 'a-f' 'A-F')"
    ;;
  *) exit 24 ;;
esac
"#,
                argv = argv_log.display(),
                events = events_log.display(),
                state = state.display(),
            ),
        );

        let codesign = root.join("fake-codesign");
        executable(
            &codesign,
            &format!(
                r#"#!/bin/sh
set -eu
printf 'codesign %s\n' "$*" >> '{argv}'
printf 'codesign:%s\n' "$1" >> '{events}'
case "$1" in
  --force)
    [ -f '{state}/unlocked' ] || exit 31
    if [ -f '{state}/fail-sign' ]; then exit 32; fi
    req=''
    target=''
    shift
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -r) req="$2"; shift 2 ;;
        --sign|--identifier|--keychain) shift 2 ;;
        *) target="$1"; shift ;;
      esac
    done
    req=${{req#=}}
    # Embed the designated requirement at the end of the fake binary so atomic rename carries it.
    printf '\n#FAKE_SIG:%s\n' "$req" >> "$target"
    ;;
  --display)
    target="$3"
    line=$(grep -a '^#FAKE_SIG:' "$target" | tail -n 1 || true)
    [ -n "$line" ] || exit 33
    printf '%s\n' "${{line#\#FAKE_SIG:}}"
    ;;
  --verify)
    want="${{4#=}}"
    target="$5"
    line=$(grep -a '^#FAKE_SIG:' "$target" | tail -n 1 || true)
    [ -n "$line" ] || exit 34
    actual="${{line#\#FAKE_SIG:designated => }}"
    [ "$actual" = "$want" ] || exit 35
    ;;
  *) exit 39 ;;
esac
"#,
                argv = argv_log.display(),
                events = events_log.display(),
                state = state.display(),
            ),
        );

        (
            dir,
            home,
            Tools {
                codesign,
                security,
                openssl,
                shell: "/bin/sh".into(),
            },
        )
    }

    #[test]
    fn node_local_self_signing_creates_reuses_and_keeps_identity_stable_across_versions() {
        let (dir, home, tools) = node_signing_fixture();
        let root = fs::canonicalize(dir.path()).unwrap();
        let label = "io.github.infranite.steve.node";

        let identity = ensure_node_identity(&home, label, &tools).unwrap();
        assert_eq!(identity.label, label);
        assert_eq!(identity.certificate_name, NODE_CERTIFICATE_NAME);
        assert_eq!(
            identity.certificate_sha1,
            "0123456789abcdef0123456789abcdef01234567"
        );
        // Keychain, identity metadata and key store entry all have strict private permissions.
        assert_eq!(
            fs::metadata(node_signing_identity_path(&home))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(node_signing_password_path(&home))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(home.join(NODE_KEYSTORE_DIR)).unwrap().mode() & 0o777,
            0o700
        );
        // Temporary key/cert/p12 files were cleaned up; password never leaked to argv or identity.json.
        let signing_entries: Vec<_> = fs::read_dir(home.join("signing"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(signing_entries.len(), 2, "{signing_entries:?}");
        let password = load_node_signing_password(&home).unwrap();
        assert_eq!(password.len(), 64);
        assert!(
            !fs::read_to_string(root.join("argv.log"))
                .unwrap()
                .contains(password.as_str())
        );
        assert!(
            !fs::read_to_string(node_signing_identity_path(&home))
                .unwrap()
                .contains(password.as_str())
        );
        assert!(!root.join("fake-state/unlocked").exists());

        // Re-running ensure_node_identity reuses the existing identity without invoking openssl/security.
        let events_before = fs::read_to_string(root.join("events.log")).unwrap();
        assert_eq!(
            ensure_node_identity(&home, label, &tools).unwrap(),
            identity
        );
        assert_eq!(
            fs::read_to_string(root.join("events.log")).unwrap(),
            events_before
        );

        // Two distinct versions signed by this installation share the exact same designated requirements.
        let v1_node = root.join("v1-steve-node");
        let v2_node = root.join("v2-steve-node");
        let v1_cli = root.join("v1-steve");
        let v2_cli = root.join("v2-steve");
        for (path, body) in [
            (&v1_node, "#!/bin/sh\necho v1-node\n"),
            (&v2_node, "#!/bin/sh\necho v2-node\n"),
            (&v1_cli, "#!/bin/sh\necho v1-cli\n"),
            (&v2_cli, "#!/bin/sh\necho v2-cli\n"),
        ] {
            executable(path, body);
        }
        sign_node_binary(
            &home,
            &v1_node,
            &identity,
            &identity.node_identifier,
            &tools,
        )
        .unwrap();
        sign_node_binary(
            &home,
            &v2_node,
            &identity,
            &identity.node_identifier,
            &tools,
        )
        .unwrap();
        sign_node_binary(&home, &v1_cli, &identity, &identity.cli_identifier, &tools).unwrap();
        sign_node_binary(&home, &v2_cli, &identity, &identity.cli_identifier, &tools).unwrap();
        let runner = home.join("services/node-boot-runner");
        let staged_runner =
            stage_signed_node_runner(&home, &v1_cli, &home.join("services"), &identity, &tools)
                .unwrap();
        fs::rename(&staged_runner, &runner).unwrap();

        assert_eq!(
            read_node_designated_requirement(&v1_node, &tools).unwrap(),
            Some(identity.node_requirement())
        );
        assert_eq!(
            read_node_designated_requirement(&v2_node, &tools).unwrap(),
            Some(identity.node_requirement())
        );
        assert_eq!(
            read_node_designated_requirement(&v1_cli, &tools).unwrap(),
            Some(identity.cli_requirement())
        );
        assert_eq!(
            read_node_designated_requirement(&v2_cli, &tools).unwrap(),
            Some(identity.cli_requirement())
        );
        assert_eq!(
            read_node_designated_requirement(&runner, &tools).unwrap(),
            Some(identity.cli_requirement())
        );
        verify_node_binary(&v2_node, &identity.node_requirement(), &tools).unwrap();
        verify_node_binary(&v2_cli, &identity.cli_requirement(), &tools).unwrap();
        verify_node_binary(&runner, &identity.cli_requirement(), &tools).unwrap();
        assert!(!root.join("fake-state/unlocked").exists());

        // Already-signed binary does not unlock the keychain again.
        let before_idempotent = fs::read_to_string(root.join("events.log")).unwrap();
        sign_node_binary(
            &home,
            &v2_node,
            &identity,
            &identity.node_identifier,
            &tools,
        )
        .unwrap();
        let after_idempotent = fs::read_to_string(root.join("events.log")).unwrap();
        assert!(!after_idempotent[before_idempotent.len()..].contains("unlock-keychain"));

        // Different certificate or ad-hoc binary fails verification.
        let other_cert = self_signed_requirement(
            &identity.node_identifier,
            "ffffffffffffffffffffffffffffffffffffffff",
        );
        assert!(verify_node_binary(&v2_node, &other_cert, &tools).is_err());
        let adhoc = root.join("adhoc");
        executable(&adhoc, "#!/bin/sh\necho adhoc\n");
        assert!(verify_node_binary(&adhoc, &identity.node_requirement(), &tools).is_err());
    }

    #[test]
    fn node_signing_refuses_partial_identity_loose_keystore_and_relocks_on_failure() {
        let (dir, home, tools) = node_signing_fixture();
        let root = fs::canonicalize(dir.path()).unwrap();
        let label = "io.github.infranite.steve.node";

        // Incomplete signing state is refused rather than silently regenerated.
        private_dir(&home.join("signing")).unwrap();
        fs::write(node_signing_keychain(&home), b"partial").unwrap();
        assert!(
            ensure_node_identity(&home, label, &tools)
                .unwrap_err()
                .to_string()
                .contains("incomplete Node signing identity")
        );
        fs::remove_file(node_signing_keychain(&home)).unwrap();

        let identity = ensure_node_identity(&home, label, &tools).unwrap();
        // Wrong label is refused.
        assert!(ensure_node_identity(&home, "other.label", &tools).is_err());

        // Group-readable password file is refused.
        fs::set_permissions(
            node_signing_password_path(&home),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(load_node_identity(&home, label).is_err());
        fs::set_permissions(
            node_signing_password_path(&home),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        // Failed codesign leaves original binary intact and relocks the keychain.
        let bin = root.join("unsigned-node");
        executable(&bin, "#!/bin/sh\necho intact\n");
        let original = fs::read(&bin).unwrap();
        fs::write(root.join("fake-state/fail-sign"), b"1").unwrap();
        assert!(
            sign_node_binary(&home, &bin, &identity, &identity.node_identifier, &tools).is_err()
        );
        assert_eq!(fs::read(&bin).unwrap(), original);
        assert!(!root.join("fake-state/unlocked").exists());
        assert!(
            fs::read_to_string(root.join("events.log"))
                .unwrap()
                .ends_with("security:lock-keychain\n")
        );
        assert!(
            !fs::read_to_string(root.join("argv.log"))
                .unwrap()
                .contains(load_node_signing_password(&home).unwrap().as_str())
        );

        // Unsafe path with quote is rejected before calling security.
        let quoted = root.join("bad\"home");
        fs::create_dir(&quoted).unwrap();
        assert!(
            ensure_node_identity(&quoted, label, &tools)
                .unwrap_err()
                .to_string()
                .contains("represented safely")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "creates a temporary macOS keychain and runs real openssl/security/codesign; opt-in only"]
    fn real_macos_self_signed_identity_persists_across_two_built_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("node-home");
        fs::create_dir(&home).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("/usr/bin/security")
                    .args(["delete-keychain"])
                    .arg(&self.0)
                    .output();
            }
        }
        let _cleanup = Cleanup(node_signing_keychain(&home));
        let tools = Tools::default();
        let identity =
            ensure_node_identity(&home, "io.github.infranite.steve.node", &tools).unwrap();
        let v1 = root.join("steve-node-v1");
        let v2 = root.join("steve-node-v2");
        fs::copy("/usr/bin/true", &v1).unwrap();
        fs::copy("/usr/bin/false", &v2).unwrap();
        fs::set_permissions(&v1, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&v2, fs::Permissions::from_mode(0o755)).unwrap();
        sign_node_binary(&home, &v1, &identity, &identity.node_identifier, &tools).unwrap();
        sign_node_binary(&home, &v2, &identity, &identity.node_identifier, &tools).unwrap();
        let req1 = read_node_designated_requirement(&v1, &tools)
            .unwrap()
            .unwrap();
        let req2 = read_node_designated_requirement(&v2, &tools)
            .unwrap()
            .unwrap();
        assert_eq!(req1, identity.node_requirement());
        assert_eq!(req1, req2);
        verify_node_binary(&v1, &req1, &tools).unwrap();
        verify_node_binary(&v2, &req1, &tools).unwrap();
    }

    #[test]
    fn hub_upgrade_from_bundles_v1_to_v2_preserves_tcc_identifier_requirement_and_stable_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(dir.path()).unwrap();
        let cert_sha1 = "11223344556677889900aabbccddeeff00112233";

        let make_hub_bundle = |dir_name: &str, version: &str, commit: &str| -> PathBuf {
            let candidate = home.join("releases").join(dir_name);
            for sub in ["bin", "ops", "web/dist"] {
                fs::create_dir_all(candidate.join(sub)).unwrap();
            }
            let manifest = json!({
                "format_version": 1,
                "version": version,
                "commit": commit,
                "hub_version": version,
                "web_version": version,
                "schema": { "target": 54 }
            });
            fs::write(
                candidate.join("release.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            fs::write(
                candidate.join("web/dist/release.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            fs::write(
                candidate.join("web/dist/index.html"),
                format!("<html><head><meta name=\"steve-web-version\" content=\"{version}\"></head></html>"),
            )
            .unwrap();
            executable(
                &candidate.join("bin/steve-hub"),
                &format!(
                    "#!/bin/sh\nif [ \"$2\" = provision-signing ]; then\n  test \"$(cat)\" = test-password\n  exit $?\nfi\nprintf '%s\\n' '{{\"version\":\"{version}\",\"commit\":\"{commit}\",\"target\":\"aarch64-apple-darwin\",\"schema\":54}}'\n"
                ),
            );
            executable(
                &candidate.join("bin/steve-keyhelper"),
                "#!/bin/sh\nexit 0\n",
            );
            executable(&candidate.join("ops/run.sh"), "#!/bin/sh\nexit 0\n");
            executable(&candidate.join("ops/release-lib.sh"), "# fixture\n");
            executable(
                &candidate.join("ops/sign-setup.sh"),
                "#!/bin/sh\nset -eu\numask 077\nprintf keychain > \"$STEVE_HOME/signing/steve-signing.keychain-db\"\nprintf test-password > \"$STEVE_HOME/signing/.setup-password\"\n",
            );
            fs::canonicalize(candidate).unwrap()
        };

        let commit_v1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let commit_v2 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let v1 = make_hub_bundle("bundles-v1", "1.0.0", commit_v1);
        let v2 = make_hub_bundle("bundles-v2", "1.1.0", commit_v2);

        let codesign = home.join("tcc-codesign");
        executable(
            &codesign,
            &format!(
                r#"#!/bin/sh
set -eu
case "$1" in
  --force)
    ident=''
    target=''
    shift
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --identifier) ident="$2"; shift 2 ;;
        --sign|--keychain) shift 2 ;;
        *) target="$1"; shift ;;
      esac
    done
    printf '\n#HUB_ID:%s\n#HUB_REQ:designated => identifier "%s" and certificate leaf = H"{cert_sha1}"\n' "$ident" "$ident" >> "$target"
    ;;
  --verify)
    target="$3"
    grep -a -q '^#HUB_ID:' "$target"
    ;;
  -dv)
    target="$2"
    id=$(grep -a '^#HUB_ID:' "$target" | tail -n 1 | sed 's/^#HUB_ID://')
    [ -n "$id" ] || exit 1
    printf 'Identifier=%s\n' "$id" >&2
    ;;
  --display)
    target="$3"
    req=$(grep -a '^#HUB_REQ:' "$target" | tail -n 1 | sed 's/^#HUB_REQ://')
    [ -n "$req" ] || exit 1
    printf '%s\n' "$req"
    ;;
  *) exit 9 ;;
esac
"#
            ),
        );
        let security = home.join("tcc-security");
        executable(
            &security,
            "#!/bin/sh\nif [ \"$1\" = '-i' ]; then cat >/dev/null; fi\nexit 0\n",
        );
        let tools = Tools {
            codesign,
            security,
            openssl: "/usr/bin/false".into(),
            shell: "/bin/sh".into(),
        };

        // 1. Setup signs bundles/v1 and links <home>/current -> v1.
        setup_with(&home, &v1, "io.github.infranite.steve.hub", &tools).unwrap();
        std::os::unix::fs::symlink(&v1, home.join("current")).unwrap();
        let setup_tcc_path = crate::service::file_access::tcc_executable_path("hub", &home);
        assert_eq!(setup_tcc_path, home.join("current/bin/steve-hub"));
        assert_eq!(
            fs::canonicalize(&setup_tcc_path).unwrap(),
            v1.join("bin/steve-hub")
        );

        // 2. Upgrade prepares bundles/v2 against the existing identity and checks TCC continuity.
        prepare_with(&home, &v2, "1.1.0", commit_v2, "test-password", &tools).unwrap();
        let continuity_req = verify_tcc_signing_continuity(
            &v1.join("bin/steve-hub"),
            &v2.join("bin/steve-hub"),
            "io.github.infranite.steve.hub",
            &tools,
        )
        .unwrap();
        assert_eq!(
            continuity_req,
            self_signed_requirement("io.github.infranite.steve.hub", cert_sha1)
        );

        // 3. Switch <home>/current -> v2; setup and upgrade keep the exact same TCC path.
        fs::remove_file(home.join("current")).unwrap();
        std::os::unix::fs::symlink(&v2, home.join("current")).unwrap();
        let upgrade_tcc_path = crate::service::file_access::tcc_executable_path("hub", &home);
        assert_eq!(setup_tcc_path, upgrade_tcc_path);
        assert_eq!(
            fs::canonicalize(&upgrade_tcc_path).unwrap(),
            v2.join("bin/steve-hub")
        );

        // 4. An ad-hoc / unsigned candidate without a certificate leaf fails TCC continuity.
        let adhoc = home.join("adhoc-hub");
        executable(&adhoc, "#!/bin/sh\n#HUB_ID:io.github.infranite.steve.hub\n");
        assert!(
            verify_tcc_signing_continuity(
                &v1.join("bin/steve-hub"),
                &adhoc,
                "io.github.infranite.steve.hub",
                &tools
            )
            .is_err()
        );
    }

    #[test]
    fn node_upgrade_from_bundles_v1_to_v2_preserves_tcc_identifier_requirement_and_stable_runner() {
        let (dir, home, tools) = node_signing_fixture();
        let root = fs::canonicalize(dir.path()).unwrap();
        let label = "io.github.infranite.steve.node";

        let identity = ensure_node_identity(&home, label, &tools).unwrap();
        let bundles_v1 = root.join("bundles/v1/bin");
        let bundles_v2 = root.join("bundles/v2/bin");
        fs::create_dir_all(&bundles_v1).unwrap();
        fs::create_dir_all(&bundles_v2).unwrap();

        let v1_node = bundles_v1.join("steve-node");
        let v1_cli = bundles_v1.join("steve");
        let v2_node = bundles_v2.join("steve-node");
        let v2_cli = bundles_v2.join("steve");
        for (path, body) in [
            (&v1_node, "#!/bin/sh\necho node-v1\n"),
            (&v1_cli, "#!/bin/sh\necho cli-v1\n"),
            (&v2_node, "#!/bin/sh\necho node-v2\n"),
            (&v2_cli, "#!/bin/sh\necho cli-v2\n"),
        ] {
            executable(path, body);
        }

        // Setup signs v1 binaries and stages the stable boot runner at services/node-boot-runner.
        sign_node_binary(
            &home,
            &v1_node,
            &identity,
            &identity.node_identifier,
            &tools,
        )
        .unwrap();
        sign_node_binary(&home, &v1_cli, &identity, &identity.cli_identifier, &tools).unwrap();
        let setup_runner = crate::service::file_access::tcc_executable_path("node", &home);
        let staged_v1 =
            stage_signed_node_runner(&home, &v1_cli, &home.join("services"), &identity, &tools)
                .unwrap();
        fs::rename(&staged_v1, &setup_runner).unwrap();

        // Upgrade reuses the exact same identity on bundles/v2 and updates the stable runner in place.
        let reloaded = ensure_node_identity(&home, label, &tools).unwrap();
        assert_eq!(reloaded, identity);
        sign_node_binary(
            &home,
            &v2_node,
            &reloaded,
            &reloaded.node_identifier,
            &tools,
        )
        .unwrap();
        sign_node_binary(&home, &v2_cli, &reloaded, &reloaded.cli_identifier, &tools).unwrap();
        let upgrade_runner = crate::service::file_access::tcc_executable_path("node", &home);
        assert_eq!(setup_runner, upgrade_runner);
        let staged_v2 =
            stage_signed_node_runner(&home, &v2_cli, &home.join("services"), &reloaded, &tools)
                .unwrap();
        fs::rename(&staged_v2, &upgrade_runner).unwrap();

        assert_eq!(
            read_node_designated_requirement(&v1_node, &tools).unwrap(),
            read_node_designated_requirement(&v2_node, &tools).unwrap()
        );
        assert_eq!(
            read_node_designated_requirement(&v1_node, &tools).unwrap(),
            Some(identity.node_requirement())
        );
        assert_eq!(
            read_node_designated_requirement(&v1_cli, &tools).unwrap(),
            read_node_designated_requirement(&v2_cli, &tools).unwrap()
        );
        assert_eq!(
            read_node_designated_requirement(&upgrade_runner, &tools).unwrap(),
            Some(identity.cli_requirement())
        );
    }
}
