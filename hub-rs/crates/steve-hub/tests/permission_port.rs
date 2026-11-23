//! Architectural fence: session requests may not be answered in the ACP reader or
//! inserted in the Hub ledger outside the resolver. One-shot model jobs are not sessions.

#[test]
fn session_permissions_have_one_ingress_and_one_hub_ledger_writer() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let node = std::fs::read_to_string(crates.join("steve-node/src/actor.rs")).unwrap();
    assert_eq!(node.matches("\"session/request_permission\"").count(), 1);
    assert_eq!(node.matches("EventBody::PermissionRequested {").count(), 1);
    // The only selected ACP answer uses the committed, attempt-bound Hub resolution.
    assert_eq!(node.matches("permission::answer(").count(), 1);
    assert!(node.contains("permission::answer(&asked, Some(res.decision))"));

    fn writers(dir: &std::path::Path, call: &str) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                if p.is_dir() {
                    writers(&p, call)
                } else if p.extension().is_some_and(|e| e == "rs") {
                    std::fs::read_to_string(p).unwrap().matches(call).count()
                } else {
                    0
                }
            })
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
