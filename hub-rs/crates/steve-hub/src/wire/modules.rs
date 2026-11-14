//! The new modules (steve-modules B0/B1, contacts, accounts, vault, steve-flow) on top of steve-core:
//! the registry, the module gateway, `steve_flow::Core` over the core, and the routes to merge under `/api`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use serde_json::Value;
use steve_core::{Core, ItemAction};
use steve_modules::{Modules, Registry};
use steve_store::Store;

use crate::backend::BackendError;

/// A background job to spawn once the Hub is up.
pub type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What schedules need from steve-core.
pub struct FlowCore {
    pub core: Core,
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
    async fn create_item(&self, req: steve_flow::ItemRequest) -> Result<String, String> {
        let c = self.core.clone();
        blocking(move || {
            let item = c
                .create_item(steve_core::CreateItem {
                    ws: req.ws,
                    goal: req.goal,
                    title: req.title,
                    bot: Some(req.bot),
                    project: req.project.map(Some),
                    source: Some(req.source),
                    parent: None,
                    tag_ids: req.tag_ids,
                    propose: req.propose,
                    opening: None,
                    files: vec![],
                    actor: Some("steve".into()),
                    requester: None,
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
            c.supplement(&id, &text, mode, &[])
                .map_err(|e| e.to_string())
        })
        .await
    }
}

/// The modules, their `/api` routes (still needing the `Caller` layer) and their background jobs.
pub struct Built {
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
    /// Recalls a channel message (YOLO's `recall` undo).
    pub recall: Arc<dyn steve_core::yolo::undo::Recall>,
    /// What runs for each connection of an adapter with a hook (started by one of `jobs`).
    pub lifecycle: Option<steve_channels::lifecycle::ConnectionRuntime>,
    /// 能力, the capabilities library: the MCP secrets a session's Node takes
    /// ([`super::bot_mcp`]) and the remote agents' tools ([`super::mcp_extra`]).
    pub caps: steve_modules::caps::Capabilities,
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
    pub flow: Arc<steve_flow::FlowCx>,
    pub core: Core,
    pub channels: steve_channels::Channels,
    pub outbox: Arc<steve_channels::Outbox>,
}

/// The knowledge state: `{type:"kb"}` events on the bus, and loaders for uploads, item and chat documents (notes and folders are built in).
fn knowledge(
    steve_home: &Path,
    data_dir: &Path,
    bus: &crate::bus::Bus,
) -> steve_knowledge::Knowledge {
    use steve_knowledge::sources::{chat::ChatLoader, items::ItemsLoader};
    let kb_bus = bus.clone();
    let kb = steve_knowledge::Knowledge::with_loaders(
        move |e: steve_knowledge::KbEvent| {
            kb_bus.publish_value(serde_json::json!({ "type": "kb", "id": e.id, "ws": e.ws }));
        },
        vec![
            // E6: mail threads added to the knowledge base are `files` documents too, so this
            // loader is asked first (it accepts only thread documents)
            Arc::new(steve_mail::knowledge::ThreadLoader),
            Arc::new(steve_knowledge::FilesLoader::new(data_dir)),
            Arc::new(ItemsLoader),
            Arc::new(ChatLoader),
        ],
    );
    kb.set_folder_policy(steve_knowledge::FolderPolicy::new(
        crate::config::home_dir(),
        steve_home,
    ));
    kb
}

/// What `build` needs from the rest of the Hub.
pub struct Env<'a> {
    pub store: &'a Store,
    pub core: &'a Core,
    pub backend: Arc<dyn crate::backend::Backend>,
    pub steve_home: &'a Path,
    pub data_dir: &'a Path,
    pub public_urls: &'a [String],
    pub bus: &'a crate::bus::Bus,
    pub vars: &'a super::Vars,
    /// Why YOLO is off by force, if it is (the `yolo` draft target refuses to switch it on).
    pub yolo_forced: Option<steve_core::yolo::Forced>,
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
}

/// Registers every module, builds [`Modules`] and the flow context, and collects the routes.
pub fn build(env: Env<'_>) -> Result<Built, BackendError> {
    let Env {
        store,
        core,
        backend,
        steve_home,
        data_dir,
        public_urls,
        bus,
        vars,
        yolo_forced,
        openviking,
        assembly,
        workspaces,
        cmd_node,
        use_nodes,
        git_nodes,
    } = env;
    use super::ports as hp;
    let mut registry = Registry::new();
    // one Ports for the contacts routes and for the registrations that need it
    let mut ports = steve_modules::accounts::ports();
    steve_modules::accounts::register(&mut registry);
    steve_modules::contacts::changes::register(&mut registry, &ports);
    steve_modules::contacts::merge::register(&mut registry, &ports);
    let setup = steve_flow::FlowSetup::new(Arc::new(FlowCore { core: core.clone() }));
    setup.register(&mut registry);
    // A2: the `connection` decision (re-authorize)
    steve_modules::accounts::connections::register(&mut registry);
    // C1: `space:<id>` visibility scopes of the spaces a viewer sees
    registry.viewer_scopes(Box::new(steve_channels::spaces::SpaceScopes));
    // C2 + S10: personas and flow nodes follow a deleted Bot
    registry.bot_deleted(Box::new(hp::BotHeirs));
    // S10: item events and the report notes, bound to the flow once it exists
    let item_events = hp::FlowItemEvents::default();
    registry.item_hooks(Box::new(item_events.clone()));
    let digest = hp::FlowDigest::default();
    registry.digest_notes(Box::new(digest.clone()));
    // YOLO: its line in `/汇报` and the `yolo` draft target (“开 YOLO，前端项目除外”)
    registry.digest_notes(Box::new(steve_core::yolo::digest::Notes {
        store: store.clone(),
    }));
    steve_core::yolo::draft::register(&mut registry, yolo_forced);
    // knowledge (K1–K5): candidate decisions, item documents on finish, prompt context, drafts
    let kb = knowledge(steve_home, data_dir, bus);
    if let Some(w) = workspaces {
        kb.set_folder_places(Arc::new(super::workspaces::KbFolders(w)));
    }
    // one knowledge store for every reader (knowledge tools, prompt context, mail and channel replies)
    // OpenViking (user decision 2026-11-04): keywords from the Hub's index, meaning from it; the
    // index is mirrored to it through the outbox (`kb.openviking`)
    let ov = openviking.map(steve_knowledge::openviking::OpenViking::new);
    let kstore: Arc<dyn steve_knowledge::port::KnowledgeStore> = match &ov {
        Some(ov) => Arc::new(steve_knowledge::openviking::Hybrid {
            keywords: steve_knowledge::port::SqliteStore,
            meaning: ov.clone(),
        }),
        None => Arc::new(steve_knowledge::port::SqliteStore),
    };
    kb.set_store(kstore.clone());
    let kb_hooks = steve_knowledge::sources::items::KnowledgeHooks::new(kb.clone());
    registry.decision(
        steve_knowledge::candidates::SUBJECT,
        Box::new(steve_knowledge::candidates::MemDecisions),
    );
    registry.item_hooks(Box::new(kb_hooks.clone()));
    registry.prompt_context(steve_knowledge::context::provider(
        store.clone(),
        kb.clone(),
    ));
    steve_knowledge::drafts::register(&mut registry, kb.clone());
    // V4: the vault (no Modules needed) and its `secret_use` decisions
    let keys = steve_modules::vault::keystore::from_env(steve_home, |k| vars(k))
        .map_err(|e| BackendError::msg(format!("vault keystore: {e}")))?;
    let vault = steve_modules::vault::Vault::new(store.clone(), keys);
    // what an interrupted rotation of an earlier Hub left `retiring` is finished once, off the start
    {
        let v = vault.clone();
        std::thread::spawn(move || {
            if let Err(e) = v.tidy_keys() {
                tracing::warn!("vault: tidy keys: {e}");
            }
        });
    }
    let uses = steve_modules::vault::SecretUses::new(
        vault.clone(),
        steve_home,
        Arc::new(hp::CoreSessions(core.clone())),
    );
    if let Some(n) = use_nodes {
        uses.set_nodes(n);
    }
    registry.decision(
        steve_modules::vault::r#use::SUBJECT,
        uses.decision_handler(),
    );
    // E3: mail drafts' approval; E4/E5: mail hooks and the item panel
    let draft_link = steve_mail::DraftLink::new();
    registry.decision(steve_mail::drafts::SUBJECT_TYPE, draft_link.handler());
    let mail_hooks = steve_mail::nodes::MailHooks::new();
    registry.item_hooks(mail_hooks.boxed());
    registry.item_panel(Box::new(steve_mail::items::MailPanel));
    // G3/G4/G5: PR open/merge and comment decisions, the wrap-up hook, the GitHub panels, the
    // `gh_repo` draft target
    let gh_slot = steve_github::accept::register(&mut registry);
    // E6: the `mail_account` draft target
    steve_mail::config::targets::register(&mut registry);
    // C8: the `space`, `persona` and `channel` draft targets; capi: the “新群” card
    steve_channels::drafts::register(&mut registry, Arc::new(steve_modules::SystemClock));
    steve_channels::space_decisions::register(&mut registry);
    // Feishu: the test base and host guard (STEVE_FEISHU_API_BASE, STEVE_FORBID_REAL_HOSTS),
    // whichever extensions are assembled (the directory sync below talks to Feishu too)
    use steve_channels::adapters::feishu;
    feishu::FeishuEnv::from_env(|k| vars(k)).install();
    // the extensions' adapters (built in: Feishu as the bot and as you, IMAP, GitHub); nothing
    // here talks to a provider
    let declared = assembly
        .declare(vars, ports.hooks.clone())
        .map_err(|e| BackendError::msg(e.to_string()))?;
    // C5/C6: the disclosure check (B1's outbound check), the `chan_message` approvals, the outbox
    // (sends as you through the declared adapters), back to the source when an item is done
    let disclosure = steve_channels::Disclosure::new();
    registry.outbound_check(Box::new(disclosure.clone()));
    let out_link = steve_channels::OutLink::new();
    registry.decision(steve_channels::respond::SUBJECT_MESSAGE, out_link.handler());
    let late = steve_channels::flow_actions::LateChannels::new();
    let mut outbox = steve_channels::Outbox::new(out_link.clone());
    for a in &declared.act_as_user {
        outbox = outbox.act_as_user(a.clone());
    }
    let outbox = Arc::new(outbox);
    registry.item_hooks(Box::new(steve_channels::outbound::ReturnToSource::new(
        outbox.clone(),
        late.clone(),
    )));

    let modules = Modules::new(store.clone(), core.module_gateway(), registry);
    // the handle systems the extensions declared, there before anything names a sender in one
    if !declared.systems.is_empty() {
        store
            .transaction(|tx| -> steve_store::Result<()> {
                for (id, label, ext) in &declared.systems {
                    steve_modules::accounts::repo::upsert_declared(tx, id, label, ext)?;
                }
                Ok(())
            })
            .map_err(|e| BackendError::msg(format!("declared handle systems: {e}")))?;
    }
    kb_hooks.bind(modules.clone());
    // an item's knowledge document goes in the transaction that deletes the item
    store.on_item_deleted(|tx, ws, id| {
        steve_knowledge::sources::items::item_deleted(tx, ws, id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    // B1: the core calls the modules' hooks, prompt contexts and decision handlers from here on
    core.attach_modules(modules.clone());
    uses.start();
    disclosure.bind(modules.store.clone(), modules.gateway.clone());
    out_link.bind_clock(modules.clock.clone());

    // V2 items, revealed after a V3 passkey check (one relying party per public origin)
    let passkeys = steve_modules::vault::webauthn::Passkeys::new(store.clone(), public_urls);
    let vault_items =
        steve_modules::vault::items::routes(vault.clone(), Arc::new(passkeys.clone()));
    // One Connections for every declared adapter (built in: GitHub (G1), Feishu bot and user
    // (F1/F6), IMAP (E1)). Nothing here talks to a provider: long connections, pollers and mail
    // runners only start for connections that exist.
    use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
    let mail_cfg = steve_mail::MailConfig::from_env(|k| vars(k))
        .map_err(|e| BackendError::msg(format!("mail config: {e}")))?;
    // the same GitHub env as the `github` extension's adapters (`github_pat` honours
    // STEVE_GITHUB_API_BASE)
    let gh_env = steve_github::GitHubEnv::from_env(|k| vars(k));
    let mut conns = Connections::builder(
        modules.clone(),
        vault.clone(),
        ConnectionsConfig::from_env({
            let vars = vars.clone();
            move |k| vars(k)
        }),
    )
    .usage(Arc::new(steve_mail::usage::MailUsage::new(
        mail_cfg.clone(),
    )))
    // spaces outlive their connection (kept as left; their settings and cursors go)
    .usage(Arc::new(steve_channels::usage::ChannelUsage))
    // a claim into 我 moves the merged contact's accounts and channel members along
    .party_hooks(ports.hooks.clone());
    for p in &declared.providers {
        conns = conns.provider(p.clone());
    }
    // what each channel side declares, for the 接入 page
    for d in declared.decls() {
        conns = conns.channel(
            &d.adapter,
            steve_channels::channel_accounts::decl_summary(d),
        );
    }
    let conns = conns.build();
    // a connection stored under another shape than its provider's now waits for a new
    // authorization instead of running on it
    if conns.check_versions().is_err() {
        tracing::error!("could not check the stored connection versions");
    }
    let gh = steve_github::GitHub::new(conns.clone(), gh_env);
    gh_slot.bind(&gh);
    if let Some(n) = git_nodes {
        gh.set_git_nodes(n);
    }
    gh.set_core(Arc::new(hp::GhItems(core.clone())));
    gh.set_session_sink(Arc::new(hp::CoreSessions(core.clone())));

    // E1/E3/E4/E5: mail, before the flow (its triage actions hold it); events bound after
    let mail_events = steve_mail::flow_events::FlowEvents::new();
    let notices = super::signals::Notices { bus: bus.clone() };
    let mut mail = steve_mail::Mail::builder(modules.clone(), conns.clone(), mail_cfg);
    for a in &declared.mail {
        mail = mail.adapter(a.clone());
    }
    let mail = mail
        .events(Arc::new(mail_events.clone()))
        .notify(Arc::new(notices.clone()))
        .drafts(draft_link)
        .hooks(Arc::new(hp::MailDraftHooks(core.clone())))
        .files(Arc::new(hp::HubFiles {
            backend,
            data_dir: data_dir.to_path_buf(),
        }))
        .items(Arc::new(hp::MailItems(core.clone())))
        .knowledge(kb.clone())
        .build();
    mail_hooks.bind(&mail);

    // S1–S4, S10: the flow engine with GitHub as an event source, the code nodes (http with
    // `{{secrets}}`, cmd), the `bot` node, mail actions, GitHub's `item.link`
    let mut setup = setup
        .connections(conns.clone())
        .node(Arc::new(steve_flow::nodes::http::Http::new(vault.clone())))
        .node(Arc::new({
            let cmd = steve_flow::nodes::cmd::Cmd::new(steve_home);
            match cmd_node {
                Some(n) => cmd.with_node(n),
                None => cmd,
            }
        }))
        .node(Arc::new(steve_flow::nodes::bot::BotNode));
    for a in &declared.event_sources {
        setup = setup.event_adapter(a.clone());
    }
    // C4: replies (sent through C6's outbox) and the `reply_as` / `send_to_space` actions
    let replies = Arc::new(
        steve_channels::Replies::new()
            .outbound(outbox.clone())
            .composer(
                steve_channels::compose::Composer::new()
                    .media(Arc::new(steve_channels::media_agent::FeishuMediaSource)),
            ),
    );
    let setup = setup
        .action(Arc::new(steve_channels::flow_actions::ReplyAs::new(
            replies.clone(),
            late.clone(),
        )))
        .action(Arc::new(steve_channels::flow_actions::SendToSpace::new(
            replies.clone(),
            late.clone(),
        )));
    let setup = steve_mail::nodes::register(setup, &mail)
        // after mail's: `item.link` with mail links and `by:"gh_pr"`
        .action(Arc::new(steve_mail::nodes::WithMailLinks(
            steve_github::flow::GhItemLink,
        )))
        .item_finder("gh_pr", Arc::new(steve_github::flow::GhPrFinder));
    let flow = setup.build(modules.clone(), Arc::new(steve_flow::GraphEngine));
    mail_events.bind(&flow);
    item_events.bind(flow.clone());
    let _ = digest.0.set(flow.clone());
    let sink: Arc<dyn steve_flow::EventSink> = Arc::new(flow.clone());

    // G2/G3: GitHub signals through S1, the “PR 检查失败” template, `{type:"gh"}` notices
    let gh_sink: Arc<dyn steve_github::events::EventSink> =
        Arc::new(steve_github::flow::FlowSink(sink.clone()));
    gh.set_event_sink(gh_sink.clone());
    gh.set_flow(flow.clone());
    gh.set_notifier(Arc::new(notices));
    drop(gh.spawn_jobs());
    // approved GitHub writes: running ones a restart cut off become unknown first
    drop(gh.spawn_effects());
    drop(mail.spawn_jobs());
    drop(mail.spawn_send_queue());

    // C1/C3: channels on the Feishu bot; F5: its address book feeds the directory sync
    let directory = steve_modules::contacts::sync::TableConnections::new().with(
        feishu::directory::ADAPTER,
        feishu::directory::NAME,
        feishu::directory::factory(conns.clone()),
    );
    let directory =
        steve_modules::contacts::sync::DirectorySync::new(modules.clone(), Arc::new(directory));
    drop(directory.spawn());
    // connected, re-verified, a permission granted: sync the address book again soon
    drop(directory.spawn_watch(bus.tap(&["connection"])));
    let mut channels = steve_channels::Channels::builder(modules.clone(), conns.clone(), sink);
    for a in &declared.channel_adapters {
        channels = channels.adapter(a.clone());
    }
    // the identities that look people up, keyed by adapter (which goes first is the names'
    // rule, not the order they come in)
    for p in &declared.profiles {
        channels = channels.profiles(p.clone());
    }
    // how the images and files of each adapter's messages are fetched
    for m in &declared.media {
        channels = channels.media(m.clone());
    }
    // how an agent reads each adapter's conversations on demand (`steve_im_read`)
    for h in &declared.history {
        channels = channels.history(h.clone());
    }
    // how each channel side's messages are told apart (provider ids, time precision, senders)
    for d in declared.decls() {
        channels = channels.decl(d.clone());
    }
    let channels = channels
        .party_hooks(ports.hooks.clone())
        .flow(flow.clone())
        .responder(replies)
        .other_events(Arc::new(super::signals::DirectoryEvents(directory.clone())))
        // “收到 → 完成” reactions, as the bot or (your `/bot`) as you
        .reactor(outbox.clone())
        // IM spaces converse through steve-core's thread (channels §2.12)
        .thread(Arc::new(super::chan::CoreThread(core.clone())))
        .yolo(Arc::new(super::chan::ChanYolo(core.clone())))
        .build();
    channels.set_knowledge_store(kstore.clone());
    late.set(&channels);
    core.set_conv_host(Arc::new(super::chan::ChanConvHost(channels.downgrade())));
    drop(outbox.clone().spawn_queue(&channels));

    // contacts ports filled by the other modules, before the contacts routes take them
    ports.sync = Some(directory);
    ports.spaces = Some(Arc::new(steve_channels::spaces::Spaces));
    ports
        .timelines
        .push(steve_channels::messages::MessageTimeline::arc());
    ports
        .timelines
        .push(Arc::new(steve_mail::timeline::MailTimeline));

    // the extensions' step two: their routes (merged with the modules' below) and jobs
    let started = crate::extension::Started::new(&channels, bus, vars);
    let routes = Router::new()
        .merge(steve_knowledge::routes(kb.clone()))
        .merge(steve_digest::routes())
        .merge(steve_modules::contacts::routes(ports))
        .merge(steve_modules::accounts::routes())
        .merge(steve_flow::routes(flow.clone()))
        .merge(connections::routes(conns.clone()))
        // the two GETs are browser navigations: `crate::api` lets them through without a session
        .merge(steve_github::manifest::routes(gh.clone()))
        .merge(steve_github::repos::routes(gh.clone()))
        .merge(steve_github::comment::routes(gh.clone()))
        .merge(steve_github::issues::routes(gh.clone()))
        .merge(steve_mail::api::routes(mail.clone()))
        .merge(steve_channels::api::routes(channels.clone()))
        .merge(steve_channels::channels_api::routes(channels.clone()))
        .merge(super::chan::routes(channels.clone(), outbox.clone()));
    let crate::extension::StartedAll {
        routes,
        jobs: ext_jobs,
        hooks,
    } = assembly
        .start(&started, &declared, routes)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    // what runs for each connection of an adapter with a hook: the events are tapped now, before
    // the runtime lists the connections
    let lifecycle = (!hooks.is_empty()).then(|| {
        let mut rt = steve_channels::lifecycle::ConnectionRuntime::builder(channels.clone());
        for h in hooks {
            rt = rt.hook(&h.adapter, h.decl, h.hook);
        }
        (rt.build(), bus.tap(&["connection"]))
    });
    // 能力: MCP servers, skills and remote agents, defined once and assigned per Bot; the
    // secrets in the vault
    let caps = steve_modules::caps::Capabilities::new(vault.clone());
    let routes = routes
        .with_state(modules.clone())
        .merge(steve_modules::caps::routes(caps.clone()))
        .merge(steve_modules::vault::routes(vault.clone()))
        .merge(steve_modules::vault::use_api::routes(vault))
        .merge(vault_items)
        .merge(steve_modules::vault::webauthn::routes(passkeys));
    // at the root: the OAuth callback (session read, not required) and the webhooks (no session)
    let root_routes = connections::callback_routes(conns.clone())
        .with_state(modules.clone())
        .merge(steve_github::hooks::routes(gh.clone()))
        .merge(super::signals::routes(super::signals::Hooks::new(
            flow.clone(),
            gh.clone(),
        )));
    // `vault.token_refresh` (5 min) and `vault.health` (15 min)
    drop(conns.spawn_jobs());
    // registered now, before boot resumes sessions or the API listens: no finish happens earlier
    let mut module_consumers: Vec<Arc<dyn steve_store::outbox::Consumer>> = vec![
        steve_mail::outbox::MailEventsOut::new(mail.clone()),
        gh.outbox_consumer(),
    ];
    let mirror = steve_knowledge::openviking::Mirror::ID;
    let mirrored_before = store
        .reader(|c| steve_store::outbox::is_registered(c, mirror))
        .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
    match &ov {
        Some(ov) => module_consumers.push(Arc::new(steve_knowledge::openviking::Mirror {
            ov: ov.clone(),
            store: store.clone(),
        })),
        // no longer mirrored: what it owed is dropped; switching back on mirrors everything again
        None if mirrored_before => {
            steve_store::outbox::unregister(store, mirror)
                .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
            tracing::info!("knowledge: no longer mirrored to OpenViking");
        }
        None => {}
    }
    let dispatcher = super::outbox::dispatcher(store, core, &flow, module_consumers)
        .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
    // the first time: everything indexed before is mirrored too
    if ov.is_some() && !mirrored_before {
        let n = steve_knowledge::openviking::backfill(store)
            .map_err(|e| BackendError::msg(format!("knowledge backfill: {e}")))?;
        tracing::info!(objects = n, "knowledge: mirroring the index to OpenViking");
    }
    let mut jobs: Vec<Job> = vec![
        Box::pin(super::outbox::run(dispatcher, store.clone(), flow.clone())),
        Box::pin(steve_flow::Scheduler::new(flow.clone()).run()),
        Box::pin(steve_flow::Poller::new(flow.clone()).run()),
        Box::pin(steve_flow::retention::run(flow.clone())),
        Box::pin(steve_channels::messages::retention(modules.clone())),
    ];
    // the extensions' (built in: Feishu app registrations resumed) and the connection runtime
    // (built in: the Feishu long connections and self pollers)
    jobs.extend(ext_jobs);
    let lifecycle = lifecycle.map(|(rt, events)| {
        let run = rt.clone();
        jobs.push(Box::pin(async move {
            let _ = run.start(events).await;
        }));
        rt
    });
    let seams = Seams {
        flow: flow.clone(),
        github: gh_sink,
        mail: Arc::new(mail_events),
        channels: channels.clone(),
    };
    let tools = Tools {
        uses,
        gh,
        mail,
        flow: flow.clone(),
        core: core.clone(),
        channels: channels.clone(),
        outbox: outbox.clone(),
    };
    // new mailboxes get the triage template
    jobs.push(Box::pin(super::signals::watch_mailboxes(
        bus.tap(&["connection"]),
        flow.clone(),
        tools.mail.clone(),
    )));
    // the index queue and the memory sweeper start now (they spawn themselves)
    drop(steve_knowledge::index::spawn_worker(
        modules.clone(),
        kb.clone(),
    ));
    drop(steve_knowledge::sync::spawn_sweeper(modules.clone()));
    // folder scans, the 30-day candidate expiry and the main-thread chat documents
    drop(steve_knowledge::spawn_scans(modules.clone(), kb.clone()));
    drop(steve_knowledge::candidates::spawn_expirer(modules.clone()));
    drop(steve_knowledge::sources::chat::spawn_job(
        modules.clone(),
        kb.clone(),
    ));
    // Intake only: phase D will consume the durable packets; no Bot starts here.
    drop(steve_digest::job::spawn_job(modules.clone()));
    // memories and Bot memories are re-indexed when the store says they changed
    let mut changes = bus.tap(&["mm", "bot"]);
    let m = modules.clone();
    jobs.push(Box::pin(async move {
        while let Some(v) = changes.recv().await {
            let s = |k: &str| {
                v.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let e = match s("type").as_str() {
                "mm" => steve_store::ChangeEvent::Mm {
                    id: s("id"),
                    ws: s("ws"),
                },
                _ => steve_store::ChangeEvent::Bot { id: s("id") },
            };
            let m = m.clone();
            match tokio::task::spawn_blocking(move || steve_knowledge::sync::on_event(&m, &e)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("kb sync: {e}"),
                Err(e) => tracing::warn!("kb sync: {e}"),
            }
        }
    }));
    Ok(Built {
        modules,
        flow,
        kb,
        routes,
        root_routes,
        seams,
        tools,
        jobs,
        recall: Arc::new(super::chan::ChanRecall {
            ch: channels.clone(),
            outbox: outbox.clone(),
        }),
        lifecycle,
        caps,
    })
}
