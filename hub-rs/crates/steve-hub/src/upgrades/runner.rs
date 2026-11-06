//! One-click upgrade of a local_release install (ops/local-release.py). The Hub is the process the deploy
//! replaces, so the work runs in a one-shot launchd job (`steve-hub --upgrade-runner <home> <id>`): it
//! fetches the release tag, checks it is the commit the release manifest names, and runs that tag's own
//! `local-release.py build` and `deploy --operation <id>`, which write their steps into the same journal
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
    pub python: String,
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
fn held(p: &Path) -> bool {
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

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

struct Tools {
    python: PathBuf,
    cargo: PathBuf,
}

/// Whether `python` has what local-release.py needs: `tarfile.data_filter` (Python 3.12+). The answer is
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
        .args(["-I", "-c", "import tarfile; tarfile.data_filter"])
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
/// which has no `python3` link when only a versioned formula is installed). A bare `python3` is often
/// macOS's 3.9, which can't run it.
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

/// The tools a local build needs, found the way the runner's environment will find them; the names of
/// those missing otherwise.
fn tools(u: &Updates) -> std::result::Result<Tools, Vec<String>> {
    #[cfg(test)]
    if matches!(u.launch, Launch::Fake(_)) {
        return Ok(Tools {
            python: "python3".into(),
            cargo: "cargo".into(),
        });
    }
    let _ = u;
    let env_file = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    };
    let cargo = env_file("STEVE_CARGO")
        .or_else(|| on_path("cargo"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cargo/bin/cargo"))
                .filter(|p| p.is_file())
        });
    let python = find_python(
        env_file("STEVE_PYTHON"),
        std::env::var_os("PATH"),
        &[
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ],
    );
    let mut missing = vec![];
    if cargo.is_none() {
        missing.push("cargo (STEVE_CARGO)".to_owned());
    }
    if python.is_none() {
        missing.push("python3 ≥3.12 (STEVE_PYTHON)".to_owned());
    }
    for tool in ["git", "npm"] {
        if on_path(tool).is_none() {
            missing.push(tool.to_owned());
        }
    }
    match (cargo, python) {
        (Some(cargo), Some(python)) if missing.is_empty() => Ok(Tools { python, cargo }),
        _ => Err(missing),
    }
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
    if !cfg!(target_os = "macos") {
        missing.push("macOS (codesign, launchd)".to_owned());
    }
    if identity_label(&u.home).is_err() {
        missing.push("signing/identity.json".to_owned());
    }
    for f in ["signing/steve-signing.keychain-db", "signing/keychain-pass"] {
        if !u.home.join(f).is_file() {
            missing.push(f.to_owned());
        }
    }
    if !u.repo.join(".git").exists() {
        missing.push(format!("STEVE_REPO ({})", u.repo.display()));
    }
    if let Err(tools) = tools(u) {
        missing.extend(tools);
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
    let tools = tools(u).map_err(|missing| {
        anyhow::anyhow!(
            "{}",
            steve_i18n::t!(
                "hub.upgrade.reason.local_missing",
                missing = missing.join(", ")
            )
        )
    })?;
    Ok(Spec {
        kind: "local_release".into(),
        repo: u.repo.to_string_lossy().into_owned(),
        source: format!("{}/{}.git", u.web_base, u.repository),
        commit: None,
        python: tools.python.to_string_lossy().into_owned(),
        cargo: tools.cargo.to_string_lossy().into_owned(),
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
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
    .collect();
    env.push(("STEVE_HOME".into(), u.home.to_string_lossy().into_owned()));
    env.push(("STEVE_CARGO".into(), spec.cargo.clone()));
    env.push(("NO_COLOR".into(), "1".into()));
    env
}

fn plist(label: &str, args: &[String], env: &[(String, String)], log: &str) -> String {
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
    Ok(Command::new("git")
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
    let result = run(&u, &mut op, &ops);
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

fn run(u: &Updates, op: &mut Operation, ops: &Path) -> Result<()> {
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

    // a python whose tarfile has the safe extraction filter local-release.py uses (prepare checked it too)
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
        let mut child = Command::new(&spec.python)
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
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
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

    say(format!("deploy {}", candidate.display()));
    let journaled = fs::read_to_string(&script).is_ok_and(|s| s.contains("--operation"));
    let mut deploy = Command::new(&spec.python);
    deploy
        .arg("-I")
        .arg(&script)
        .arg("deploy")
        .arg(&candidate)
        .stdin(Stdio::null());
    if journaled {
        deploy.args(["--operation", &op.id]);
    }
    let status = deploy.status()?;
    let mut fresh = u.operation(&op.id)?;
    if fresh.terminal() {
        *op = fresh;
        return Ok(());
    }
    // the deploy did not write its result: what its exit code says (ops/local-release.py's docstring)
    let unchanged = matches!(fresh.phase.as_str(), "staging" | "ready");
    let (phase, error) = match status.code() {
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
