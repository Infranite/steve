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
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

#[derive(Clone)]
pub(crate) struct Tools {
    pub codesign: PathBuf,
    pub security: PathBuf,
    pub shell: PathBuf,
}
impl Default for Tools {
    fn default() -> Self {
        Self {
            codesign: "/usr/bin/codesign".into(),
            security: "/usr/bin/security".into(),
            shell: "/bin/zsh".into(),
        }
    }
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
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("start native maintenance command")?;
    let stdout = child.stdout.take().context("command stdout missing")?;
    let stderr = child.stderr.take().context("command stderr missing")?;
    let drain = |mut source: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let out = drain(Box::new(stdout));
    let err = drain(Box::new(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("native maintenance command timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader stopped"))??,
        stderr: err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader stopped"))??,
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
        Duration::from_secs(15),
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
        Duration::from_secs(30),
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
        Duration::from_secs(30),
    )?;
    ensure!(
        identifier(binary, tools)? == expected,
        "installed code signature identifier differs from signing identity"
    );
    Ok(())
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
            Duration::from_secs(30),
        );
    }
}
fn unlock<'a>(
    keychain: &'a Path,
    password: &str,
    tools: &'a Tools,
    timeout: Duration,
) -> Result<Unlocked<'a>> {
    use std::os::fd::AsRawFd;

    ensure!(
        !password.is_empty()
            && !password
                .chars()
                .any(|c| c.is_whitespace() || "\"'\\".contains(c)),
        "signing password cannot be represented safely"
    );
    let keychain_text = keychain.to_string_lossy();
    ensure!(
        !keychain_text.contains(['"', '\\', '\n', '\r']),
        "signing keychain path cannot be represented safely"
    );
    let mut command = Command::new(&tools.security);
    command
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Even a failed or interrupted command may have unlocked the keychain.
    let guard = Unlocked { keychain, tools };
    let mut child = command.spawn()?;
    let input = Zeroizing::new(format!(
        "unlock-keychain -p {password} \"{keychain_text}\"\n"
    ));
    let deadline = Instant::now() + timeout;
    let result = (|| -> Result<()> {
        let mut stdin = child.stdin.take().context("keychain stdin missing")?;
        let fd = stdin.as_raw_fd();
        // Bound writing as well as waiting, even if security stops reading stdin.
        // SAFETY: stdin keeps this descriptor valid for both flag operations.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
            "could not configure keychain input"
        );
        let mut written = 0;
        while written < input.len() {
            ensure!(
                Instant::now() < deadline,
                "signing keychain unlock timed out"
            );
            match stdin.write(&input.as_bytes()[written..]) {
                Ok(0) => bail!("keychain input closed"),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error.into()),
            }
        }
        drop(stdin);
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "signing keychain did not unlock");
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "signing keychain unlock timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    Ok(guard)
}
fn sign(binary: &Path, keychain: &Path, password: &str, id: &str, tools: &Tools) -> Result<()> {
    sign_with_timeout(
        binary,
        keychain,
        password,
        id,
        tools,
        Duration::from_secs(30),
    )
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
        Duration::from_secs(30),
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
        Duration::from_secs(60),
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
    sign(
        &candidate.join("bin/steve-hub"),
        &keychain,
        password,
        ident["hub_identifier"].as_str().unwrap(),
        tools,
    )?;
    ensure!(
        hash(&helper)? == helper_hash && build_info(&candidate.join("bin/steve-hub"))? == info,
        "installed helper or prepared Hub identity changed"
    );
    Ok(json!({"candidate":candidate,"build":info,"source":"release-bundle"}))
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
            fs::symlink_metadata(&keychain).is_err()
                && fs::symlink_metadata(signing.join("keychain-pass")).is_err(),
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
    let output = command.stdin(Stdio::inherit()).output()?;
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
    let password_file = signing.join("keychain-pass");
    owned_file(&password_file)?;
    let password = Zeroizing::new(fs::read_to_string(&password_file)?.trim().to_owned());
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
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
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
            "#!/bin/sh\nprintf '%s\\n' '{\"version\":\"1.2.3\",\"commit\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"target\":\"aarch64-apple-darwin\",\"schema\":54}'\n",
        );
        executable(
            &candidate.join("bin/steve-keyhelper"),
            "#!/bin/sh\nexit 0\n",
        );
        executable(&candidate.join("ops/run.sh"), "#!/bin/sh\nexit 0\n");
        executable(&candidate.join("ops/release-lib.sh"), "# fixture\n");
        executable(
            &candidate.join("ops/sign-setup.sh"),
            "#!/bin/sh\nset -eu\numask 077\nprintf keychain > \"$STEVE_HOME/signing/steve-signing.keychain-db\"\nprintf test-password > \"$STEVE_HOME/signing/keychain-pass\"\n",
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
            while !fs::read_to_string(&log).is_ok_and(|events| events.contains("-i\n")) {
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "unlock fixture did not become ready"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
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
}
