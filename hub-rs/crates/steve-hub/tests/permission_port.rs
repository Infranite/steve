//! Architectural fence: interactive session permissions have one ingress and one Hub ledger
//! writer (`PermissionResolver`), and one-shot ACP calls never run a second approval rule engine
//! or self-approve `session/request_permission`.

use std::path::Path;

fn strip_tests(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

fn scan_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            scan_rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn session_permissions_have_one_ingress_and_one_hub_ledger_writer() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let node = std::fs::read_to_string(crates.join("steve-node/src/actor.rs")).unwrap();
    assert_eq!(node.matches("\"session/request_permission\"").count(), 1);
    assert_eq!(node.matches("EventBody::PermissionRequested {").count(), 1);
    // The only selected ACP answer uses the committed, attempt-bound Hub resolution.
    assert_eq!(node.matches("permission::answer(").count(), 1);
    assert!(node.contains("permission::answer(&asked, Some(res.decision))"));

    // One-shot ACP calls unconditionally refuse `session/request_permission` and carry no second
    // approval rule engine.
    let oneshot = std::fs::read_to_string(crates.join("steve-acp/src/oneshot.rs")).unwrap();
    let oneshot_prod = strip_tests(&oneshot);
    assert!(
        !oneshot_prod.contains("PermissionDecision::AllowOnce"),
        "oneshot.rs must never self-approve PermissionDecision::AllowOnce"
    );
    assert!(
        !oneshot_prod.contains("PermissionDecision::AllowAlways"),
        "oneshot.rs must never self-approve PermissionDecision::AllowAlways"
    );
    assert!(oneshot_prod.contains("PermissionDecision::Reject"));
    assert!(oneshot_prod.contains("steve_permission_refused_total"));
    assert!(oneshot_prod.contains("OneShotError::ToolRefused"));

    // Across every crate's production source, `allow_tool` is completely gone and
    // `session/request_permission` only exists in the interactive session actor and oneshot rejector.
    let mut prod_files = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let src_dir = entry.unwrap().path().join("src");
        if src_dir.is_dir() {
            scan_rs_files(&src_dir, &mut prod_files);
        }
    }
    let mut req_perm_files = Vec::new();
    for file in &prod_files {
        let rel = file
            .strip_prefix(crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).unwrap();
        let prod = strip_tests(&text);
        assert!(
            !text.contains("allow_tool"),
            "{rel} still references removed allow_tool rule engine"
        );
        if prod.contains("\"session/request_permission\"") {
            req_perm_files.push(rel);
        }
    }
    req_perm_files.sort();
    assert_eq!(
        req_perm_files,
        vec![
            "steve-acp/src/oneshot.rs".to_string(),
            "steve-node/src/actor.rs".to_string(),
        ],
        "unexpected production handler for session/request_permission"
    );

    fn writers(dir: &Path, call: &str) -> usize {
        let mut files = Vec::new();
        scan_rs_files(dir, &mut files);
        files
            .into_iter()
            .map(|p| std::fs::read_to_string(p).unwrap().matches(call).count())
            .sum()
    }
    let hub = crates.join("steve-hub/src");
    let perm = std::fs::read_to_string(hub.join("wire/node_runtime/perm.rs")).unwrap();
    assert_eq!(writers(&hub, "node_write::insert_permission_in("), 1);
    assert_eq!(writers(&hub, "steve_core::approval::resolve("), 1);
    assert!(perm.contains("Request::Permission(request)"));
    assert!(perm.contains("steve_core::approval::resolve("));
    assert!(perm.contains("node_write::insert_permission_in("));
}
