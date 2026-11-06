//! Send idempotency for `POST /thread` and `POST /items`: the Web gives each draft one
//! `client_id` and sends it again on every retry (a lost response, a reload, a second click).
//! The first request with an id runs; a repeat within [`TTL`] gets the first one's answer
//! without doing anything, and a repeat while the first is still running waits for it. A failed
//! or abandoned first run leaves nothing behind, so the retry really retries. Kept in memory:
//! after a Hub restart an old id runs once more, which only matters for a retry that straddles
//! the restart.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::watch;

/// How long a finished send is remembered.
pub const TTL: Duration = Duration::from_secs(10 * 60);

/// Workspace, user, route, client id.
type Key = (String, String, &'static str, String);

enum Slot {
    /// Running; the receiver sees the answer once it is there (or the sender go away on failure).
    Running(watch::Receiver<Option<Arc<Value>>>),
    Done(Instant, Arc<Value>),
}

#[derive(Default)]
pub struct Dedupe {
    slots: Mutex<HashMap<Key, Slot>>,
}

/// Removes a still-running slot when its run fails or its request is dropped.
struct Abandon<'a> {
    dedupe: &'a Dedupe,
    key: Option<Key>,
}

impl Drop for Abandon<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let mut slots = self.dedupe.lock();
            if matches!(slots.get(&key), Some(Slot::Running(_))) {
                slots.remove(&key);
            }
        }
    }
}

impl Dedupe {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` once per `(ws, user, route, client_id)` within [`TTL`]; without a client id it
    /// just runs.
    pub async fn once<E, F, Fut>(
        &self,
        ws: &str,
        user: &str,
        route: &'static str,
        client_id: Option<String>,
        f: F,
    ) -> Result<Value, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, E>>,
    {
        let Some(id) = client_id else {
            return f().await;
        };
        let key: Key = (ws.to_string(), user.to_string(), route, id);
        let tx = loop {
            let mut rx = {
                let mut slots = self.lock();
                let now = Instant::now();
                slots.retain(|_, s| match s {
                    Slot::Done(at, _) => now.duration_since(*at) < TTL,
                    Slot::Running(_) => true,
                });
                match slots.get(&key) {
                    Some(Slot::Done(_, v)) => return Ok((**v).clone()),
                    Some(Slot::Running(rx)) => rx.clone(),
                    None => {
                        let (tx, rx) = watch::channel(None);
                        slots.insert(key.clone(), Slot::Running(rx));
                        break tx;
                    }
                }
            };
            // the first run either answers or drops its sender; either way look again
            if let Ok(v) = rx.wait_for(Option::is_some).await
                && let Some(v) = v.as_ref()
            {
                return Ok((**v).clone());
            }
        };
        let mut abandon = Abandon {
            dedupe: self,
            key: Some(key),
        };
        let v = Arc::new(f().await?);
        if let Some(key) = abandon.key.take() {
            self.lock()
                .insert(key, Slot::Done(Instant::now(), v.clone()));
        }
        let _ = tx.send(Some(v.clone()));
        Ok((*v).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn repeats_get_the_first_answer() {
        let d = Dedupe::default();
        let runs = AtomicUsize::new(0);
        let run = |n: usize| {
            let runs = &runs;
            move || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(Value::from(n))
            }
        };
        let id = || Some("c1".to_string());
        assert_eq!(d.once("w", "u", "thread", id(), run(1)).await, Ok(1.into()));
        assert_eq!(d.once("w", "u", "thread", id(), run(2)).await, Ok(1.into()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // another route, user, workspace or id is another send; no id never dedupes
        for (w, u, r, c) in [
            ("w", "u", "items", id()),
            ("w", "v", "thread", id()),
            ("x", "u", "thread", id()),
            ("w", "u", "thread", Some("c2".into())),
            ("w", "u", "thread", None),
            ("w", "u", "thread", None),
        ] {
            assert_eq!(d.once(w, u, r, c, run(3)).await, Ok(3.into()));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn a_failed_or_dropped_run_can_be_retried() {
        let d = Dedupe::default();
        let id = || Some("c".to_string());
        let r = d
            .once("w", "u", "thread", id(), || async {
                Err::<Value, _>("down")
            })
            .await;
        assert_eq!(r, Err("down"));
        // the request went away mid-run (client disconnect)
        let pending = d.once("w", "u", "thread", id(), || {
            std::future::pending::<Result<Value, ()>>()
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pending)
                .await
                .is_err()
        );
        let r = d
            .once("w", "u", "thread", id(), || async {
                Ok::<_, ()>(Value::from("sent"))
            })
            .await;
        assert_eq!(r, Ok("sent".into()));
    }

    #[tokio::test]
    async fn a_repeat_waits_for_the_running_first() {
        let d = Arc::new(Dedupe::default());
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let d = d.clone();
            tokio::spawn(async move {
                d.once("w", "u", "thread", Some("c".into()), || async move {
                    let _ = wait.await;
                    Ok::<_, ()>(Value::from("first"))
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = {
            let d = d.clone();
            tokio::spawn(async move {
                d.once("w", "u", "thread", Some("c".into()), || async {
                    Ok::<_, ()>(Value::from("second"))
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!second.is_finished());
        go.send(()).expect("first waits");
        assert_eq!(first.await.expect("first"), Ok("first".into()));
        assert_eq!(second.await.expect("second"), Ok("first".into()));
    }

    #[tokio::test]
    async fn forgotten_after_ttl() {
        let d = Dedupe::default();
        let Some(long_ago) = Instant::now().checked_sub(TTL + Duration::from_secs(1)) else {
            return; // a machine up for less than the TTL cannot backdate an Instant
        };
        d.lock().insert(
            ("w".into(), "u".into(), "thread", "old".into()),
            Slot::Done(long_ago, Arc::new(1.into())),
        );
        let r = d
            .once("w", "u", "thread", Some("old".into()), || async {
                Ok::<_, ()>(Value::from(2))
            })
            .await;
        assert_eq!(r, Ok(2.into()));
        assert_eq!(d.lock().len(), 1);
    }
}
