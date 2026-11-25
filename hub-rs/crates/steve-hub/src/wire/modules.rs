//! The new modules (steve-modules B0/B1, contacts, accounts, vault, steve-flow) on top of steve-core:
//! the registry, the module gateway, `steve_flow::Core` over the core, and the routes to merge under `/api`.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use serde_json::Value;
use steve_core::{Core, ItemAction};
use steve_modules::Modules;
use steve_store::Store;

use crate::error::BackendError;

/// A named background task prepared during assembly, started after boot.
pub struct Job {
    pub name: &'static str,
    start: Box<dyn FnOnce() -> steve_runtime::Task + Send>,
}
impl Job {
    pub fn new(name: &'static str, future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self::task(name, move || steve_runtime::spawn(name, future))
    }
    /// Starts a long-running background job under bounded exponential-backoff panic supervision.
    pub fn supervised<F, Fut>(name: &'static str, make: F) -> Self
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::task(name, move || {
            steve_runtime::supervise(name, Arc::new(steve_runtime::SystemClock), make)
        })
    }
    pub fn task(
        name: &'static str,
        start: impl FnOnce() -> steve_runtime::Task + Send + 'static,
    ) -> Self {
        Self {
            name,
            start: Box::new(start),
        }
    }
    /// Starts a standalone job. The caller owns cancellation through the returned task.
    pub fn spawn(self) -> steve_runtime::Task {
        (self.start)()
    }
    /// Starts once, registering the owned handle before returning.
    pub fn start(self, owner: &crate::jobs::Tasks) {
        owner.start(self.name, self.start);
    }
}

/// What schedules need from steve-core.
pub struct FlowCore {
    pub core: Core,
    /// [`steve_flow::Core::can_run`]: `None` lets everything run.
    pub preflight: Option<Arc<super::preflight::Preflight>>,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[async_trait]
impl steve_flow::Core for FlowCore {
    fn can_run(
        &self,
        store: &steve_store::Store,
        ws: &str,
        requester: Option<&str>,
        bot: &str,
    ) -> Result<(), String> {
        let Some(preflight) = &self.preflight else {
            return Ok(());
        };
        store
            .read(|c| {
                Ok::<_, steve_store::StoreError>(preflight.can_run_schedule(c, ws, requester, bot))
            })
            .map_err(|error| error.to_string())?
            .map_err(|blocked| blocked.reason)
    }
    async fn create_item(&self, req: steve_flow::ItemRequest) -> Result<String, String> {
        let c = self.core.clone();
        blocking(move || {
            let item = c
                .create_item(steve_core::CreateItem {
                    ws: req.ws,
                    goal: req.goal,
                    title: req.title,
                    bot: req.bot,
                    team: None,
                    node: None,
                    node_tag: req.node_tag,
                    required_specialties: vec![],
                    project: req.project.map(Some),
                    source: Some(req.source),
                    parent: None,
                    tag_ids: req.tag_ids,
                    deps: vec![],
                    propose: req.propose,
                    opening: None,
                    files: vec![],
                    actor: Some("steve".into()),
                    requester: req.requester,
                })
                .map_err(|e| e.to_string())?;
            if req.accept_auto {
                let p = steve_store::ItemPatch {
                    accept_mode: Some(Some("auto".into())),
                    ..Default::default()
                };
                c.store()
                    .store()
                    .items()
                    .patch(&item.id, &p)
                    .map_err(|e| e.to_string())?;
            }
            Ok(item.id)
        })
        .await
    }
    async fn pause_item(&self, _ws: &str, item_id: &str) -> Result<(), String> {
        let (c, id) = (self.core.clone(), item_id.to_string());
        blocking(move || {
            c.item_action(&id, ItemAction::Pause, "steve")
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }
    async fn resume_item(&self, _ws: &str, item_id: &str) -> Result<(), String> {
        let (c, id) = (self.core.clone(), item_id.to_string());
        blocking(move || {
            c.item_action(&id, ItemAction::Resume, "steve")
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }
    async fn post_steve_message(&self, ws: &str, text: &str, meta: Value) -> Result<(), String> {
        let (s, ws, text) = (
            self.core.store().store().clone(),
            ws.to_string(),
            text.to_string(),
        );
        blocking(move || {
            s.messages()
                .add(&ws, "steve", &text, None, &meta, "steve")
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }
    async fn digest_text(&self, ws: &str) -> Result<String, String> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || c.digest_text(&ws).map_err(|e| e.to_string())).await
    }
    /// `item.link` (S2): the text goes to the item like a user supplement (`auto`: steer a running
    /// turn, else a new one).
    async fn supplement(
        &self,
        ws: &str,
        item_id: &str,
        text: &str,
        mode: &str,
    ) -> Result<(), String> {
        let mode = match mode {
            "steer" => steve_core::SupplementMode::Steer,
            "new" => steve_core::SupplementMode::New,
            _ => steve_core::SupplementMode::Auto,
        };
        let (c, ws, id, text) = (
            self.core.clone(),
            ws.to_string(),
            item_id.to_string(),
            text.to_string(),
        );
        blocking(move || {
            match c
                .store()
                .store()
                .items()
                .get(&id)
                .map_err(|e| e.to_string())?
            {
                Some(i) if i.workspace_id == ws => {}
                _ => return Err(crate::api::Noun::Item.missing()),
            }
            c.supplement(&id, &text, mode, &[], "steve")
                .map_err(|e| e.to_string())
        })
        .await
    }
}

/// The modules, their `/api` routes (still needing the `Caller` layer) and their background jobs.
pub struct Built {
    pub plugins: crate::plugins::Plugins,
    pub modules: Modules,
    pub flow: Arc<steve_flow::FlowCx>,
    /// The knowledge module's state, for its MCP tools.
    pub kb: steve_knowledge::Knowledge,
    pub routes: Router,
    /// Routes at the root that read the session but don't require it (the OAuth connect callback).
    pub root_routes: Router,
    /// The wired event entry points, for tests that drive a producer end to end.
    pub seams: Seams,
    /// What the modules' MCP tools run on.
    pub tools: Tools,
    pub jobs: Vec<Job>,
    /// What runs for each connection of an adapter with a hook (started by one of `jobs`).
    pub lifecycle: Option<steve_channels::lifecycle::ConnectionRuntime>,
    /// 能力, the capabilities library: the remote agents' tools ([`super::mcp_extra`]).
    pub caps: steve_modules::caps::Capabilities,
    /// The Hub's connections to the library's MCP servers ([`super::tool_registry`]).
    pub library: Arc<super::library_mcp::LibraryMcp>,
}

/// The objects the event producers call, exactly as wired: G2's sink, E1's events, C1's inbound.
#[derive(Clone)]
pub struct Seams {
    pub flow: Arc<steve_flow::FlowCx>,
    pub github: Arc<dyn steve_github::events::EventSink>,
    pub mail: Arc<dyn steve_mail::MailEvents>,
    pub channels: steve_channels::Channels,
}

/// The module services behind the agents' MCP tools (vault, GitHub, mail, flow) and the core for
/// their approval effects.
#[derive(Clone)]
pub struct Tools {
    pub uses: steve_modules::vault::SecretUses,
    pub gh: steve_github::GitHub,
    pub mail: steve_mail::Mail,
    pub calendar: steve_calendar::Calendar,
    pub flow: Arc<steve_flow::FlowCx>,
    pub core: Core,
    pub channels: steve_channels::Channels,
    pub outbox: Arc<steve_channels::Outbox>,
}

/// The knowledge state: `{type:"kb"}` events on the bus, and loaders for uploads and mail threads
/// (notes and folders are built in).
pub(crate) fn knowledge(
    steve_home: &Path,
    data_dir: &Path,
    bus: &crate::bus::Bus,
) -> steve_knowledge::Knowledge {
    let kb_bus = bus.clone();
    let kb = steve_knowledge::Knowledge::with_loaders(
        move |e: steve_knowledge::KbEvent| {
            kb_bus.publish(steve_types::DomainEvent::from(
                steve_types::KnowledgeEvent::Kb {
                    id: e.id,
                    ws: e.ws.into(),
                },
            ));
        },
        vec![
            // E6: mail threads added to the knowledge base are `files` documents too, so this
            // loader is asked first (it accepts only thread documents)
            Arc::new(steve_mail::knowledge::ThreadLoader),
            Arc::new(steve_knowledge::FilesLoader::new(data_dir)),
        ],
    );
    kb.set_folder_policy(steve_knowledge::FolderPolicy::new(
        crate::config::home_dir(),
        steve_home,
    ));
    kb
}

/// What `build` needs from the rest of the Hub.
#[derive(Clone)]
pub struct Env<'a> {
    pub store: &'a Store,
    pub core: &'a Core,
    pub backend: crate::wire::store::StoreBackend,
    pub steve_home: &'a Path,
    pub data_dir: &'a Path,
    pub public_urls: &'a [String],
    pub bus: &'a crate::bus::Bus,
    pub vars: &'a super::Vars,
    /// The OpenViking server knowledge is stored in (`None`: the Hub's own index only).
    pub openviking: Option<steve_knowledge::openviking::OpenVikingConfig>,
    /// The extensions whose adapters, routes and jobs are registered.
    pub assembly: &'a crate::extension::Assembly,
    /// The router, when the local Node is on: knowledge folders are then scanned and read by
    /// their Node (`None`: on the Hub, as before).
    pub workspaces: Option<Arc<super::workspaces::Workspaces>>,
    /// Where `cmd` flow steps run when the project is on a Node (`None`: on the Hub, as before).
    pub cmd_node: Option<Arc<dyn steve_flow::nodes::cmd::CmdNode>>,
    /// Where a session's secret uses run when it works on a Node (`None`: on the Hub, as before).
    pub use_nodes: Option<Arc<dyn steve_modules::vault::UseNodes>>,
    /// Where GitHub reads and pushes an item's branch when it is on a Node (`None`: the Hub's own
    /// git, as before).
    pub git_nodes: Option<Arc<dyn steve_github::push::GitNodes>>,
    /// Whether a schedule's principal and Bot can run ([`super::preflight::Preflight`]);
    /// `None`: not asked.
    pub preflight: Option<Arc<super::preflight::Preflight>>,
}

/// Registers every module, builds [`Modules`] and the flow context, and collects the routes.
pub fn build(env: Env<'_>) -> Result<Built, BackendError> {
    let mut registry = crate::wire::install::domain::ModuleRegistry::new();
    registry.install(&env)?;
    Ok(registry.finish())
}
