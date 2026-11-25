//! Node measurements reach the metrics recorder and token usage retains workspace attribution.

mod node_harness;

use std::sync::Arc;
use steve_runtime::testkit::eventually_sync;

use node_harness::*;
use steve_hub::{metrics, nodes::vitals};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

/// Serialize tests in this binary that share the process-global `metrics` recorder (`node="local"`).
static METRICS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_node_updates_the_metrics_recorder() {
    let _guard = METRICS_LOCK.lock().await;
    metrics::install();
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let _l = dial(&h, &id, &node, FakeNode::new(node.as_str()), 1)
        .await
        .unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;
    let local: Arc<dyn Node> = FakeNode::new("local");
    vitals::sample_once(Some(&local), Some(&h.svc)).await;
    let text = metrics::render().unwrap();
    for line in [
        format!("steve_node_cpu_load_ratio{{node=\"{node}\"}} 0.25"),
        format!("steve_node_memory_used_ratio{{node=\"{node}\"}} 0.75"),
        "steve_node_cpu_load_ratio{node=\"local\"} 0.25".into(),
    ] {
        assert!(text.lines().any(|seen| seen == line), "{line} missing");
    }
    assert!(text.contains(&format!("steve_node_rtt_seconds{{node=\"{node}\"}} ")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disk_written_bytes_and_session_vitals_survive_node_to_hub_with_full_fidelity() {
    let _guard = METRICS_LOCK.lock().await;
    metrics::install();
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let fake_remote = FakeNode::new(node.as_str());
    let s1 = SessionId::new("sess_heavy").unwrap();
    let s2 = SessionId::new("sess_light").unwrap();
    let remote_proc_written: u64 = 12_345_678;
    let remote_adapter_written: u64 = 98_765_432;
    fake_remote.set_vitals(NodeVitals {
        at: Timestamp(42),
        cpus: Some(8),
        load: Some([1.5, 1.0, 0.5]),
        mem_total: Some(16 << 30),
        mem_available: Some(8 << 30),
        roots: vec![],
        sessions: 2,
        adapter_disk_written_bytes: remote_adapter_written,
        session_vitals: vec![
            SessionVitals {
                session_id: s1.clone(),
                cpu_seconds: 12.5,
                resident_bytes: 512 << 20,
                disk_written_bytes: 80_000_000,
            },
            SessionVitals {
                session_id: s2.clone(),
                cpu_seconds: 2.25,
                resident_bytes: 128 << 20,
                disk_written_bytes: 18_765_432,
            },
        ],
        meters: Some(NodeMeters {
            process: Some(ProcessMeters {
                cpu_seconds: 4.0,
                resident_bytes: 64 << 20,
                disk_read_bytes: Some(4_096),
                disk_written_bytes: Some(remote_proc_written),
                ..Default::default()
            }),
            ..Default::default()
        }),
    });
    let _l = dial(&h, &id, &node, fake_remote, 1).await.unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;

    let fake_local = FakeNode::new("local");
    let local_proc_written: u64 = 5_555_555;
    let local_adapter_written: u64 = 7_777_777;
    fake_local.set_vitals(NodeVitals {
        at: Timestamp(43),
        cpus: Some(4),
        load: Some([0.5, 0.5, 0.5]),
        mem_total: Some(8 << 30),
        mem_available: Some(4 << 30),
        roots: vec![],
        sessions: 1,
        adapter_disk_written_bytes: local_adapter_written,
        session_vitals: vec![SessionVitals {
            session_id: SessionId::new("sess_local").unwrap(),
            cpu_seconds: 3.5,
            resident_bytes: 256 << 20,
            disk_written_bytes: local_adapter_written,
        }],
        meters: Some(NodeMeters {
            process: Some(ProcessMeters {
                cpu_seconds: 1.5,
                resident_bytes: 32 << 20,
                disk_read_bytes: None,
                disk_written_bytes: Some(local_proc_written),
                ..Default::default()
            }),
            ..Default::default()
        }),
    });
    let local: Arc<dyn Node> = fake_local;
    vitals::sample_once(Some(&local), Some(&h.svc)).await;

    // 1. Verify /api/vitals backing snapshots preserve 100% of process + adapter + per-session bytes.
    let now = steve_store::now();
    let days = metrics::node_days(
        [node.as_str().to_string(), "local".to_string()],
        now,
        Default::default(),
        Default::default(),
    );
    let remote_latest = &days[node.as_str()].latest;
    assert_eq!(
        remote_latest
            .process
            .as_ref()
            .and_then(|p| p.disk_written_bytes),
        Some(remote_proc_written)
    );
    assert_eq!(
        remote_latest.adapter_disk_written_bytes,
        remote_adapter_written
    );
    assert_eq!(remote_latest.session_vitals.len(), 2);
    assert_eq!(
        remote_latest.session_vitals[0].session_id.as_str(),
        "sess_heavy"
    );
    assert_eq!(
        remote_latest.session_vitals[0].disk_written_bytes,
        80_000_000
    );
    assert_eq!(
        remote_latest.session_vitals[1].disk_written_bytes,
        18_765_432
    );

    // Local Node (own_process = false) must ALSO retain process.disk_written_bytes with 100% fidelity.
    let local_latest = &days["local"].latest;
    assert_eq!(
        local_latest
            .process
            .as_ref()
            .and_then(|p| p.disk_written_bytes),
        Some(local_proc_written)
    );
    assert_eq!(
        local_latest.adapter_disk_written_bytes,
        local_adapter_written
    );
    assert_eq!(local_latest.session_vitals.len(), 1);
    assert_eq!(
        local_latest.session_vitals[0].disk_written_bytes,
        local_adapter_written
    );

    // 2. Verify Prometheus exposes low-cardinality per-Node disk write gauges and NEVER session_id labels.
    let text = metrics::render().unwrap();
    let remote_total = remote_proc_written + remote_adapter_written;
    let local_total = local_proc_written + local_adapter_written;
    for expected in [
        format!("steve_node_disk_written_bytes{{node=\"{node}\"}} {remote_total}"),
        format!(
            "steve_node_adapter_disk_written_bytes{{node=\"{node}\"}} {remote_adapter_written}"
        ),
        format!("steve_node_disk_written_bytes{{node=\"local\"}} {local_total}"),
        format!("steve_node_adapter_disk_written_bytes{{node=\"local\"}} {local_adapter_written}"),
    ] {
        assert!(
            text.lines().any(|line| line == expected),
            "{expected} missing from Prometheus exposition"
        );
    }
    assert!(
        !text.contains("sess_heavy") && !text.contains("session_id="),
        "session_id must never appear as a Prometheus label"
    );
}

#[test]
fn session_token_windows_are_attributed_to_the_node_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    steve_store::testkit::hub::node_statistics::seed_session_token_windows(&store).unwrap();
    let totals = store
        .read(|c| {
            steve_store::nodes::session_tokens(
                c,
                "default",
                "2026-11-02",
                "2026-11-01",
                1_000,
                61_000,
            )
        })
        .unwrap();
    assert_eq!(
        totals,
        [("local".into(), (35, 15)), ("remote".into(), (55, 55))].into()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_node_disk_written_bytes_and_session_vitals_roundtrip_in_process() {
    let _guard = METRICS_LOCK.lock().await;
    metrics::install();
    let fake_local = FakeNode::new("local");
    let proc_written: u64 = 12_345_678;
    let adapter_written: u64 = 87_654_321;
    fake_local.set_vitals(NodeVitals {
        at: Timestamp(100),
        cpus: Some(8),
        load: Some([1.0, 0.8, 0.5]),
        mem_total: Some(16 << 30),
        mem_available: Some(8 << 30),
        roots: vec![],
        sessions: 2,
        adapter_disk_written_bytes: adapter_written,
        session_vitals: vec![
            SessionVitals {
                session_id: SessionId::new("sess_inproc_1").unwrap(),
                cpu_seconds: 9.5,
                resident_bytes: 512 << 20,
                disk_written_bytes: 60_000_000,
            },
            SessionVitals {
                session_id: SessionId::new("sess_inproc_2").unwrap(),
                cpu_seconds: 1.25,
                resident_bytes: 64 << 20,
                disk_written_bytes: 27_654_321,
            },
        ],
        meters: Some(NodeMeters {
            process: Some(ProcessMeters {
                cpu_seconds: 2.0,
                resident_bytes: 48 << 20,
                disk_read_bytes: Some(1_024),
                disk_written_bytes: Some(proc_written),
                ..Default::default()
            }),
            ..Default::default()
        }),
    });
    let local: Arc<dyn Node> = fake_local;
    vitals::sample_once(Some(&local), None).await;
    let now = steve_store::now();
    let days = metrics::node_days(
        ["local".to_string()],
        now,
        [("local".into(), (1000, 50_000))].into(),
        [("local".into(), (12.34, 2.50))].into(),
    );
    let day = &days["local"];
    assert_eq!(
        day.latest
            .process
            .as_ref()
            .and_then(|p| p.disk_written_bytes),
        Some(proc_written)
    );
    assert_eq!(day.latest.adapter_disk_written_bytes, adapter_written);
    assert_eq!(day.latest.session_vitals.len(), 2);
    assert_eq!(day.latest.session_vitals[0].disk_written_bytes, 60_000_000);
    assert_eq!(day.latest.session_vitals[1].disk_written_bytes, 27_654_321);
    assert!((day.tokens.total_cost_usd - 12.34).abs() < 1e-9);
    assert!((day.tokens.today_cost_usd - 2.50).abs() < 1e-9);

    let prom = metrics::render().unwrap();
    let total = proc_written + adapter_written;
    assert!(
        prom.lines()
            .any(|l| l == format!("steve_node_disk_written_bytes{{node=\"local\"}} {total}")),
        "missing total disk write gauge in:\n{prom}"
    );
    assert!(
        prom.lines().any(|l| {
            l == format!(
                "steve_node_adapter_disk_written_bytes{{node=\"local\"}} {adapter_written}"
            )
        }),
        "missing adapter disk write gauge"
    );
    assert!(
        !prom.contains("sess_inproc_1") && !prom.contains("session_id="),
        "session_id must never appear in Prometheus exposition"
    );
}
