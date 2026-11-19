use super::*;

install! { install(reg, cx) [db, mut jobs] => [db, jobs] {
    let server = crate::admin::bind(&cx.cfg.home, db.clone())
        .map_err(|e| BackendError::msg(format!("management socket: {e:#}")))?;
    // Administrative access stays available on isolated NO_RESUME Hubs too.
    jobs.push(Box::pin(async move {
        if let Err(e) = server.run().await {
            tracing::error!("management socket stopped: {e:#}");
        }
    }));
}}
