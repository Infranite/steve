//! The preflight: only the harness's readiness on the Nodes (installed, ready, signed in) stops a
//! Bot, for every principal alike; nothing at the Hub has to be configured for a service session.

use super::*;
use steve_store::{NewBot, NoPublisher, Store, StoreError, WS};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(d.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    for (id, harness) in [("coder", "codex"), ("writer", "claude-code")] {
        s.bots()
            .seed(
                id,
                false,
                &NewBot {
                    name: id.into(),
                    harness: harness.into(),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }
    (d, s)
}

fn code(r: Result<(), Blocked>) -> Option<&'static str> {
    r.err().map(|b| b.code)
}

/// What a Node reports of `harness`: the adapter answered, and its scratch session opened or not.
fn cap(harness: &str, session: Result<(), &str>) -> steve_node_api::HarnessCapability {
    let adapter: steve_node_api::AdapterInfo = serde_json::from_value(serde_json::json!({
        "title": harness,
        "status": "ready",
        "auth": [{"id": "login", "name": "Log in", "description": "run its login command"}],
        "session": session.ok().map(|_| serde_json::json!({"modes": [], "models": [], "efforts": []})),
        "session_error": session.err(),
    }))
    .unwrap();
    steve_node_api::HarnessCapability {
        name: harness.into(),
        version: String::new(),
        sessions: true,
        jobs: false,
        models: vec![],
        resume: false,
        cancel: false,
        permissions: false,
        images: false,
        read_attached: false,
        enforcement: vec![],
        setup: None,
        adapter: Some(adapter),
    }
}

fn preflight_of(hs: &Arc<crate::harnesses::HubHarnesses>, linked: Vec<String>) -> Arc<Preflight> {
    let hs = hs.clone();
    Preflight::new(Some(Arc::new(move || hs.readiness(&linked))))
}

/// A service principal's Bot runs on the harness's own sign-in on the Node: with the harness
/// signed in there, nothing has to be configured at the Hub. A harness the Node reports as not
/// signed in (its scratch session refused: "Authentication required") stops every principal's Bot
/// alike, pointing at the Devices page where the harness is signed in, never at a provider setting.
#[tokio::test]
async fn a_signed_in_harness_runs_for_everyone_and_one_that_is_not_is_refused_on_devices() {
    let (_d, s) = store();
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k| {
        (k == "STEVE_HOME").then(|| home.path().display().to_string())
    })
    .unwrap();
    let hs = crate::harnesses::HubHarnesses::of(
        &cfg,
        crate::system::RuntimeReports::new(Arc::new(steve_runtime::cache::Caches::new(cfg.cache))),
    );
    hs.update(vec![
        cap("codex", Ok(())),
        cap("claude-code", Err("Authentication required")),
    ])
    .await;
    let p = preflight_of(&hs, vec![]);
    let svc = Principal::Svc(WS.into());
    let me = Principal::User("owner".into());
    s.read::<_, StoreError>(|c| {
        assert_eq!(code(p.can_run(c, &svc, "coder")), None);
        assert_eq!(code(p.can_run(c, &me, "coder")), None);
        for who in [&svc, &me] {
            let b = p.can_run(c, who, "writer").unwrap_err();
            assert_eq!((b.code, b.fix), ("harness_not_ready", "devices"));
            assert!(b.reason.contains("claude-code"), "{}", b.reason);
            let lower = b.reason.to_lowercase();
            assert!(
                !lower.contains("provider") && !lower.contains("vault") && !lower.contains("key"),
                "{}",
                b.reason
            );
        }
        // the schedule's view: the service principal unless a member is named
        assert_eq!(
            code(p.can_run_schedule(c, WS, None, "writer")),
            Some("harness_not_ready")
        );
        assert_eq!(code(p.can_run_schedule(c, WS, None, "coder")), None);
        // a Bot that is not there is not this check's to refuse
        assert_eq!(code(p.can_run(c, &svc, "nobody")), None);
        Ok(())
    })
    .unwrap();
    // once the harness is signed in on the device, the same Bot runs
    hs.update(vec![cap("codex", Ok(())), cap("claude-code", Ok(()))])
        .await;
    s.read::<_, StoreError>(|c| {
        assert_eq!(code(p.can_run(c, &svc, "writer")), None);
        Ok(())
    })
    .unwrap();
    // nothing reported at all: unknown, not refused
    let unknown = Preflight::new(None);
    s.read::<_, StoreError>(|c| {
        assert_eq!(code(unknown.can_run(c, &svc, "writer")), None);
        Ok(())
    })
    .unwrap();
}

/// #234: the Hub's own Node says Claude Code is not ready, and a second Node linked but has not
/// answered yet. The second Node may well have it, so the Bot is not refused; once every linked
/// Node has answered and none has it ready, it is.
#[tokio::test]
async fn a_node_that_linked_but_has_not_answered_is_not_a_refusal() {
    let (_d, s) = store();
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k| {
        (k == "STEVE_HOME").then(|| home.path().display().to_string())
    })
    .unwrap();
    let hs = crate::harnesses::HubHarnesses::of(
        &cfg,
        crate::system::RuntimeReports::new(Arc::new(steve_runtime::cache::Caches::new(cfg.cache))),
    );
    hs.update(vec![steve_node_api::HarnessCapability {
        name: "claude-code".into(),
        version: String::new(),
        sessions: false,
        jobs: false,
        models: vec![],
        resume: false,
        cancel: false,
        permissions: false,
        images: false,
        read_attached: false,
        enforcement: vec![],
        setup: None,
        adapter: None,
    }])
    .await;
    let linked = Arc::new(std::sync::Mutex::new(vec!["n_second".to_string()]));
    let readiness: Readiness = {
        let (hs, linked) = (hs.clone(), linked.clone());
        Arc::new(move || hs.readiness(&linked.lock().unwrap()))
    };
    let p = Preflight::new(Some(readiness));
    let me = Principal::User("owner".into());
    s.read::<_, StoreError>(|c| {
        assert_eq!(code(p.can_run(c, &me, "writer")), None);
        // the second Node went away without answering: only the Hub's own Node is left
        linked.lock().unwrap().clear();
        assert_eq!(code(p.can_run(c, &me, "writer")), Some("harness_not_ready"));
        Ok(())
    })
    .unwrap();
}

/// Service/Off model jobs use local-jobs; a Hub without a model Node must only
/// wait for the linked Nodes it actually has, rather than a fictitious local one.
#[tokio::test]
async fn preflight_uses_the_actual_model_node_or_only_linked_nodes_when_absent() {
    let (_d, s) = store();
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k| {
        (k == "STEVE_HOME").then(|| home.path().display().to_string())
    })
    .unwrap();
    for model in [Some("local-jobs"), None] {
        let reports = crate::system::RuntimeReports::new(Arc::new(
            steve_runtime::cache::Caches::new(cfg.cache),
        ));
        if let Some(node) = model {
            reports.bind_model_node(node);
        }
        let hs = crate::harnesses::HubHarnesses::of(&cfg, reports.clone());
        let node = model.unwrap_or("remote");
        let p = preflight_of(&hs, vec![node.into()]);
        let svc = Principal::Svc(WS.into());
        let me = Principal::User("owner".into());
        s.read::<_, StoreError>(|c| {
            assert_eq!(code(p.can_run(c, &svc, "writer")), None);
            Ok(())
        })
        .unwrap();
        reports
            .seed(
                node,
                vec![cap("claude-code", Err("Authentication required"))],
                None,
            )
            .await;
        s.read::<_, StoreError>(|c| {
            for who in [&svc, &me] {
                let b = p.can_run(c, who, "writer").unwrap_err();
                assert_eq!((b.code, b.fix), ("harness_not_ready", "devices"));
            }
            assert_eq!(
                code(p.can_run_schedule(c, WS, None, "writer")),
                Some("harness_not_ready")
            );
            Ok(())
        })
        .unwrap();
    }
}
