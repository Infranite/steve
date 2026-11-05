//! Who holds each Node's link right now. Installing generation N fences every older link of that
//! Node (`fenced`, close 4409); its in-flight calls resolve by the written rule. Revocation kills
//! the link at once (close 4403).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use steve_node_wire::{Conn, close};

struct Slot {
    generation: u64,
    conn: Arc<Conn>,
}

#[derive(Default)]
pub struct NodeRegistry {
    slots: Mutex<HashMap<String, Slot>>,
}

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
            old.conn.close(
                Some(("fenced", json!({"gen": generation}))),
                close::FENCED,
                "fenced",
            );
        }
        slots.insert(node.to_string(), Slot { generation, conn });
        true
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
