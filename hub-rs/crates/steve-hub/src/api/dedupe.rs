//! Send idempotency for `POST /thread` and `POST /items`: the Web gives each draft one
//! `client_id` and sends it again on every retry (a lost response, a reload, a second click).
//! The first request with an id runs; a repeat within [`TTL`] gets the first one's answer
//! without doing anything, and a repeat while the first is still running waits for it. The run
//! is detached from its request: when the connection drops mid-send the send still finishes
//! (the message is stored, the item made), so the retry waits for it and gets its answer instead
//! of sending again. Only a run that fails leaves nothing behind, so that retry really retries.
//! Kept in memory: after a Hub restart an old id runs once more, which only matters for a retry
//! that straddles the restart.

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

type Slots = Arc<Mutex<HashMap<Key, Slot>>>;

#[derive(Default)]
pub struct Dedupe {
    slots: Slots,
}

fn lock(slots: &Mutex<HashMap<Key, Slot>>) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
    slots.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes a still-running slot when its run fails (or panics).
struct Abandon {
    slots: Slots,
    key: Option<Key>,
}

impl Drop for Abandon {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let mut slots = lock(&self.slots);
            if matches!(slots.get(&key), Some(Slot::Running(_))) {
                slots.remove(&key);
            }
        }
    }
}

impl Dedupe {
    #[cfg(test)]
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
        lock(&self.slots)
    }

    /// Run `f` once per `(ws, user, route, client_id)` within [`TTL`]; without a client id it
    /// just runs. With one, `f` runs in its own task, so dropping this future (the client went
    /// away) does not stop or forget a send that has started.
    pub async fn once<E, F, Fut>(
        &self,
        ws: &str,
        user: &str,
        route: &'static str,
        client_id: Option<String>,
        f: F,
    ) -> Result<Value, E>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, E>> + Send + 'static,
        E: Send + 'static,
    {
        let Some(id) = client_id else {
            return f().await;
        };
        let key: Key = (ws.to_string(), user.to_string(), route, id);
        let tx = loop {
            let mut rx = {
                let mut slots = lock(&self.slots);
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
            slots: self.slots.clone(),
            key: Some(key),
        };
        let run = tokio::spawn(async move {
            let v = Arc::new(f().await?);
            if let Some(key) = abandon.key.take() {
                lock(&abandon.slots).insert(key, Slot::Done(Instant::now(), v.clone()));
            }
            let _ = tx.send(Some(v.clone()));
            Ok((*v).clone())
        });
        match run.await {
            Ok(r) => r,
            // a panic in `f` stays a panic of this request, as when it ran inline
            Err(e) => match e.try_into_panic() {
                Ok(p) => std::panic::resume_unwind(p),
                Err(e) => panic!("send task cancelled: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn repeats_get_the_first_answer() {
        let d = Dedupe::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let run = |n: usize| {
            let runs = runs.clone();
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
    async fn a_failed_run_can_be_retried() {
        let d = Dedupe::default();
        let id = || Some("c".to_string());
        let r = d
            .once("w", "u", "thread", id(), || async {
                Err::<Value, _>("down")
            })
            .await;
        assert_eq!(r, Err("down"));
        let r = d
            .once("w", "u", "thread", id(), || async {
                Ok::<_, &str>(Value::from("sent"))
            })
            .await;
        assert_eq!(r, Ok("sent".into()));
    }

    /// The client disconnects while the send is still running: the send goes on, and the
    /// browser's retry waits for it and gets its answer without sending a second time.
    #[tokio::test]
    async fn a_dropped_request_still_finishes_and_its_retry_does_not_run_again() {
        let d = Dedupe::default();
        let id = || Some("c".to_string());
        let runs = Arc::new(AtomicUsize::new(0));
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let first = d.once("w", "u", "thread", id(), {
            let runs = runs.clone();
            move || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let _ = wait.await;
                Ok::<_, ()>(Value::from("first"))
            }
        });
        // the request went away mid-run (client disconnect): its future is dropped
        assert!(
            tokio::time::timeout(Duration::from_millis(20), first)
                .await
                .is_err()
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let retry = d.once("w", "u", "thread", id(), {
            let runs = runs.clone();
            move || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(Value::from("second"))
            }
        });
        tokio::pin!(retry);
        // still running: the retry waits instead of sending again
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut retry)
                .await
                .is_err()
        );
        go.send(()).expect("the first run is still alive");
        assert_eq!(retry.await, Ok("first".into()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // and once finished, later repeats get the answer straight away
        let again = d
            .once("w", "u", "thread", id(), || async {
                Ok::<_, ()>(Value::from("third"))
            })
            .await;
        assert_eq!(again, Ok("first".into()));
    }

    /// A send whose request was dropped and which then fails leaves nothing behind.
    #[tokio::test]
    async fn a_dropped_request_that_fails_can_be_retried() {
        let d = Dedupe::default();
        let id = || Some("c".to_string());
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let first = d.once("w", "u", "thread", id(), move || async move {
            let _ = wait.await;
            Err::<Value, _>("down")
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), first)
                .await
                .is_err()
        );
        go.send(()).expect("the first run is still alive");
        let r = d
            .once("w", "u", "thread", id(), || async {
                Ok::<_, &str>(Value::from("sent"))
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
