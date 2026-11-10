//! Keeping the adapters Steve installed up to date ([`steve_acp::managed`]): a minute after the
//! Node starts and then once a day, the npm registry is asked for each one's latest release. As
//! `STEVE_ADAPTER_UPDATES` says, a newer one is installed (`auto`, the default) once the Node has
//! no session or job running, only shown (`check`), or nothing is asked (`off`). What the last
//! check found goes out with the probe ([`steve_node_api::AdapterInfo::update`]). Adapters
//! installed otherwise (globally, or named by the manifest) are left alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use steve_acp::AcpConfig;
use steve_acp::managed::{self, UpdatePolicy};
use steve_node_api::AdapterUpdate;

/// The first check, after the Node has settled.
pub(crate) const FIRST: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(24 * 3600);
/// A newer release waiting for the Node to be idle is tried again this often.
const BUSY_AGAIN: Duration = Duration::from_secs(600);

/// The Steve home whose `adapters` `command` is installed in (`<home>/adapters/node_modules/…`).
pub(crate) fn managed_home(command: &Path) -> Option<PathBuf> {
    managed::home_of(command)
}

#[derive(Default)]
pub(crate) struct AdapterUpdates {
    state: Mutex<HashMap<String, AdapterUpdate>>,
    started: AtomicBool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn policy_name(p: UpdatePolicy) -> &'static str {
    match p {
        UpdatePolicy::Auto => "auto",
        UpdatePolicy::Check => "check",
        UpdatePolicy::Off => "off",
    }
}

impl AdapterUpdates {
    /// Whether the checks still have to be started (the first caller starts them).
    pub(crate) fn first_start(&self) -> bool {
        !self.started.swap(true, Ordering::SeqCst)
    }

    /// What the last check found about adapter `id`, if Steve keeps it up to date.
    pub(crate) fn get(&self, cfg: &AcpConfig, id: &str) -> Option<AdapterUpdate> {
        let home = managed_home(cfg.adapters.get(id)?)?;
        let package = managed::package(id)?;
        let mut u = self
            .state
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_else(|| AdapterUpdate {
                policy: policy_name(UpdatePolicy::from_env()).into(),
                ..Default::default()
            });
        u.installed = managed::installed(&home, package);
        Some(u)
    }

    /// One round: asks the registry about each adapter Steve keeps, and installs a newer release
    /// when the policy is `auto` and `idle()` says nothing runs. Returns (a newer release still
    /// waits, something was installed).
    pub(crate) async fn check(&self, cfg: &AcpConfig, idle: impl Fn() -> bool) -> (bool, bool) {
        let policy = UpdatePolicy::from_env();
        if policy == UpdatePolicy::Off {
            return (false, false);
        }
        let Some(npm) = managed::npm(Some(&cfg.node)) else {
            return (false, false);
        };
        let (mut waiting, mut installed_any) = (false, false);
        for (id, command) in &cfg.adapters {
            let (Some(home), Some(package)) = (managed_home(command), managed::package(id)) else {
                continue;
            };
            /* an adapter the user left out at setup is not installed behind their back */
            if !managed::wanted(&home, id) {
                continue;
            }
            let (n, p) = (npm.clone(), package.to_string());
            let latest = tokio::task::spawn_blocking(move || managed::latest(&n, &p))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let installed = managed::installed(&home, package);
            let mut u = AdapterUpdate {
                policy: policy_name(policy).into(),
                installed: installed.clone(),
                checked: Some(now_ms()),
                ..Default::default()
            };
            match latest {
                Ok(l) => {
                    let newer = installed.as_deref().is_none_or(|i| managed::newer(&l, i));
                    u.latest = Some(l.clone());
                    if newer && policy == UpdatePolicy::Auto {
                        if idle() {
                            let (n, h, p) = (npm.clone(), home.clone(), package.to_string());
                            match tokio::task::spawn_blocking(move || {
                                managed::install(&n, &h, &[p.as_str()])
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                            {
                                Ok(()) => {
                                    tracing::info!("adapter {id}: {package} {l} installed");
                                    u.installed = managed::installed(&home, package);
                                    installed_any = true;
                                }
                                Err(e) => {
                                    tracing::warn!("adapter {id}: upgrade to {l}: {e}");
                                    u.error = Some(e);
                                }
                            }
                        } else {
                            u.pending = true;
                            waiting = true;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("adapter {id}: latest release of {package}: {e}");
                    u.error = Some(e);
                }
            }
            self.state.lock().unwrap().insert(id.clone(), u);
        }
        (waiting, installed_any)
    }

    /// How long until the next round.
    pub(crate) fn next(waiting: bool) -> Duration {
        if waiting { BUSY_AGAIN } else { EVERY }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_adapters_in_a_steve_home_are_kept() {
        assert_eq!(
            managed_home(Path::new(
                "/u/.steve/adapters/node_modules/@agentclientprotocol/codex-acp/dist/index.js"
            )),
            Some(PathBuf::from("/u/.steve"))
        );
        assert_eq!(
            managed_home(Path::new(
                "/opt/homebrew/lib/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"
            )),
            None
        );
        assert_eq!(managed_home(Path::new("claude-code-acp")), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_adapter_left_out_at_setup_is_not_installed_by_the_check() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        // an npm that answers 9.9.9 for every package and "installs" by writing its manifest
        let (npm, log) = (temp.path().join("npm"), temp.path().join("npm.log"));
        std::fs::write(
            &npm,
            format!(
                r#"#!/bin/sh
echo "$*" >> '{}'
case "$1" in
  view) echo '"9.9.9"' ;;
  install)
    for a in "$@"; do
      case "$a" in *@latest)
        p="$3/node_modules/${{a%@latest}}"; mkdir -p "$p"; echo '{{"version": "9.9.9"}}' > "$p/package.json" ;;
      esac
    done ;;
esac
"#,
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ids: Vec<&str> = steve_acp::adapters::builtin_ids().collect();
        let (kept, left_out) = (ids[0], ids[1]);
        let cfg = AcpConfig {
            adapters: ids
                .iter()
                .map(|id| (id.to_string(), managed::command(&home, id).unwrap()))
                .chain([("elsewhere".to_string(), PathBuf::from("/opt/tools/elsewhere-acp"))])
                .collect(),
            declared: HashMap::new(),
            node: temp.path().join("node"),
            claude_executable: PathBuf::new(),
            steve_home: home.clone(),
            codex_home: PathBuf::new(),
            user_home: PathBuf::new(),
            version: "test".into(),
        };
        std::fs::create_dir_all(managed::dir(&home)).unwrap();
        std::fs::write(managed::dir(&home).join("selected.json"), format!("[\"{kept}\"]\n")).unwrap();

        let updates = AdapterUpdates::default();
        // something runs: the newer release waits
        assert_eq!(updates.check(&cfg, || false).await, (true, false));
        assert!(updates.get(&cfg, kept).unwrap().pending);
        // idle: the chosen one is installed, the other is neither asked about nor installed
        assert_eq!(updates.check(&cfg, || true).await, (false, true));
        let package = |id: &str| managed::package(id).unwrap();
        assert_eq!(managed::installed(&home, package(kept)).as_deref(), Some("9.9.9"));
        assert_eq!(managed::installed(&home, package(left_out)), None);
        let asked = std::fs::read_to_string(&log).unwrap();
        assert!(asked.contains(package(kept)) && !asked.contains(package(left_out)), "{asked}");
        let found = updates.get(&cfg, kept).unwrap();
        assert_eq!((found.installed.as_deref(), found.latest.as_deref(), found.pending), (Some("9.9.9"), Some("9.9.9"), false));
        assert_eq!(updates.get(&cfg, left_out).unwrap().latest, None, "nothing was asked about it");
        assert!(updates.get(&cfg, "elsewhere").is_none(), "an adapter installed otherwise is left alone");
        // up to date: nothing more to install
        assert_eq!(updates.check(&cfg, || true).await, (false, false));
    }
}
