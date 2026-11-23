//! Ordered domain installation. Slots are concrete startup dependencies, not runtime services.
use crate::backend::BackendError;
use crate::wire::modules::*;
use crate::wire::modules::{Env, knowledge};
use crate::wire::ports as hp;
use axum::Router;
use std::sync::Arc;
use steve_modules::{Modules, Registry};

// An installer is an ordinary function pointer; the macro only moves typed input/output slots.
macro_rules! install {
    ($name:ident($reg:ident, $cx:ident) [$($input:tt)*] => [$($output:ident),*] { $($body:tt)* }) => {
        pub(super) fn $name<'a>($reg: &mut ModuleRegistry<'a>, $cx: &Env<'a>) -> Result<(), BackendError> {
            take!($reg; $($input)*);
            $($body)*
            put!($reg; $($output),*);
            Ok(())
        }
    };
}

// Entries run in order: hooks precede the gateway; late bindings precede routes and workers.
ordered_installers! { MODULES: Install;
    mod accounts; prepare,
    mod contacts; prepare,
    mod flow; prepare,
    mod connections; prepare,
    mod channels; scopes,
    flow::hooks,
    mod yolo; prepare,
    mod knowledge; prepare,
    mod vault; prepare,
    mod caps; install,
    vault::uses,
    mod mail; prepare,
    mod github; prepare,
    mail::targets,
    channels::prepare,
    mod extensions; prepare,
    channels::outbound,
    mod gateway; install,
    vault::items,
    connections::install,
    github::install,
    mail::install,
    mod calendar; install,
    flow::install,
    github::start,
    mail::start,
    contacts::install,
    channels::install,
    contacts::finish,
    extensions::context,
    mod plugins; install,
    plugins::routes,
    knowledge::routes,
    mod digest; routes,
    mod feed; routes,
    mod attention; routes,
    mod digest_bot; routes,
    contacts::routes,
    accounts::routes,
    flow::routes,
    connections::routes,
    github::routes,
    mail::routes,
    calendar::routes,
    channels::routes,
    extensions::mount,
    gateway::routes,
    caps::routes,
    vault::routes,
    connections::root,
    github::root,
    flow::root,
    connections::start,
    mod outbox; install,
    outbox::jobs,
    channels::notifications,
    flow::clocks,
    digest_bot::jobs,
    flow::pollers,
    channels::retention,
    plugins::jobs,
    calendar::jobs,
    extensions::start,
    mod handles; install,
    mail::watch,
    knowledge::start,
    digest::start,
    knowledge::sync,
    handles::finish,
}
type Install = for<'a> fn(&mut ModuleRegistry<'a>, &Env<'a>) -> Result<(), BackendError>;

#[derive(Default)]
pub(crate) struct ModuleRegistry<'a> {
    registry: Option<Registry>,
    ports: Option<steve_modules::contacts::Ports>,
    setup: Option<steve_flow::FlowSetup>,
    item_events: Option<crate::wire::ports::FlowItemEvents>,
    digest: Option<crate::wire::ports::FlowDigest>,
    kb: Option<steve_knowledge::Knowledge>,
    ov: Option<Option<Arc<steve_knowledge::openviking::OpenViking>>>,
    kstore: Option<Arc<dyn steve_knowledge::port::KnowledgeStore>>,
    kb_hooks: Option<steve_knowledge::sources::items::KnowledgeHooks>,
    vault: Option<steve_modules::vault::Vault>,
    caps: Option<steve_modules::caps::Capabilities>,
    library: Option<Arc<crate::wire::library_mcp::LibraryMcp>>,
    uses: Option<steve_modules::vault::SecretUses>,
    draft_link: Option<steve_mail::DraftLink>,
    mail_hooks: Option<steve_mail::nodes::MailHooks>,
    gh_slot: Option<steve_github::accept::Slot>,
    declared: Option<crate::extension::Declare<'a>>,
    started: Option<crate::extension::Started>,
    disclosure: Option<steve_channels::Disclosure>,
    out_link: Option<steve_channels::OutLink>,
    late: Option<Arc<steve_channels::flow_actions::LateChannels>>,
    outbox: Option<Arc<steve_channels::Outbox>>,
    modules: Option<Modules>,
    passkeys: Option<steve_modules::vault::webauthn::Passkeys>,
    vault_items: Option<Router>,
    mail_cfg: Option<steve_mail::MailConfig>,
    gh_env: Option<steve_github::GitHubEnv>,
    conns: Option<steve_modules::accounts::connections::Connections>,
    gh: Option<steve_github::GitHub>,
    mail_events: Option<steve_mail::flow_events::FlowEvents>,
    mail: Option<steve_mail::Mail>,
    calendar: Option<steve_calendar::Calendar>,
    replies: Option<Arc<steve_channels::Replies>>,
    flow: Option<Arc<steve_flow::FlowCx>>,
    sink: Option<Arc<dyn steve_flow::EventSink>>,
    gh_sink: Option<Arc<dyn steve_github::events::EventSink>>,
    directory: Option<Arc<steve_modules::contacts::sync::DirectorySync>>,
    channels: Option<steve_channels::Channels>,
    plugins: Option<crate::plugins::Plugins>,
    module_routes: Option<Router<Modules>>,
    ext_jobs: Option<Vec<Job>>,
    pending_lifecycle: Option<
        Option<(
            steve_channels::lifecycle::ConnectionRuntime,
            tokio::sync::mpsc::Receiver<steve_types::Notification>,
        )>,
    >,
    routes: Option<Router>,
    root_routes: Option<Router>,
    dispatcher: Option<Arc<steve_store::outbox::Dispatcher>>,
    jobs: Option<Vec<Job>>,
    lifecycle: Option<Option<steve_channels::lifecycle::ConnectionRuntime>>,
    seams: Option<Seams>,
    tools: Option<Tools>,
    built: Option<Built>,
}

impl<'a> ModuleRegistry<'a> {
    pub(crate) fn new() -> Self {
        Self {
            module_routes: Some(Router::new()),
            jobs: Some(Vec::new()),
            ..Self::default()
        }
    }
    pub(crate) fn install(&mut self, cx: &Env<'a>) -> Result<(), BackendError> {
        for install in MODULES {
            install(self, cx)?;
        }
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Built {
        self.built.take().expect("module handles installed")
    }
}
