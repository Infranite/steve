use super::*;

install! { install(reg, cx) [core_cfg, db, harnesses, host, mode, mut node_jobs, secrets] => [core_cfg, db, fleet, gateway, harnesses, host, node_host, node_jobs, nodes, preflight, routed, secrets, workspaces] {
    let HubEnv { cfg, bus, vars, .. } = *cx;
    let nodes = remote_nodes(cfg, &db, host.clone(), &vars);
    if let Some(s) = &nodes {
        let _ = s.secrets.set(secrets.clone());
    }
    let fleet = nodes.as_ref().map(|s| s.fleet.clone());
    // The Hub's colocated Node always runs in its own `steve-node --local-node` process and links
    // over loopback through the Node listener.
    let node_host: Option<SessionHost> = match mode {
        None => None,
        Some(NodeMode::Service) => {
            let svc = nodes.as_ref().ok_or_else(|| {
                BackendError::msg(
                    "STEVE_NODE_MODE=service needs the Node listener (STEVE_NODE_LISTEN is off)",
                )
            })?;
            let s = local_service::host(cfg, &vars, &db, host.clone(), svc, &mut node_jobs)?;
            Some(SessionHost {
                runtime: s.runtime,
                node: s.node,
                tools: s.tools,
            })
        }
    };
    if let Some(h) = &node_host {
        let _ = host.local_node.set(h.node.clone());
    }
    // The Hub handles model calls by dispatching a job to the colocated Node process.
    let gateway: Arc<dyn steve_core::ModelGateway> = match &node_host {
        Some(h) => {
            let models = steve_core::models::Models::new(
                db.clone(),
                steve_core::models::Defaults::of(&core_cfg),
            );
            models.set_harnesses(harnesses.source());
            tracing::info!("conversation model calls stream through the colocated Node");
            let model = steve_core::SingleModelGateway {
                one: node_runtime::NodePrint {
                    runtime: h.runtime.clone(),
                    data_dir: cfg.data_dir.clone(),
                },
                fast: cfg.models.fast.clone(),
                smart: cfg.models.smart.clone(),
                models: Some(models),
            };
            Arc::new(steve_core::stats::RecordingGateway::new(Arc::new(model), db.clone()))
        }
        None => Arc::new(DiagnosticGateway),
    };
    if node_host.is_some() || nodes.is_some() {
        let harnesses_job = harnesses.clone();
        let local_node_ref = node_host.as_ref().map(|h| h.node.node());
        let nodes_ref = nodes.clone();
        node_jobs.push(Job::task("harnesses.follow", move || {
            harnesses_job.follow_task(local_node_ref, nodes_ref)
        }));
    }
    let workspaces = workspaces::Workspaces::new(
        db.clone(),
        node_host.as_ref().map(|h| workspaces::LocalFace {
            node: h.node.clone(),
            runtime: h.runtime.clone(),
        }),
        fleet.clone(),
        workspaces::configured(cfg),
    );
    if node_host.is_some() {
        // the local roots follow projects and knowledge folders, with no restart
        let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(bus.tap(|event| {
            matches!(
                event.as_domain(),
                Some(
                    steve_types::DomainEvent::Config(steve_types::ConfigEvent::Project { .. })
                        | steve_types::DomainEvent::Knowledge(_)
                )
            )
        }))));
        let bus = bus.clone();
        let ws_job = workspaces.clone();
        node_jobs.push(Job::supervised("workspaces.follow", move || {
            let events = initial
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| {
                    bus.tap(|event| {
                        matches!(
                            event.as_domain(),
                            Some(
                                steve_types::DomainEvent::Config(
                                    steve_types::ConfigEvent::Project { .. }
                                ) | steve_types::DomainEvent::Knowledge(_)
                            )
                        )
                    })
                });
            ws_job.clone().follow("workspaces.follow", events)
        }));
    }
    // whether a principal's Bot can run at all: what the Nodes said of its harness last (installed,
    // ready, signed in); a Node linked but not heard from yet leaves its harnesses unknown, never
    // refused (#234)
    let preflight = {
        let h = harnesses.clone();
        let svc = nodes.clone();
        let readiness: preflight::Readiness = Arc::new(move || {
            let linked = svc
                .as_ref()
                .map(|s| s.registry.linked())
                .unwrap_or_default();
            h.readiness(&linked)
        });
        preflight::Preflight::new(Some(readiness))
    };
    let routed = Arc::new(crate::wire::core::RoutedRuntime {
        node: node_host.as_ref().map(|h| h.runtime.clone()),
        fleet: fleet.clone(),
        store: db.clone(),
        host: host.clone(),
        workspaces: workspaces.clone(),
        worktrees: node_host.as_ref().map(|_| cfg.worktrees.clone()),
        dispatch_harnesses: Some(harnesses.clone()),
    });
}}

struct DiagnosticGateway;

#[async_trait::async_trait]
impl steve_core::ModelGateway for DiagnosticGateway {
    async fn call(
        &self,
        _: &str,
        _: steve_core::Tier,
        _: std::time::Duration,
        _: &steve_core::gateway::ThinkTools,
    ) -> steve_core::Result<steve_core::gateway::Printed> {
        Err(steve_types::ConfigReason::diagnostic_model_disabled(
            "Model execution is disabled on this diagnostic Hub.",
        )
        .into())
    }

    async fn attempt(
        &self,
        prompt: &str,
        tier: steve_core::Tier,
        timeout: std::time::Duration,
        tools: &steve_core::gateway::ThinkTools,
        _: Option<steve_core::gateway::OnText<'_>>,
    ) -> steve_core::gateway::Attempt {
        steve_core::gateway::Attempt {
            result: self.call(prompt, tier, timeout, tools).await,
            attempts: 0,
            duration_ms: 0,
            usage: vec![],
            model: String::new(),
            failures: vec![],
            harness: String::new(),
        }
    }
}
