//! A target's merge-queue policy: every number the queue and the release train go by, each with a
//! default and each settable per target (a project may give defaults of its own). Stored as JSON in
//! `mq_targets.policy`; a field left out takes its default, an unknown one is kept as it is.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// The most entries one batch takes, from the head of the queue.
    pub max_batch: usize,
    /// Seconds a queue that just received an entry waits for more before a batch starts (a
    /// debounce, from the latest arrival). 0: a batch starts as soon as the target is idle.
    pub settle_s: u64,
    /// Commands the Node runs on a batch before it lands (the owner sets them; none by default).
    pub checks: Vec<String>,
    /// Deadline of each Node check command.
    pub check_deadline_s: u64,
    /// How long the host's required checks may stay without a change before they count as no result.
    pub host_deadline_s: u64,
    /// A release train closes this long after its first change landed.
    pub release_window_s: u64,
    /// A release train closes once it has this many changes.
    pub release_max: usize,
    /// A release train closes once nothing landed for this long. `None`: never for that.
    pub release_idle_s: Option<u64>,
    /// How long the queue waits for its Node to come back before a card asks what to do.
    pub node_wait_s: u64,
    /// How many batches may be under way at once (1: one after the other; only 1 is implemented).
    pub speculate: u32,
    /// How many times YOLO may answer "run the checks again" on one batch whose checks gave no
    /// result (a person may always).
    pub yolo_reruns: u32,
    /// The queue starts no new batch (one may be let through at a time).
    pub paused: bool,
    /// The Node the target's staging is kept on, when the owner named one.
    pub node: Option<String>,
    /// How often a target that keeps moving is integrated again before the batch stops.
    pub moved_retries: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            max_batch: 8,
            settle_s: 0,
            checks: vec![],
            check_deadline_s: 45 * 60,
            host_deadline_s: 60 * 60,
            release_window_s: 2 * 3600,
            release_max: 10,
            release_idle_s: None,
            node_wait_s: 30 * 60,
            speculate: 1,
            yolo_reruns: 1,
            paused: false,
            node: None,
            moved_retries: 2,
        }
    }
}

impl Policy {
    /// The policy stored as `json` (`{}` or garbage: the defaults), with `project`'s defaults
    /// under it: a field the target sets wins, then the project's, then the built-in default.
    pub fn of(json: &str, project: Option<&serde_json::Value>) -> Policy {
        let mut v = serde_json::to_value(Policy::default()).unwrap_or_default();
        for layer in [project.cloned(), serde_json::from_str(json).ok()]
            .into_iter()
            .flatten()
        {
            if let (Some(base), serde_json::Value::Object(o)) = (v.as_object_mut(), layer) {
                for (k, x) in o {
                    base.insert(k, x);
                }
            }
        }
        serde_json::from_value(v).unwrap_or_default()
    }

    /// At least one, whatever was stored.
    pub fn batch_limit(&self) -> usize {
        self.max_batch.max(1)
    }

    pub fn train_limit(&self) -> usize {
        self.release_max.max(1)
    }

    /// The window, in milliseconds.
    pub fn window_ms(&self) -> i64 {
        self.release_window_s as i64 * 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_their_defaults_and_layers_override_in_order() {
        assert_eq!(Policy::of("{}", None), Policy::default());
        assert_eq!(Policy::of("not json", None), Policy::default());
        let p = Policy::of(
            r#"{"max_batch": 3, "checks": ["make test"]}"#,
            Some(&serde_json::json!({"max_batch": 5, "release_max": 4})),
        );
        assert_eq!(p.max_batch, 3, "the target's own wins");
        assert_eq!(p.release_max, 4, "then the project's");
        assert_eq!(p.release_window_s, 7200, "then the default");
        assert_eq!(p.checks, ["make test"]);
        assert_eq!(Policy::of(r#"{"max_batch": 0}"#, None).batch_limit(), 1);
    }
}
