//! The recorder's typed Node snapshots. Keep raw counters only until the next observation,
//! minute points for a day, and the full machine description only for the latest observation.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use steve_node_api::{NetworkBytes, NodeVitals};
use steve_types::devices::{
    NetworkRate, NodeVitalsDay, NodeVitalsLatest, NodeVitalsPoint, VitalsGap, VitalsRoot,
    VitalsSampling, VitalsTokens,
};

pub(crate) const EVERY: Duration = Duration::from_secs(60);
pub(crate) const ASK: Duration = Duration::from_secs(15);
const MINUTE: i64 = EVERY.as_millis() as i64;
const MAX_GAP: i64 = MINUTE + ASK.as_millis() as i64;
const KEEP: i64 = 24 * 60 * MINUTE;
const CAPACITY: usize = (KEEP / MINUTE) as usize;
const BUCKET: i64 = 5 * MINUTE;

pub(crate) struct Observation {
    pub id: String,
    pub at: i64,
    pub rtt: Option<i64>,
    pub own_process: bool,
    pub vitals: NodeVitals,
}

#[derive(Default)]
pub(super) struct History(HashMap<String, NodeHistory>);

struct NodeHistory {
    latest: NodeVitalsLatest,
    network: BTreeMap<String, NetworkBytes>,
    points: VecDeque<NodeVitalsPoint>,
}

impl History {
    pub(super) fn record(&mut self, observations: Vec<Observation>, now: i64) {
        self.0.retain(|_, node| node.latest.at > now - KEEP);
        for observation in observations {
            let Observation {
                id,
                at,
                rtt,
                own_process,
                vitals,
            } = observation;
            let (previous, mut points) = match self.0.remove(&id) {
                Some(node) => (Some((node.latest.at, node.network)), node.points),
                None => (None, VecDeque::new()),
            };
            let rate = vitals.meters.as_ref().and_then(|meters| {
                let (before, counters) = previous.as_ref()?;
                network_rate(counters, &meters.network, at - before)
            });
            let latest = NodeVitalsLatest {
                at,
                sampled_at: vitals.at.0,
                cpus: vitals.cpus,
                load: vitals
                    .load
                    .filter(|load| load.iter().all(|value| value.is_finite())),
                mem_total: vitals.mem_total,
                mem_available: vitals.mem_available,
                network: rate,
                roots: vitals
                    .roots
                    .into_iter()
                    .map(|root| VitalsRoot {
                        root: root.root.to_string(),
                        display: root.display,
                        free: root.free,
                        total: root.total,
                        inodes_free: root.inodes_free,
                    })
                    .collect(),
                sessions: vitals.sessions,
                rtt_ms: rtt,
            };
            let point = point(&latest);
            super::node_figures(&id, &latest, point);
            let network = vitals
                .meters
                .map(|meters| {
                    super::node_meters(&id, &meters, own_process);
                    meters.network
                })
                .unwrap_or_default();
            while points.front().is_some_and(|p| p.at <= now - KEEP) || points.len() >= CAPACITY {
                points.pop_front();
            }
            points.push_back(point);
            self.0.insert(
                id,
                NodeHistory {
                    latest,
                    network,
                    points,
                },
            );
        }
    }

    pub(super) fn latest(&self, id: &str, since: i64) -> Option<&NodeVitalsLatest> {
        self.0.get(id).map(|n| &n.latest).filter(|n| n.at >= since)
    }

    pub(super) fn day(&self, id: &str, now: i64, tokens: VitalsTokens) -> Option<NodeVitalsDay> {
        let node = self.0.get(id)?;
        let latest = self.latest(id, now - KEEP)?.clone();
        let mut series: Vec<NodeVitalsPoint> = Vec::new();
        let mut gaps = Vec::new();
        let mut previous = None;
        for point in node.points.iter().filter(|p| p.at > now - KEEP) {
            if let Some(from) = previous
                && point.at - from > MAX_GAP
            {
                gaps.push(VitalsGap { from, to: point.at });
            }
            previous = Some(point.at);
            match series.last_mut() {
                Some(last) if last.at.div_euclid(BUCKET) == point.at.div_euclid(BUCKET) => {
                    *last = *point
                }
                _ => series.push(*point),
            }
        }
        Some(NodeVitalsDay {
            latest,
            series,
            gaps,
            sampling: VitalsSampling {
                interval_ms: MINUTE,
                stale_after_ms: 2 * MINUTE + ASK.as_millis() as i64,
                now,
            },
            tokens,
        })
    }
}

fn point(latest: &NodeVitalsLatest) -> NodeVitalsPoint {
    let load = latest
        .cpus
        .filter(|&n| n > 0)
        .zip(latest.load)
        .map(|(n, load)| load[0] / f64::from(n));
    let mem = latest
        .mem_total
        .zip(latest.mem_available)
        .filter(|&(total, free)| total > 0 && free <= total)
        .map(|(total, free)| (total - free) as f64 / total as f64);
    NodeVitalsPoint {
        at: latest.at,
        load,
        mem,
        rtt: latest.rtt_ms,
        network: latest.network,
    }
}

/// A missing interval, changed interface set or any backwards counter leaves a gap in the rate.
fn network_rate(
    previous: &BTreeMap<String, NetworkBytes>,
    current: &BTreeMap<String, NetworkBytes>,
    elapsed_ms: i64,
) -> Option<NetworkRate> {
    if !(1..=MAX_GAP).contains(&elapsed_ms) || current.is_empty() || current.len() != previous.len()
    {
        return None;
    }
    let (mut rx, mut tx) = (0.0, 0.0);
    for (name, now) in current {
        let before = previous.get(name)?;
        rx += now.received.checked_sub(before.received)? as f64;
        tx += now.sent.checked_sub(before.sent)? as f64;
    }
    let seconds = elapsed_ms as f64 / 1_000.0;
    Some(NetworkRate {
        rx_bytes_per_second: rx / seconds,
        tx_bytes_per_second: tx / seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::{NodeMeters, Timestamp};

    #[test]
    fn snapshots_compute_rates_and_keep_gaps_through_downsampling() {
        let record = |history: &mut History, at, interface: &str, rx| {
            history.record(
                vec![Observation {
                    id: "local".into(),
                    at,
                    rtt: None,
                    own_process: true,
                    vitals: NodeVitals {
                        at: Timestamp(at),
                        cpus: Some(4),
                        load: Some([2.0, 1.0, 0.0]),
                        mem_total: Some(100),
                        mem_available: Some(25),
                        roots: vec![],
                        sessions: 0,
                        meters: Some(NodeMeters {
                            network: [(
                                interface.into(),
                                NetworkBytes {
                                    received: rx,
                                    sent: rx * 2,
                                },
                            )]
                            .into(),
                            ..Default::default()
                        }),
                    },
                }],
                at,
            );
        };
        let mut history = History::default();
        let read =
            |history: &History, now| history.day("local", now, VitalsTokens::default()).unwrap();
        record(&mut history, MINUTE, "eth0", 100);
        assert_eq!(read(&history, MINUTE).latest.network, None);
        record(&mut history, 2 * MINUTE, "eth0", 160);
        assert_eq!(
            read(&history, 2 * MINUTE).latest.network,
            Some(NetworkRate {
                rx_bytes_per_second: 1.0,
                tx_bytes_per_second: 2.0,
            })
        );
        record(&mut history, 3 * MINUTE, "eth0", 10);
        assert_eq!(read(&history, 3 * MINUTE).latest.network, None);
        record(&mut history, 4 * MINUTE, "eth1", 160);
        assert_eq!(read(&history, 4 * MINUTE).latest.network, None);

        record(&mut history, 7 * MINUTE, "eth1", 220);
        let day = read(&history, 7 * MINUTE);
        assert_eq!(day.latest.network, None);
        assert_eq!(day.series.len(), 2);
        assert_eq!(day.series[0].load, Some(0.5));
        assert_eq!(day.series[0].mem, Some(0.75));
        assert_eq!((day.gaps[0].from, day.gaps[0].to), (4 * MINUTE, 7 * MINUTE));

        record(&mut history, KEEP + 8 * MINUTE, "eth1", 280);
        assert_eq!(read(&history, KEEP + 8 * MINUTE).series.len(), 1);
        history.record(vec![], 2 * KEEP + 9 * MINUTE);
        assert!(history.0.is_empty());
    }
}
