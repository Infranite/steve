//! What each configured harness adapter says about itself (`initialize`, and the modes, models
//! and efforts of a scratch `session/new`: no prompt, no model is asked anything), for discovery
//! (harness parity S1, S4, S8). Asked side by side, at most every half hour unless the Hub asks for
//! a fresh probe; an adapter that cannot start or answer is reported so, and nothing is refused
//! because of it. Adapters that declare `probe_initialize: false` are not asked.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_acp::adapters::{Probe, ProbeStatus};
use steve_node_api::{
    AdapterAuth, AdapterInfo, AdapterStatus, HarnessCapability, MissingCommandDiagnostic,
    extract_login_shell_path, login_shell_path_script,
};

const TTL: Duration = Duration::from_secs(1800);
const INITIALIZE: Duration = Duration::from_secs(20);
const LOGIN_SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginShellEnv {
    pub shell: String,
    pub path: String,
}

fn resolve_user_login_shell() -> String {
    if let Some(shell) = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && steve_acp::is_executable_file(p))
        && let Some(text) = shell.to_str()
    {
        return text.to_owned();
    }
    #[cfg(unix)]
    {
        // SAFETY: `getpwuid` is queried once for the current UID and the string is copied immediately.
        let pw = unsafe { libc::getpwuid(libc::getuid()) };
        if !pw.is_null() && !unsafe { (*pw).pw_shell }.is_null() {
            let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_shell) };
            if let Ok(text) = cstr.to_str() {
                let p = Path::new(text);
                if p.is_absolute() && steve_acp::is_executable_file(p) {
                    return text.to_owned();
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    if steve_acp::is_executable_file(Path::new("/bin/zsh")) {
        return "/bin/zsh".to_owned();
    }
    "/bin/sh".to_owned()
}

/// Probes the Node account's login shell (`$SHELL -l -c`) once to see whether a command missing
/// from the service's own `PATH` is actually installed in the user's interactive/login `PATH`.
pub(crate) async fn probe_login_shell_env() -> Option<LoginShellEnv> {
    let shell = resolve_user_login_shell();
    let script = login_shell_path_script(Path::new(&shell));
    let output = tokio::time::timeout(
        LOGIN_SHELL_PROBE_TIMEOUT,
        tokio::process::Command::new(&shell)
            .args(["-l", "-c", &script])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = extract_login_shell_path(&stdout)?;
    Some(LoginShellEnv { shell, path })
}

fn missing_command_diagnostic(
    cfg: &AcpConfig,
    id: &str,
    service_path: &str,
    login_env: Option<&LoginShellEnv>,
    refresh_command: &str,
) -> MissingCommandDiagnostic {
    let service_os = OsStr::new(service_path);
    let login_os = login_env.map(|e| OsStr::new(e.path.as_str()));

    let mut candidates: Vec<String> = Vec::new();
    let mut primary = None;
    if let Ok((program, _)) = cfg.adapter_command(id) {
        if program.components().count() == 1 && !program.is_absolute() {
            if let Some(name) = program.to_str() {
                primary = Some(name.to_owned());
                candidates.push(name.to_owned());
            }
        } else if cfg.adapters.get(id).is_some_and(|own| own.is_file())
            && steve_acp::is_node_script(cfg.adapters.get(id).unwrap())
            && cfg.node.components().count() == 1
            && !cfg.node.is_absolute()
            && cfg
                .node
                .to_str()
                .is_some_and(|n| steve_acp::which_in(n, Some(service_os)).is_none())
        {
            if let Some(node_bin) = cfg.node.to_str() {
                primary = Some(node_bin.to_owned());
                candidates.push(node_bin.to_owned());
            }
        } else if let Some(name) = program.to_str() {
            primary = Some(name.to_owned());
        }
    }
    if let Some(b) = steve_harness_spec::builtin(id) {
        if !candidates.iter().any(|c| c == b.cli_bin) {
            candidates.push(b.cli_bin.to_owned());
        }
        for alt in &b.detect_bins {
            if !candidates.iter().any(|c| c == *alt) {
                candidates.push((*alt).to_owned());
            }
        }
        if primary.is_none() {
            primary = Some(b.cli_bin.to_owned());
        }
    }

    let mut found_in_login = None;
    let mut found_command = None;
    if let Some(login_path) = login_os {
        for candidate in &candidates {
            if let Some(resolved) = steve_acp::which_in(candidate, Some(login_path)) {
                found_in_login = Some(resolved.to_string_lossy().into_owned());
                found_command = Some(candidate.clone());
                break;
            }
        }
    }

    MissingCommandDiagnostic {
        command: found_command.or(primary).unwrap_or_else(|| id.to_owned()),
        service_path: service_path.to_owned(),
        login_shell_path: found_in_login,
        login_shell: login_env.map(|e| e.shell.clone()),
        refresh_command: refresh_command.to_owned(),
    }
}

#[derive(Default)]
pub(crate) struct AdapterProbes(tokio::sync::Mutex<Option<(Instant, HashMap<String, Probe>)>>);

impl AdapterProbes {
    pub(crate) async fn cached(&self) -> HashMap<String, Probe> {
        self.0
            .lock()
            .await
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
    /// The next [`Self::get`] asks the adapters again.
    pub(crate) async fn forget(&self) {
        *self.0.lock().await = None;
    }

    pub(crate) async fn get(
        &self,
        cfg: &AcpConfig,
        cwd: &Path,
        fresh: bool,
    ) -> HashMap<String, Probe> {
        let mut last = self.0.lock().await;
        if !fresh
            && let Some((at, v)) = last.as_ref()
            && at.elapsed() < TTL
        {
            return v.clone();
        }
        let ids: Vec<String> = cfg
            .adapter_ids()
            .into_iter()
            .filter(|id| cfg.adapter_properties(id).probe_initialize)
            .collect();
        // the scratch sessions' directory: one of the Node's own, the same every time
        let cwd = cwd.join("probe");
        let _ = std::fs::create_dir_all(&cwd);
        let mut set = tokio::task::JoinSet::new();
        for id in ids {
            let cfg = cfg.clone();
            let cwd = cwd.clone();
            set.spawn(async move {
                let p = steve_acp::adapters::probe(&cfg, &id, &cwd, INITIALIZE).await;
                (id, p)
            });
        }
        let mut v = HashMap::new();
        while let Some(r) = set.join_next().await {
            if let Ok((id, p)) = r {
                v.insert(id, p);
            }
        }
        *last = Some((Instant::now(), v.clone()));
        v
    }
}

/// The adapter part of `id`'s capability record.
pub(crate) fn info(
    cfg: &AcpConfig,
    id: &str,
    p: &Probe,
    service_path: &str,
    login_env: Option<&LoginShellEnv>,
    refresh_command: &str,
) -> AdapterInfo {
    let spec = cfg.adapter_spec(id);
    // a name the manifest declared wins over the adapter's own title
    let declared = cfg
        .declared
        .get(id)
        .and_then(|d| d.name.clone())
        .filter(|n| !n.trim().is_empty());
    let title = declared
        .or_else(|| (!spec.builtin).then(|| p.title.clone()).flatten())
        .unwrap_or(spec.name);
    let missing_command = (p.status == ProbeStatus::AdapterMissing)
        .then(|| missing_command_diagnostic(cfg, id, service_path, login_env, refresh_command));
    AdapterInfo {
        title,
        builtin: spec.builtin,
        status: match p.status {
            ProbeStatus::Ready => AdapterStatus::Ready,
            ProbeStatus::AdapterMissing => AdapterStatus::AdapterMissing,
            ProbeStatus::InitializeFailed => AdapterStatus::InitializeFailed,
        },
        error: p.error.clone(),
        version: p.version.clone(),
        load_session: p.load_session,
        list_sessions: p.list_sessions,
        mcp_http: p.mcp_http,
        embedded_context: p.embedded_context,
        auth: p
            .auth
            .iter()
            .map(|a| AdapterAuth {
                id: a.id.clone(),
                name: a.name.clone(),
                description: a.description.clone(),
            })
            .collect(),
        install_hint: spec.install_hint,
        system_prompt: p.system_prompt,
        session: p.session.as_ref().map(|o| steve_node_api::AdapterOffer {
            modes: o.modes.clone(),
            models: o
                .models
                .iter()
                .map(|m| steve_node_api::AdapterModel {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    description: m.description.clone(),
                })
                .collect(),
            default_model: o.default_model.clone(),
            efforts: o.efforts.clone(),
        }),
        session_error: p.session_error.clone(),
        session_auth_required: p.session_auth_required.then_some(true),
        execution_modes: None,
        resume_command: spec.properties.resume_cli,
        fork_command: spec.properties.fork_cli,
        capabilities: Some(steve_acp::adapter::capabilities_for(id)),
        missing_command,
    }
}

/// Fills `h` from what its adapter said. What `initialize` reported replaces the defaults only
/// when it answered: an adapter that is not there keeps today's record, now with its status.
pub(crate) fn apply(
    cfg: &AcpConfig,
    h: &mut HarnessCapability,
    p: Option<&Probe>,
    service_path: &str,
    login_env: Option<&LoginShellEnv>,
    refresh_command: &str,
) {
    let Some(p) = p else { return };
    if p.status == ProbeStatus::Ready {
        h.images = p.image;
        h.resume = p.load_session;
        if h.version.is_empty() {
            h.version = p.version.clone().unwrap_or_default();
        }
        if let Some(o) = &p.session
            && h.models.is_empty()
        {
            h.models = o.models.iter().map(|m| m.id.clone()).collect();
        }
    }
    h.adapter = Some(info(
        cfg,
        &h.name,
        p,
        service_path,
        login_env,
        refresh_command,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use steve_node_api::PermissionMode;

    fn cfg_with_antigravity(dir: &Path, bin: std::path::PathBuf) -> AcpConfig {
        AcpConfig {
            adapters: HashMap::from([("antigravity".into(), bin)]),
            declared: HashMap::new(),
            node: "node".into(),
            claude_executable: "/usr/bin/false".into(),
            claude_config_dir: None,
            steve_home: dir.join("steve"),
            codex_home: dir.join("codex"),
            user_home: dir.to_path_buf(),
            version: "test".into(),
        }
    }

    #[tokio::test]
    async fn node_probe_reports_antigravity_missing_and_ready() {
        let dir = tempfile::tempdir().unwrap();
        let probes = AdapterProbes::default();

        // Missing binary: AdapterMissing with login-shell diagnostic evidence
        let login_bin = dir.path().join("login-bin");
        std::fs::create_dir(&login_bin).unwrap();
        let login_agy = login_bin.join("agy");
        std::fs::write(&login_agy, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&login_agy, std::fs::Permissions::from_mode(0o755)).unwrap();
        let login_env = LoginShellEnv {
            shell: "/bin/zsh".into(),
            path: format!("{}:/usr/bin:/bin", login_bin.display()),
        };

        let missing_cfg = cfg_with_antigravity(dir.path(), dir.path().join("no-such-agy"));
        let got = probes.get(&missing_cfg, dir.path(), true).await;
        let info_missing = info(
            &missing_cfg,
            "antigravity",
            &got["antigravity"],
            "/usr/bin:/bin:/usr/sbin:/sbin",
            Some(&login_env),
            "steve node refresh-env",
        );
        assert_eq!(info_missing.status, AdapterStatus::AdapterMissing);
        assert_eq!(info_missing.title, "Antigravity CLI");
        assert!(info_missing.builtin);
        assert_eq!(
            info_missing.resume_command.as_deref(),
            Some("agy --resume {id}")
        );
        let diag = info_missing
            .missing_command
            .expect("missing command diagnostic");
        assert_eq!(diag.command, "agy");
        assert_eq!(diag.service_path, "/usr/bin:/bin:/usr/sbin:/sbin");
        assert_eq!(
            diag.login_shell_path.as_deref(),
            Some(login_agy.to_str().unwrap())
        );
        assert_eq!(diag.login_shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(diag.refresh_command, "steve node refresh-env");

        let info_truly_missing = info(
            &missing_cfg,
            "antigravity",
            &got["antigravity"],
            "/usr/bin:/bin",
            Some(&LoginShellEnv {
                shell: "/bin/zsh".into(),
                path: "/usr/bin:/bin".into(),
            }),
            "steve node refresh-env",
        );
        let truly_diag = info_truly_missing.missing_command.unwrap();
        assert_eq!(truly_diag.login_shell_path, None);
        assert_eq!(truly_diag.service_path, "/usr/bin:/bin");

        // Installed ACP binary: Ready with modes, models, efforts
        let fake = dir.path().join("agy");
        std::fs::write(
            &fake,
            r#"#!/usr/bin/env node
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    send({ id: m.id, result: {
      protocolVersion: 1,
      agentInfo: { name: "Antigravity ACP Server", version: "1.5.16" },
      agentCapabilities: { loadSession: true, promptCapabilities: { image: true, embeddedContext: true }, sessionCapabilities: { list: {} } },
      authMethods: [{ id: "oauth-personal", name: "Log in with Google" }]
    }});
  } else if (m.method === "session/new") {
    send({ id: m.id, result: {
      sessionId: "s1",
      modes: { currentModeId: "default", availableModes: [{ id: "default" }, { id: "accept-edits" }, { id: "auto" }, { id: "plan" }] },
      configOptions: [
        { id: "model", category: "model", type: "select", currentValue: "gemini-4-neon", options: [{ value: "gemini-4-neon", name: "Gemini 4 Neon" }, { value: "gemini-3.8-flash", name: "Gemini 3.8 Flash" }] },
        { id: "thought_level", category: "thought_level", type: "select", currentValue: "high", options: [{ value: "low" }, { value: "high" }] }
      ]
    }});
  }
});
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let ready_cfg = cfg_with_antigravity(dir.path(), fake);
        let got = probes.get(&ready_cfg, dir.path(), true).await;
        let mut cap = HarnessCapability {
            name: "antigravity".into(),
            version: String::new(),
            sessions: true,
            jobs: true,
            models: vec![],
            resume: false,
            cancel: true,
            permissions: true,
            images: false,
            read_attached: false,
            enforcement: vec![],
            adapter: None,
            setup: None,
        };
        apply(
            &ready_cfg,
            &mut cap,
            got.get("antigravity"),
            "/usr/bin:/bin",
            None,
            "steve node refresh-env",
        );
        assert!(cap.images && cap.resume);
        assert_eq!(cap.version, "1.5.16");
        assert_eq!(cap.models, ["gemini-4-neon", "gemini-3.8-flash"]);
        let ad = cap.adapter.expect("adapter info");
        assert_eq!(ad.status, AdapterStatus::Ready);
        assert!(ad.missing_command.is_none());
        let offer = ad.session.expect("session offer");
        assert_eq!(
            offer.modes,
            vec![
                PermissionMode::Plan,
                PermissionMode::Ask,
                PermissionMode::AcceptEdits,
                PermissionMode::Auto,
                PermissionMode::FullAccess,
            ]
        );
        assert_eq!(offer.default_model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(offer.efforts, ["low", "high"]);
    }
}
