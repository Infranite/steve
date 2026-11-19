use super::*;
use steve_modules::accounts::connections;

install! { prepare(reg, _cx) [mut registry] => [registry] {
    // A2: the `connection` decision (re-authorize)
    steve_modules::accounts::connections::register(&mut registry);
}}

install! { install(reg, cx) [declared, modules, ports, vault] => [conns, declared, gh_env, mail_cfg, modules, ports, vault] {
    let Env { vars, .. } = cx.clone();
    // One Connections for every declared adapter (built in: GitHub (G1), Feishu bot and user
    // (F1/F6), IMAP (E1)). Nothing here talks to a provider: long connections, pollers and mail
    // runners only start for connections that exist.
    use steve_modules::accounts::connections::{Connections, ConnectionsConfig};
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
}}

install! { routes(reg, _cx) [conns, mut module_routes] => [conns, module_routes] {
    module_routes = module_routes.merge(connections::routes(conns.clone()));
}}

install! { root(reg, _cx) [conns, modules] => [conns, modules, root_routes] {
    // at the root: the OAuth callback reads the session without requiring it
    let root_routes = connections::callback_routes(conns.clone()).with_state(modules.clone());
}}

install! { start(reg, _cx) [conns, mut jobs] => [jobs] {
    // `vault.token_refresh` (5 min) and `vault.health` (15 min)
    {
        let conns = conns;
        jobs.push(Job::spawned("connections.maintenance", move || vec![conns.spawn_jobs()]));
    }
}}
