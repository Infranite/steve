//! One-click upgrade of a local_release install (ops/local-release.py). The Hub is the process the deploy
//! replaces, so the work runs in a one-shot launchd job (`steve-hub --upgrade-runner <home> <id>`): it
//! downloads the published release bundle, verifies its identity, preserves the installation's signing
//! identity and deploys through the native installation library. No interpreter, checkout or compiler is needed.
//! The deployment writes its steps into the same journal
//! the managed supervisor uses. The page follows it across the restart by reading the journal.
use super::*;
use std::process::{Command, Stdio};

/// What the runner builds, recorded in the operation when it is requested (paths, never secrets).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    pub kind: String,
    /// The Steve checkout the release tag is fetched into (STEVE_REPO).
    pub repo: String,
    /// The configured repository's git URL: the only place the tag comes from.
    pub source: String,
    /// The commit the release manifest names, set once the release is resolved.
    #[serde(default)]
    pub commit: Option<String>,
    /// Read only by already-persisted legacy source-build operations.
    #[serde(default)]
    pub python: String,
    #[serde(default)]
    pub cargo: String,
    #[serde(default)]
    pub log: Option<String>,
    #[serde(default)]
    pub launched_at: Option<u64>,
}

/// How a runner job is started: launchd, or (tests) recorded instead of run.
#[derive(Clone, Debug)]
pub enum Launch {
    Launchd,
    #[cfg(test)]
    Fake(std::sync::Arc<std::sync::Mutex<Vec<Operation>>>),
}

/// Held by the runner for its whole life: free means no runner is running.
fn runner_lock(u: &Updates) -> PathBuf {
    u.dir().join("runner.lock")
}

/// Whether another process holds the flock on `p` (the runner's lock).
fn busy(p: &Path) -> bool {
    match OpenOptions::new().read(true).write(true).open(p) {
        Ok(f) => f.try_lock().is_err(),
        Err(_) => false,
    }
}

/// Whether a live process holds the fcntl lock on `p`: ops/release.sh and ops/local-release.py both take
/// it on `releases/.release.lock` (`zsystem flock`, `fcntl.lockf`).
pub(super) fn held(p: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(file) = OpenOptions::new().read(true).write(true).open(p) else {
        return false;
    };
    // SAFETY: a zeroed flock is a valid argument; F_GETLK only fills it in.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as libc::c_short;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: the fd is open for the duration of the call and `fl` outlives it.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut fl) };
    rc == 0 && fl.l_type != libc::F_UNLCK as libc::c_short
}

/// Whether `python` can run the bundled installer (Python 3.9+). The answer is
/// remembered per path and modification time, since the update page asks on every load.
fn python_ok(python: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Probed = (Option<std::time::SystemTime>, bool);
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, Probed>>> = OnceLock::new();
    let stamp = fs::metadata(python).and_then(|m| m.modified()).ok();
    let seen = SEEN.get_or_init(Default::default);
    if let Some((at, ok)) = seen.lock().unwrap_or_else(|e| e.into_inner()).get(python)
        && *at == stamp
    {
        return *ok;
    }
    let ok = Command::new(python)
        .args([
            "-I",
            "-c",
            "import sys,ssl,sqlite3; assert sys.version_info >= (3,9)",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    seen.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(python.to_owned(), (stamp, ok));
    ok
}

/// The first python new enough for local-release.py: `STEVE_PYTHON`, then python3.13, python3.12 and
/// python3 on `path`, then the same names in `extra` (Homebrew's bin, which launchd's PATH may lack and
/// which has no `python3` link when only a versioned formula is installed). The installer supports the system Python 3.9 as well as newer interpreters.
#[cfg(test)]
pub(super) fn find_python(
    steve_python: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
    extra: &[PathBuf],
) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let names = ["python3.13", "python3.12", "python3"];
    steve_python
        .into_iter()
        .chain(
            dirs.iter()
                .chain(extra)
                .flat_map(|d| names.iter().map(move |n| d.join(n))),
        )
        .filter(|p| p.is_file())
        .find(|p| python_ok(p))
}

/// Where a Python that gets the signing keychain's password lives: the system's and Homebrew's
/// directories, never one a journal, `STEVE_PYTHON` or a `PATH` names.
pub(super) const PYTHON_DIRS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/Library/Frameworks/Python.framework/Versions/Current/bin",
    "/usr/bin",
];

/// What the runner holds a build to before it hands it the signing keychain's password. Whoever can
/// run the signed Hub (any process of the user, an agent's shell among them) can start
/// `steve-hub --upgrade-runner <home> <id>` with a journal and an environment of their own, so neither
/// decides: the home is the account's `~/.steve`, steve-keyhelper the one in it (not
/// `STEVE_KEYHELPER`), the Python one of [`PYTHON_DIRS`], and the source and commit those of the
/// release manifest of the configured repository, read again here rather than taken from the journal.
/// What this can't rule out, as long as agents run unconfined as the same user: a build whose inputs
/// they changed (the checkout's objects, `~/.cargo`, Homebrew's directory, `STEVE_UPDATE_REPOSITORY`
/// in the caller's environment) still gets signed as the Hub. That takes an OS sandbox around the
/// agents, not checks here.
pub(super) struct Trust {
    /// The one `STEVE_HOME` whose password a build gets.
    pub home: Option<PathBuf>,
    pub python_dirs: Vec<PathBuf>,
    /// For a version: the repository's git URL and the commit its release manifest names.
    #[allow(clippy::type_complexity)]
    pub release: Box<dyn Fn(&str) -> Result<(String, String)>>,
}

impl Trust {
    /// What only this binary decides.
    pub(super) fn installed() -> Self {
        Self {
            home: account_home().map(|h| h.join(".steve")),
            python_dirs: PYTHON_DIRS.map(PathBuf::from).to_vec(),
            release: Box::new(released),
        }
    }

    /// Why the build of `spec` (release `version`, `commit`) in `home` does not get the password.
    fn refuses(&self, home: &Path, version: &str, spec: &Spec, commit: &str) -> Option<String> {
        let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let ours = self.home.as_deref().map(real);
        if ours.as_deref() != Some(real(home).as_path()) {
            return Some(steve_i18n::t!(
                "hub.upgrade.local.untrusted_home",
                home = ours
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
                got = home.to_string_lossy().as_ref()
            ));
        }
        let python = Path::new(&spec.python);
        let named = python
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| ["python3.13", "python3.12", "python3"].contains(&n));
        if spec.kind == "local_release"
            && (!named
                || !python
                    .parent()
                    .is_some_and(|d| self.python_dirs.iter().any(|t| t == d)))
        {
            return Some(steve_i18n::t!(
                "hub.upgrade.local.untrusted_python",
                python = spec.python.as_str()
            ));
        }
        match (self.release)(version) {
            Ok((source, manifest)) if source == spec.source && manifest == commit => None,
            Ok((source, manifest)) => Some(steve_i18n::t!(
                "hub.upgrade.local.release_other",
                version = version,
                source = source.as_str(),
                commit = manifest.as_str()
            )),
            Err(e) => Some(steve_i18n::t!(
                "hub.upgrade.local.release_unconfirmed",
                version = version,
                error = format!("{e:#}").as_str()
            )),
        }
    }
}

/// This user's home directory as the account database has it (not `HOME`, which a caller sets).
pub(super) fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: a zeroed passwd is a valid out-parameter; getpwuid_r writes only into it and `buf`
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call, `buf.len()` is its size
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pw,
            buf.as_mut_ptr(),
            buf.len(),
            &mut out,
        )
    };
    if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into `buf`, NUL-terminated, while `buf` lives
    let dir = unsafe { std::ffi::CStr::from_ptr(pw.pw_dir) };
    let dir = PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes()));
    dir.is_absolute().then_some(dir)
}

/// The configured repository's git URL and the commit of `version`'s release manifest, from GitHub.
fn released(version: &str) -> Result<(String, String)> {
    let repository = repository()?;
    let version = version.to_owned();
    // the runner runs inside the Hub binary's runtime; this blocks on a runtime of its own
    std::thread::spawn(move || -> Result<(String, String)> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let u = Updates {
            repository: repository.clone(),
            client: reqwest::Client::builder()
                .user_agent("Steve-updater/1")
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            api_base: "https://api.github.com".into(),
            web_base: "https://github.com".into(),
            ..Updates::journal(Path::new("/"))
        };
        let (_, manifest) = rt.block_on(u.resolve(&version))?;
        Ok((format!("{}/{repository}.git", u.web_base), manifest.commit))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("the release check stopped"))?
}

fn identity_label(home: &Path) -> Result<String> {
    let ident: Value = read(&home.join("signing/identity.json"))?;
    let label = ident["label"].as_str().unwrap_or_default();
    if label.is_empty()
        || !label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        bail!("signing/identity.json has no usable label")
    }
    Ok(label.to_owned())
}

/// What this machine lacks for a one-click local build and deploy; empty when it has everything.
pub fn local_missing(u: &Updates) -> Vec<String> {
    let mut missing = vec![];
    // Match the signing runner's trust boundary before offering the action. Custom-home managed
    // installs do not use this signing path; never advertise a local signer that will refuse later.
    let check_home = match &u.launch {
        Launch::Launchd => true,
        #[cfg(test)]
        Launch::Fake(_) => false,
    };
    if check_home {
        let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        match account_home().map(|h| h.join(".steve")) {
            Some(home) if real(&home) == real(&u.home) => {}
            Some(home) => missing.push(steve_i18n::t!(
                "hub.upgrade.local.untrusted_home",
                home = home.to_string_lossy().as_ref(),
                got = u.home.to_string_lossy().as_ref()
            )),
            None => missing.push("the account's installation home cannot be established".into()),
        }
    }
    if !cfg!(target_os = "macos") {
        missing.push("macOS (codesign, launchd)".to_owned());
    }
    if identity_label(&u.home).is_err() {
        missing.push("signing/identity.json".to_owned());
    }
    if !u.home.join("signing/steve-signing.keychain-db").is_file() {
        missing.push("signing/steve-signing.keychain-db".to_owned());
    }
    // the keychain's password: in the key store once the Hub moved it there, else still the file
    let store = super::signing::helper(&u.home);
    if !super::signing::present(&u.home, store.as_deref()) {
        missing.push(super::signing::FILE.to_owned());
    }
    missing
}

/// The runner part of a new local_release request: only a stable version newer than this Hub, only when
/// no release (ops/release.sh, local-release.py, another runner) is running.
pub(super) fn prepare(u: &Updates, version: &semver::Version) -> Result<Spec> {
    let current = semver::Version::parse(u.current_hub.trim_start_matches('v')).map_err(|_| {
        anyhow::anyhow!(
            "{}",
            steve_i18n::t!(
                "hub.upgrade.local.unknown_current",
                current = u.current_hub.as_str()
            )
        )
    })?;
    if *version <= current {
        bail!(
            "{}",
            steve_i18n::t!(
                "hub.upgrade.local.not_newer",
                version = version.to_string(),
                current = current.to_string()
            )
        )
    }
    if busy(&runner_lock(u)) {
        bail!("{}", steve_i18n::t!("hub.upgrade.in_progress"))
    }
    if held(&u.home.join("releases/.release.lock")) {
        bail!("{}", steve_i18n::t!("hub.upgrade.local.release_running"))
    }
    Ok(Spec {
        kind: "release_bundle".into(),
        repo: String::new(),
        source: format!("{}/{}.git", u.web_base, u.repository),
        commit: None,
        python: String::new(),
        cargo: String::new(),
        log: None,
        launched_at: None,
    })
}

/// Resolve the release (tags only, from the configured repository), record its commit, start the job.
pub(super) async fn launch(u: &Updates, op: &mut Operation) -> Result<()> {
    let (_, manifest) = u.resolve(&op.version).await?;
    {
        let _guard = lock(&u.dir().join("journal.lock"))?;
        let mut fresh = u.operation(&op.id)?;
        if fresh.phase != "staging"
            || fresh
                .runner
                .as_ref()
                .is_none_or(|r| r.launched_at.is_some())
        {
            *op = fresh;
            return Ok(());
        }
        if busy(&runner_lock(u)) {
            bail!("{}", steve_i18n::t!("hub.upgrade.in_progress"))
        }
        let log = u.home.join("logs/release").join(format!(
            "upgrade-{}-{}.log",
            fresh.version,
            &fresh.id[..8]
        ));
        let spec = fresh.runner.as_mut().context("not a local upgrade")?;
        spec.commit = Some(manifest.commit.clone());
        spec.log = Some(log.to_string_lossy().into_owned());
        spec.launched_at = Some(now());
        fresh.updated_at = now();
        u.save(&fresh)?;
        *op = fresh;
    }
    match &u.launch {
        Launch::Launchd => launchd(u, op),
        #[cfg(test)]
        Launch::Fake(started) => {
            started.lock().unwrap().push(op.clone());
            Ok(())
        }
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The job's environment: what finding the tools and the Hub takes, never the Hub's own environment
/// (steve.env holds credentials; local-release.py reads what it needs from it itself).
fn job_env(u: &Updates, spec: &Spec) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "STEVE_PORT",
        "CARGO_HOME",
        "RUSTUP_HOME",
        // the repository the runner reads the release manifest from again
        "STEVE_UPDATE_REPOSITORY",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
    .collect();
    env.push(("STEVE_HOME".into(), u.home.to_string_lossy().into_owned()));
    if spec.kind == "local_release" {
        env.push(("STEVE_CARGO".into(), spec.cargo.clone()));
    }
    env.push(("NO_COLOR".into(), "1".into()));
    env
}

pub(super) fn plist(label: &str, args: &[String], env: &[(String, String)], log: &str) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("<string>{}</string>", xml(a)))
        .collect();
    let env: String = env
        .iter()
        .map(|(k, v)| format!("<key>{}</key><string>{}</string>", xml(k), xml(v)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array>{args}</array>
<key>EnvironmentVariables</key><dict>{env}</dict>
<key>RunAtLoad</key><true/>
<key>AbandonProcessGroup</key><true/>
<key>ProcessType</key><string>Standard</string>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        label = xml(label),
        log = xml(log)
    )
}

fn launchd(u: &Updates, op: &Operation) -> Result<()> {
    let spec = op.runner.as_ref().context("not a local upgrade")?;
    let label = format!("{}.upgrade", identity_label(&u.home)?);
    // the release directory's own binary: `current` moves during the deploy
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let log = spec.log.clone().context("no log path")?;
    let log_path = PathBuf::from(&log);
    fs::create_dir_all(log_path.parent().context("log needs a parent")?)?;
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)?;
    }
    let file = u.dir().join("runner").join(format!("{}.plist", op.id));
    let args = [
        exe.to_string_lossy().into_owned(),
        "--upgrade-runner".into(),
        u.home.to_string_lossy().into_owned(),
        op.id.clone(),
    ];
    let body = plist(&label, &args, &job_env(u, spec), &log);
    fs::create_dir_all(file.parent().unwrap())?;
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)?;
        f.write_all(body.as_bytes())?;
    }
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    // an earlier runner job stays loaded after it exits; the free runner lock says it isn't running
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let out = Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(&file)
        .output()?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap {label}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
    Ok(())
}

/// A local upgrade nobody finishes: its runner is gone (a reboot, a crash, the job never started)
/// without writing a result. Said once, with whether `current` had already moved.
pub(super) fn reap(u: &Updates, op: &mut Operation) -> Result<()> {
    let Some(spec) = op.runner.as_ref() else {
        return Ok(());
    };
    if op.terminal() || op.phase == "awaiting_approval" {
        return Ok(());
    }
    // the Hub resolves the release before the job starts, and launchd takes a moment to start it
    let (since, grace) = match spec.launched_at {
        Some(at) => (at.max(op.updated_at), 60_000),
        None => (op.updated_at, 600_000),
    };
    if now().saturating_sub(since) < grace || busy(&runner_lock(u)) {
        return Ok(());
    }
    let _guard = lock(&u.dir().join("journal.lock"))?;
    let mut fresh = u.operation(&op.id)?;
    if fresh.terminal() || busy(&runner_lock(u)) {
        *op = fresh;
        return Ok(());
    }
    let log = spec.log.clone().unwrap_or_default();
    let current = fs::canonicalize(u.home.join("current")).ok();
    let switched = current.is_some()
        && fresh.target.as_ref().and_then(|t| fs::canonicalize(t).ok()) == current;
    let error = if switched {
        steve_i18n::t!("hub.upgrade.local.gone_switched", log = log.as_str())
    } else {
        steve_i18n::t!("hub.upgrade.local.gone", log = log.as_str())
    };
    u.phase(&mut fresh, "failed", Some(error))?;
    *op = fresh;
    Ok(())
}

fn say(msg: impl std::fmt::Display) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the zeroed tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    println!(
        "{:02}:{:02}:{:02} upgrade-runner: {msg}",
        tm.tm_hour, tm.tm_min, tm.tm_sec
    );
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    // a replace ref (`refs/replace/<commit>`) would make the release commit read as another
    Ok(Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()?)
}

fn git_commit(repo: &Path, rev: &str) -> Option<String> {
    let out = git(
        repo,
        &["rev-parse", "-q", "--verify", &format!("{rev}^{{commit}}")],
    )
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `steve-hub --upgrade-runner <home> <id>`: the job launchd starts. Exit 0 once the journal has a result.
pub fn main(home: &Path, id: &str) -> i32 {
    main_with(home, id, &Trust::installed())
}

/// [`main`], holding the build to `trust`.
pub(super) fn main_with(home: &Path, id: &str, trust: &Trust) -> i32 {
    let u = Updates::journal(home);
    let Ok(guard) = lock_file(&runner_lock(&u)) else {
        say("another runner holds the runner lock");
        return 75;
    };
    let mut op = match u.operation(id) {
        Ok(op) => op,
        Err(e) => {
            say(format!("no operation {id}: {e:#}"));
            return 1;
        }
    };
    if op.phase != "staging" || op.runner.is_none() {
        say(format!("operation {id} is {}: nothing to do", op.phase));
        return 0;
    }
    let ops = u.dir().join("runner").join(format!("{id}-ops"));
    let result = run(&u, &mut op, &ops, trust);
    let _ = fs::remove_dir_all(&ops);
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            say(format!("✗ {e:#}"));
            if let Ok(mut fresh) = u.operation(id)
                && !fresh.terminal()
            {
                let _ = u.phase(&mut fresh, "failed", Some(format!("{e:#}")));
            }
            1
        }
    };
    if let Ok(op) = u.operation(id) {
        say(format!(
            "result: {} {}",
            op.phase,
            op.error.unwrap_or_default()
        ));
    }
    drop(guard);
    code
}

fn lock_file(p: &Path) -> Result<File> {
    fs::create_dir_all(p.parent().context("lock needs a parent")?)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p)?;
    f.try_lock().map_err(|_| anyhow::anyhow!("busy"))?;
    Ok(f)
}

fn run(u: &Updates, op: &mut Operation, ops: &Path, trust: &Trust) -> Result<()> {
    match op.runner.as_ref().map(|s| s.kind.as_str()) {
        Some("release_bundle") => return run_bundle(u, op, trust),
        Some("local_release") => {} // a persisted request from an older Hub
        _ => bail!("unsupported upgrade runner"),
    }
    let spec = op.runner.clone().context("not a local upgrade")?;
    let commit = spec
        .commit
        .clone()
        .context("the release commit was not recorded")?;
    if commit.len() != 40 || !commit.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid release commit")
    }
    let version = op.version.clone();
    let tag = format!("v{version}");
    let repo = PathBuf::from(&spec.repo);
    let log = spec.log.clone().unwrap_or_default();
    say(format!(
        "upgrade to {version} ({commit}), operation {}",
        op.id
    ));

    // the interpreter recorded by the legacy request
    if !python_ok(Path::new(&spec.python)) {
        bail!(
            "{}",
            steve_i18n::t!(
                "hub.upgrade.local.python_old",
                python = spec.python.as_str()
            )
        )
    }

    // the tag, from the configured repository only; it must be the commit the release manifest names
    say(format!("fetch {tag} from {}", spec.source));
    match git(
        &repo,
        &[
            "fetch",
            "--no-tags",
            "--quiet",
            &spec.source,
            &format!("refs/tags/{tag}"),
        ],
    ) {
        Ok(out) if out.status.success() => {
            let fetched = git_commit(&repo, "FETCH_HEAD").unwrap_or_default();
            if fetched != commit {
                bail!(
                    "{}",
                    steve_i18n::t!(
                        "hub.upgrade.local.tag_mismatch",
                        tag = tag.as_str(),
                        got = fetched.as_str(),
                        commit = commit.as_str()
                    )
                )
            }
        }
        Ok(out) => say(format!(
            "fetch failed ({}); using what the checkout has",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => say(format!("fetch failed ({e}); using what the checkout has")),
    }
    if let Some(local) = git_commit(&repo, &format!("refs/tags/{tag}"))
        && local != commit
    {
        bail!(
            "{}",
            steve_i18n::t!(
                "hub.upgrade.local.tag_mismatch",
                tag = tag.as_str(),
                got = local.as_str(),
                commit = commit.as_str()
            )
        )
    }
    if git_commit(&repo, &commit).as_deref() != Some(commit.as_str()) {
        bail!(
            "{}",
            steve_i18n::t!("hub.upgrade.local.commit_missing", commit = commit.as_str())
        )
    }

    // that commit's own release scripts: the checkout's working tree is never used or touched
    let archive = git(&repo, &["archive", "--format=tar", &commit, "ops"])?;
    if !archive.status.success() {
        bail!(
            "git archive {commit} ops: {}",
            String::from_utf8_lossy(&archive.stderr).trim()
        )
    }
    let _ = fs::remove_dir_all(ops);
    fs::create_dir_all(ops)?;
    tar::Archive::new(archive.stdout.as_slice()).unpack(ops)?;
    let script = ops.join("ops/local-release.py");
    if !script.is_file() {
        bail!("{tag} has no ops/local-release.py")
    }

    let candidate = u
        .home
        .join("releases")
        .join(format!("{version}-local-{}", &commit[..7]));
    let built = |dir: &Path| {
        read::<Value>(&dir.join("release.json")).is_ok_and(|m| {
            m["version"].as_str() == Some(version.as_str())
                && m["commit"].as_str() == Some(commit.as_str())
        }) && dir.join("bin/steve-hub").is_file()
            && dir.join("web/dist/index.html").is_file()
    };
    if candidate.exists() {
        if !built(&candidate) {
            bail!(
                "{}",
                steve_i18n::t!(
                    "hub.upgrade.local.candidate_incomplete",
                    dir = candidate.to_string_lossy().as_ref()
                )
            )
        }
        say(format!(
            "{} is already built: deploying it",
            candidate.display()
        ));
    } else {
        say(format!(
            "build {} (cargo {})",
            candidate.display(),
            spec.cargo
        ));
        // the signing keychain's password, over a pipe: the runner is the signed Hub, the one
        // process steve-keyhelper gives it to (a script from before this reads signing/keychain-pass),
        // and only to a build of what the Hub would build itself
        let piped = fs::read_to_string(&script).is_ok_and(|s| s.contains("--keychain-pass-stdin"));
        let pass = if piped {
            if let Some(why) = trust.refuses(&u.home, &version, &spec, &commit) {
                bail!("{why}")
            }
            let store = super::signing::installed_helper(&u.home);
            Some(
                super::signing::password(&u.home, store.as_deref())?
                    .context("no signing keychain password (ops/sign-setup.sh)")?,
            )
        } else {
            None
        };
        let mut build = Command::new(&spec.python);
        build
            .arg("-I")
            .arg(&script)
            .args([
                "build",
                &commit,
                "--version",
                &version,
                "--repo",
                &spec.repo,
            ])
            .stdout(Stdio::piped());
        if pass.is_some() {
            build.arg("--keychain-pass-stdin").stdin(Stdio::piped());
        } else {
            build.stdin(Stdio::null());
        }
        let mut child = build.spawn()?;
        if let (Some(p), Some(mut stdin)) = (pass, child.stdin.take()) {
            use std::io::Write;
            let line = zeroize::Zeroizing::new(format!("{}\n", p.as_str()));
            // a script that exits before reading it only closes the pipe early
            let _ = stdin.write_all(line.as_bytes());
        }
        // local-release.py prints this line once it holds releases/.release.lock and has found no
        // candidate, just before it makes the directory: only then is the directory this run's
        let started = format!(
            " build {version} from {commit} into {}",
            candidate.display()
        );
        let mut ours = false;
        if let Some(out) = child.stdout.take() {
            use std::io::{BufRead, Write};
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                ours |= line.ends_with(&started);
                println!("{line}");
                let _ = std::io::stdout().flush();
            }
        }
        let status = child.wait()?;
        if !status.success() || !built(&candidate) {
            // a half-built directory of this run would only block the next attempt; one another build
            // made (it took the lock between the check above and this build) is left alone
            if ours {
                let _ = fs::remove_dir_all(&candidate);
            }
            bail!(
                "{}",
                steve_i18n::t!(
                    "hub.upgrade.local.build_failed",
                    code = status.code().unwrap_or(-1).to_string(),
                    log = log.as_str()
                )
            )
        }
    }
    op.target = Some(candidate.to_string_lossy().into_owned());
    u.phase(op, "ready", None)?;

    deploy(u, op, &spec, &script, &candidate)
}

/// A published artifact takes the same verified deployment path as an explicit maintainer build.
/// Nothing in this path invokes a compiler, package manager or Git checkout.
fn run_bundle(u: &Updates, op: &mut Operation, trust: &Trust) -> Result<()> {
    let spec = op.runner.clone().context("not a bundle upgrade")?;
    let commit = spec.commit.as_deref().context("release commit missing")?;
    if let Some(why) = trust.refuses(&u.home, &op.version, &spec, commit) {
        bail!("{why}")
    }
    let home = u.home.clone();
    let repository = repository()?;
    let mut staged = op.clone();
    // The CLI already runs within Tokio. Keep this one-shot runner's network on its own runtime.
    *op = std::thread::spawn(move || -> Result<Operation> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let network = Updates {
            home,
            repository,
            api_base: "https://api.github.com".into(),
            web_base: "https://github.com".into(),
            client: reqwest::Client::builder()
                .user_agent("Steve-updater/1")
                .timeout(Duration::from_secs(600))
                .build()?,
            ..Updates::journal(Path::new("/"))
        };
        rt.block_on(network.stage(&mut staged))?;
        Ok(staged)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("release download stopped"))??;
    let candidate = PathBuf::from(op.target.as_ref().context("download has no candidate")?);
    prepare_bundle(u, op, &spec, &candidate, trust)?;
    u.phase(op, "ready", None)?;
    let result = steve_install::release::deploy(
        &u.home,
        &candidate,
        Some(&op.id),
        Duration::from_secs(600),
    )?;
    finish_deploy(u, op, &spec, Some(result.exit_code))
}

pub(super) fn prepare_bundle(
    u: &Updates,
    op: &mut Operation,
    spec: &Spec,
    candidate: &Path,
    trust: &Trust,
) -> Result<()> {
    prepare_bundle_with(
        u,
        op,
        spec,
        candidate,
        trust,
        |home, candidate, version, commit, password| {
            steve_install::signing::prepare_bundle(home, candidate, version, commit, password)
                .map(|_| ())
        },
    )
}

pub(super) fn prepare_bundle_with(
    u: &Updates,
    op: &mut Operation,
    spec: &Spec,
    candidate: &Path,
    trust: &Trust,
    prepare: impl FnOnce(&Path, &Path, &str, &str, &str) -> Result<()>,
) -> Result<()> {
    let commit = spec.commit.as_deref().context("release commit missing")?;
    if let Some(why) = trust.refuses(&u.home, &op.version, spec, commit) {
        bail!("{why}")
    }
    u.progress(op, "signing", None)?;
    let store = super::signing::installed_helper(&u.home);
    let pass = super::signing::password(&u.home, store.as_deref())?
        .context("no existing installation signing identity")?;
    prepare(&u.home, candidate, &op.version, commit, pass.as_str())
}

fn deploy(
    u: &Updates,
    op: &mut Operation,
    spec: &Spec,
    script: &Path,
    candidate: &Path,
) -> Result<()> {
    say(format!("deploy {}", candidate.display()));
    let journaled = fs::read_to_string(script).is_ok_and(|s| s.contains("--operation"));
    let mut deploy = Command::new(&spec.python);
    deploy
        .arg("-I")
        .arg(script)
        .arg("deploy")
        .arg(candidate)
        .env("STEVE_HOME", &u.home)
        .stdin(Stdio::null());
    if journaled {
        deploy.args(["--operation", &op.id]);
    }
    let status = deploy.status()?;
    finish_deploy(u, op, spec, status.code())
}

fn finish_deploy(u: &Updates, op: &mut Operation, spec: &Spec, code: Option<i32>) -> Result<()> {
    let log = spec.log.clone().unwrap_or_default();
    let mut fresh = u.operation(&op.id)?;
    if fresh.terminal() {
        *op = fresh;
        return Ok(());
    }
    // the deploy did not write its result: what its exit code says (ops/local-release.py's docstring)
    let unchanged = matches!(fresh.phase.as_str(), "staging" | "ready");
    let (phase, error) = match code {
        Some(0) => ("succeeded", None),
        Some(1) if unchanged => (
            "failed",
            Some(steve_i18n::t!(
                "hub.upgrade.local.deploy_refused",
                log = log.as_str()
            )),
        ),
        Some(1) => (
            "rolled_back",
            Some(steve_i18n::t!(
                "hub.upgrade.local.rolled_back",
                log = log.as_str()
            )),
        ),
        Some(3) => (
            "failed",
            Some(steve_i18n::t!(
                "hub.upgrade.local.committed_failed",
                log = log.as_str()
            )),
        ),
        _ => (
            "failed",
            Some(steve_i18n::t!(
                "hub.upgrade.local.not_recovered",
                log = log.as_str()
            )),
        ),
    };
    u.phase(&mut fresh, phase, error)?;
    *op = fresh;
    Ok(())
}
