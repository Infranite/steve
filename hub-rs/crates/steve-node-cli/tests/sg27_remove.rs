use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_node_api::{ErrorCode, NodeRemove, NodeRemoveState, OperationId, RemoveCommit};
use steve_node_cli::remove::{self, RemoveOps, RemoveTiming, ServiceRemover, Work};
use steve_runtime::testkit::eventually_sync;

#[derive(Default)]
struct FakeWork {
    running: AtomicUsize,
    retiring: AtomicBool,
    cancels: Mutex<Vec<String>>,
    cancel_ends: bool,
}
#[async_trait]
impl Work for FakeWork {
    fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }
    fn set_retiring(&self, on: bool) {
        self.retiring.store(on, Ordering::SeqCst);
    }
    async fn cancel_running(&self, reason: &str) -> usize {
        self.cancels.lock().unwrap().push(reason.into());
        if self.cancel_ends {
            self.running.store(0, Ordering::SeqCst);
        }
        self.running()
    }
}
fn id(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}
fn timing() -> RemoveTiming {
    RemoveTiming {
        poll: Duration::from_millis(1),
        cancel_wait: Duration::ZERO,
        ready_wait: Duration::from_secs(60),
        drop_after: Duration::ZERO,
    }
}
fn read(home: &Path, op: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(home.join(remove::DIR).join(format!("{op}.json"))).unwrap(),
    )
    .unwrap()
}
async fn ready(r: &ServiceRemover, op: &str) -> NodeRemoveState {
    assert!(
        eventually_sync("removal ready", || r
            .status_of(&id(op))
            .is_ok_and(|s| s.phase == "ready"))
        .await
    );
    r.status_of(&id(op)).unwrap()
}

#[tokio::test]
async fn removal_drain_cancel_and_busy_timeout_keep_the_original_phase_and_running_count() {
    for (running, cancel_ends, warning) in [(0, false, false), (3, true, false), (5, false, true)] {
        let home = tempfile::tempdir().unwrap();
        let work = Arc::new(FakeWork {
            running: AtomicUsize::new(running),
            cancel_ends,
            ..Default::default()
        });
        let remover = ServiceRemover::new(
            home.path(),
            timing(),
            Box::new(|_, _| panic!("not confirmed")),
        );
        let ask = NodeRemove {
            operation: id("sg27-drain"),
            grace_secs: 0,
        };
        let first = remover.begin_with(work.clone(), ask.clone()).await.unwrap();
        assert_eq!(first.phase, "draining");
        assert!(work.retiring.load(Ordering::SeqCst));
        let second = remover.begin_with(work.clone(), ask.clone()).await.unwrap();
        assert_eq!(second, first);
        let mut conflict = ask.clone();
        conflict.grace_secs = 1;
        assert_eq!(
            remover
                .begin_with(work.clone(), conflict)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            remover
                .begin_with(
                    work.clone(),
                    NodeRemove {
                        operation: id("sg27-other"),
                        grace_secs: 0
                    }
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        let state = ready(&remover, "sg27-drain").await;
        assert_eq!(state.running, if warning { running as u32 } else { 0 });
        assert_eq!(state.error, warning.then(|| "5 turns or commands still ran after they were cancelled; stopping the service ends them".into()));
        let reasons = work.cancels.lock().unwrap().clone();
        assert_eq!(
            reasons,
            if running == 0 {
                vec![]
            } else {
                vec!["the node is being removed from its machine".to_string()]
            }
        );
        assert_eq!(read(home.path(), "sg27-drain")["state"]["phase"], "ready");
        let abandoned = remover
            .commit_with(
                work.clone(),
                RemoveCommit {
                    operation: ask.operation.clone(),
                    abandon: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(abandoned.phase, "abandoned");
        assert!(!work.retiring.load(Ordering::SeqCst));
        assert_eq!(
            remover
                .commit_with(
                    work.clone(),
                    RemoveCommit {
                        operation: ask.operation,
                        abandon: false
                    }
                )
                .await
                .unwrap(),
            abandoned
        );
    }
}

#[tokio::test]
async fn removal_validation_and_early_confirmation_never_spawn_or_delete_anything() {
    let home = tempfile::tempdir().unwrap();
    let work = Arc::new(FakeWork {
        running: AtomicUsize::new(2),
        ..Default::default()
    });
    let remover = ServiceRemover::new(
        home.path(),
        timing(),
        Box::new(|_, _| panic!("must not spawn")),
    );
    for operation in ["..", "a:b", &"x".repeat(97)] {
        assert_eq!(
            remover
                .begin_with(
                    work.clone(),
                    NodeRemove {
                        operation: id(operation),
                        grace_secs: 0
                    }
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Invalid
        );
    }
    assert_eq!(
        remover
            .begin_with(
                work.clone(),
                NodeRemove {
                    operation: id("sg27-long"),
                    grace_secs: 86_401
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    assert!(!home.path().join(remove::DIR).exists());
    let ask = NodeRemove {
        operation: id("sg27-early"),
        grace_secs: 86_400,
    };
    remover.begin_with(work.clone(), ask.clone()).await.unwrap();
    let error = remover
        .commit_with(
            work.clone(),
            RemoveCommit {
                operation: ask.operation.clone(),
                abandon: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert_eq!(error.safe_detail, "the node still runs 2 turns or commands");
    assert_eq!(remover.status_of(&ask.operation).unwrap().phase, "draining");
    remover
        .commit_with(
            work,
            RemoveCommit {
                operation: ask.operation,
                abandon: true,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn confirmation_persists_removing_before_spawn_and_failure_resumes_work() {
    for fail in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let remover = ServiceRemover::new(
            home.path(),
            timing(),
            Box::new(move |root, op| {
                assert_eq!(read(root, op)["state"]["phase"], "removing");
                captured.fetch_add(1, Ordering::SeqCst);
                if fail {
                    Err(std::io::Error::other("spawn refused"))
                } else {
                    Ok(())
                }
            }),
        );
        let work = Arc::new(FakeWork::default());
        remover
            .begin_with(
                work.clone(),
                NodeRemove {
                    operation: id("sg27-confirm"),
                    grace_secs: 0,
                },
            )
            .await
            .unwrap();
        ready(&remover, "sg27-confirm").await;
        let dropped = remover.dropped();
        let state = remover
            .commit_with(
                work.clone(),
                RemoveCommit {
                    operation: id("sg27-confirm"),
                    abandon: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if fail {
            assert_eq!(state.phase, "failed");
            assert_eq!(
                state.error.as_deref(),
                Some("starting the removal: spawn refused")
            );
            assert!(!work.retiring.load(Ordering::SeqCst));
        } else {
            assert_eq!(state.phase, "removing");
            tokio::time::timeout(Duration::from_secs(2), dropped.notified())
                .await
                .unwrap();
        }
        // A failed terminal record is idempotent; an unconfirmed process never uninstalls.
        if fail {
            assert_eq!(
                remover
                    .commit_with(
                        work,
                        RemoveCommit {
                            operation: id("sg27-confirm"),
                            abandon: false
                        }
                    )
                    .await
                    .unwrap(),
                state
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        assert!(home.path().is_dir());
    }
}

#[tokio::test]
async fn ready_without_hub_confirmation_is_abandoned_and_a_restart_settles_inflight_records() {
    let home = tempfile::tempdir().unwrap();
    let work = Arc::new(FakeWork::default());
    let mut t = timing();
    t.ready_wait = Duration::ZERO;
    let remover = ServiceRemover::new(home.path(), t, Box::new(|_, _| panic!("unconfirmed")));
    remover
        .begin_with(
            work.clone(),
            NodeRemove {
                operation: id("sg27-no-hub"),
                grace_secs: 0,
            },
        )
        .await
        .unwrap();
    assert!(
        eventually_sync("ready confirmation expired", || remover
            .status_of(&id("sg27-no-hub"))
            .unwrap()
            .phase
            == "abandoned")
        .await
    );
    let state = remover.status_of(&id("sg27-no-hub")).unwrap();
    assert_eq!(
        state.error.as_deref(),
        Some("the hub did not confirm within 0 s; the node takes work again")
    );
    assert!(!work.retiring.load(Ordering::SeqCst));
    for phase in ["draining", "cancelling", "ready", "removing", "failed"] {
        record(home.path(), phase, phase);
    }
    let restarted = ServiceRemover::new(
        home.path(),
        timing(),
        Box::new(|_, _| panic!("restart must not spawn")),
    );
    for phase in ["draining", "cancelling", "ready"] {
        let s = restarted.status_of(&id(phase)).unwrap();
        assert_eq!(s.phase, "abandoned");
        assert_eq!(
            s.error.as_deref(),
            Some("the node restarted during the removal")
        );
    }
    for phase in ["removing", "failed"] {
        assert_eq!(restarted.status_of(&id(phase)).unwrap().phase, phase);
    }
}

fn record(home: &Path, op: &str, phase: &str) {
    std::fs::create_dir_all(home.join(remove::DIR)).unwrap();
    std::fs::write(
        home.join(remove::DIR).join(format!("{op}.json")),
        json!({
            "state":{"operation":op,"phase":phase,"running":0,"error":null,"updated":1},
            "request":{"operation":op,"grace_secs":0}
        })
        .to_string(),
    )
    .unwrap();
}
fn service_home() -> tempfile::TempDir {
    let h = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(h.path().join("services")).unwrap();
    std::fs::write(h.path().join("services/node.json"), "{}").unwrap();
    std::fs::write(h.path().join("keep"), "retained until confirmed").unwrap();
    h
}
struct Service {
    calls: Vec<&'static str>,
    stop_fails: bool,
    uninstall_fails: usize,
}
impl RemoveOps for Service {
    fn stop(&mut self) -> anyhow::Result<()> {
        self.calls.push("stop");
        anyhow::ensure!(!self.stop_fails, "stop refused");
        Ok(())
    }
    fn uninstall(&mut self) -> anyhow::Result<()> {
        self.calls.push("uninstall");
        if self.uninstall_fails > 0 {
            self.uninstall_fails -= 1;
            anyhow::bail!("uninstall refused");
        }
        Ok(())
    }
}

#[test]
fn removal_helper_keeps_service_order_retry_budget_failures_and_confirmation_guard() {
    for (stop_fails, uninstall_fails, phase) in [
        (false, 2, "removing"),
        (true, 0, "removing"),
        (false, 31, "removing"),
        (false, 0, "ready"),
    ] {
        let home = service_home();
        record(home.path(), "sg27-helper", phase);
        let mut svc = Service {
            calls: vec![],
            stop_fails,
            uninstall_fails,
        };
        let result = remove::run_remove(
            home.path(),
            "sg27-helper",
            &mut svc,
            Duration::ZERO,
            Duration::ZERO,
        );
        if phase == "ready" {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("not confirmed by the hub")
            );
            assert!(svc.calls.is_empty());
            assert_eq!(read(home.path(), "sg27-helper")["state"]["phase"], "ready");
        } else if stop_fails {
            assert_eq!(result.unwrap_err().to_string(), "stopping the node");
            assert_eq!(svc.calls, ["stop"]);
            let r = read(home.path(), "sg27-helper");
            assert_eq!(r["state"]["phase"], "failed");
            assert_eq!(r["state"]["error"], "stopping the node: stop refused");
        } else if uninstall_fails == 31 {
            assert_eq!(result.unwrap_err().to_string(), "uninstalling the service");
            assert_eq!(svc.calls[0], "stop");
            assert_eq!(svc.calls.iter().filter(|c| **c == "uninstall").count(), 31);
            assert_eq!(
                read(home.path(), "sg27-helper")["state"]["error"],
                "uninstalling the service: uninstall refused"
            );
        } else {
            result.unwrap();
            assert_eq!(svc.calls, ["stop", "uninstall", "uninstall", "uninstall"]);
            assert!(!home.path().exists());
        }
    }
    let home = service_home();
    record(home.path(), "sg27-no-service", "removing");
    std::fs::remove_file(home.path().join("services/node.json")).unwrap();
    let mut svc = Service {
        calls: vec![],
        stop_fails: false,
        uninstall_fails: 0,
    };
    assert!(
        remove::run_remove(
            home.path(),
            "sg27-no-service",
            &mut svc,
            Duration::ZERO,
            Duration::ZERO
        )
        .unwrap_err()
        .to_string()
        .contains("holds no node service record")
    );
    assert!(svc.calls.is_empty());
    assert!(home.path().join("keep").is_file());
}
