//! Running plugins: one process per connected instance, started with the run contract of
//! [`super`], restarted when it exits, stopped when its instance goes. A plugin's exit or hang
//! only pauses what it serves; nothing here blocks the Hub.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// The log of an instance is started again when it has grown past this.
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// How long a plugin gets to exit after its standard input closes and it is signalled.
const STOP_GRACE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A run this long counts as a good one: the next restart is prompt again.
const SETTLED: Duration = Duration::from_secs(60);

/// What the Hub knows of an instance's process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Starting,
    Running {
        pid: u32,
    },
    /// It exited or could not be started; the Hub tries again after a pause.
    Waiting {
        error: String,
    },
    Stopped,
}

/// What to run: resolved again at every start, so an upgraded plugin's next start is the new one.
pub struct Spec {
    pub id: String,
    /// The instance's directory: `config.json`, `token`, `data/`, `plugin.log`.
    pub dir: PathBuf,
    pub port: u16,
    /// The executable to start now.
    pub exe: Box<dyn Fn() -> anyhow::Result<PathBuf> + Send + Sync>,
}

struct Proc {
    stop: CancellationToken,
    status: Arc<Mutex<Status>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone, Default)]
pub struct Runner(Arc<Mutex<HashMap<String, Proc>>>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The variables a plugin starts with: the contract's own and what a program needs to find its
/// libraries, its language and a temporary directory. Nothing else of the Hub's environment is
/// passed on, since that may hold the Hub's secrets.
fn environment(spec: &Spec) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "TZ"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    let at = |name: &str| spec.dir.join(name).display().to_string();
    env.extend([
        (
            "STEVE_PLUGIN_LISTEN".into(),
            format!("127.0.0.1:{}", spec.port),
        ),
        ("STEVE_PLUGIN_TOKEN_FILE".into(), at("token")),
        ("STEVE_PLUGIN_CONFIG".into(), at("config.json")),
        ("STEVE_PLUGIN_DATA".into(), at("data")),
        ("STEVE_PLUGIN_SUPERVISED".into(), "1".into()),
    ]);
    env
}

fn log_file(dir: &Path) -> std::io::Result<std::fs::File> {
    let path = dir.join("plugin.log");
    let grown = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES);
    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true);
    if grown {
        o.truncate(true);
    } else {
        o.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn spawn(spec: &Spec) -> anyhow::Result<tokio::process::Child> {
    let exe = (spec.exe)()?;
    std::fs::create_dir_all(spec.dir.join("data"))?;
    let log = log_file(&spec.dir)?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.current_dir(spec.dir.join("data"))
        .env_clear()
        .envs(environment(spec))
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true);
    Ok(cmd.spawn()?)
}

/// Ends a run: its standard input is closed (the contract's sign that the Hub is done with it),
/// it is signalled, and after [`STOP_GRACE`] it is killed.
async fn end(mut child: tokio::process::Child, stdin: Option<tokio::process::ChildStdin>) {
    drop(stdin);
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: a signal to a child this process started and has not reaped yet.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    if tokio::time::timeout(STOP_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
}

async fn supervise(spec: Spec, stop: CancellationToken, status: Arc<Mutex<Status>>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        let error = match spawn(&spec) {
            Ok(mut child) => {
                *lock(&status) = Status::Running {
                    pid: child.id().unwrap_or_default(),
                };
                // held here for as long as it runs: `wait` would close it, and to the plugin
                // that is the Hub going away
                let stdin = child.stdin.take();
                tokio::select! {
                    _ = stop.cancelled() => {
                        end(child, stdin).await;
                        break;
                    }
                    exit = child.wait() => match exit {
                        Ok(s) => format!("exited ({s})"),
                        Err(e) => format!("not waited for: {e}"),
                    },
                }
            }
            Err(e) => format!("not started: {e:#}"),
        };
        tracing::warn!(instance = %spec.id, "plugin {error}");
        *lock(&status) = Status::Waiting { error };
        if started.elapsed() >= SETTLED {
            backoff = Duration::from_secs(1);
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    *lock(&status) = Status::Stopped;
}

impl Runner {
    /// Starts the instance unless it is already running.
    pub fn ensure(&self, spec: Spec) {
        let mut procs = lock(&self.0);
        if procs.get(&spec.id).is_some_and(|p| !p.task.is_finished()) {
            return;
        }
        let stop = CancellationToken::new();
        let status = Arc::new(Mutex::new(Status::Starting));
        let id = spec.id.clone();
        let task = tokio::spawn(supervise(spec, stop.clone(), status.clone()));
        procs.insert(id, Proc { stop, status, task });
    }

    pub fn status(&self, id: &str) -> Status {
        lock(&self.0)
            .get(id)
            .map(|p| lock(&p.status).clone())
            .unwrap_or(Status::Stopped)
    }

    pub fn ids(&self) -> Vec<String> {
        lock(&self.0).keys().cloned().collect()
    }

    /// Stops the instance and waits until its process is gone.
    pub async fn stop(&self, id: &str) {
        let Some(p) = lock(&self.0).remove(id) else {
            return;
        };
        p.stop.cancel();
        let _ = p.task.await;
    }

    /// Stops and starts it again when it is running (after an upgrade).
    pub async fn restart(&self, spec: Spec) {
        self.stop(&spec.id).await;
        self.ensure(spec);
    }

    pub async fn stop_all(&self) {
        for id in self.ids() {
            self.stop(&id).await;
        }
    }

    /// Waits until something accepts connections on the instance's port. `Err` with what is known
    /// when its process exits first or nothing listens in time.
    pub async fn ready(&self, id: &str, port: u16, within: Duration) -> Result<(), String> {
        let deadline = Instant::now() + within;
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return Ok(());
            }
            match self.status(id) {
                Status::Waiting { error } => return Err(error),
                Status::Stopped => return Err("stopped".into()),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err("it did not start listening in time".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// A port nothing listens on now, for a new instance.
pub fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

/// The last lines of an instance's log, for an error shown to the person connecting it.
pub fn log_tail(dir: &Path, lines: usize) -> String {
    let text = std::fs::read(dir.join("plugin.log")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("run.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn spec(dir: &Path, exe: PathBuf) -> Spec {
        Spec {
            id: "i1".into(),
            dir: dir.to_path_buf(),
            port: 1,
            exe: Box::new(move || Ok(exe.clone())),
        }
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..100 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never: {what}");
    }

    #[tokio::test]
    async fn a_plugin_gets_the_contract_and_none_of_the_hubs_variables() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(dir.path(), "env > env.txt; pwd > pwd.txt; cat >/dev/null");
        let r = Runner::default();
        r.ensure(spec(dir.path(), exe));
        let env_file = dir.path().join("data/env.txt");
        until("the plugin wrote its environment", || {
            std::fs::read_to_string(&env_file).is_ok_and(|s| s.contains("STEVE_PLUGIN_SUPERVISED"))
        })
        .await;
        let env = std::fs::read_to_string(&env_file).unwrap();
        for want in [
            "STEVE_PLUGIN_LISTEN=127.0.0.1:1".to_string(),
            format!(
                "STEVE_PLUGIN_TOKEN_FILE={}",
                dir.path().join("token").display()
            ),
            format!(
                "STEVE_PLUGIN_CONFIG={}",
                dir.path().join("config.json").display()
            ),
            format!("STEVE_PLUGIN_DATA={}", dir.path().join("data").display()),
            "STEVE_PLUGIN_SUPERVISED=1".to_string(),
        ] {
            assert!(env.lines().any(|l| l == want), "{want} in {env}");
        }
        // and nothing else: the shell's own (`PWD`, `SHLVL`, `_`) aside, every name is one the
        // contract gives or one a program needs to run at all
        for line in env.lines() {
            let name = line.split('=').next().unwrap_or_default();
            assert!(
                name.starts_with("STEVE_PLUGIN_")
                    || name.starts_with('_')
                    || [
                        "PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "TZ", "PWD", "OLDPWD", "SHLVL"
                    ]
                    .contains(&name),
                "{line}"
            );
        }
        assert!(matches!(r.status("i1"), Status::Running { .. }));
        // closing its standard input ends it (`cat` reads until then), well within the grace
        let t = Instant::now();
        r.stop("i1").await;
        assert!(t.elapsed() < STOP_GRACE);
        assert_eq!(r.status("i1"), Status::Stopped);
    }

    #[tokio::test]
    async fn a_plugin_that_exits_is_started_again_and_its_output_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(
            dir.path(),
            "echo run >> runs.txt; echo could not bind >&2; exit 3",
        );
        let r = Runner::default();
        r.ensure(spec(dir.path(), exe));
        let runs = dir.path().join("data/runs.txt");
        until("a second run", || {
            std::fs::read_to_string(&runs).is_ok_and(|s| s.lines().count() >= 2)
        })
        .await;
        assert!(log_tail(dir.path(), 5).contains("could not bind"));
        let e = r.ready("i1", 1, Duration::from_secs(5)).await.unwrap_err();
        assert!(e.contains("exited"), "{e}");
        r.stop("i1").await;
    }

    #[tokio::test]
    async fn a_missing_executable_is_an_error_to_wait_on_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let r = Runner::default();
        r.ensure(spec(dir.path(), dir.path().join("nothing-here")));
        until("the failed start is recorded", || {
            matches!(r.status("i1"), Status::Waiting { .. })
        })
        .await;
        r.stop_all().await;
        assert!(r.ids().is_empty());
    }
}
