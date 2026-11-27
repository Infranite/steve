use super::*;
use crate::{NodeConfig, PolicyCeiling, Storage};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
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
    for c in steve_harness_spec::components("pi") {
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
        Code::Busy
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
        Code::Busy
    );
    drop(lease);
    let cli = managed::prefix_lock(&f.home, true).unwrap();
    assert_eq!(
        n.upgrade_stage(stage.clone()).await.unwrap_err().code,
        Code::Busy
    );
    drop(cli);
    assert_eq!(n.upgrade_stage(stage).await.unwrap().phase, "staged");
    assert!(!n.harness_updates(false).await.unwrap().harnesses[0].can_upgrade);
    assert_eq!(
        n.upgrade_harness(f.command("component_during_release", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
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
        Code::Busy
    );
    let mut conflict = c.clone();
    conflict
        .body
        .versions
        .insert("adapter".into(), "3.0.0".into());
    assert_eq!(
        n.upgrade_harness(conflict).await.unwrap_err().code,
        Code::Conflict
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
        Code::Stale
    );
    let active = f.command("active_exec", "2.0.0");
    n.accept("exec", &active, |_| Ok(Decided::accepted()))
        .unwrap();
    let queued = f.command("queued_while_exec_active", "2.0.0");
    let queued_receipt = n.upgrade_harness(queued.clone()).await.unwrap();
    assert!(
        matches!(queued_receipt.state, Observed::Waiting { .. }),
        "active work queues the upgrade until idle: {queued_receipt:?}"
    );
    assert_eq!(f.installs(), 0, "must not start installer while busy");
    n.0.j.tx(|t| t.set_state(&active.id, &succeeded())).unwrap();
    n.0.wake.notify_one();
    assert!(matches!(
        finished(&n, queued.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert_eq!(f.installs(), 1);
    // Reset installed versions back to 1.0.0 so we can test external prefix lock and node upgrade.
    for c in steve_harness_spec::components("pi") {
        std::fs::write(
            managed::root(&f.home).join(c.package).join("package.json"),
            serde_json::json!({"name":c.package,"version":"1.0.0"}).to_string(),
        )
        .unwrap();
    }
    checked(&n).await;
    let prefix = managed::prefix_lock(&f.home, false).unwrap();
    assert_eq!(
        n.upgrade_harness(f.command("prefix_locked", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    drop(prefix);
    n.0.node_upgrading.store(true, Ordering::SeqCst);
    assert_eq!(
        n.upgrade_harness(f.command("node_upgrade", "2.0.0"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    n.0.node_upgrading.store(false, Ordering::SeqCst);
    assert_eq!(f.installs(), 1);
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
        Code::Forbidden
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
        Code::Unsupported
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
        Code::Forbidden
    );
    assert_eq!(f.installs(), 0);
    assert_eq!(
        std::fs::read_to_string(package.join("package.json")).unwrap(),
        r#"{"version":"1.0.0"}"#
    );
    n.shutdown().await;
}

#[test]
fn brew_info_json_extracts_cask_and_formula_versions() {
    let cask = r#"{"formulae":[],"casks":[{"token":"claude-code","version":"2.1.315,abc"}]}"#;
    assert_eq!(
        parse_brew_latest_json(BrewKind::Cask, cask).as_deref(),
        Some("2.1.315")
    );
    let formula = r#"{"formulae":[{"versions":{"stable":"0.171.0"}}],"casks":[]}"#;
    assert_eq!(
        parse_brew_latest_json(BrewKind::Formula, formula).as_deref(),
        Some("0.171.0")
    );
    assert_eq!(parse_brew_latest_json(BrewKind::Cask, "{}"), None);
}

fn put_active_session(n: &LocalNode, sid: &str, harness: &str, active: bool) {
    let session = SessionId::new(sid).unwrap();
    let ws = WorkspaceRef {
        node: n.id().clone(),
        id: WorkspaceId::new("ws_1").unwrap(),
    };
    let rev = Revision::new("rev_1").unwrap();
    let choice = HarnessChoice {
        harness: harness.into(),
        model: None,
        effort: None,
    };
    let open = OpenSession {
        session: session.clone(),
        workspace: ws,
        choice: choice.clone(),
        policy: ExecutionPolicy {
            revision: rev.clone(),
            permissions: PermissionMode::Ask,
            sandbox: SandboxRequirement::None,
            readable: vec![],
            network: NetworkScope::Unrestricted,
            tools: GrantId::new("g").unwrap(),
        },
        resume: None,
        env: vec![],
        state: None,
        artifact_view: None,
        skills: vec![],
        system_context: None,
    };
    let view = SessionView {
        id: session.clone(),
        revision: rev,
        phase: if active {
            SessionPhase::Active
        } else {
            SessionPhase::Ready
        },
        active: active.then(|| ExecutionRef::Turn {
            session,
            turn: TurnId::new("turn_1").unwrap(),
            attempt: AttemptId::new("att_1").unwrap(),
        }),
        effective: choice,
        effort_mapping: None,
        effective_permissions: PermissionMode::Ask,
        guarantees: vec![],
        resume: None,
    };
    n.0.j
        .tx(|t| {
            t.put_session(&crate::journal::SessionRow {
                open,
                view,
                incarnation: n.0.incarnation,
                acp_id: None,
            })
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn external_homebrew_cask_upgrade_handles_success_failure_unknown_and_active_session_queue() {
    let mut f = Fixture::new("brew-node", "1.0.0", "1.0.0", "2.0.0");
    let cask_bin = f
        .dir
        .path()
        .join("homebrew/Caskroom/claude-code/2.1.310/claude");
    let brew_link = f.dir.path().join("homebrew/bin/claude");
    let ver_file = f.dir.path().join("claude-version");
    let brew_log = f.dir.path().join("brew.log");
    std::fs::write(&ver_file, "2.1.310").unwrap();
    fixture::executable(
        &cask_bin,
        &format!(
            "#!/bin/sh\necho \"$(cat '{}') (Claude Code)\"\n",
            ver_file.display()
        ),
    );
    std::fs::create_dir_all(brew_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&cask_bin, &brew_link).unwrap();

    // Fake `brew` placed next to `cfg.acp.node` so PATH is untouched.
    let base_json = serde_json::to_string(&f.dir.path()).unwrap();
    fixture::executable(
        &f.dir.path().join("brew"),
        &format!(
            r#"#!/usr/bin/env python3
import json, pathlib, sys
base = pathlib.Path({base_json})
args = sys.argv[1:]
with (base / 'brew.log').open('a') as fp: fp.write(json.dumps(args) + '\n')
if args[:3] == ['info', '--json=v2', '--cask']:
    latest = (base / 'brew-latest').read_text().strip()
    print(json.dumps({{"formulae": [], "casks": [{{"token": args[3], "version": latest}}]}}))
    sys.exit(0)
if args[:2] == ['upgrade', '--cask']:
    if (base / 'brew-fail').exists():
        print('fixture-secret-brew-token', file=sys.stderr)
        sys.exit(23)
    latest = (base / 'brew-latest').read_text().strip()
    (base / 'claude-version').write_text(latest)
    sys.exit(0)
sys.exit(1)
"#
        ),
    );
    std::fs::write(f.dir.path().join("brew-latest"), "2.1.315").unwrap();

    f.cfg.acp.adapters = HashMap::from([(
        "claude-code".into(),
        managed::command(&f.home, "claude-code").unwrap(),
    )]);
    f.cfg.acp.claude_executable = brew_link.clone();

    let n = f.node();
    let report = checked(&n).await;
    let claude = report
        .harnesses
        .iter()
        .find(|h| h.harness == "claude-code")
        .unwrap();
    let cli = claude.components.iter().find(|c| c.id == "cli").unwrap();
    assert_eq!(cli.source, ComponentInstallSource::HomebrewCask);
    assert_eq!(cli.upgrade_method, UpgradeMethod::HomebrewCask);
    assert_eq!(
        cli.upgrade_command.as_deref(),
        Some("brew upgrade --cask claude-code")
    );
    assert!(cli.can_upgrade);
    assert_eq!(cli.state, ComponentUpdateState::Available);
    assert_eq!(cli.installed.as_deref(), Some("2.1.310"));
    assert_eq!(cli.latest.as_deref(), Some("2.1.315"));
    assert!(claude.can_upgrade);

    // 1. Failure hides stderr secrets and surfaces a visible safe detail.
    f.mark("brew-fail");
    let fail_cmd = Command {
        id: OperationId::new("brew_fail").unwrap(),
        issued_at: Timestamp(1),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "claude-code".into(),
            versions: BTreeMap::from([("cli".into(), "2.1.315".into())]),
            self_update: vec![],
        },
    };
    n.upgrade_harness(fail_cmd.clone()).await.unwrap();
    let fail_receipt = finished(&n, fail_cmd.id).await;
    match &fail_receipt.state {
        Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(err),
            ..
        } => {
            assert!(err.safe_detail.contains("Homebrew cask upgrade"));
            assert!(!err.safe_detail.contains("fixture-secret-brew-token"));
        }
        other => panic!("expected failed receipt, got {other:?}"),
    }
    f.unmark("brew-fail");

    // 2. Active session on claude-code queues the upgrade in Waiting until idle.
    put_active_session(&n, "sess_claude_1", "claude-code", true);
    let busy_report = n.harness_updates(false).await.unwrap();
    let busy_claude = busy_report
        .harnesses
        .iter()
        .find(|h| h.harness == "claude-code")
        .unwrap();
    assert_eq!(busy_claude.active_sessions, 1);
    assert!(
        busy_claude.can_upgrade,
        "harness with active sessions stays clickable to queue until idle"
    );

    let upgrades_before = std::fs::read_to_string(&brew_log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"upgrade\""))
        .count();
    let queued_cmd = Command {
        id: OperationId::new("brew_queued").unwrap(),
        issued_at: Timestamp(2),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "claude-code".into(),
            versions: BTreeMap::from([("cli".into(), "2.1.315".into())]),
            self_update: vec![],
        },
    };
    let queued_receipt = n.upgrade_harness(queued_cmd.clone()).await.unwrap();
    assert!(
        matches!(queued_receipt.state, Observed::Waiting { .. }),
        "expected Waiting while session active, got {queued_receipt:?}"
    );
    // Duplicate click while queued returns Busy.
    let dup_cmd = Command {
        id: OperationId::new("brew_queued_dup").unwrap(),
        ..queued_cmd.clone()
    };
    assert_eq!(
        n.upgrade_harness(dup_cmd).await.unwrap_err().code,
        Code::Busy
    );
    let upgrades_while_busy = std::fs::read_to_string(&brew_log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"upgrade\""))
        .count();
    assert_eq!(
        upgrades_while_busy, upgrades_before,
        "brew upgrade must not run while a session is active"
    );

    // End the session -> queued upgrade runs and verifies 2.1.315.
    put_active_session(&n, "sess_claude_1", "claude-code", false);
    n.0.wake.notify_one();
    let ok_receipt = finished(&n, queued_cmd.id).await;
    assert!(
        matches!(
            ok_receipt.state,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "{ok_receipt:?}"
    );
    let after = n.harness_updates(false).await.unwrap();
    let after_cli = after
        .harnesses
        .iter()
        .find(|h| h.harness == "claude-code")
        .unwrap()
        .components
        .iter()
        .find(|c| c.id == "cli")
        .unwrap();
    assert_eq!(after_cli.installed.as_deref(), Some("2.1.315"));
    assert_eq!(after_cli.state, ComponentUpdateState::Current);

    // 3. Restart while queued/running becomes Unknown without replaying.
    std::fs::write(f.dir.path().join("brew-latest"), "2.1.316").unwrap();
    checked(&n).await;
    let interrupted = Command {
        id: OperationId::new("brew_interrupted").unwrap(),
        issued_at: Timestamp(3),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "claude-code".into(),
            versions: BTreeMap::from([("cli".into(), "2.1.316".into())]),
            self_update: vec![],
        },
    };
    n.accept("upgrade_harness", &interrupted, |t| {
        t.set_installation("upgrade:claude-code", &interrupted.id)?;
        let mut d = Decided::accepted();
        d.state = Observed::Waiting { on: vec![] };
        Ok(d)
    })
    .unwrap();
    drop(n);
    let n = f.node();
    assert!(matches!(
        n.upgrade_harness(interrupted).await.unwrap().state,
        Observed::Unknown { .. }
    ));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn external_standalone_self_update_runs_without_npm_or_brew() {
    let mut f = Fixture::new("standalone-node", "1.0.0", "1.0.0", "2.0.0");
    // Remove fake npm so this proves standalone self-update needs neither npm nor brew.
    std::fs::remove_file(f.dir.path().join("npm")).unwrap();
    let agy_bin = f.cfg.acp.user_home.join(".local/bin/agy");
    let agy_ver = f.dir.path().join("agy-version");
    std::fs::write(&agy_ver, "1.5.16").unwrap();
    fixture::executable(
        &agy_bin,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then cat '{}'; echo; exit 0; fi\nif [ \"$1\" = \"update\" ]; then printf '1.6.0' > '{}'; exit 0; fi\nexit 1\n",
            agy_ver.display(),
            agy_ver.display()
        ),
    );
    f.cfg.acp.adapters = HashMap::from([("antigravity".into(), agy_bin)]);
    let n = f.node();
    let report = checked(&n).await;
    let agy = report
        .harnesses
        .iter()
        .find(|h| h.harness == "antigravity")
        .unwrap();
    let cli = agy.components.iter().find(|c| c.id == "cli").unwrap();
    assert_eq!(cli.source, ComponentInstallSource::Standalone);
    assert_eq!(cli.upgrade_method, UpgradeMethod::SelfUpdate);
    assert_eq!(cli.upgrade_command.as_deref(), Some("agy update"));
    assert_eq!(cli.state, ComponentUpdateState::CheckOnUpgrade);
    assert_eq!(cli.installed.as_deref(), Some("1.5.16"));
    assert!(cli.can_upgrade);
    assert!(agy.can_upgrade);

    let cmd = Command {
        id: OperationId::new("agy_self_update").unwrap(),
        issued_at: Timestamp(1),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "antigravity".into(),
            versions: BTreeMap::new(),
            self_update: vec!["cli".into()],
        },
    };
    n.upgrade_harness(cmd.clone()).await.unwrap();
    let r = finished(&n, cmd.id).await;
    assert!(
        matches!(
            r.state,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "{r:?}"
    );
    let after = n.harness_updates(false).await.unwrap();
    let after_cli = after.harnesses[0]
        .components
        .iter()
        .find(|c| c.id == "cli")
        .unwrap();
    assert_eq!(after_cli.installed.as_deref(), Some("1.6.0"));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn global_npm_component_is_upgraded_via_npm_global_prefix() {
    let mut f = Fixture::new("global-npm-node", "1.0.0", "1.0.0", "2.0.0");
    let global_prefix = f.dir.path().join("global-npm");
    let pkg_dir = global_prefix.join("lib/node_modules/@openai/codex");
    let entry = pkg_dir.join("bin/codex.js");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(
        pkg_dir.join("package.json"),
        r#"{"name":"@openai/codex","version":"1.0.0"}"#,
    )
    .unwrap();
    fixture::executable(&entry, "#!/bin/sh\necho codex-cli 1.0.0\n");

    // Override fake npm to also handle `-g --prefix <p>` (writes into `<p>/lib/node_modules`).
    let base = serde_json::to_string(&f.dir.path()).unwrap();
    fixture::executable(
        &f.dir.path().join("npm"),
        &format!(
            r#"#!/usr/bin/env python3
import json, pathlib, sys
base = pathlib.Path({base})
args = sys.argv[1:]
if args[0] == 'view':
    print(json.dumps('2.0.0'))
    sys.exit(0)
prefix = pathlib.Path(args[args.index('--prefix') + 1])
root = (prefix / 'lib' / 'node_modules') if '-g' in args else (prefix / 'node_modules')
for spec in args:
    if not spec.startswith('@') or '@' not in spec[1:]: continue
    pkg, ver = spec.rsplit('@', 1)
    d = root / pkg
    d.mkdir(parents=True, exist_ok=True)
    (d / 'package.json').write_text(json.dumps({{'name': pkg, 'version': ver}}))
"#
        ),
    );

    // Point codex CLI via PATH? Better: put codex adapter on a single-package kimi-code in global npm.
    let kimi_pkg = global_prefix.join("lib/node_modules/@moonshot-ai/kimi-code");
    let kimi_entry = kimi_pkg.join("dist/main.mjs");
    std::fs::create_dir_all(kimi_entry.parent().unwrap()).unwrap();
    std::fs::write(
        kimi_pkg.join("package.json"),
        r#"{"name":"@moonshot-ai/kimi-code","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(&kimi_entry, "// kimi\n").unwrap();
    f.cfg.acp.adapters = HashMap::from([("kimi-code".into(), kimi_entry)]);

    let n = f.node();
    let report = checked(&n).await;
    let kimi = report
        .harnesses
        .iter()
        .find(|h| h.harness == "kimi-code")
        .unwrap();
    let comp = &kimi.components[0];
    assert_eq!(comp.source, ComponentInstallSource::NpmGlobal);
    assert_eq!(comp.upgrade_method, UpgradeMethod::NpmGlobal);
    assert!(comp.can_upgrade);
    assert_eq!(comp.state, ComponentUpdateState::Available);

    let cmd = Command {
        id: OperationId::new("kimi_global_npm").unwrap(),
        issued_at: Timestamp(1),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "kimi-code".into(),
            versions: BTreeMap::from([(comp.id.clone(), "2.0.0".into())]),
            self_update: vec![],
        },
    };
    n.upgrade_harness(cmd.clone()).await.unwrap();
    assert!(matches!(
        finished(&n, cmd.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    let after = n.harness_updates(false).await.unwrap();
    assert_eq!(
        after.harnesses[0].components[0].installed.as_deref(),
        Some("2.0.0")
    );
    assert_eq!(
        after.harnesses[0].components[0].state,
        ComponentUpdateState::Current
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_check_during_inflight_sweep_queues_follow_up_pass() {
    let f = Fixture::new("coalesce", "1.0.0", "1.0.0", "2.0.0");
    let hold = f.dir.path().join("view-hold");
    let started = f.dir.path().join("view-started");
    let latest_file = f.dir.path().join("latest");
    // Read `latest` before pausing on `view-hold` so the in-flight sweep captures "2.0.0" while
    // a concurrent `harness_updates(true)` advances `latest` to "3.0.0".
    fixture::executable(
        &f.dir.path().join("npm"),
        &format!(
            "#!/bin/sh\nv=$(cat '{}')\ntouch '{}'\nwhile [ -f '{}' ]; do sleep .01; done\nprintf '\"%s\"\\n' \"$v\"\n",
            latest_file.display(),
            started.display(),
            hold.display()
        ),
    );
    f.mark("view-hold");
    let n = f.node();
    let first = n.harness_updates(true).await.unwrap();
    assert!(first.checking);
    eventually_sync(
        on_timeout(|| async { "in-flight check sweep started (view-started file missing)".into() }),
        || started.exists(),
    )
    .await;
    std::fs::write(&latest_file, "3.0.0").unwrap();
    let during = n.harness_updates(true).await.unwrap();
    assert!(during.checking);
    f.unmark("view-hold");
    let report = checked(&n).await;
    assert!(
        report.harnesses[0]
            .components
            .iter()
            .all(|c| c.latest.as_deref() == Some("3.0.0")),
        "queued fresh check must observe latest 3.0.0 after in-flight sweep completes: {report:?}"
    );
    n.shutdown().await;
}
