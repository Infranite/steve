use super::*;

install! { prepare(reg, cx) [mut registry] => [caps, library, registry, uses, vault] {
    let Env { store, core, steve_home, data_dir, vars, use_nodes, .. } = cx.clone();
    // V4: the vault (no Modules needed) and its `secret_use` decisions
    let keys = steve_modules::vault::keystore::from_env(steve_home, |k| vars(k))
        .map_err(|e| BackendError::msg(format!("vault keystore: {e}")))?;
    let vault = steve_modules::vault::Vault::new(store.clone(), keys);
    // 能力: MCP servers, skills and remote agents, defined once and assigned per Bot; the
    // secrets in the vault. The Hub is the MCP client of the library's servers (their stdio ones
    // start in the sandbox a Node's runs get), and a session's brief says what its tools cover.
    let caps = steve_modules::caps::Capabilities::new(vault.clone());
    let library = crate::wire::library_mcp::LibraryMcp::new(
        caps.clone(),
        Some(steve_node::Sandbox::open(
            steve_home,
            &data_dir.join("node"),
        )),
    );
    registry.prompt_context(Box::new(crate::wire::tool_service::CatalogContext {
        library: library.clone(),
    }));
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
}}

install! { items(reg, cx) [vault] => [passkeys, vault, vault_items] {
    let Env { store, public_urls, .. } = cx.clone();
    // V2 items, revealed after a V3 passkey check (one relying party per public origin)
    let passkeys = steve_modules::vault::webauthn::Passkeys::new(store.clone(), public_urls);
    let vault_items =
        steve_modules::vault::items::routes(vault.clone(), Arc::new(passkeys.clone()));
}}

install! { routes(reg, _cx) [caps, module_routes, modules, passkeys, vault, vault_items] => [caps, modules, routes] {
    let routes = module_routes
        .with_state(modules.clone())
        .merge(steve_modules::caps::routes(caps.clone()))
        .merge(steve_modules::vault::routes(vault.clone()))
        .merge(steve_modules::vault::use_api::routes(vault))
        .merge(vault_items)
        .merge(steve_modules::vault::webauthn::routes(passkeys));
}}
