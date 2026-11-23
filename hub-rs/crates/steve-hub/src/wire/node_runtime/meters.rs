//! What the Hub measures at one Node's protocol boundary (S1): the event batches it commits
//! ([`super::receive`]) and the commands it sends ([`super::send`]). Handles are registered once per
//! Node, or once per method and result; recording a batch or a command allocates nothing.
use std::collections::HashMap;
use std::sync::Mutex;

use metrics::{Counter, Histogram, SharedString};

/// The methods the coordinator sends (bounded labels); anything else is `other`.
const METHODS: [&str; 13] = [
    "prepare_workspace",
    "open_session",
    "start_turn",
    "start_job",
    "cancel",
    "release_session",
    "configure_session",
    "resolve_permission",
    "abandon",
    "recover",
    "exec",
    "git_apply",
    "tidy_workspace",
];

fn method(m: &str) -> &'static str {
    METHODS
        .iter()
        .find(|x| **x == m)
        .copied()
        .unwrap_or("other")
}

pub(super) struct Meters {
    node: SharedString,
    pub commits: Counter,
    pub events: Histogram,
    pub seconds: Histogram,
    pub mode_state: Histogram,
    commands: Mutex<HashMap<(&'static str, &'static str), (Counter, Histogram)>>,
    retries: Mutex<HashMap<&'static str, Counter>>,
}

impl Meters {
    pub fn new(node: &str) -> Meters {
        let node = SharedString::from(node.to_string());
        Meters {
            commits: metrics::counter!("steve_node_ingest_commits_total", "node" => node.clone()),
            events: metrics::histogram!("steve_node_ingest_events", "node" => node.clone()),
            seconds: metrics::histogram!("steve_node_ingest_seconds", "node" => node.clone()),
            mode_state: metrics::histogram!("steve_node_mode_state_seconds", "node" => node.clone()),
            node,
            commands: Mutex::default(),
            retries: Mutex::default(),
        }
    }

    /// A command sent and what came back, after `seconds`.
    pub fn command(&self, m: &str, result: Result<(), steve_node_api::Code>, seconds: f64) {
        let m = method(m);
        let result = result.map_or_else(steve_node_api::Code::as_str, |()| "ok");
        let (count, time) = self
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((m, result))
            .or_insert_with(|| {
                (
                    metrics::counter!("steve_node_commands_total", "node" => self.node.clone(), "method" => m, "result" => result),
                    metrics::histogram!("steve_node_command_seconds", "node" => self.node.clone(), "method" => m),
                )
            })
            .clone();
        count.increment(1);
        time.record(seconds);
    }

    /// A command to be sent again later.
    pub fn retry(&self, m: &str) {
        let m = method(m);
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(m)
            .or_insert_with(|| {
                metrics::counter!("steve_node_command_retries_total", "node" => self.node.clone(), "method" => m)
            })
            .increment(1);
    }
}
