use super::*;

install! { install(reg, cx) [] => [db] {
    let HubEnv { cfg, bus, asm, .. } = *cx;
    asm.check().map_err(|e| BackendError::msg(e.to_string()))?;
    let db = steve_store::Store::open(cfg.db_path(), store::BusPublisher(bus.clone()))
        .map_err(|e| BackendError::msg(format!("open {}: {e}", cfg.db_path().display())))?;
    if !cfg.no_resume {
        db.stats()
            .gateway_recover()
            .map_err(|e| BackendError::msg(format!("recover gateway accounting: {e}")))?;
    }

}}
