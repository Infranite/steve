//! The library's skills a Bot uses reach its session's Node: the Hub puts the ones the Bot has
//! switched on into `open_session`, as they are when the session binds.

mod node_harness;

use std::sync::Arc;

use node_harness::*;
use serde_json::{Value, json};
use steve_modules::caps::Capabilities;
use steve_modules::caps::skills::SkillIn;
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;

fn skill(v: Value) -> SkillIn {
    serde_json::from_value(v).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bots_enabled_skills_go_into_open_session() {
    let h = Hub::start().await;
    let (_id, node) = h.paired("u1").await;
    steve_store::testkit::hub::hub_fixtures::seed_bots(&h.store).unwrap();
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        h.store.clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    let svc = Capabilities::new(vault);
    svc.skill_create(
        "default",
        "u1",
        skill(json!({"name": "code-review", "description": "Review a change.", "content": "# Review", "bot": "dev"})),
    )
    .unwrap();
    let off = svc
        .skill_create(
            "default",
            "u1",
            skill(json!({"name": "drafts", "description": "Off."})),
        )
        .unwrap();
    svc.assign(
        "default",
        "u1",
        "dev",
        "skill",
        off["id"].as_str().unwrap(),
        false,
    )
    .unwrap();
    h.session("s1");
    steve_store::testkit::hub::hub_fixtures::set_sessions_bot_id_dev(&h.store).unwrap();
    let _rt = h.bind(&node, "s1");
    let open: String =
        steve_store::testkit::hub::node_checks::command_request_by_method(&h.store, "open_session")
            .unwrap();
    let open: Value = serde_json::from_str(&open).unwrap();
    assert_eq!(
        open["body"]["skills"],
        json!([{"name": "code-review", "description": "Review a change.", "content": "# Review"}]),
        "{open}"
    );
}
