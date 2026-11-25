use super::*;

install! { prepare(reg, cx) [db] => [auth_cfg, backend, db] {
    let HubEnv { vars, .. } = *cx;
    let auth_cfg = steve_auth::AuthConfig::from_vars(|k| vars(k))
        .map_err(|e| BackendError::msg(format!("auth config: {e}")))?;
    let backend = store::StoreBackend(db.clone());
}}

install! { install(reg, cx) [auth_cfg, db] => [auth, db] {
    let HubEnv { cfg, .. } = *cx;
    let auth = steve_auth::Auth::new(crate::wire::auth::AuthStore(db.clone()), auth_cfg)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    /* single sign-on set up in the settings, unless the environment configures it */
    if !auth.oidc_enabled()
        && let Err(e) = crate::access::restore_oidc(&db, &cfg.home, &auth)
    {
        tracing::warn!("single sign-on from the settings: {e}");
    }
}}
