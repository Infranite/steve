//! Who holds each Node's link right now. Installing generation N fences every older link of that
//! Node (`fenced`, close 4409); its in-flight calls resolve by the written rule. Revocation kills
//! the link at once (close 4403).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use steve_node_wire::{Conn, close};

struct Slot {
    generation: u64,
    conn: Arc<Conn>,
}

#[derive(Default)]
pub struct NodeRegistry {
    slots: Mutex<HashMap<String, Slot>>,
    /// When each Node's live link was taken by a newer one, recently (M4).
    takeovers: Mutex<HashMap<String, VecDeque<Instant>>>,
}

/// A live link taken over this often within [`TAKEOVER_WINDOW`] is two parties holding one key
/// taking turns (M4): a Node that just lost its network leaves a dead link, taken over once.
pub const TAKEOVERS: usize = 6;
pub const TAKEOVER_WINDOW: Duration = Duration::from_secs(60);

impl NodeRegistry {
    /// `false`: a newer link already holds the Node (two dials raced; the higher generation wins)
    /// and this one was closed.
    pub fn install(&self, node: &str, generation: u64, conn: Arc<Conn>) -> bool {
        let mut slots = self.slots.lock().unwrap();
        if let Some(old) = slots.get(node) {
            if old.generation > generation {
                conn.close(
                    Some(("fenced", json!({"gen": old.generation}))),
                    close::FENCED,
                    "fenced",
                );
                return false;
            }
            if !old.conn.is_closed() {
                let mut t = self.takeovers.lock().unwrap();
                let seen = t.entry(node.to_string()).or_default();
                let now = Instant::now();
                seen.push_back(now);
                while seen
                    .front()
                    .is_some_and(|f| now.duration_since(*f) > TAKEOVER_WINDOW)
                    || seen.len() > TAKEOVERS
                {
                    seen.pop_front();
                }
            }
            old.conn.close(
                Some(("fenced", json!({"gen": generation}))),
                close::FENCED,
                "fenced",
            );
        }
        slots.insert(node.to_string(), Slot { generation, conn });
        true
    }

    /// The Node's live link was taken over [`TAKEOVERS`] times within the window; asking forgets
    /// them (one alert per burst).
    pub fn taken_over_often(&self, node: &str) -> bool {
        let mut t = self.takeovers.lock().unwrap();
        let often = t.get(node).is_some_and(|seen| {
            seen.len() >= TAKEOVERS && seen.front().is_some_and(|f| f.elapsed() <= TAKEOVER_WINDOW)
        });
        if often {
            t.remove(node);
        }
        often
    }

    /// The Nodes with a live link now.
    pub fn linked(&self) -> Vec<String> {
        self.slots
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| !s.conn.is_closed())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn current(&self, node: &str) -> Option<u64> {
        self.slots
            .lock()
            .unwrap()
            .get(node)
            .filter(|s| !s.conn.is_closed())
            .map(|s| s.generation)
    }

    pub fn conn(&self, node: &str) -> Option<Arc<Conn>> {
        self.slots.lock().unwrap().get(node).map(|s| s.conn.clone())
    }

    /// Forget the link of `generation` once it is down (a newer one stays).
    pub fn remove(&self, node: &str, generation: u64) {
        let mut slots = self.slots.lock().unwrap();
        if slots.get(node).is_some_and(|s| s.generation == generation) {
            slots.remove(node);
        }
    }

    /// Close the Node's link now (revoked).
    pub fn kill(&self, node: &str, reason: &str) {
        let slot = self.slots.lock().unwrap().remove(node);
        if let Some(s) = slot {
            s.conn.close(None, close::DENIED, reason);
        }
    }
}
