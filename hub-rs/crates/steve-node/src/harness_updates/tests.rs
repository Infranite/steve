use super::*;
use crate::{NodeConfig, PolicyCeiling, Storage};
use steve_runtime::testkit::{eventually_sync, on_timeout};

#[path = "../../tests/fixtures/harness_updates.rs"]
mod fixture;
use fixture::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_change_while_requirements_run_cannot_create_a_false_success() {
    let f = Fixture::new("node", "2.0.0", "2.0.0", "2.0.0");
    let n = f.node();
    checked(&n).await;
    let hold = f.dir.path().join("requirement-hold");
    let started = f.dir.path().join("requirement-started");
    fixture::executable(
        &f.cfg.acp.node,
        &format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ -f '{}' ]; do sleep .01; done\necho v26.0.0\n",
            started.display(),
            hold.display()
        ),
    );
    f.mark("requirement-hold");
    let cmd = f.command("changed_during_admission", "2.0.0");
    let task = {
        let n = n.clone();
        let cmd = cmd.clone();
        tokio::spawn(async move { n.upgrade_harness(cmd).await })
    };
    eventually_sync(
        on_timeout(|| async { format!("requirements entered: {}", started.display()) }),
        || started.exists(),
    )
    .await;
    let prefix = managed::prefix_lock(&f.home, true).unwrap();
    for c in adapters::components("pi") {
        std::fs::write(
            managed::root(&f.home).join(c.package).join("package.json"),
            serde_json::json!({"name":c.package,"version":"1.0.0"}).to_string(),
        )
        .unwrap();
    }
    drop(prefix);
    f.unmark("requirement-hold");
    assert_eq!(task.await.unwrap().unwrap().state, Observed::Running);
    assert!(matches!(
        finished(&n, cmd.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(f.installs(), 1);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_cli_and_acp_are_checked_and_upgraded_as_separate_packages() {
    let mut f = Fixture::new("node", "1.0.0", "1.0.0", "2.0.0");
    let root = managed::root(&f.home);
    let cli = root.join("@anthropic-ai/claude-code/cli.js");
    let acp = managed::command(&f.home, "claude-code").unwrap();
    for (package, entry, v) in [
        ("@anthropic-ai/claude-code", &cli, "1.0.0"),
        ("@agentclientprotocol/claude-agent-acp", &acp, "1.5.0"),
    ] {
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(entry, "// fixture entry").unwrap();
        std::fs::write(
            root.join(package).join("package.json"),
            serde_json::json!({"name":package,"version":v}).to_string(),
        )
        .unwrap();
    }
    f.cfg.acp.adapters = HashMap::from([("claude-code".into(), acp)]);
    f.cfg.acp.claude_executable = cli;
    let n = f.node();
    let report = checked(&n).await;
    assert_eq!(
        report.harnesses[0]
            .components
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["cli", "adapter"]
    );
    assert!(
        report.harnesses[0]
            .components
            .iter()
            .all(|c| c.managed && c.state == ComponentUpdateState::Available)
    );
    let mut c = f.command("separate_cli", "2.0.0");
    c.body.harness = "claude-code".into();
    c.body.versions = BTreeMap::from([
        ("cli".into(), "2.0.0".into()),
        ("adapter".into(), "2.0.0".into()),
    ]);
    n.upgrade_harness(c.clone()).await.unwrap();
    assert!(matches!(
        finished(&n, c.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(
        managed::installed(&f.home, "@anthropic-ai/claude-code").as_deref(),
        Some("2.0.0")
    );
    assert_eq!(
        managed::installed(&f.home, "@agentclientprotocol/claude-agent-acp").as_deref(),
        Some("2.0.0")
    );
    assert_eq!(f.installs(), 1);
    n.shutdown().await;
}

struct PendingUpgrader(AtomicBool);
#[async_trait::async_trait]
impl crate::Upgrader for PendingUpgrader {
    fn build(&self) -> BuildIdentity {
        BuildIdentity {
            version: "fixture".into(),
            commit: "fixture".into(),
            target: "fixture".into(),
        }
    }
    fn maintenance_pending(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    async fn stage(&self, _: &LocalNode, u: NodeUpgrade) -> ApiResult<NodeUpgradeState> {
        self.0.store(true, Ordering::SeqCst);
        self.status(&u.operation)
    }
    async fn switch(&self, _: &LocalNode, u: UpgradeSwitch) -> ApiResult<NodeUpgradeState> {
        if u.abandon {
            self.0.store(false, Ordering::SeqCst);
        }
        self.status(&u.operation)
    }
    fn status(&self, op: &OperationId) -> ApiResult<NodeUpgradeState> {
        Ok(NodeUpgradeState {
            operation: op.clone(),
            phase: if self.maintenance_pending() {
                "staged"
            } else {
                "abandoned"
            }
            .into(),
            version: "fixture".into(),
            previous: None,
            error: None,
            updated: Timestamp(now()),
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn node_release_upgrade_and_component_installation_exclude_each_other_even_when_forced() {
    let f = Fixture::new("node", "1.0.0", "1.0.0", "2.0.0");
    let n = f.node();
    checked(&n).await;
    n.set_upgrader(Arc::new(PendingUpgrader(AtomicBool::new(false))));
    let op = OperationId::new("release").unwrap();
    let stage = NodeUpgrade {
        operation: op.clone(),
        build: BuildIdentity {
            version: "fixture".into(),
            commit: "fixture".into(),
            target: "fixture".into(),
        },
        artifact: ArtifactId::new("bundle").unwrap(),
        size: 1,
        sha256: "unused".into(),
        health_secs: 60,
    };
    let lease = adapter_setup::Lease::reserve(&n.0, "pi").unwrap();
    assert_eq!(
        n.upgrade_stage(stage.clone()).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        n.upgrade_switch(UpgradeSwitch {
            operation: op.clone(),
            abandon: false,
            force: true
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    drop(lease);
    let cli = managed::prefix_lock(&f.home, true).unwrap();
    assert_eq!(
        n.upgrade_stage(stage.clone()).await.unwrap_err().code,
        ErrorCode::Busy
    );
    drop(cli);
    assert_eq!(n.upgrade_stage(stage).await.unwrap().phase, "staged");
    assert!(!n.harness_updates(false).await.unwrap().harnesses[0].can_upgrade);
    assert_eq!(
        n.upgrade_harness(f.command("component_during_release", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(adapter_setup::Lease::reserve(&n.0, "pi").is_none());
    n.upgrade_switch(UpgradeSwitch {
        operation: op,
        abandon: true,
        force: false,
    })
    .await
    .unwrap();
    assert!(n.harness_updates(false).await.unwrap().harnesses[0].can_upgrade);
    assert_eq!(f.installs(), 0);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_node_checks_its_cli_and_acp_and_retains_check_failures() {
    let (a, b) = (
        Fixture::new("alpha", "1.0.0", "1.1.0", "2.0.0"),
        Fixture::new("beta", "1.2.0", "1.2.0", "1.2.0"),
    );
    let (n1, n2) = (a.node(), b.node());
    let (r1, r2) = tokio::join!(checked(&n1), checked(&n2));
    assert_ne!(r1.node, r2.node);
    assert_eq!(
        r1.harnesses[0]
            .components
            .iter()
            .map(|c| c.installed.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["1.0.0", "1.1.0"]
    );
    assert!(
        r1.harnesses[0]
            .components
            .iter()
            .all(|c| c.state == ComponentUpdateState::Available && c.checked.is_some())
    );
    assert!(
        r2.harnesses[0]
            .components
            .iter()
            .all(|c| c.state == ComponentUpdateState::Current)
    );
    a.mark("registry-fail");
    let failed = checked(&n1).await;
    assert!(
        failed.harnesses[0]
            .components
            .iter()
            .all(|c| c.state == ComponentUpdateState::CheckFailed && c.latest.is_none())
    );
    assert!(!failed.harnesses[0].can_upgrade);
    assert_eq!(a.installs() + b.installs(), 0);
    n1.shutdown().await;
    n2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_is_idempotent_verifies_versions_and_preserves_provider_files() {
    let f = Fixture::new("remote", "1.0.0", "1.1.0", "2.0.0");
    let n = f.node();
    checked(&n).await;
    let files = [
        f.home.join("harness/provider.json"),
        f.home.join("harness/auth.json"),
        f.cfg.acp.codex_home.join("config.toml"),
        f.cfg.acp.user_home.join("model-settings"),
    ];
    for p in &files {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"provider-and-login-state-fixture").unwrap();
    }
    f.mark("hold");
    let c = f.command("upgrade_once", "2.0.0");
    let r = n.upgrade_harness(c.clone()).await.unwrap();
    assert_eq!(r.state, Observed::Running);
    assert_eq!(
        n.upgrade_harness(c.clone()).await.unwrap().operation,
        r.operation
    );
    assert_eq!(
        n.upgrade_harness(f.command("duplicate_click", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let mut conflict = c.clone();
    conflict
        .body
        .versions
        .insert("adapter".into(), "3.0.0".into());
    assert_eq!(
        n.upgrade_harness(conflict).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    f.unmark("hold");
    assert!(matches!(
        finished(&n, c.id.clone()).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(n.upgrade_harness(c.clone()).await.unwrap().operation, c.id);
    assert!(matches!(
        n.upgrade_harness(f.command("already_installed", "2.0.0"))
            .await
            .unwrap()
            .state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(f.installs(), 1);
    let report = n.harness_updates(false).await.unwrap();
    assert!(report.harnesses[0].components.iter().all(
        |c| c.installed.as_deref() == Some("2.0.0") && c.state == ComponentUpdateState::Current
    ));
    for p in files {
        assert_eq!(
            std::fs::read(p).unwrap(),
            b"provider-and-login-state-fixture"
        );
    }
    n.shutdown().await;
    drop(n);
    let n = f.node();
    assert!(matches!(
        n.harness_updates(false).await.unwrap().harnesses[0]
            .operation
            .as_ref()
            .unwrap()
            .state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert!(matches!(
        n.upgrade_harness(c).await.unwrap().state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(f.installs(), 1);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn npm_failure_and_false_success_are_failed_receipts_without_secret_output() {
    for marker in ["fail", "wrong-version"] {
        let f = Fixture::new("node", "1.0.0", "1.0.0", "2.0.0");
        let n = f.node();
        checked(&n).await;
        f.mark(marker);
        let c = f.command("upgrade_failed", "2.0.0");
        n.upgrade_harness(c.clone()).await.unwrap();
        let r = finished(&n, c.id.clone()).await;
        assert!(
            matches!(
                r.state,
                Observed::Finished {
                    outcome: Outcome::Failed,
                    ..
                }
            ),
            "{r:?}"
        );
        assert!(
            !serde_json::to_string(&r)
                .unwrap()
                .contains("fixture-secret")
        );
        assert_eq!(n.upgrade_harness(c).await.unwrap(), r);
        f.unmark(marker);
        checked(&n).await;
        let retry = f.command("explicit_retry", "2.0.0");
        n.upgrade_harness(retry.clone()).await.unwrap();
        assert!(matches!(
            finished(&n, retry.id).await.state,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
        assert_eq!(f.installs(), 2);
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_active_work_prefix_locks_and_unchecked_targets_block_installation() {
    let f = Fixture::new("node", "1.0.0", "1.0.0", "2.0.0");
    let n = f.node();
    checked(&n).await;
    assert_eq!(
        n.upgrade_harness(f.command("not_checked", "9.9.9"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Stale
    );
    let active = f.command("active_exec", "2.0.0");
    n.accept("exec", &active, |_| Ok(Decided::accepted()))
        .unwrap();
    assert_eq!(
        n.upgrade_harness(f.command("busy", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    n.0.j.tx(|t| t.set_state(&active.id, &succeeded())).unwrap();
    let prefix = managed::prefix_lock(&f.home, false).unwrap();
    assert_eq!(
        n.upgrade_harness(f.command("prefix_locked", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    drop(prefix);
    n.0.node_upgrading.store(true, Ordering::SeqCst);
    assert_eq!(
        n.upgrade_harness(f.command("node_upgrade", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    n.0.node_upgrading.store(false, Ordering::SeqCst);
    assert_eq!(f.installs(), 0);
    n.shutdown().await;
    let mut denied = Fixture::new("denied", "1.0.0", "1.0.0", "2.0.0");
    denied.cfg.ceiling.exec = false;
    let n = denied.node();
    checked(&n).await;
    assert_eq!(
        n.upgrade_harness(denied.command("denied", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Denied
    );
    assert_eq!(denied.installs(), 0);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_observes_unknown_without_replaying_and_explicit_retry_is_required() {
    let f = Fixture::new("node", "1.0.0", "1.0.0", "2.0.0");
    let n = f.node();
    let c = f.command("interrupted", "2.0.0");
    n.accept("upgrade_harness", &c, |t| {
        t.set_installation("upgrade:pi", &c.id)?;
        let mut d = Decided::accepted();
        d.state = Observed::Running;
        Ok(d)
    })
    .unwrap();
    drop(n);
    let n = f.node();
    assert!(matches!(
        n.upgrade_harness(c).await.unwrap().state,
        Observed::Unknown { .. }
    ));
    assert_eq!(f.installs(), 0);
    checked(&n).await;
    let retry = f.command("retry_after_inspection", "2.0.0");
    n.upgrade_harness(retry.clone()).await.unwrap();
    assert!(matches!(
        finished(&n, retry.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(f.installs(), 1);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn external_and_symlinked_components_are_read_only() {
    let mut f = Fixture::new("external", "1.0.0", "1.0.0", "2.0.0");
    let outside = f.dir.path().join("external/node_modules");
    let package = outside.join("@openma/pi-acp");
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(package.join("package.json"), r#"{"version":"1.0.0"}"#).unwrap();
    std::fs::write(package.join("dist/bin.js"), "// external").unwrap();
    f.cfg
        .acp
        .adapters
        .insert("pi".into(), package.join("dist/bin.js"));
    let n = f.node();
    let report = checked(&n).await;
    assert!(report.harnesses[0].components.iter().all(|c| !c.managed));
    assert!(!report.harnesses[0].can_upgrade);
    assert_eq!(
        n.upgrade_harness(f.command("external", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    n.shutdown().await;
    drop(n);
    f.cfg
        .acp
        .adapters
        .insert("pi".into(), managed::command(&f.home, "pi").unwrap());
    let target = managed::root(&f.home).join("@openma/pi-acp");
    std::fs::remove_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&package, &target).unwrap();
    let n = f.node();
    let report = checked(&n).await;
    assert!(
        !report.harnesses[0]
            .components
            .iter()
            .find(|c| c.id == "adapter")
            .unwrap()
            .managed
    );
    let mut cmd = f.command("symlink", "2.0.0");
    cmd.body.versions.remove("pi");
    assert_eq!(
        n.upgrade_harness(cmd).await.unwrap_err().code,
        ErrorCode::Denied
    );
    assert_eq!(f.installs(), 0);
    assert_eq!(
        std::fs::read_to_string(package.join("package.json")).unwrap(),
        r#"{"version":"1.0.0"}"#
    );
    n.shutdown().await;
}
