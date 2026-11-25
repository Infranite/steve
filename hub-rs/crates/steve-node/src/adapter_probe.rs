//! What each configured harness adapter says about itself (`initialize`, and the modes, models
//! and efforts of a scratch `session/new`: no prompt, no model is asked anything), for discovery
//! (harness parity S1, S4, S8). Asked side by side, at most every half hour unless the Hub asks for
//! a fresh probe; an adapter that cannot start or answer is reported so, and nothing is refused
//! because of it. Adapters that declare `probe_initialize: false` are not asked.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_acp::adapters::{Probe, ProbeStatus};
use steve_node_api::{AdapterAuth, AdapterInfo, AdapterStatus, HarnessCapability};

const TTL: Duration = Duration::from_secs(1800);
const INITIALIZE: Duration = Duration::from_secs(20);

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
pub(crate) fn info(cfg: &AcpConfig, id: &str, p: &Probe) -> AdapterInfo {
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
    }
}

/// Fills `h` from what its adapter said. What `initialize` reported replaces the defaults only
/// when it answered: an adapter that is not there keeps today's record, now with its status.
pub(crate) fn apply(cfg: &AcpConfig, h: &mut HarnessCapability, p: Option<&Probe>) {
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
    h.adapter = Some(info(cfg, &h.name, p));
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

        // Missing binary: AdapterMissing
        let missing_cfg = cfg_with_antigravity(dir.path(), dir.path().join("no-such-agy"));
        let got = probes.get(&missing_cfg, dir.path(), true).await;
        let info_missing = info(&missing_cfg, "antigravity", &got["antigravity"]);
        assert_eq!(info_missing.status, AdapterStatus::AdapterMissing);
        assert_eq!(info_missing.title, "Antigravity CLI");
        assert!(info_missing.builtin);
        assert_eq!(
            info_missing.resume_command.as_deref(),
            Some("agy --resume {id}")
        );

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
        apply(&ready_cfg, &mut cap, got.get("antigravity"));
        assert!(cap.images && cap.resume);
        assert_eq!(cap.version, "1.5.16");
        assert_eq!(cap.models, ["gemini-4-neon", "gemini-3.8-flash"]);
        let ad = cap.adapter.expect("adapter info");
        assert_eq!(ad.status, AdapterStatus::Ready);
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
