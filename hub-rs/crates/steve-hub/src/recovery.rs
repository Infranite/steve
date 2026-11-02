//! What the listener-ready recovery did, for `/healthz` (`recovery`): `null` until it ran, then
//! `{state: ok|partial|failed, resumed, held, failed, error?}`. Diagnostics only; a recovery problem
//! never stops the Hub from serving (the durable intents stay for the next boot).

use std::sync::Mutex;

use serde_json::{Value, json};

static STATUS: Mutex<Option<Value>> = Mutex::new(None);

fn set(v: Value) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
}

/// The outcome of `Domain::ready`'s main-conversation part.
pub fn record(r: &crate::backend::BResult<steve_core::thread::ThreadReady>) {
    match r {
        Ok(t) => set(json!({
            "state": if t.failed > 0 { "partial" } else { "ok" },
            "resumed": t.resumed,
            "held": t.held,
            "failed": t.failed,
        })),
        Err(e) => failed(&e.to_string()),
    }
}

/// Recovery could not run at all.
pub fn failed(error: &str) {
    tracing::error!("recovery: {error}");
    // `/healthz` is public: paths, database details and other raw failures belong only in logs.
    set(json!({"state": "failed", "error": "recovery_failed"}));
}

pub fn status() -> Value {
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_recovery_is_reported_not_raised() {
        record(&Ok(steve_core::thread::ThreadReady {
            resumed: 2,
            held: 1,
            failed: 1,
        }));
        assert_eq!(status()["state"], "partial");
        assert_eq!(status()["resumed"], 2);
        failed("database is locked at /private/test.sqlite; token=synthetic");
        assert_eq!(status()["state"], "failed");
        assert_eq!(status()["error"], "recovery_failed");
        let public = status().to_string();
        assert!(!public.contains("database"));
        assert!(!public.contains("/private"));
        assert!(!public.contains("synthetic"));
    }
}
