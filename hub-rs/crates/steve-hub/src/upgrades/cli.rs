//! Local OS-owner upgrades, without a browser session or an interpreter.
//! Only the independently launched signed Hub runner obtains signing credentials.
use super::*;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::process::{Command, Output, Stdio};

const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const MAX_JSON: u64 = 1024 * 1024;

#[derive(Debug, PartialEq)]
struct Arguments {
    command: String,
    version: Option<String>,
    key: Option<String>,
    operation: Option<String>,
    timeout: Duration,
}

fn stable(value: &str) -> Result<semver::Version> {
    let parsed = semver::Version::parse(value)?;
    if parsed.to_string() != value || !parsed.pre.is_empty() || !parsed.build.is_empty() {
        bail!("version must be a stable X.Y.Z without a prefix or suffix")
    }
    Ok(parsed)
}

fn arguments(args: &[String]) -> Result<Arguments> {
    let command = args
        .first()
        .context("check, request, status or watch required")?
        .clone();
    if !["check", "request", "status", "watch"].contains(&command.as_str()) {
        bail!("unknown upgrade command")
    }
    let mut result = Arguments {
        command,
        version: None,
        key: None,
        operation: None,
        timeout: Duration::from_secs(600),
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 1;
    while index < args.len() {
        let option = &args[index];
        if !seen.insert(option) {
            bail!("duplicate option {option}")
        }
        let value = args.get(index + 1).context("option requires a value")?;
        match (result.command.as_str(), option.as_str()) {
            ("check" | "request", "--version") => {
                stable(value)?;
                result.version = Some(value.clone());
            }
            ("request", "--key") if !value.is_empty() && value.len() <= 200 => {
                result.key = Some(value.clone())
            }
            ("status" | "watch", "--operation") => {
                canonical_id(value)?;
                result.operation = Some(value.clone());
            }
            ("watch", "--timeout") => {
                let seconds: f64 = value.parse()?;
                if !seconds.is_finite() || seconds <= 0.0 || seconds > 86400.0 {
                    bail!("watch timeout must be within 0..86400 seconds")
                }
                result.timeout = Duration::from_secs_f64(seconds);
            }
            _ => bail!("unsupported upgrade option {option}"),
        }
        index += 2;
    }
    if result.command == "request" && (result.version.is_none() || result.key.is_none()) {
        bail!("request requires --version and --key")
    }
    Ok(result)
}

fn canonical_id(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value)?.to_string() != value {
        bail!("operation must be a canonical UUID")
    }
    Ok(())
}

fn owned(path: &Path, directory: bool, uid: u32) -> Result<()> {
    let info = fs::symlink_metadata(path)?;
    if info.uid() != uid
        || info.mode() & 0o022 != 0
        || info.file_type().is_symlink()
        || if directory {
            !info.is_dir()
        } else {
            !info.is_file()
        }
    {
        bail!(
            "installation path must be owned by this user, without links or group/other writes: {}",
            path.display()
        )
    }
    Ok(())
}

fn document(path: &Path, uid: u32) -> Result<Value> {
    use std::io::Read;
    owned(path, false, uid)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let info = file.metadata()?;
    if info.len() > MAX_JSON || !info.is_file() || info.uid() != uid || info.mode() & 0o022 != 0 {
        bail!("metadata exceeds size limit")
    }
    let mut bytes = Vec::new();
    file.take(MAX_JSON + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JSON {
        bail!("metadata exceeds size limit")
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        bail!("metadata must be an object")
    }
    Ok(value)
}

fn directory(path: &Path, uid: u32) -> Result<()> {
    owned(path.parent().context("directory needs parent")?, true, uid)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
        _ => {}
    }
    owned(path, true, uid)
}

fn owned_lock(path: &Path, uid: u32, release: bool) -> Result<File> {
    use std::os::fd::AsRawFd;
    owned(path.parent().context("lock needs parent")?, true, uid)?;
    if fs::symlink_metadata(path).is_ok() {
        owned(path, false, uid)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    owned(path, false, uid)?;
    if release {
        // SAFETY: flock is initialized, its pointer and the file descriptor stay valid.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as libc::c_short;
        lock.l_whence = libc::SEEK_SET as libc::c_short;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } != 0 {
            bail!("another release owns the installation")
        }
    } else {
        file.try_lock().context("another upgrade owns this lock")?;
    }
    Ok(file)
}

fn literal(raw: &str) -> Result<String> {
    if raw.chars().any(|c| "$`\\;\n\r".contains(c)) {
        bail!("upgrade settings require literal assignments")
    }
    let raw = raw.trim();
    let value = if raw.starts_with(['\'', '"']) {
        let quote = raw.chars().next().unwrap();
        let end = raw[1..].find(quote).context("unterminated setting quote")? + 1;
        let rest = raw[end + 1..].trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            bail!("setting must have one literal value")
        }
        raw[1..end].to_owned()
    } else {
        let words: Vec<_> = raw.split_whitespace().collect();
        if words.is_empty()
            || (words.len() > 1 && !words[1].starts_with('#'))
            || words[0].contains(['\'', '"'])
        {
            bail!("setting must have one literal value")
        }
        words[0].to_owned()
    };
    if value.is_empty() {
        bail!("empty installation setting")
    }
    Ok(value)
}

fn settings(text: &str, environment: &Value) -> Result<(String, String)> {
    let keys = [
        "STEVE_UPDATE_REPOSITORY",
        "STEVE_PORT",
        "STEVE_INSTALL_KIND",
    ];
    let mut values = std::collections::BTreeMap::new();
    for key in keys {
        if let Some(value) = environment.get(key).and_then(Value::as_str) {
            values.insert(key.to_owned(), value.to_owned());
        }
    }
    let pattern = regex::Regex::new(
        r"^\s*(?:export\s+)?(STEVE_UPDATE_REPOSITORY|STEVE_PORT|STEVE_INSTALL_KIND)\s*=(.*)$",
    )?;
    let mut seen = std::collections::BTreeSet::new();
    for line in text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        if let Some(parts) = pattern.captures(line) {
            let key = parts[1].to_owned();
            if !seen.insert(key.clone()) {
                bail!("duplicate installation setting")
            }
            values.insert(key, literal(&parts[2])?);
        } else if keys.iter().any(|key| line.contains(key)) {
            bail!("upgrade settings require literal assignments")
        }
    }
    if values
        .get("STEVE_INSTALL_KIND")
        .is_some_and(|s| s == "external")
    {
        bail!("installation is externally managed")
    }
    let repository = values
        .get("STEVE_UPDATE_REPOSITORY")
        .cloned()
        .unwrap_or_else(|| "Infranite/steve".into());
    let repository_pattern =
        regex::Regex::new(r"^[A-Za-z0-9_-][A-Za-z0-9_.-]*/[A-Za-z0-9_-][A-Za-z0-9_.-]*$")?;
    if !repository_pattern.is_match(&repository) || repository.split('/').any(|p| p.ends_with('.'))
    {
        bail!("invalid installed repository")
    }
    let port = values
        .get("STEVE_PORT")
        .cloned()
        .unwrap_or_else(|| "8440".into());
    if port.is_empty()
        || !port.bytes().all(|c| c.is_ascii_digit())
        || port.parse::<u16>().ok().filter(|v| *v > 0).is_none()
    {
        bail!("invalid installed port")
    }
    Ok((repository, port))
}

struct Owner {
    uid: u32,
    account_home: PathBuf,
    home: PathBuf,
    identity: Value,
    binary: PathBuf,
    info: Value,
    repository: String,
    port: String,
    #[cfg(test)]
    execute: Option<TestExecute>,
}

#[cfg(test)]
type TestExecute = std::sync::Arc<dyn Fn(&[&str]) -> Result<Output> + Send + Sync>;

impl Owner {
    fn load(home: &Path) -> Result<Self> {
        if !cfg!(target_os = "macos") {
            bail!("local signed-Hub upgrades require macOS; Linux uses the managed Hub update API")
        }
        // SAFETY: querying process/account identifiers has no pointer arguments.
        let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
        if uid == 0 || uid != euid {
            bail!("run as the installation's OS user without sudo or setuid")
        }
        let account_home =
            fs::canonicalize(runner::account_home().context("OS account home unavailable")?)?;
        Self::inspect(
            home,
            account_home,
            uid,
            #[cfg(test)]
            None,
        )
    }

    fn inspect(
        home: &Path,
        account_home: PathBuf,
        uid: u32,
        #[cfg(test)] execute: Option<TestExecute>,
    ) -> Result<Self> {
        let expected = account_home.join(".steve");
        if home != expected {
            bail!("signed upgrade home must be the OS account's default .steve directory")
        }
        owned(&account_home, true, uid)?;
        owned(home, true, uid)?;
        for name in ["signing", "releases", "bin"] {
            owned(&home.join(name), true, uid)?;
        }
        let identity = document(&home.join("signing/identity.json"), uid)?;
        let identifier = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,200}$")?;
        for key in [
            "label",
            "hub_identifier",
            "keyhelper_identifier",
            "vault_service",
        ] {
            if !identity
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| identifier.is_match(s))
            {
                bail!("invalid signing identity field {key}")
            }
        }
        owned(&home.join("signing/steve-signing.keychain-db"), false, uid)?;
        let current = home.join("current");
        if fs::symlink_metadata(&current)?.uid() != uid {
            bail!("current belongs to another OS user")
        }
        let release = fs::canonicalize(current)?;
        if release.parent() != Some(home.join("releases").as_path()) {
            bail!("current must identify a direct release child")
        }
        owned(&release, true, uid)?;
        owned(&release.join("bin"), true, uid)?;
        let mut owner = Self {
            uid,
            account_home,
            home: home.into(),
            identity,
            binary: release.join("bin/steve-hub"),
            info: Value::Null,
            repository: "Infranite/steve".into(),
            port: "8440".into(),
            #[cfg(test)]
            execute,
        };
        owner.signature(
            &owner.binary,
            owner.identity["hub_identifier"].as_str().unwrap(),
        )?;
        owner.signature(
            &home.join("bin/steve-keyhelper"),
            owner.identity["keyhelper_identifier"].as_str().unwrap(),
        )?;
        let info: Value = serde_json::from_slice(
            &owner
                .command(
                    &[
                        owner.binary.to_str().context("non-UTF8 binary path")?,
                        "--build-info",
                    ],
                    true,
                )?
                .stdout,
        )?;
        let manifest = document(&release.join("release.json"), uid)?;
        stable(
            info["version"]
                .as_str()
                .context("missing installed version")?,
        )?;
        if info["target"] != "aarch64-apple-darwin"
            || info["commit"]
                .as_str()
                .is_none_or(|s| s.len() != 40 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
            || ["version", "commit"]
                .iter()
                .any(|key| info[*key] != manifest[*key])
        {
            bail!("installed Hub and release identities disagree")
        }
        if info["upgrade_runner"]["journal"] != 1 || info["upgrade_runner"]["native"] != true {
            bail!("installed Hub does not provide the native upgrade runner")
        }
        owner.info = info;
        let config = owner
            .account_home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", owner.label()));
        owned(config.parent().unwrap().parent().unwrap(), true, uid)?;
        owned(config.parent().unwrap(), true, uid)?;
        let definition = owner.plist(&config)?;
        if definition["Label"] != owner.identity["label"]
            || ["Program", "RootDirectory", "UserName", "GroupName"]
                .iter()
                .any(|key| definition.get(*key).is_some())
        {
            bail!("signed Hub service ownership is ambiguous")
        }
        let expected_legacy = json!(["/bin/zsh", home.join("current/ops/run.sh")]);
        if definition["ProgramArguments"] != expected_legacy {
            bail!("signed Hub service does not use its installed entry point")
        }
        let environment = definition
            .get("EnvironmentVariables")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !environment
            .as_object()
            .is_some_and(|map| map.values().all(Value::is_string))
        {
            bail!("invalid installed service environment")
        }
        if environment
            .get("STEVE_HOME")
            .is_some_and(|p| p.as_str() != home.to_str())
            || environment
                .get("HOME")
                .is_some_and(|p| p.as_str() != owner.account_home.to_str())
        {
            bail!("installed service names another home")
        }
        let env_file = home.join("steve.env");
        let env_text = if fs::symlink_metadata(&env_file).is_ok() {
            owned(&env_file, false, uid)?;
            fs::read_to_string(env_file)?
        } else {
            String::new()
        };
        (owner.repository, owner.port) = settings(&env_text, &environment)?;
        let record = home.join("services/hub.json");
        if fs::symlink_metadata(&record).is_ok() {
            owned(record.parent().unwrap(), true, uid)?;
            if document(&record, uid)?["label"] != owner.identity["label"] {
                bail!("service record differs from signing identity")
            }
        }
        Ok(owner)
    }

    fn actor(&self) -> String {
        format!("local-os:{}", self.uid)
    }
    fn label(&self) -> &str {
        self.identity["label"].as_str().unwrap()
    }
    fn environment(&self) -> Vec<(String, String)> {
        [
            ("HOME", self.account_home.to_string_lossy().into_owned()),
            ("PATH", SAFE_PATH.into()),
            ("LANG", "en_US.UTF-8".into()),
            ("STEVE_HOME", self.home.to_string_lossy().into_owned()),
            ("STEVE_PORT", self.port.clone()),
            ("STEVE_UPDATE_REPOSITORY", self.repository.clone()),
            ("NO_COLOR", "1".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect()
    }
    fn command(&self, args: &[&str], check: bool) -> Result<Output> {
        let mut command = Command::new(args[0]);
        command
            .args(&args[1..])
            .env_clear()
            .envs(self.environment())
            .stdin(Stdio::null());
        #[cfg(test)]
        let output = if let Some(execute) = &self.execute {
            execute(args)?
        } else {
            bounded_output(command)?
        };
        #[cfg(not(test))]
        let output = bounded_output(command)?;
        if check && !output.status.success() {
            bail!(
                "command {} failed: {}",
                args[0],
                String::from_utf8_lossy(&output.stderr).trim()
            )
        }
        Ok(output)
    }
    fn plist(&self, path: &Path) -> Result<Value> {
        owned(path, false, self.uid)?;
        if fs::metadata(path)?.len() > MAX_JSON {
            bail!("service definition exceeds size limit")
        }
        Ok(serde_json::from_slice(
            &self
                .command(
                    &[
                        "/usr/bin/plutil",
                        "-convert",
                        "json",
                        "-o",
                        "-",
                        path.to_str().context("non-UTF8 service path")?,
                    ],
                    true,
                )?
                .stdout,
        )?)
    }
    fn signature(&self, path: &Path, expected: &str) -> Result<()> {
        owned(path, false, self.uid)?;
        let path = path.to_str().context("non-UTF8 signature path")?;
        self.command(&["/usr/bin/codesign", "--verify", "--strict", path], true)?;
        let info = self.command(&["/usr/bin/codesign", "-dv", path], true)?;
        if String::from_utf8_lossy(&info.stderr)
            .lines()
            .find_map(|line| line.strip_prefix("Identifier="))
            != Some(expected)
        {
            bail!("installed signature differs from signing identity")
        }
        Ok(())
    }
    fn updates(&self) -> Result<Updates> {
        Ok(Updates {
            home: self.home.clone(),
            repository: self.repository.clone(),
            current_hub: self.info["version"].as_str().unwrap().into(),
            current_web: self.info["version"].as_str().unwrap().into(),
            api_base: "https://api.github.com".into(),
            web_base: "https://github.com".into(),
            client: reqwest::Client::builder()
                .no_proxy()
                .user_agent("Steve-local-upgrade/1")
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    let url = attempt.url();
                    if attempt.previous().len() >= 5
                        || url.scheme() != "https"
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || !url.host_str().is_some_and(|host| {
                            ["api.github.com", "github.com"].contains(&host)
                                || host.ends_with(".githubusercontent.com")
                        })
                    {
                        attempt.error("release redirect left GitHub HTTPS")
                    } else {
                        attempt.follow()
                    }
                }))
                .build()?,
            ..Updates::journal(&self.home)
        })
    }
    fn published(&self, version: Option<&str>) -> Result<Manifest> {
        let u = self.updates()?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let version = match version {
                Some(value) => value.to_owned(),
                None => {
                    // This local check does not create an update cache or follow a caller's
                    // updates/ symlink. All writes belong to an explicitly requested operation.
                    let metadata: Value = serde_json::from_slice(
                        &u.fetch(
                            &format!("{}/repos/{}/releases/latest", u.api_base, u.repository),
                            MAX_JSON,
                        )
                        .await?,
                    )?;
                    if metadata["draft"] != false
                        || metadata["prerelease"] != false
                        || !metadata["published_at"].is_string()
                    {
                        bail!("release is not published and stable")
                    }
                    let tag = metadata["tag_name"]
                        .as_str()
                        .context("release tag missing")?;
                    stable(
                        tag.strip_prefix('v')
                            .context("release tag must start with v")?,
                    )?
                    .to_string()
                }
            };
            let (release, manifest) = u.resolve(&version).await?;
            let matching: Vec<_> = manifest
                .assets
                .iter()
                .filter(|a| a.target == "aarch64-apple-darwin")
                .collect();
            if matching.len() != 1
                || matching[0].name
                    != format!("steve-{}-aarch64-apple-darwin.tar.gz", manifest.version)
                || matching[0].size == 0
                || matching[0].size > 512 * 1024 * 1024
                || !release.contains_asset("steve-release.json")
                || !release.contains_asset(&matching[0].name)
            {
                bail!("published release has no matching bundle")
            }
            Ok(manifest)
        })
    }
    fn operation(&self, id: &str) -> Result<Operation> {
        canonical_id(id)?;
        let dir = self.home.join("updates/operations");
        owned(&dir, true, self.uid)?;
        let op: Operation =
            serde_json::from_value(document(&dir.join(format!("{id}.json")), self.uid)?)?;
        if op.id != id {
            bail!("operation journal identity mismatch")
        }
        Ok(op)
    }
    fn active(&self) -> Result<Option<Operation>> {
        let file = self.home.join("updates/active.json");
        if fs::symlink_metadata(&file).is_err() {
            return Ok(None);
        }
        Ok(Some(
            self.operation(
                document(&file, self.uid)?["id"]
                    .as_str()
                    .context("active operation missing id")?,
            )?,
        ))
    }
    fn existing(&self, version: &str, key: &str) -> Result<Option<Operation>> {
        let mut records = vec![];
        let directory = self.home.join("updates/operations");
        if directory.exists() {
            owned(&directory, true, self.uid)?;
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "json") {
                    records.push(
                        self.operation(
                            path.file_stem()
                                .and_then(|s| s.to_str())
                                .context("invalid journal filename")?,
                        )?,
                    );
                }
            }
        }
        let matches: Vec<_> = records
            .iter()
            .filter(|op| op.idempotency_key == key)
            .collect();
        if matches.len() > 1 {
            bail!("duplicate idempotency records require inspection")
        }
        let existing = matches.first().copied();
        if existing.is_some_and(|op| op.version != version || op.requested_by != self.actor()) {
            bail!("idempotency key conflict")
        }
        let pending = records
            .iter()
            .any(|op| !op.terminal() && existing.is_none_or(|old| op.id != old.id));
        let active = self.active()?;
        if let Some(op) = existing {
            if !op.terminal() {
                if pending || active.is_some_and(|active| !active.terminal() && active.id != op.id)
                {
                    bail!("another active operation conflicts with this retry")
                }
                atomic(&self.home.join("updates/active.json"), &json!({"id":op.id}))?;
            }
            return Ok(Some(self.reap(op.clone())?));
        }
        if pending || active.is_some_and(|op| !op.terminal()) {
            bail!("an upgrade or approval is already pending; no operation changed")
        }
        Ok(None)
    }
    fn job(&self) -> Result<&'static str> {
        let output = self.command(
            &[
                "/bin/launchctl",
                "print",
                &format!("gui/{}/{}.upgrade", self.uid, self.label()),
            ],
            false,
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let combined =
                format!("{text}{}", String::from_utf8_lossy(&output.stderr)).to_lowercase();
            return Ok(
                if combined.contains("could not find service")
                    || combined.contains("no such process")
                {
                    "absent"
                } else {
                    "unknown"
                },
            );
        }
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix(&format!("{name} = ")))
        };
        if field("pid")
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|p| p > 0)
        {
            return Ok("live");
        }
        if !matches!(field("state"), Some("exited" | "not running")) {
            return Ok("unknown");
        }
        let Some(path) = field("path").map(PathBuf::from) else {
            return Ok("unknown");
        };
        if path.parent() != Some(self.home.join("updates/runner").as_path()) {
            return Ok("foreign");
        }
        let verified = (|| -> Result<bool> {
            owned(path.parent().unwrap(), true, self.uid)?;
            let def = self.plist(&path)?;
            let op = self.operation(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .context("invalid runner name")?,
            )?;
            let args = def["ProgramArguments"]
                .as_array()
                .context("runner argv missing")?;
            let binary = PathBuf::from(
                args.first()
                    .and_then(Value::as_str)
                    .context("runner binary missing")?,
            );
            Ok(def["Label"] == format!("{}.upgrade", self.label())
                && args.len() == 4
                && args[1] == "--upgrade-runner"
                && args[2].as_str() == self.home.to_str()
                && args[3] == op.id
                && binary
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    == Some(self.home.join("releases").as_path())
                && def.get("Program").is_none())
        })();
        Ok(match verified {
            Ok(true) => "exited",
            Ok(false) => "foreign",
            Err(_) => "unknown",
        })
    }
    fn reap(&self, op: Operation) -> Result<Operation> {
        if op.terminal()
            || op.requested_by != self.actor()
            || op.phase == "awaiting_approval"
            || now().saturating_sub(
                op.runner
                    .as_ref()
                    .and_then(|r| r.launched_at)
                    .unwrap_or(0)
                    .max(op.updated_at),
            ) < 60_000
        {
            return Ok(op);
        }
        let Ok(_runner) = owned_lock(&self.home.join("updates/runner.lock"), self.uid, false)
        else {
            return Ok(op);
        };
        let Ok(_release) = owned_lock(&self.home.join("releases/.release.lock"), self.uid, true)
        else {
            return Ok(op);
        };
        if !matches!(self.job()?, "absent" | "exited") {
            return Ok(op);
        }
        let fresh = self.operation(&op.id)?;
        if serde_json::to_value(&fresh)? != serde_json::to_value(&op)? {
            return Ok(fresh);
        }
        let mut failed = fresh;
        Updates::journal(&self.home).phase(&mut failed,"failed",Some("Upgrade runner disappeared without a result; it was not restarted. Inspect current, the recorded backup and runner log before using a new request key.".into()))?;
        Ok(failed)
    }
    fn request(&self, version: &str, key: &str) -> Result<Operation> {
        let updates = self.home.join("updates");
        directory(&updates, self.uid)?;
        directory(&updates.join("operations"), self.uid)?;
        {
            let _lock = owned_lock(&updates.join("journal.lock"), self.uid, false)?;
            if let Some(op) = self.existing(version, key)? {
                return Ok(op);
            }
        }
        let manifest = self.published(Some(version))?;
        let _journal = owned_lock(&updates.join("journal.lock"), self.uid, false)?;
        let fresh = Self::load(&self.home)?;
        if (
            fresh.binary,
            fresh.info,
            fresh.repository,
            fresh.identity,
            fresh.port,
        ) != (
            self.binary.clone(),
            self.info.clone(),
            self.repository.clone(),
            self.identity.clone(),
            self.port.clone(),
        ) {
            bail!("installation changed during release check")
        }
        if let Some(op) = self.existing(version, key)? {
            return Ok(op);
        }
        if stable(version)? <= stable(self.info["version"].as_str().unwrap())? {
            bail!("upgrade target must be newer than installed Hub")
        }
        let (op, plist) = {
            let _runner = owned_lock(&updates.join("runner.lock"), self.uid, false)?;
            let _release = owned_lock(&self.home.join("releases/.release.lock"), self.uid, true)?;
            let job = self.job()?;
            if !matches!(job, "absent" | "exited") {
                bail!("upgrade job is live or unknown; it was not stopped")
            }
            directory(&self.home.join("logs"), self.uid)?;
            directory(&self.home.join("logs/release"), self.uid)?;
            directory(&updates.join("runner"), self.uid)?;
            let id = uuid::Uuid::new_v4().to_string();
            let at = now();
            let log = self
                .home
                .join("logs/release")
                .join(format!("upgrade-{version}-{}.log", &id[..8]));
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&log)?;
            let op = Operation {
                id: id.clone(),
                idempotency_key: key.into(),
                version: version.into(),
                phase: "staging".into(),
                created_at: at,
                updated_at: at,
                requested_by: self.actor(),
                approved_by: Some(self.actor()),
                approved_at: Some(at),
                error: None,
                previous: None,
                target: None,
                backup: None,
                progress: None,
                drain: None,
                runner: Some(runner::Spec {
                    kind: "release_bundle".into(),
                    source: format!("https://github.com/{}.git", self.repository),
                    commit: Some(manifest.commit),
                    log: Some(log.to_string_lossy().into_owned()),
                    launched_at: Some(at),
                    ..Default::default()
                }),
            };
            let label = format!("{}.upgrade", self.label());
            let plist = updates.join("runner").join(format!("{id}.plist"));
            let args = [
                self.binary.to_string_lossy().into_owned(),
                "--upgrade-runner".into(),
                self.home.to_string_lossy().into_owned(),
                id.clone(),
            ];
            let body = runner::plist(
                &label,
                &args,
                &self.environment(),
                log.to_str().context("invalid log path")?,
            );
            write_bytes(&plist, body.as_bytes())?;
            if job == "exited" {
                self.command(
                    &[
                        "/bin/launchctl",
                        "bootout",
                        &format!("gui/{}/{label}", self.uid),
                    ],
                    true,
                )?;
            }
            Updates::journal(&self.home).save(&op)?;
            atomic(&updates.join("active.json"), &json!({"id":id}))?;
            (op, plist)
        };
        if let Err(error) = self.command(
            &[
                "/bin/launchctl",
                "bootstrap",
                &format!("gui/{}", self.uid),
                plist.to_str().context("invalid runner path")?,
            ],
            true,
        ) {
            let mut fresh = self.operation(&op.id)?;
            if !fresh.terminal() {
                if let (Ok(_runner), Ok(_release)) = (
                    owned_lock(&updates.join("runner.lock"), self.uid, false),
                    owned_lock(&self.home.join("releases/.release.lock"), self.uid, true),
                ) {
                    let latest = self.operation(&op.id)?;
                    if serde_json::to_value(&latest)? != serde_json::to_value(&fresh)? {
                        return Ok(latest);
                    }
                    if self.job()? == "absent" {
                        fresh.phase = "failed".into();
                    }
                    fresh.error = Some(format!("launch result: {error:#}"));
                    fresh.updated_at = now();
                    Updates::journal(&self.home).save(&fresh)?;
                }
            }
        }
        self.operation(&op.id)
    }
    fn status(&self, id: Option<&str>) -> Result<Value> {
        let updates = self.home.join("updates");
        if updates.exists() {
            owned(&updates, true, self.uid)?;
        }
        let mut operation = match id {
            Some(id) => Some(self.operation(id)?),
            None => self.active()?,
        };
        if let Some(op) = operation.as_ref().filter(|op| !op.terminal()) {
            let _journal = owned_lock(&updates.join("journal.lock"), self.uid, false)?;
            operation = Some(self.reap(self.operation(&op.id)?)?);
        }
        Ok(
            json!({"actor":self.actor(),"current":self.info,"operation":operation,"job":self.job()?}),
        )
    }
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file needs parent")?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if tmp.exists() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn bounded_output(mut command: Command) -> Result<Output> {
    use std::io::Read;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("child stdout missing")?;
    let stderr = child.stderr.take().context("child stderr missing")?;
    let collect = |reader: Box<dyn Read + Send>| {
        std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            reader.take(MAX_JSON + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    };
    let out = collect(Box::new(stdout));
    let err = collect(Box::new(stderr));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("command output reader stopped"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("command error reader stopped"))??;
    if stdout.len() as u64 > MAX_JSON || stderr.len() as u64 > MAX_JSON {
        bail!("command output exceeds limit")
    }
    Ok(Output {
        status: status.context("service/signature command timed out")?,
        stdout,
        stderr,
    })
}

pub fn main(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}", json!({"error":format!("{error:#}")}));
            1
        }
    }
}
fn dispatch(args: &[String]) -> Result<i32> {
    if args.is_empty() || args.iter().any(|a| a == "--help") {
        println!(
            "{}",
            json!({"usage":"--upgrade-cli <home> check [--version X.Y.Z] | request --version X.Y.Z --key KEY | status [--operation UUID] | watch [--operation UUID] [--timeout SECONDS]"})
        );
        return Ok(0);
    }
    let home = PathBuf::from(&args[0]);
    let args = arguments(&args[1..])?;
    let owner = Owner::load(&home)?;
    match args.command.as_str() {
        "check" => {
            let manifest = owner.published(args.version.as_deref())?;
            println!(
                "{}",
                json!({"current":owner.info,"version":manifest.version,"commit":manifest.commit,"available":stable(&manifest.version)?>stable(owner.info["version"].as_str().unwrap())?,"runner":"release_bundle","repository":owner.repository,"actor":owner.actor()})
            );
            Ok(0)
        }
        "request" => {
            let op = owner.request(
                args.version.as_deref().unwrap(),
                args.key.as_deref().unwrap(),
            )?;
            println!("{}", serde_json::to_string(&op)?);
            Ok(if matches!(op.phase.as_str(), "failed" | "rolled_back") {
                1
            } else {
                0
            })
        }
        "status" => {
            println!("{}", owner.status(args.operation.as_deref())?);
            Ok(0)
        }
        "watch" => {
            let deadline = std::time::Instant::now() + args.timeout;
            let mut previous = String::new();
            let mut id = args.operation;
            loop {
                let owner = Owner::load(&home)?;
                let report = owner.status(id.as_deref())?;
                if let Some(value) = report["operation"]["id"].as_str() {
                    id = Some(value.into());
                }
                let serialized = serde_json::to_string(&report)?;
                if serialized != previous {
                    println!("{serialized}");
                    std::io::stdout().flush()?;
                    previous = serialized;
                }
                match report["operation"]["phase"].as_str() {
                    Some("succeeded") => return Ok(0),
                    Some("failed" | "rolled_back") => return Ok(1),
                    _ => {}
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(2);
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_are_literal_and_do_not_execute_shell() {
        assert_eq!(
            settings(
                "export STEVE_PORT='8441'\nSTEVE_UPDATE_REPOSITORY=example/steve # comment",
                &json!({})
            )
            .unwrap(),
            ("example/steve".into(), "8441".into())
        );
        for text in [
            "STEVE_PORT=$(id)",
            "STEVE_PORT=8440;evil",
            "STEVE_PORT=8440\nSTEVE_PORT=8441",
            "STEVE_PORT=99999",
            "STEVE_UPDATE_REPOSITORY=../steve",
            "STEVE_INSTALL_KIND=external",
        ] {
            assert!(settings(text, &json!({})).is_err(), "{text}");
        }
    }
    #[test]
    fn command_contract_rejects_ambiguous_or_unbounded_input() {
        for args in [
            vec!["request", "--version", "v1.2.3", "--key", "k"],
            vec!["request", "--version", "1.2.3"],
            vec!["watch", "--timeout", "NaN"],
            vec!["watch", "--timeout", "0"],
            vec!["check", "--version", "1.2.3", "--version", "2.0.0"],
            vec!["status", "--operation", "../x"],
        ] {
            assert!(arguments(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
        assert!(
            arguments(&["request", "--version", "1.2.3", "--key", "k"].map(str::to_owned)).is_ok()
        );
    }
    #[test]
    fn metadata_and_locks_refuse_links_and_publicly_writable_paths() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::getuid() };
        let file = root.path().join("record.json");
        fs::write(&file, "{}").unwrap();
        assert!(document(&file, uid).is_ok());
        let link = root.path().join("alias");
        symlink(&file, &link).unwrap();
        assert!(document(&link, uid).is_err());
        assert!(owned_lock(&link, uid, false).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(document(&file, uid).is_err());
    }

    #[test]
    fn retries_keep_completed_receipts_and_refuse_other_pending_work() {
        let root = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::getuid() };
        let owner = Owner {
            uid,
            account_home: root.path().into(),
            home: root.path().into(),
            identity: json!({"label":"test.steve"}),
            binary: root.path().join("bin/steve-hub"),
            info: json!({"version":"1.0.0"}),
            repository: "test/steve".into(),
            port: "8440".into(),
            execute: None,
        };
        directory(&root.path().join("updates"), uid).unwrap();
        directory(&root.path().join("updates/operations"), uid).unwrap();
        let mut op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            idempotency_key: "retry".into(),
            version: "1.1.0".into(),
            phase: "succeeded".into(),
            created_at: 1,
            updated_at: 2,
            requested_by: owner.actor(),
            approved_by: Some(owner.actor()),
            approved_at: Some(1),
            error: None,
            previous: None,
            target: None,
            runner: None,
            backup: None,
            progress: None,
            drain: None,
        };
        let journal = Updates::journal(root.path());
        journal.save(&op).unwrap();
        assert_eq!(owner.existing("1.1.0", "retry").unwrap().unwrap().id, op.id);
        assert!(owner.existing("1.2.0", "retry").is_err());
        op.id = uuid::Uuid::new_v4().to_string();
        op.idempotency_key = "other".into();
        op.phase = "awaiting_approval".into();
        journal.save(&op).unwrap();
        assert!(owner.existing("1.2.0", "new").is_err());
        assert_eq!(owner.operation(&op.id).unwrap().phase, "awaiting_approval");
        assert!(
            owner
                .existing("1.1.0", "retry")
                .unwrap()
                .unwrap()
                .terminal()
        );
        // Duplicate historical keys are an integrity failure, even if both operations finished.
        op.id = uuid::Uuid::new_v4().to_string();
        op.idempotency_key = "retry".into();
        op.phase = "failed".into();
        journal.save(&op).unwrap();
        assert!(owner.existing("1.1.0", "retry").is_err());
    }

    #[test]
    fn stale_runner_is_failed_only_when_the_owned_job_is_absent_and_locks_are_free() {
        use std::os::unix::process::ExitStatusExt;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::getuid() };
        for relative in ["updates", "updates/operations", "releases"] {
            directory(&root.path().join(relative), uid).unwrap();
        }
        let live = Arc::new(AtomicBool::new(false));
        let job_live = live.clone();
        let owner = Owner {
            uid,
            account_home: root.path().into(),
            home: root.path().into(),
            identity: json!({"label":"test.steve"}),
            binary: root.path().join("bin/steve-hub"),
            info: json!({"version":"1.0.0"}),
            repository: "test/steve".into(),
            port: "8440".into(),
            execute: Some(Arc::new(move |args| {
                anyhow::ensure!(
                    args[0] == "/bin/launchctl" && args[1] == "print",
                    "reaping must never start or stop a service"
                );
                Ok(if job_live.load(Ordering::Relaxed) {
                    Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: b"state = running\n pid = 42\n".to_vec(),
                        stderr: vec![],
                    }
                } else {
                    Output {
                        status: std::process::ExitStatus::from_raw(256),
                        stdout: vec![],
                        stderr: b"Could not find service".to_vec(),
                    }
                })
            })),
        };
        let mut op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            idempotency_key: "stale".into(),
            version: "1.1.0".into(),
            phase: "staging".into(),
            created_at: 1,
            updated_at: 1,
            requested_by: owner.actor(),
            approved_by: Some(owner.actor()),
            approved_at: Some(1),
            error: None,
            previous: None,
            target: None,
            runner: None,
            backup: None,
            progress: None,
            drain: None,
        };
        let journal = Updates::journal(root.path());
        journal.save(&op).unwrap();
        let runner_lock = owned_lock(&root.path().join("updates/runner.lock"), uid, false).unwrap();
        assert_eq!(owner.reap(op.clone()).unwrap().phase, "staging");
        drop(runner_lock);
        live.store(true, Ordering::Relaxed);
        assert_eq!(owner.reap(op.clone()).unwrap().phase, "staging");
        live.store(false, Ordering::Relaxed);
        op.updated_at = now();
        journal.save(&op).unwrap();
        assert_eq!(owner.reap(op.clone()).unwrap().phase, "staging");
        op.updated_at = 1;
        journal.save(&op).unwrap();
        let failed = owner.reap(op).unwrap();
        assert_eq!(failed.phase, "failed");
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("was not restarted")
        );
        assert_eq!(owner.operation(&failed.id).unwrap().phase, "failed");
        assert!(owner.reap(failed).unwrap().terminal());
    }

    #[test]
    fn signed_owner_and_offline_status_use_only_verified_installation_metadata() {
        use std::os::unix::{fs::symlink, process::ExitStatusExt};
        let root = tempfile::tempdir().unwrap();
        let account = root.path().canonicalize().unwrap();
        let home = account.join(".steve");
        let uid = unsafe { libc::getuid() };
        for path in [
            home.clone(),
            home.join("releases"),
            home.join("bin"),
            home.join("signing"),
            account.join("Library"),
            account.join("Library/LaunchAgents"),
        ] {
            directory(&path, uid).unwrap();
        }
        let release = home.join("releases/1.2.3");
        directory(&release, uid).unwrap();
        directory(&release.join("bin"), uid).unwrap();
        for path in [
            release.join("bin/steve-hub"),
            home.join("bin/steve-keyhelper"),
            home.join("signing/steve-signing.keychain-db"),
        ] {
            fs::write(path, b"fixture").unwrap();
        }
        let identity = json!({"label":"test.steve","hub_identifier":"test.hub","keyhelper_identifier":"test.helper","vault_service":"test.vault"});
        atomic(&home.join("signing/identity.json"), &identity).unwrap();
        let info = json!({"version":"1.2.3","commit":"a".repeat(40),"target":"aarch64-apple-darwin","upgrade_runner":{"journal":1,"native":true}});
        atomic(&release.join("release.json"), &info).unwrap();
        symlink(&release, home.join("current")).unwrap();
        let plist = account.join("Library/LaunchAgents/test.steve.plist");
        let definition = json!({"Label":"test.steve","ProgramArguments":["/bin/zsh",home.join("current/ops/run.sh")],"EnvironmentVariables":{"STEVE_HOME":home}});
        atomic(&plist, &definition).unwrap();
        let execute: TestExecute = std::sync::Arc::new(move |args| {
            let mut out = Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            };
            if args.get(1) == Some(&"--build-info") {
                out.stdout = serde_json::to_vec(&info)?;
            } else if args[0] == "/usr/bin/codesign" {
                if args[1] == "-dv" {
                    out.stderr = if args.last().unwrap().ends_with("steve-keyhelper") {
                        b"Identifier=test.helper\n".to_vec()
                    } else {
                        b"Identifier=test.hub\n".to_vec()
                    };
                }
            } else if args[0] == "/usr/bin/plutil" {
                out.stdout = fs::read(args.last().unwrap())?;
            } else if args[0] == "/bin/launchctl" && args[1] == "print" {
                out.status = std::process::ExitStatus::from_raw(256);
                out.stderr = b"Could not find service".to_vec();
            } else {
                bail!("unexpected OS operation {args:?}")
            }
            Ok(out)
        });
        let owner = Owner::inspect(&home, account.clone(), uid, Some(execute.clone())).unwrap();
        let status = owner.status(None).unwrap();
        assert_eq!(status["job"], "absent");
        assert!(status["operation"].is_null());
        assert!(
            !home.join("updates").exists(),
            "offline status never initializes a journal"
        );
        assert!(
            Owner::inspect(
                &account.join("another-home"),
                account.clone(),
                uid,
                Some(execute.clone())
            )
            .is_err()
        );
        let mut foreign = definition.clone();
        foreign["Program"] = json!("/foreign/executable");
        atomic(&plist, &foreign).unwrap();
        assert!(Owner::inspect(&home, account.clone(), uid, Some(execute.clone())).is_err());
        atomic(&plist, &definition).unwrap();
        atomic(
            &release.join("release.json"),
            &json!({"version":"1.2.3","commit":"b".repeat(40)}),
        )
        .unwrap();
        assert!(Owner::inspect(&home, account, uid, Some(execute)).is_err());
    }
}
