use super::*;

install! { install(reg, cx) [] => [db] {
    let HubEnv { cfg, bus, asm, .. } = *cx;
    asm.check().map_err(|e| BackendError::msg(e.to_string()))?;
    let engine = steve_store_sqlite::open::open_runtime(cfg.db_path().parent().ok_or_else(|| BackendError::msg("database directory missing"))?)
        .map_err(|e| BackendError::msg(format!("open {}: {e}", cfg.db_path().display())))?;
    let db = steve_store::Store::from_engine(engine, store::BusPublisher(bus.clone()), steve_store::FacadeMode::Runtime)
        .map_err(|e| BackendError::msg(format!("assemble storage: {e}")))?;
    if !cfg.no_resume {
        db.stats()
            .gateway_recover()
            .map_err(|e| BackendError::msg(format!("recover gateway accounting: {e}")))?;
        if let Err(error) = output_files::recover_legacy(&db, &cfg.home, &cfg.db_path()) {
            tracing::warn!(%error, "legacy delivery output recovery failed; will retry on next start");
        }
    }

}}
