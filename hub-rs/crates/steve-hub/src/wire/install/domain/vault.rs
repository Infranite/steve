use super::*;

install! { prepare(reg, cx) [] => [vault] {
    let Env { store, steve_home, vars, .. } = cx.clone();
    // V4: the vault (no Modules needed) and its `secret_use` decisions
    let keys = steve_modules::vault::keystore::from_env(steve_home, |k| vars(k))
        .map_err(|e| BackendError::msg(format!("vault keystore: {e}")))?;
    let vault = steve_modules::vault::Vault::new(store.clone(), keys);
}}

install! { uses(reg, cx) [mut registry, vault] => [registry, uses, vault] {
    let Env { core, use_nodes, .. } = cx.clone();
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

install! { routes(reg, _cx) [passkeys, routes, vault, vault_items] => [routes] {
    let routes = routes
        .merge(steve_modules::vault::routes(vault.clone()))
        .merge(steve_modules::vault::use_api::routes(vault))
        .merge(vault_items)
        .merge(steve_modules::vault::webauthn::routes(passkeys));
}}
