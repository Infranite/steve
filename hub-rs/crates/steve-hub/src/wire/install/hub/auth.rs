use super::*;

install! { prepare(reg, cx) [db] => [auth_cfg, backend, db, yolo_forced] {
    let HubEnv { cfg, vars, .. } = *cx;
    let auth_cfg = steve_auth::AuthConfig::from_vars(|k| vars(k))
        .map_err(|e| BackendError::msg(format!("auth config: {e}")))?;
    let backend: Arc<dyn Backend> = Arc::new(store::StoreBackend::new(db.clone())?);
    // YOLO is off by force on a production copy (STEVE_NO_RESUME=1) and with STEVE_YOLO=off
    let yolo_forced =
        steve_core::yolo::Forced::from_env(vars("STEVE_YOLO").as_deref(), cfg.diagnostic());
}}

install! { install(reg, cx) [auth_cfg, db] => [auth, db] {
    let HubEnv { cfg, .. } = *cx;
    let auth = steve_auth::Auth::new(crate::wire::auth::AuthStore(db.clone()), auth_cfg)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    /* single sign-on set up in the settings (A4b), unless the environment configures it */
    if !auth.oidc_enabled()
        && let Err(e) = crate::access::restore_oidc(&db, &cfg.home, &auth)
    {
        tracing::warn!("single sign-on from the settings: {e}");
    }
}}
