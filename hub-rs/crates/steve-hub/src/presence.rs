//! Which signed-in clients hold the live stream (`/api/stream`) open now (G6), and ending those
//! streams when a client is signed out (G11).
//!
//! A client is online while at least one of its streams is open: the first one opening and the
//! last one closing each tell the client's user (`{type:"client", id, user}`, see
//! [`crate::bus::Scope::User`]). Signing a client out ends its open streams at once
//! ([`Presence::kick`]); a client signed out where the Hub does not see it (`steve access revoke`
//! in another process) is found by [`Presence::sweep`] within [`SWEEP_EVERY`]. Kept in memory
//! only: after a Hub restart every client is offline until its page connects again, which it does
//! within seconds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use steve_store::Store;
use tokio::sync::watch;

/// How often clients signed out elsewhere are looked for.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Presence {
    clients: Mutex<HashMap<String, Entry>>,
    /// Numbers each entry, so a stream of an entry that was kicked never counts against a later one.
    next: std::sync::atomic::AtomicU64,
}

struct Entry {
    user: String,
    streams: usize,
    since: i64,
    entry: u64,
    kick: watch::Sender<bool>,
}

/// A client with its stream open: how many streams, and since when (ms) it has had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Online {
    pub streams: usize,
    pub since: i64,
}

/// One open stream of a client; dropping it closes it.
pub struct Here {
    presence: Arc<Presence>,
    store: Option<Store>,
    id: String,
    entry: u64,
    kicked: watch::Receiver<bool>,
}

impl Here {
    /// Resolves once the client was signed out: the stream must end.
    pub async fn kicked(&mut self) {
        // the sender gone without a word is an entry that is no more: end too
        let _ = self.kicked.wait_for(|k| *k).await;
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        self.presence
            .leave(self.store.as_ref(), &self.id, self.entry);
    }
}

fn tell(store: Option<&Store>, id: &str, user: &str) {
    if let Some(s) = store {
        s.publish(steve_types::DomainEvent::from(
            steve_types::DevicesEvent::Client {
                id: {
                    let value: String = id.into();
                    steve_types::ClientId::from(value)
                },
                user: {
                    let value: String = user.into();
                    steve_types::UserId::from(value)
                },
            },
        ));
    }
}

impl Presence {
    /// Client `id` of `user` opened a stream. `store`: where its user is told (none in tests).
    pub fn enter(self: &Arc<Self>, store: Option<&Store>, id: &str, user: &str) -> Here {
        let mut m = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let e = m.entry(id.to_string()).or_insert_with(|| Entry {
            user: user.to_string(),
            streams: 0,
            since: steve_store::now(),
            entry: self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            kick: watch::channel(false).0,
        });
        e.streams += 1;
        let first = e.streams == 1;
        let here = Here {
            presence: self.clone(),
            store: store.cloned(),
            id: id.to_string(),
            entry: e.entry,
            kicked: e.kick.subscribe(),
        };
        drop(m);
        if first {
            tell(store, id, user);
        }
        here
    }

    fn leave(&self, store: Option<&Store>, id: &str, entry: u64) {
        let mut m = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let Some(e) = m.get_mut(id).filter(|e| e.entry == entry) else {
            return;
        };
        e.streams = e.streams.saturating_sub(1);
        if e.streams > 0 {
            return;
        }
        let user = e.user.clone();
        m.remove(id);
        drop(m);
        tell(store, id, &user);
    }

    /// Whether client `id` has a stream open now, and since when.
    pub fn online(&self, id: &str) -> Option<Online> {
        let m = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        m.get(id).map(|e| Online {
            streams: e.streams,
            since: e.since,
        })
    }

    /// The clients with a stream open now.
    pub fn present(&self) -> Vec<String> {
        let m = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        m.keys().cloned().collect()
    }

    /// Ends every open stream of client `id` (it was signed out); `true` when it had one.
    pub fn kick(&self, store: Option<&Store>, id: &str) -> bool {
        let e = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let Some(e) = e else { return false };
        e.kick.send_replace(true);
        tell(store, id, &e.user);
        true
    }

    /// Ends the streams of clients signed out where this Hub did not see it, until the Hub stops.
    pub async fn sweep(self: Arc<Self>, store: Store) {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            self.sweep_once(&store).await;
        }
    }

    /// One look: every client with a stream open that is signed out (or gone) now is kicked.
    pub async fn sweep_once(&self, store: &Store) {
        let ids = self.present();
        if ids.is_empty() {
            return;
        }
        let s = store.clone();
        let out = tokio::task::spawn_blocking(move || {
            ids.into_iter()
                .filter(|id| match s.access().client(id) {
                    Ok(Some(c)) => c.revoked.is_some(),
                    Ok(None) => true,
                    // a store that cannot say ends nothing
                    Err(_) => false,
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for id in out {
            tracing::info!(client = %id, "a signed-out client's stream ended");
            self.kick(Some(store), &id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_client_is_online_while_one_of_its_streams_is_open() {
        let p = Arc::new(Presence::default());
        assert_eq!(p.online("cli_a"), None);
        let a = p.enter(None, "cli_a", "u1");
        let since = p.online("cli_a").unwrap().since;
        let b = p.enter(None, "cli_a", "u1");
        assert_eq!(
            p.online("cli_a"),
            Some(Online { streams: 2, since }),
            "since its first stream"
        );
        drop(a);
        assert_eq!(p.online("cli_a").unwrap().streams, 1);
        drop(b);
        assert_eq!(p.online("cli_a"), None);
        assert!(p.present().is_empty());
    }

    /// A client signed out by another process (`steve access revoke`) is found by the sweep; its user
    /// hears it come, and go.
    #[tokio::test]
    async fn the_sweep_ends_streams_of_clients_signed_out_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let keep = seen.clone();
        let store = Store::open(
            dir.path().join("steve.sqlite3"),
            steve_store::FnPublisher(move |e| keep.lock().unwrap().push(e)),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        let user = store.users().create("local:a", "A", None, "owner").unwrap();
        let (_, token) = store.access().token_create(&user, "t", None).unwrap();
        let new = |name| steve_store::access::NewClient {
            kind: "cli",
            name,
            platform: "test",
            version: "1",
            origin: None,
        };
        let (_, gone, _) = store
            .access()
            .sign_in("A", &token, &new("gone"))
            .unwrap()
            .unwrap();
        let (_, kept, _) = store
            .access()
            .sign_in("A", &token, &new("kept"))
            .unwrap()
            .unwrap();
        let p = Arc::new(Presence::default());
        let mut a = p.enter(Some(&store), &gone.id, &user);
        let mut b = p.enter(Some(&store), &kept.id, &user);
        p.sweep_once(&store).await;
        assert_eq!(p.present().len(), 2, "nothing signed out yet");
        assert!(store.access().client_revoke(&gone.id).unwrap());
        p.sweep_once(&store).await;
        tokio::time::timeout(Duration::from_secs(1), a.kicked())
            .await
            .expect("ended");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), b.kicked())
                .await
                .is_err()
        );
        assert_eq!(p.present(), vec![kept.id.clone()]);
        let told = |id: &str| {
            seen.lock()
                .unwrap()
                .iter()
                .filter(
                    |e| matches!(e, steve_types::BusEvent::Domain(steve_types::DomainEvent::Devices(steve_types::DevicesEvent::Client { id: i, user: u })) if i.as_str() == id && u.as_str() == user.as_str()),
                )
                .count()
        };
        // signed in, came onto the stream, signed out, its stream ended
        assert_eq!(told(&gone.id), 4);
        // signed in, came onto the stream
        assert_eq!(told(&kept.id), 2);
    }

    #[tokio::test]
    async fn signing_a_client_out_ends_its_streams_and_only_its() {
        let p = Arc::new(Presence::default());
        let mut a = p.enter(None, "cli_a", "u1");
        let mut b = p.enter(None, "cli_a", "u1");
        let mut other = p.enter(None, "cli_b", "u1");
        assert!(p.kick(None, "cli_a"));
        assert!(!p.kick(None, "cli_a"), "nothing left to end");
        for h in [&mut a, &mut b] {
            tokio::time::timeout(Duration::from_secs(1), h.kicked())
                .await
                .expect("ended");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), other.kicked())
                .await
                .is_err()
        );
        assert_eq!(p.online("cli_a"), None);
        // a stream of the kicked entry closing later does not touch a new one
        let again = p.enter(None, "cli_a", "u1");
        drop(a);
        assert_eq!(p.online("cli_a").unwrap().streams, 1);
        drop(again);
        assert_eq!(p.online("cli_b").unwrap().streams, 1);
    }
}
