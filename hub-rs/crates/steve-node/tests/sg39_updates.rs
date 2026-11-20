use std::collections::BTreeMap;
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Storage};
use steve_node_api::*;
use steve_runtime::testkit::eventually;

#[path = "fixtures/harness_updates.rs"]
mod fixture;
use fixture::Fixture;

async fn checked(n: &LocalNode) {
    n.harness_updates(true).await.unwrap();
    assert!(
        eventually("component registry check", || async {
            !n.harness_updates(false).await.unwrap().checking
        })
        .await
    );
}
async fn finished(n: &LocalNode, op: OperationId) -> Receipt {
    assert!(
        eventually("component terminal receipt", || async {
            n.operation(op.clone()).await.unwrap().state.is_finished()
        })
        .await
    );
    n.operation(op).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg39_upgrade_admission_noop_and_replay_are_stable() {
    let f = Fixture::new("sg39", "1.0.0", "1.0.0", "2.0.0");
    let n = f.node();
    let c = f.command("unchecked", "2.0.0");
    assert_eq!(
        n.upgrade_harness(c).await.unwrap_err().code,
        ErrorCode::Stale
    );
    let mut c = f.command("unknown", "1.0.0");
    c.body.versions = BTreeMap::from([("unknown".into(), "1.0.0".into())]);
    assert_eq!(
        n.upgrade_harness(c).await.unwrap_err().code,
        ErrorCode::Invalid
    );
    let c = f.command("noop", "1.0.0");
    let r = n.upgrade_harness(c.clone()).await.unwrap();
    assert!(matches!(
        r.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(n.upgrade_harness(c.clone()).await.unwrap(), r);
    let mut changed = c;
    changed
        .body
        .versions
        .values_mut()
        .for_each(|v| *v = "2.0.0".into());
    assert_eq!(
        n.upgrade_harness(changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(f.installs(), 0);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg39_upgrade_verifies_packages_and_sanitizes_installer_failures() {
    for mode in ["success", "fail", "wrong-version"] {
        let f = Fixture::new("sg39", "1.0.0", "1.0.0", "2.0.0");
        if mode != "success" {
            f.mark(mode);
        }
        let n = f.node();
        checked(&n).await;
        let command = f.command("upgrade", "2.0.0");
        let r = n.upgrade_harness(command.clone()).await.unwrap();
        assert_eq!(r.state, Observed::Running);
        let final_r = finished(&n, command.id.clone()).await;
        match &final_r.state {
            Observed::Finished { outcome, error, .. } if mode == "success" => {
                assert_eq!(*outcome, Outcome::Succeeded);
                assert!(error.is_none());
                for c in steve_acp::adapters::components("pi") {
                    assert_eq!(
                        steve_acp::managed::installed(&f.home, c.package).as_deref(),
                        Some("2.0.0")
                    );
                }
            }
            Observed::Finished {
                outcome,
                error: Some(error),
                ..
            } => {
                assert_eq!(*outcome, Outcome::Failed);
                assert_eq!(error.code, ErrorCode::Internal);
                assert!(!error.safe_detail.contains("fixture-secret"));
                assert!(error.safe_detail.contains(if mode == "fail" {
                    "npm upgrade failed"
                } else {
                    "requested component version was not installed"
                }));
            }
            other => panic!("{mode}: {other:?}"),
        }
        assert_eq!(n.upgrade_harness(command).await.unwrap(), final_r);
        assert_eq!(f.installs(), 1);
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(f.dir.path().join("npm.log"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let install = lines.iter().find(|args| args[0] == "install").unwrap();
        for c in steve_acp::adapters::components("pi") {
            assert!(
                install
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(format!("{}@2.0.0", c.package)))
            );
        }
        n.shutdown().await;
    }
}
