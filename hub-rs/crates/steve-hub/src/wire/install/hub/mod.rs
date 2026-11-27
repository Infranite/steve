//! Hub installation order: storage -> host -> runtime -> core -> modules -> consumers.
use crate::wire::modules::Job;
use crate::wire::*;

// An installer is an ordinary function pointer; the macro only moves typed input/output slots.
macro_rules! install {
    ($name:ident($reg:ident, $cx:ident) [$($input:tt)*] => [$($output:ident),*] { $($body:tt)* }) => {
        pub(super) fn $name<'a>($reg: &mut HubRegistry, $cx: &HubEnv<'a>) -> Result<(), BackendError> {
            take!($reg; $($input)*);
            $($body)*
            put!($reg; $($output),*);
            Ok(())
        }
    };
}

ordered_installers! { MODULES: fn(&mut HubRegistry, &HubEnv<'_>) -> Result<(), BackendError>;
    mod storage; install,
    mod host; install,
    mod runtime; install,
    mod core; install,
    mod research; install,
    mod auth; prepare,
    mod modules; install,
    auth::install,
    mod mcp; install,
    mod stats; install,
    mod backup; install,
    mod admin; install,
    mod retention; install,
    stats::routes,
    mod handles; install,
}

#[derive(Clone, Copy)]
pub(crate) struct HubEnv<'a> {
    pub cfg: &'a Arc<Config>,
    pub bus: &'a Bus,
    pub vars: &'a Vars,
    pub asm: &'a crate::extension::Assembly,
}

#[derive(Default)]
pub(crate) struct HubRegistry {
    db: Option<steve_store::Store>,
    host: Option<Arc<crate::wire::core::HubHost>>,
    core_cfg: Option<steve_core::CoreConfig>,
    node_jobs: Option<Vec<crate::wire::modules::Job>>,
    secrets: Option<Arc<node_secrets::NodeSecrets>>,
    mode: Option<Option<NodeMode>>,
    harnesses: Option<Arc<crate::harnesses::HubHarnesses>>,
    gateway: Option<Arc<dyn steve_core::ModelGateway>>,
    nodes: Option<Option<Arc<crate::nodes::NodeService>>>,
    fleet: Option<Option<Arc<crate::nodes::NodeFleet>>>,
    node_host: Option<Option<SessionHost>>,
    workspaces: Option<Arc<workspaces::Workspaces>>,
    preflight: Option<Arc<preflight::Preflight>>,
    routed: Option<Arc<crate::wire::core::RoutedRuntime>>,
    core: Option<steve_core::Core>,
    auth_cfg: Option<steve_auth::AuthConfig>,
    backend: Option<store::StoreBackend>,
    built: Option<crate::wire::modules::Built>,
    jobs: Option<Vec<crate::wire::modules::Job>>,
    auth: Option<steve_auth::Auth<crate::wire::auth::AuthStore>>,
    tool_service: Option<Arc<tool_service::ToolService>>,
    local_runtime: Option<Option<Arc<node_runtime::NodeRuntime>>>,
    mcp: Option<Router>,
    module_routes: Option<Router>,
    price_url: Option<Option<String>>,
    wired: Option<Wired>,
}
impl HubRegistry {
    pub(crate) fn install(&mut self, cx: &HubEnv<'_>) -> Result<(), BackendError> {
        for install in MODULES {
            install(self, cx)?;
        }
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Wired {
        self.wired.take().expect("hub handles installed")
    }
}
