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
    AdapterAuth, AdapterInfo, AdapterStatus, HarnessCapability, LOGIN_SHELL_PROBE_TIMEOUT_SECS,
    MissingCommandDiagnostic, MissingCommandKind, extract_login_shell_path,
    login_shell_path_script, shell_supports_interactive_login,
};

/// Cache TTL for adapter discovery probes (30 minutes) unless the Hub requests a fresh probe.
const TTL: Duration = Duration::from_secs(1800);
/// Upper bound for ACP adapter `initialize` + scratch `session/new` handshake.
const INITIALIZE: Duration = Duration::from_secs(20);
/// Upper bound for probing the user's interactive/login shell `PATH` so slow rc scripts cannot
/// block Node discovery.
const LOGIN_SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(LOGIN_SHELL_PROBE_TIMEOUT_SECS);
/// Maximum stdout/stderr bytes accepted from a login-shell `PATH` probe (256 KiB covers any real
/// PATH plus rc banners while bounding memory).
const SHELL_PROBE_MAX_OUTPUT_BYTES: usize = 256 * 1024;

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

async fn run_shell_probe(
    shell: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
    timeout: Duration,
) -> Option<String> {
    let mut cmd = steve_runtime::process::DetachedCommand::new(shell);
    cmd.args(args).env("TERM", "dumb");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    // `DetachedCommand` uses `POSIX_SPAWN_SETSID` on macOS (zero `fork()` in multithreaded
    // `steve-node`) and `setsid()` on Linux so interactive shells have no controlling `/dev/tty`,
    // while `kill_group_after_exit` terminates any `.zshrc` background helpers BEFORE reaping.
    let output = steve_runtime::process::run_detached_async(
        cmd,
        shell,
        steve_runtime::process::BoundedOptions {
            max_output_bytes: SHELL_PROBE_MAX_OUTPUT_BYTES,
            kill_group_after_exit: true,
            ..steve_runtime::process::BoundedOptions::capture("shell_probe", timeout)
        },
    )
    .await
    .ok()?;
    if !output.status.success() {
        return None;
    }
    extract_login_shell_path(&String::from_utf8_lossy(&output.stdout))
}

async fn probe_shell_env_for(
    shell: &str,
    extra_env: &[(&str, &str)],
    timeout: Duration,
) -> Option<LoginShellEnv> {
    let script = login_shell_path_script(Path::new(shell));
    if shell_supports_interactive_login(Path::new(shell))
        && let Some(path) =
            run_shell_probe(shell, &["-l", "-i", "-c", &script], extra_env, timeout).await
    {
        return Some(LoginShellEnv {
            shell: shell.to_owned(),
            path,
        });
    }
    let path = run_shell_probe(shell, &["-l", "-c", &script], extra_env, timeout).await?;
    Some(LoginShellEnv {
        shell: shell.to_owned(),
        path,
    })
}

/// Probes the Node account's shell (`$SHELL -l -i -c` for zsh/bash, falling back to `$SHELL -l -c`)
/// once to see whether a command missing from the service's own `PATH` is actually installed in
/// the user's interactive/login `PATH`.
pub(crate) async fn probe_login_shell_env() -> Option<LoginShellEnv> {
    let shell = resolve_user_login_shell();
    probe_shell_env_for(&shell, &[], LOGIN_SHELL_PROBE_TIMEOUT).await
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
    let login_shell = login_env.map(|e| e.shell.clone());
    let builtin = steve_harness_spec::builtin(id);
    let declared = cfg.declared.contains_key(id);

    // 1. Adapter script exists on disk, runs via Node.js, and bare `node` is missing from
    //    the service PATH: diagnose the Node.js interpreter rather than the harness CLI.
    if let Some(own) = cfg.adapters.get(id)
        && own.is_file()
        && steve_acp::is_node_script(own)
        && cfg.node.components().count() == 1
        && !cfg.node.is_absolute()
        && let Some(node_bin) = cfg.node.to_str()
        && steve_acp::which_in(node_bin, Some(service_os)).is_none()
    {
        let found_in_login = login_os
            .and_then(|p| steve_acp::which_in(node_bin, Some(p)))
            .map(|p| p.to_string_lossy().into_owned());
        let kind = if found_in_login.is_some() {
            MissingCommandKind::CommandNotInServicePath
        } else {
            MissingCommandKind::BothMissing
        };
        return MissingCommandDiagnostic {
            kind,
            command: node_bin.to_owned(),
            adapter_package: None,
            detected_cli: None,
            cli_service_path: None,
            service_path: service_path.to_owned(),
            login_shell_path: found_in_login,
            login_shell,
            refresh_command: refresh_command.to_owned(),
        };
    }

    // 2. Identify the actual ACP adapter command and npm package declared by the harness spec,
    //    keeping `detect_bins` (underlying CLI) strictly separate from the adapter entry point.
    let adapter_package = (!declared)
        .then(|| builtin.and_then(|b| b.npm_package).map(str::to_owned))
        .flatten();

    let mut adapter_bins: Vec<String> = Vec::new();
    let mut primary: Option<String> = None;
    if let Ok((program, _)) = cfg.adapter_command(id) {
        if program.components().count() == 1 && !program.is_absolute() {
            if let Some(name) = program.to_str() {
                primary = Some(name.to_owned());
                adapter_bins.push(name.to_owned());
            }
        } else if let Some(name) = program.to_str() {
            primary = Some(name.to_owned());
        }
    }
    if let Some(b) = builtin.filter(|_| !declared) {
        match &b.command {
            steve_harness_spec::DefaultCommand::Path { bin } => {
                if !adapter_bins.iter().any(|c| c == bin) {
                    adapter_bins.push(bin.clone());
                }
                if primary.is_none() {
                    primary = Some(bin.clone());
                }
            }
            steve_harness_spec::DefaultCommand::Claude
            | steve_harness_spec::DefaultCommand::Codex
            | steve_harness_spec::DefaultCommand::NpmEntry { .. }
            | steve_harness_spec::DefaultCommand::NpmBin { .. } => {
                // `resolve_builtin_adapters` accepts `cli_bin` on PATH as an alternative to the
                // managed npm prefix; check `cli_bin` (never `detect_bins`) for PATH mismatches.
                if !adapter_bins.iter().any(|c| c == b.cli_bin) {
                    adapter_bins.push(b.cli_bin.to_owned());
                }
                primary = Some(b.cli_bin.to_owned());
            }
        }
    }

    // 3. If the actual ACP adapter command is absent from service PATH yet present on the login
    //    shell PATH, refreshing the service environment resolves it directly.
    if let Some(login_path) = login_os {
        for bin in &adapter_bins {
            if steve_acp::which_in(bin, Some(service_os)).is_none()
                && let Some(resolved) = steve_acp::which_in(bin, Some(login_path))
            {
                return MissingCommandDiagnostic {
                    kind: MissingCommandKind::CommandNotInServicePath,
                    command: bin.clone(),
                    adapter_package,
                    detected_cli: None,
                    cli_service_path: None,
                    service_path: service_path.to_owned(),
                    login_shell_path: Some(resolved.to_string_lossy().into_owned()),
                    login_shell,
                    refresh_command: refresh_command.to_owned(),
                };
            }
        }
    }

    // 4. Adapter command is absent everywhere. Inspect `detect_bins` (underlying harness CLI) to
    //    distinguish "CLI present, ACP adapter missing" from "both missing". Check service PATH
    //    first so a CLI already on service PATH is never misreported as a PATH mismatch.
    if let Some(b) = builtin.filter(|_| !declared) {
        for cli in &b.detect_bins {
            if let Some(resolved) = steve_acp::which_in(cli, Some(service_os)) {
                return MissingCommandDiagnostic {
                    kind: MissingCommandKind::AdapterPackageMissing,
                    command: primary.unwrap_or_else(|| b.cli_bin.to_owned()),
                    adapter_package,
                    detected_cli: Some((*cli).to_owned()),
                    cli_service_path: Some(resolved.to_string_lossy().into_owned()),
                    service_path: service_path.to_owned(),
                    login_shell_path: None,
                    login_shell: None,
                    refresh_command: refresh_command.to_owned(),
                };
            }
        }
        if let Some(login_path) = login_os {
            for cli in &b.detect_bins {
                if let Some(resolved) = steve_acp::which_in(cli, Some(login_path)) {
                    return MissingCommandDiagnostic {
                        kind: MissingCommandKind::AdapterPackageMissing,
                        command: primary.unwrap_or_else(|| b.cli_bin.to_owned()),
                        adapter_package,
                        detected_cli: Some((*cli).to_owned()),
                        cli_service_path: None,
                        service_path: service_path.to_owned(),
                        login_shell_path: Some(resolved.to_string_lossy().into_owned()),
                        login_shell,
                        refresh_command: refresh_command.to_owned(),
                    };
                }
            }
        }
    }

    // 5. Neither the ACP adapter nor any underlying CLI was found in service or login PATH.
    MissingCommandDiagnostic {
        kind: MissingCommandKind::BothMissing,
        command: primary.unwrap_or_else(|| id.to_owned()),
        adapter_package,
        detected_cli: None,
        cli_service_path: None,
        service_path: service_path.to_owned(),
        login_shell_path: None,
        login_shell,
        refresh_command: refresh_command.to_owned(),
    }
}

/// Cached result of one Node adapter discovery pass: per-adapter ACP probes, the login-shell
/// `PATH` diagnostic (when any adapter is missing), and the single shared Node.js runtime probe.
#[derive(Clone, Default)]
pub(crate) struct ProbeSnapshot {
    pub(crate) adapters: HashMap<String, Probe>,
    pub(crate) login_env: Option<LoginShellEnv>,
    pub(crate) node_runtime: Option<crate::adapter_setup::NodeRuntimeProbe>,
}

async fn probe_adapters(cfg: &AcpConfig, cwd: &Path) -> HashMap<String, Probe> {
    let ids: Vec<String> = cfg
        .adapter_ids()
        .into_iter()
        .filter(|id| cfg.adapter_properties(id).probe_initialize)
        .collect();
    // the scratch sessions' directory: one of the Node's own, the same every time.
    // Never recreate a retired/removed Node home via create_dir_all; probe() falls back to temp_dir
    // when this directory is absent.
    let cwd = cwd.join("probe");
    if cwd.parent().is_some_and(Path::is_dir) {
        let _ = std::fs::create_dir(&cwd);
    }
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
    v
}

#[derive(Default)]
pub(crate) struct AdapterProbes(tokio::sync::Mutex<Option<(Instant, ProbeSnapshot)>>);

impl AdapterProbes {
    pub(crate) async fn cached(&self) -> ProbeSnapshot {
        self.0
            .lock()
            .await
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
    /// The next [`Self::get_snapshot`] asks the adapters and host environment again.
    pub(crate) async fn forget(&self) {
        *self.0.lock().await = None;
    }

    pub(crate) async fn get_snapshot(
        &self,
        cfg: &AcpConfig,
        cwd: &Path,
        fresh: bool,
    ) -> ProbeSnapshot {
        let mut last = self.0.lock().await;
        if !fresh
            && let Some((at, v)) = last.as_ref()
            && at.elapsed() < TTL
        {
            return v.clone();
        }
        let needs_node_runtime = cfg
            .adapters
            .keys()
            .any(|id| !steve_harness_spec::components(id).is_empty());
        let (adapters, node_runtime) = tokio::join!(probe_adapters(cfg, cwd), async {
            if needs_node_runtime {
                Some(crate::adapter_setup::probe_node_runtime(cfg).await)
            } else {
                None
            }
        });
        let any_adapter_missing = adapters
            .values()
            .any(|p| p.status == ProbeStatus::AdapterMissing);
        let login_env = if any_adapter_missing {
            probe_login_shell_env().await
        } else {
            None
        };
        let snapshot = ProbeSnapshot {
            adapters,
            login_env,
            node_runtime,
        };
        *last = Some((Instant::now(), snapshot.clone()));
        snapshot
    }

    #[cfg(test)]
    pub(crate) async fn get(
        &self,
        cfg: &AcpConfig,
        cwd: &Path,
        fresh: bool,
    ) -> HashMap<String, Probe> {
        if !fresh
            && let Some((at, v)) = self.0.lock().await.as_ref()
            && at.elapsed() < TTL
        {
            return v.adapters.clone();
        }
        let adapters = probe_adapters(cfg, cwd).await;
        *self.0.lock().await = Some((
            Instant::now(),
            ProbeSnapshot {
                adapters: adapters.clone(),
                login_env: None,
                node_runtime: None,
            },
        ));
        adapters
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
                    efforts: m.efforts.clone(),
                })
                .collect(),
            default_model: o.default_model.clone(),
            efforts: o.efforts.clone(),
            variant_models: o.variant_models.clone(),
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
        assert_eq!(diag.kind, MissingCommandKind::CommandNotInServicePath);
        assert_eq!(diag.command, "agy");
        assert_eq!(diag.adapter_package, None);
        assert_eq!(diag.detected_cli, None);
        assert_eq!(diag.cli_service_path, None);
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
        assert_eq!(truly_diag.kind, MissingCommandKind::BothMissing);
        assert_eq!(truly_diag.login_shell_path, None);
        assert_eq!(truly_diag.service_path, "/usr/bin:/bin");

        // Installed ACP binary: Ready with modes, models, per-model efforts, and variant models
        let fake = dir.path().join("agy");
        std::fs::write(
            &fake,
            r#"#!/usr/bin/env node
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");
const optsFor = (model) => {
  const base = [{
    id: "model", category: "model", type: "select", currentValue: model,
    options: [
      { value: "gemini-4-neon", name: "Gemini 4 Neon" },
      { value: "gemini-pro-orchestrator-neon", name: "Gemini Pro Orchestrator Neon" }
    ]
  }];
  if (model === "gemini-4-neon") {
    base.push({
      id: "thought_level", category: "thought_level", type: "select", currentValue: "high",
      options: [{ value: "low" }, { value: "medium" }, { value: "high" }, { value: "xhigh" }]
    });
  }
  return base;
};
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    send({ id: m.id, result: {
      protocolVersion: 1,
      agentInfo: { name: "Antigravity ACP Server", version: "1.5.17" },
      agentCapabilities: { loadSession: true, promptCapabilities: { image: true, embeddedContext: true }, sessionCapabilities: { list: {} } },
      authMethods: [{ id: "oauth-personal", name: "Log in with Google" }]
    }});
  } else if (m.method === "session/new") {
    send({ id: m.id, result: {
      sessionId: "s1",
      modes: { currentModeId: "default", availableModes: [{ id: "default" }, { id: "accept-edits" }, { id: "auto" }, { id: "plan" }] },
      models: {
        currentModelId: "gemini-4-neon-high",
        availableModels: [
          { modelId: "gemini-4-neon-low" },
          { modelId: "gemini-4-neon-medium" },
          { modelId: "gemini-4-neon-high" },
          { modelId: "gemini-4-neon-xhigh" },
          { modelId: "gemini-pro-orchestrator-neon" }
        ]
      },
      configOptions: optsFor("gemini-4-neon")
    }});
  } else if (m.method === "session/set_config_option" && m.params.configId === "model") {
    send({ id: m.id, result: { configOptions: optsFor(m.params.value) } });
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
        assert_eq!(cap.version, "1.5.17");
        assert_eq!(
            cap.models,
            ["gemini-4-neon", "gemini-pro-orchestrator-neon"]
        );
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
        assert_eq!(offer.efforts, ["low", "medium", "high", "xhigh"]);
        assert_eq!(
            offer.models[0].efforts.as_deref(),
            Some(
                ["low", "medium", "high", "xhigh"]
                    .map(String::from)
                    .as_slice()
            )
        );
        assert_eq!(offer.models[1].efforts.as_deref(), Some(&[][..]));
        assert!(
            offer
                .variant_models
                .contains(&"gemini-4-neon-xhigh".to_string())
        );
    }

    #[tokio::test]
    async fn probe_shell_env_loads_zshrc_only_path_and_falls_back_to_login_on_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let rc_bin = dir.path().join("zshrc-bin");
        let login_bin = dir.path().join("zprofile-bin");
        let zdotdir = dir.path().join("zdotdir");
        std::fs::create_dir_all(&rc_bin).unwrap();
        std::fs::create_dir_all(&login_bin).unwrap();
        std::fs::create_dir_all(&zdotdir).unwrap();
        let agy = rc_bin.join("agy");
        std::fs::write(&agy, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&agy, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Write PATH exclusively in .zshrc (never in .zprofile) and assert TERM=dumb + non-blocking stdin.
        std::fs::write(
            zdotdir.join(".zshrc"),
            format!(
                "if [ \"$TERM\" != \"dumb\" ]; then exit 42; fi\nread _ignored || true\nexport PATH=\"{}:$PATH\"\n",
                rc_bin.display()
            ),
        )
        .unwrap();

        let fake_zsh = dir.path().join("zsh");
        std::fs::write(
            &fake_zsh,
            r#"#!/bin/sh
interactive=0
login=0
script=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -l) login=1; shift ;;
    -i) interactive=1; shift ;;
    -c) script="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ "$login" = "1" ] && [ -f "$ZDOTDIR/.zprofile" ]; then
  . "$ZDOTDIR/.zprofile"
fi
if [ "$interactive" = "1" ] && [ -f "$ZDOTDIR/.zshrc" ]; then
  . "$ZDOTDIR/.zshrc"
fi
exec /bin/sh -c "$script"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake_zsh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let zdotdir_str = zdotdir.to_str().unwrap();
        let env = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", zdotdir_str),
            ("ZDOTDIR", zdotdir_str),
        ];

        let probed =
            probe_shell_env_for(fake_zsh.to_str().unwrap(), &env, LOGIN_SHELL_PROBE_TIMEOUT)
                .await
                .expect("interactive login probe should capture .zshrc-only PATH");
        assert_eq!(
            steve_acp::which_in("agy", Some(OsStr::new(&probed.path))),
            Some(agy.clone()),
            "expected agy from .zshrc in {:?}",
            probed.path
        );

        // Also verify against real /bin/zsh when installed (macOS and zsh Linux hosts).
        if steve_acp::is_executable_file(Path::new("/bin/zsh")) {
            let real = probe_shell_env_for("/bin/zsh", &env, LOGIN_SHELL_PROBE_TIMEOUT)
                .await
                .expect("real /bin/zsh interactive login probe should succeed");
            assert_eq!(
                steve_acp::which_in("agy", Some(OsStr::new(&real.path))),
                Some(agy.clone()),
                "real /bin/zsh must load .zshrc-only PATH: {:?}",
                real.path
            );
        }

        // Hanging .zshrc times out cleanly and falls back to login-only (-l -c) reading .zprofile.
        std::fs::write(
            zdotdir.join(".zprofile"),
            format!("export PATH=\"{}:$PATH\"\n", login_bin.display()),
        )
        .unwrap();
        std::fs::write(zdotdir.join(".zshrc"), "sleep 10\n").unwrap();
        let fallback =
            probe_shell_env_for(fake_zsh.to_str().unwrap(), &env, Duration::from_millis(250))
                .await
                .expect("fallback to -l -c should succeed after interactive timeout");
        assert!(
            std::env::split_paths(OsStr::new(&fallback.path)).any(|p| p == login_bin),
            "expected login_bin from .zprofile fallback in {:?}",
            fallback.path
        );

        // Background grandchild launched by .zshrc inheriting stdout must not hang read_task or survive.
        let bg_pid_file = dir.path().join("zshrc-bg.pid");
        std::fs::write(
            zdotdir.join(".zshrc"),
            format!(
                "/bin/sleep 60 & printf '%s' \"$!\" > '{}'\nexport PATH=\"{}:$PATH\"\n",
                bg_pid_file.display(),
                rc_bin.display()
            ),
        )
        .unwrap();
        let started = Instant::now();
        let bg_probed =
            probe_shell_env_for(fake_zsh.to_str().unwrap(), &env, LOGIN_SHELL_PROBE_TIMEOUT)
                .await
                .expect("background grandchild holding stdout must not hang shell probe");
        assert_eq!(
            steve_acp::which_in("agy", Some(OsStr::new(&bg_probed.path))),
            Some(agy),
            "expected agy from .zshrc despite background child: {:?}",
            bg_probed.path
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "grandchild pipe cleanup must return promptly, took {:?}",
            started.elapsed()
        );
        #[cfg(unix)]
        {
            let bg_pid: libc::pid_t = std::fs::read_to_string(&bg_pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(bg_pid, 0) } == 0 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                unsafe { libc::kill(bg_pid, 0) },
                -1,
                "background .zshrc child {bg_pid} survived shell probe"
            );
        }
    }

    fn write_exec(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn cfg_for_builtin(dir: &Path, id: &str, cmd: PathBuf) -> AcpConfig {
        AcpConfig {
            adapters: HashMap::from([(id.into(), cmd)]),
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

    /// Case 1: Adapter executable (PATH bin or npm cli_bin) is absent from service PATH yet
    /// present in login shell PATH -> `CommandNotInServicePath` (suggests `refresh-env`).
    #[tokio::test]
    async fn diagnosis_command_not_in_service_path_for_path_and_npm_bins() {
        let dir = tempfile::tempdir().unwrap();
        let login_bin = dir.path().join("login-bin");
        let empty_service_bin = dir.path().join("service-bin");
        std::fs::create_dir_all(&login_bin).unwrap();
        std::fs::create_dir_all(&empty_service_bin).unwrap();

        let login_agy = login_bin.join("agy");
        write_exec(&login_agy, "#!/bin/sh\nexit 0\n");
        let login_pi_acp = login_bin.join("openma-pi-acp");
        write_exec(&login_pi_acp, "#!/bin/sh\nexit 0\n");

        let login_env = LoginShellEnv {
            shell: "/bin/zsh".into(),
            path: login_bin.to_string_lossy().into_owned(),
        };
        let service_path = empty_service_bin.to_string_lossy().into_owned();
        let probes = AdapterProbes::default();

        // PATH-command spec (`antigravity`, command = Path { bin: "agy" }): point the configured
        // path at an absent temp file so the probe is isolated from whatever binaries exist on
        // the test host's own PATH while `missing_command_diagnostic` still resolves `b.command`.
        let agy_cfg = cfg_for_builtin(dir.path(), "antigravity", dir.path().join("no-agy"));
        let agy_probe = probes.get(&agy_cfg, dir.path(), true).await;
        let agy_info = info(
            &agy_cfg,
            "antigravity",
            &agy_probe["antigravity"],
            &service_path,
            Some(&login_env),
            "steve node refresh-env",
        );
        let agy_diag = agy_info.missing_command.expect("diagnostic");
        assert_eq!(agy_diag.kind, MissingCommandKind::CommandNotInServicePath);
        assert_eq!(agy_diag.command, "agy");
        assert_eq!(agy_diag.adapter_package, None);
        assert_eq!(
            agy_diag.login_shell_path.as_deref(),
            Some(login_agy.to_str().unwrap())
        );

        // npm_entry spec (`pi`, cli_bin = "openma-pi-acp"): when the ACP binary itself is in
        // login shell PATH (and absent from service PATH), `refresh-env` resolves it.
        let pi_managed = steve_acp::managed::command(&dir.path().join("steve"), "pi").unwrap();
        let pi_cfg = cfg_for_builtin(dir.path(), "pi", pi_managed);
        let pi_probe = probes.get(&pi_cfg, dir.path(), true).await;
        let pi_info = info(
            &pi_cfg,
            "pi",
            &pi_probe["pi"],
            &service_path,
            Some(&login_env),
            "steve node refresh-env",
        );
        let pi_diag = pi_info.missing_command.expect("diagnostic");
        assert_eq!(pi_diag.kind, MissingCommandKind::CommandNotInServicePath);
        assert_eq!(pi_diag.command, "openma-pi-acp");
        assert_eq!(pi_diag.adapter_package.as_deref(), Some("@openma/pi-acp"));
        assert_eq!(
            pi_diag.login_shell_path.as_deref(),
            Some(login_pi_acp.to_str().unwrap())
        );
    }

    /// Case 2: Underlying CLI (`detect_bins`, e.g. `pi` or `cursor`) is ALREADY in service PATH,
    /// while the ACP adapter package/binary (`@openma/pi-acp` / `cursor-agent`) is missing ->
    /// `AdapterPackageMissing` (never misclassified as `CommandNotInServicePath`).
    #[tokio::test]
    async fn diagnosis_cli_present_but_acp_adapter_package_missing() {
        let dir = tempfile::tempdir().unwrap();
        let shared_bin = dir.path().join("usr-local-bin");
        std::fs::create_dir_all(&shared_bin).unwrap();
        let pi_bin = shared_bin.join("pi");
        write_exec(&pi_bin, "#!/bin/sh\nexit 0\n");
        let cursor_bin = shared_bin.join("cursor");
        write_exec(&cursor_bin, "#!/bin/sh\nexit 0\n");

        // Same directory appears in both service PATH and login shell PATH (reproducing the
        // MacBook Node screenshot where `/usr/local/bin/pi` was already on service PATH).
        let shared_path = shared_bin.to_string_lossy().into_owned();
        let login_env = LoginShellEnv {
            shell: "/bin/zsh".into(),
            path: shared_path.clone(),
        };
        let probes = AdapterProbes::default();

        // npm_entry spec: `pi` (`@openma/pi-acp` missing, `pi` in service PATH)
        let pi_managed = steve_acp::managed::command(&dir.path().join("steve"), "pi").unwrap();
        let pi_cfg = cfg_for_builtin(dir.path(), "pi", pi_managed);
        let pi_probe = probes.get(&pi_cfg, dir.path(), true).await;
        assert_eq!(pi_probe["pi"].status, ProbeStatus::AdapterMissing);
        let pi_info = info(
            &pi_cfg,
            "pi",
            &pi_probe["pi"],
            &shared_path,
            Some(&login_env),
            "steve node refresh-env",
        );
        let pi_diag = pi_info.missing_command.expect("diagnostic");
        assert_eq!(pi_diag.kind, MissingCommandKind::AdapterPackageMissing);
        assert_eq!(pi_diag.command, "openma-pi-acp");
        assert_eq!(pi_diag.adapter_package.as_deref(), Some("@openma/pi-acp"));
        assert_eq!(pi_diag.detected_cli.as_deref(), Some("pi"));
        assert_eq!(
            pi_diag.cli_service_path.as_deref(),
            Some(pi_bin.to_str().unwrap())
        );
        assert_eq!(
            pi_diag.login_shell_path, None,
            "CLI already on service PATH must not be misreported as login-shell-only"
        );

        // Also when `pi` CLI is ONLY in login shell PATH and `@openma/pi-acp` is missing:
        // still `AdapterPackageMissing` (since refresh-env alone cannot install `@openma/pi-acp`),
        // carrying the login shell CLI path as evidence.
        let empty_service = dir.path().join("empty-service");
        std::fs::create_dir_all(&empty_service).unwrap();
        let pi_login_only = info(
            &pi_cfg,
            "pi",
            &pi_probe["pi"],
            empty_service.to_str().unwrap(),
            Some(&login_env),
            "steve node refresh-env",
        );
        let pi_login_diag = pi_login_only.missing_command.expect("diagnostic");
        assert_eq!(
            pi_login_diag.kind,
            MissingCommandKind::AdapterPackageMissing
        );
        assert_eq!(pi_login_diag.detected_cli.as_deref(), Some("pi"));
        assert_eq!(pi_login_diag.cli_service_path, None);
        assert_eq!(
            pi_login_diag.login_shell_path.as_deref(),
            Some(pi_bin.to_str().unwrap())
        );

        // PATH-command spec with distinct `detect_bins`: `cursor` (`cursor` in service PATH,
        // `cursor-agent` missing everywhere).
        let cursor_cfg = cfg_for_builtin(dir.path(), "cursor", dir.path().join("no-cursor-agent"));
        let cursor_probe = probes.get(&cursor_cfg, dir.path(), true).await;
        let cursor_info = info(
            &cursor_cfg,
            "cursor",
            &cursor_probe["cursor"],
            &shared_path,
            Some(&login_env),
            "steve node refresh-env",
        );
        let cursor_diag = cursor_info.missing_command.expect("diagnostic");
        assert_eq!(cursor_diag.kind, MissingCommandKind::AdapterPackageMissing);
        assert_eq!(cursor_diag.adapter_package, None);
        assert_eq!(cursor_diag.detected_cli.as_deref(), Some("cursor"));
        assert_eq!(
            cursor_diag.cli_service_path.as_deref(),
            Some(cursor_bin.to_str().unwrap())
        );
        assert_eq!(cursor_diag.login_shell_path, None);
    }

    /// Case 3: Neither the ACP adapter nor any `detect_bins` CLI exists in service or login PATH
    /// -> `BothMissing`.
    #[tokio::test]
    async fn diagnosis_both_cli_and_adapter_missing() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let empty_path = empty.to_string_lossy().into_owned();
        let login_env = LoginShellEnv {
            shell: "/bin/zsh".into(),
            path: empty_path.clone(),
        };
        let probes = AdapterProbes::default();

        let pi_managed = steve_acp::managed::command(&dir.path().join("steve"), "pi").unwrap();
        let pi_cfg = cfg_for_builtin(dir.path(), "pi", pi_managed);
        let pi_probe = probes.get(&pi_cfg, dir.path(), true).await;
        let pi_info = info(
            &pi_cfg,
            "pi",
            &pi_probe["pi"],
            &empty_path,
            Some(&login_env),
            "steve node refresh-env",
        );
        let pi_diag = pi_info.missing_command.expect("diagnostic");
        assert_eq!(pi_diag.kind, MissingCommandKind::BothMissing);
        assert_eq!(pi_diag.adapter_package.as_deref(), Some("@openma/pi-acp"));
        assert_eq!(pi_diag.detected_cli, None);
        assert_eq!(pi_diag.cli_service_path, None);
        assert_eq!(pi_diag.login_shell_path, None);
    }

    /// Case 4: Adapter binary/package IS installed, but fails to start (non-executable or exits
    /// before answering `initialize`) -> `AdapterStatus::InitializeFailed` with no
    /// `missing_command` diagnostic.
    #[tokio::test]
    async fn diagnosis_installed_adapter_startup_failure_is_initialize_failed() {
        let dir = tempfile::tempdir().unwrap();
        let probes = AdapterProbes::default();

        // 4a. Installed adapter exists, starts, and exits immediately without answering initialize.
        let crashing = dir.path().join("crashing-adapter");
        write_exec(&crashing, "#!/bin/sh\nexit 1\n");
        let crash_cfg = cfg_for_builtin(dir.path(), "antigravity", crashing);
        let crash_probe = probes.get(&crash_cfg, dir.path(), true).await;
        let crash_info = info(
            &crash_cfg,
            "antigravity",
            &crash_probe["antigravity"],
            "/usr/bin:/bin",
            None,
            "steve node refresh-env",
        );
        assert_eq!(crash_info.status, AdapterStatus::InitializeFailed);
        assert!(crash_info.missing_command.is_none());

        // 4b. Installed adapter file exists on disk, but cannot even spawn (not executable).
        let unexec = dir.path().join("non-executable-adapter");
        std::fs::write(&unexec, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&unexec, std::fs::Permissions::from_mode(0o644)).unwrap();
        let unexec_cfg = cfg_for_builtin(dir.path(), "antigravity", unexec);
        let unexec_probe = probes.get(&unexec_cfg, dir.path(), true).await;
        let unexec_info = info(
            &unexec_cfg,
            "antigravity",
            &unexec_probe["antigravity"],
            "/usr/bin:/bin",
            None,
            "steve node refresh-env",
        );
        assert_eq!(unexec_info.status, AdapterStatus::InitializeFailed);
        assert!(unexec_info.missing_command.is_none());
    }

    /// `get_snapshot` probes `node --version` once across all component-bearing harnesses and
    /// serves cached subsequent `fresh = false` reads without spawning `node` again until
    /// `forget()` or `fresh = true`.
    #[tokio::test]
    async fn probe_snapshot_probes_node_version_once_and_caches_subsequent_reads() {
        let dir = tempfile::tempdir().unwrap();
        let count_file = dir.path().join("node-calls.log");
        let fake_node = dir.path().join("node");
        write_exec(
            &fake_node,
            &format!(
                "#!/bin/sh\nprintf 'x\\n' >> '{}'\nprintf 'v22.5.0\\n'\n",
                count_file.display()
            ),
        );
        let fake_npm = dir.path().join("npm");
        write_exec(&fake_npm, "#!/bin/sh\nexit 0\n");

        // Configure multiple built-in harnesses that declare managed npm components, each with
        // an existing crashing adapter binary so `probe_adapters` reports `InitializeFailed`
        // (not `AdapterMissing`, keeping this unit test independent of the host login shell).
        let crashing = dir.path().join("crashing-adapter");
        write_exec(&crashing, "#!/bin/sh\nexit 1\n");
        let cfg = AcpConfig {
            adapters: HashMap::from([
                ("claude".into(), crashing.clone()),
                ("codex".into(), crashing.clone()),
                ("pi".into(), crashing),
            ]),
            declared: HashMap::new(),
            node: fake_node,
            claude_executable: "/usr/bin/false".into(),
            claude_config_dir: None,
            steve_home: dir.path().join("steve"),
            codex_home: dir.path().join("codex"),
            user_home: dir.path().to_path_buf(),
            version: "test".into(),
        };

        let calls = || {
            std::fs::read_to_string(&count_file)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };
        let probes = AdapterProbes::default();

        let first = probes.get_snapshot(&cfg, dir.path(), false).await;
        assert_eq!(
            calls(),
            1,
            "node --version must run once across all component harnesses, not once per harness"
        );
        let node_runtime = first.node_runtime.as_ref().expect("node runtime probed");
        let req = crate::adapter_setup::requirements_from(node_runtime, "claude");
        assert!(
            req.node_error.is_none() && req.npm_error.is_none(),
            "fake node v22.5.0 + npm beside it should satisfy requirements: node={:?} npm={:?}",
            req.node_error,
            req.npm_error
        );

        let _cached = probes.get_snapshot(&cfg, dir.path(), false).await;
        assert_eq!(
            calls(),
            1,
            "cached fresh=false probe must not re-run node --version"
        );

        probes.forget().await;
        let _after_forget = probes.get_snapshot(&cfg, dir.path(), false).await;
        assert_eq!(
            calls(),
            2,
            "forget() must invalidate cached node runtime probe"
        );
    }

    #[tokio::test]
    async fn probe_adapters_does_not_recreate_missing_home_directories() {
        let root = tempfile::tempdir().unwrap();
        let steve_home = root.path().join("retired-steve-home");
        let node_home = steve_home.join("data/node");
        let cfg = AcpConfig {
            adapters: HashMap::from([("pi".into(), steve_home.join("adapters/pi"))]),
            declared: HashMap::new(),
            node: PathBuf::from("/usr/bin/false"),
            claude_executable: PathBuf::from("/usr/bin/false"),
            claude_config_dir: None,
            steve_home: steve_home.clone(),
            codex_home: steve_home.join("codex"),
            user_home: root.path().to_path_buf(),
            version: "test".into(),
        };

        let result = probe_adapters(&cfg, &node_home).await;
        assert_eq!(result["pi"].status, ProbeStatus::AdapterMissing);
        assert!(
            !steve_home.exists(),
            "probe_adapters must not recreate missing steve_home"
        );
        assert!(
            !node_home.exists(),
            "probe_adapters must not recreate missing node_home"
        );
    }
}
