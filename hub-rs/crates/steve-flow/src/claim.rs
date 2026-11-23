//! Claim/plan failures are terminal for their row, not infrastructure failures of the source.
use steve_store::{Store, W};

use crate::FlowResult;

/// A failed row rolls back its partial writes. Terminal state and audit use the same outer write.
pub(crate) fn row<T>(
    store: &Store,
    table: &str,
    now: i64,
    scope: impl FnOnce(&mut W<'_>) -> steve_store::Result<Option<(String, String)>>,
    attempt: impl FnOnce(&mut W<'_>) -> FlowResult<Option<T>>,
    terminal: impl FnOnce(&mut W<'_>, &crate::FlowError) -> FlowResult<Option<T>>,
) -> FlowResult<Option<T>> {
    store.write(|w| match w.scope(attempt) {
        Ok(value) => Ok(value),
        Err(error) => {
            let target = scope(w);
            let value = w.scope(|w| terminal(w, &error))?;
            if let Ok(Some((ws, subject))) = target {
                record_failure(w, table, &ws, &subject, &error, now);
            } else {
                tracing::error!(table, %error, "flow row claim failed; terminal");
            }
            Ok(value)
        }
    })
}

pub(crate) fn record_failure(
    w: &mut steve_store::W<'_>,
    table: &str,
    ws: &str,
    subject: &str,
    error: &dyn std::fmt::Display,
    now: i64,
) {
    tracing::error!(table, subject, %error, "flow row claim failed; terminal");
    if let Err(audit) = w.scope(|w| {
        w.audit(
            ws,
            "steve",
            "flow.claim_failed",
            subject,
            &serde_json::json!({
                "table": table,
                "error": error.to_string(),
                "at": now,
            }),
        )
    }) {
        tracing::error!(%audit, "recording terminal flow claim diagnostic failed");
    }
}
