//! Cleaning up what an identity brought in (`steve_channels::cleanup`): the dry run, the text
//! purge, the full cleanup with everything referenced kept, a disconnected identity by its
//! account, the backup taken first, the leftovers list, and the confirmation.

#[path = "support/cleanup_backup.rs"]
mod cleanup_backup;
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;
use steve_store::testkit::channels::inspect;
use steve_store::testkit::channels::seed;
use steve_store::testkit::channels::seed::people::Bot;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::FeishuApp;
use steve_channels::fake::feishu::{FakeFeishu, ids};
use steve_channels::{Channels, channels_api, space_decisions};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

struct H {
    env: TestEnv,
    _keys: TempDir,
    _fake: FakeFeishu,
    api: Router,
}

async fn setup() -> (H, String, String) {
    setup_with_backup(Some(false)).await
}

async fn setup_with_backup(fail: Option<bool>) -> (H, String, String) {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        space_decisions::register(r);
    });
    steve_store::testkit::channels::seed::people::bots(env.store(), &[Bot::default()]).unwrap();
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let app = Arc::new(FeishuApp::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None),
    )
    .provider(app.clone())
    .usage(Arc::new(steve_channels::usage::ChannelUsage))
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .adapter(app)
        .build();
    let routes = channels_api::routes(ch);
    let routes = if let Some(fail) = fail {
        let backup = cleanup_backup::port(
            env.store().path().to_owned(),
            env.dir.path().join("backups"),
            fail,
        );
        routes.layer(axum::Extension(backup))
    } else {
        routes
    };
    let api = connections::routes(conns)
        .merge(routes)
        .with_state(env.modules.clone());
    let h = H {
        env,
        _keys: keys,
        _fake: fake,
        api,
    };
    let (st, v) = h
        .call(
            "POST",
            "/channels",
            Some(json!({
                "adapter": "feishu_app",
                "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
                "config": {"api_base": h._fake.base_url()},
            })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let con = v["connection"]["id"]
        .as_str()
        .or(v["connection_id"].as_str())
        .or(v["id"].as_str())
        .expect("connection id")
        .to_string();
    let acc = inspect::connections::account_id(h.env.store(), &con).unwrap();
    (h, con, acc)
}

impl H {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call_in_workspace(method, path, body, WS).await
    }

    async fn call_in_workspace(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        ws: &str,
    ) -> (u16, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "hub.test:8440");
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        let mut req = req
            .body(match body {
                Some(b) => Body::from(b.to_string()),
                None => Body::empty(),
            })
            .expect("request");
        req.extensions_mut().insert(caller(ws, ME));
        let res = self.api.clone().oneshot(req).await.expect("infallible");
        let status = res.status().as_u16();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        let v = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, v)
    }

    /// Spaces of `con` (or, `con` empty, of disconnected account `acc`): 客户群 with an auto
    /// contact (Ann), a confirmed one (Bob) and a shared auto one (Cat); 项目群 whose one message
    /// an item points at (Cat is there too), and 空群.
    fn seed(&self, p: &str, con: &str, acc: &str) {
        seed::spaces::seed_cleanup_spaces(
            self.env.store(),
            p,
            (!con.is_empty()).then_some(con),
            acc,
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_then_text_purge() {
    let (h, con, _) = setup().await;
    let base = inspect::spaces::spaces_for_connection(h.env.store(), &con).unwrap();
    h.seed("", &con, "");

    // confirm is required, and the target
    let (st, _) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"connection_id": con, "level": "text"})),
        )
        .await;
    assert_eq!(st, 400);
    let (st, _) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"level": "text"})),
        )
        .await;
    assert_eq!(st, 400);
    let (st, _) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"connection_id": "con_nope", "level": "text"})),
        )
        .await;
    assert_eq!(st, 404);

    // dry run: m1, m2, m4 lose their text; m3 is an item's, kept with its text
    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"connection_id": con, "level": "text"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["messages"], 3, "{v}");
    assert_eq!(v["spaces"], 2 + base, "{v}");
    assert_eq!(v["contacts"], 0);
    assert_eq!(v["kept_count"], 1);
    assert_eq!(
        v["kept"][0],
        json!({"kind": "message", "id": "m3", "name": "项目群", "reason": "被事项引用"})
    );
    assert!(v.get("backup").is_none());
    assert_eq!(
        inspect::messages::count_chan_messages_by_text_null(h.env.store()).unwrap(),
        0,
        "a preview changes nothing"
    );

    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"connection_id": con, "level": "text", "confirm": true})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["messages"], 3);
    let backup = v["backup"].as_str().expect("backup");
    assert!(steve_types::backup::SnapshotId::try_from(backup.to_owned()).is_ok());
    assert_eq!(
        inspect::messages::count_chan_messages_by_text_null_count(h.env.store()).unwrap(),
        3
    );
    assert_eq!(
        inspect::messages::message_text_by_m3(h.env.store()).unwrap(),
        "帮我跟进报价"
    );
    assert_eq!(
        inspect::messages::count_chan_messages(h.env.store()).unwrap(),
        4,
        "rows stay"
    );
    assert_eq!(
        inspect::signals::count_ext_events_by_ev1(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::audit::count_audit_by_channel_cleanup(h.env.store(), backup).unwrap(),
        1
    );
    // nothing left to purge
    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"connection_id": con, "level": "text"})),
        )
        .await;
    assert_eq!((st, v["messages"].as_i64()), (200, Some(0)));
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_but_what_is_referenced() {
    let (h, con, acc) = setup().await;
    h.seed("", &con, "");
    // Cat is also in a group of another account; Bob was confirmed by hand
    seed::people::seed_everything_but_what_is_referenced(h.env.store()).unwrap();
    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"connection_id": con, "level": "all"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let kept: Vec<(String, String)> = v["kept"]
        .as_array()
        .expect("kept")
        .iter()
        .map(|k| {
            (
                format!(
                    "{}:{}",
                    k["kind"].as_str().unwrap_or(""),
                    k["id"].as_str().unwrap_or("")
                ),
                k["reason"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert!(
        kept.contains(&("message:m3".into(), "被事项引用".into())),
        "{kept:?}"
    );
    assert!(
        kept.contains(&("space:sp_b".into(), "里面有被引用的消息".into())),
        "{kept:?}"
    );
    assert!(
        kept.contains(&("contact:bob".into(), "你确认过".into())),
        "{kept:?}"
    );
    assert!(
        kept.contains(&("contact:cat".into(), "在别的群里".into())),
        "{kept:?}"
    );
    assert_eq!(v["contacts"], 1, "Ann: {v}");
    assert_eq!(v["accounts"], 1);
    assert_eq!(v["messages_purged"], 1, "m4 in the kept 项目群");
    let spaces = v["spaces"].as_i64().expect("spaces");
    assert!(spaces >= 2, "客户群 and 空群: {v}");

    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"connection_id": con, "level": "all", "confirm": true})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        inspect::spaces::count_spaces_by_ids(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::spaces::count_spaces_by_sp_b(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::messages::count_chan_messages_count_2(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::spaces::count_space_members(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::signals::count_ext_events_by_ev1_count(h.env.store()).unwrap(),
        0,
        "an unused signal goes"
    );
    assert_eq!(
        inspect::messages::message_text_by_m3(h.env.store()).unwrap(),
        "帮我跟进报价"
    );
    assert_eq!(
        inspect::messages::count_chan_messages_by_m4_and_text_null(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::people::count_parties_by_ann(h.env.store()).unwrap(),
        0,
        "auto contact deleted"
    );
    assert_eq!(
        inspect::people::count_accounts_by_a_ann(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::people::count_account_handles(h.env.store()).unwrap(),
        0
    );
    assert_eq!(inspect::people::count_links(h.env.store()).unwrap(), 0);
    assert_eq!(
        inspect::people::count_parties_by_ids(h.env.store()).unwrap(),
        2
    );
    // the identity itself is untouched
    assert_eq!(
        inspect::people::account_count(h.env.store(), &acc).unwrap(),
        1
    );
    assert_eq!(
        inspect::connections::count_by_id(h.env.store(), &con).unwrap(),
        1
    );
    assert!(
        steve_types::backup::SnapshotId::try_from(v["backup"].as_str().unwrap().to_owned()).is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnected_identity_by_its_account() {
    let (h, _con, _) = setup().await;
    seed::people::seed_disconnected_identity_by_its_account(h.env.store()).unwrap();
    h.seed("x", "", "acc_old");

    let (st, v) = h.call("GET", "/channels/cleanup/leftovers", None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        v,
        json!([{"account_id": "acc_old", "name": "旧应用", "system_id": "feishu", "spaces": 3, "messages": 4}])
    );

    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"account_id": "acc_old", "level": "all", "confirm": true})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (
            v["spaces"].as_i64(),
            v["messages"].as_i64(),
            v["contacts"].as_i64()
        ),
        (Some(2), Some(2), Some(1)),
        "{v}"
    );
    assert_eq!(v["name"], "旧应用");
    assert_eq!(
        inspect::spaces::count_spaces_count(h.env.store()).unwrap(),
        1,
        "项目群 stays"
    );
    let (_, v) = h.call("GET", "/channels/cleanup/leftovers", None).await;
    assert_eq!(v[0]["spaces"], 1);
    let (st, _) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"account_id": "acc_nope", "level": "all"})),
        )
        .await;
    assert_eq!(st, 404);
}

/// One space per chat: a space your identity sees is its to clean only while no other live identity
/// sees it; one it is an identity of (first seen through another, since gone) counts as its own.
#[tokio::test(flavor = "multi_thread")]
async fn only_spaces_no_other_live_identity_sees() {
    let (h, con, _) = setup().await;
    let base = inspect::spaces::spaces_for_connection(h.env.store(), &con).unwrap();
    h.seed("", &con, "");
    seed::people::seed_only_spaces_no_other_live_identity_sees(
        h.env.store(),
        &format!(r#"{{"app_connection":"{}"}}"#, con),
        &con,
    )
    .unwrap();
    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup/preview",
            Some(json!({"connection_id": con, "level": "all"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // 客户群 (sp_a) is yours too: kept out; 空群 (sp_c, the other identity left) and sp_d count;
    // 项目群 (sp_b) has an item's message
    assert_eq!(v["spaces"], 2 + base, "{v}");
    let (st, v) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"connection_id": con, "level": "all", "confirm": true})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        inspect::spaces::count_spaces_by_ids_count(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::spaces::count_space_identities(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::spaces::count_spaces_by_sp_a(h.env.store()).unwrap(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_decisions_are_previewed_and_deleted_atomically() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    seed::decisions::seed_standalone_decisions_are_previewed_and_deleted_atomically(h.env.store())
        .unwrap();
    let req = json!({"connection_id":con,"level":"all"});
    let (st, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!(preview["decisions"], 2);
    let mut run = req.clone();
    run["confirm"] = json!(true);
    let (st, _) = h.call("POST", "/channels/cleanup", Some(run.clone())).await;
    assert_ne!(st, 200);
    assert_eq!(
        inspect::decisions::count_decisions_by_ids(h.env.store()).unwrap(),
        2
    );
    run["decision_ids"] = preview["decision_ids"].clone();
    seed::harness::install_fail_cleanup(h.env.store()).unwrap();
    let (st, _) = h.call("POST", "/channels/cleanup", Some(run.clone())).await;
    assert_ne!(st, 200);
    assert_eq!(
        inspect::decisions::count_decisions_by_ids(h.env.store()).unwrap(),
        2
    );
    assert_eq!(
        inspect::messages::message_decision_id_by_m1(h.env.store()).unwrap(),
        "d1"
    );
    seed::harness::remove_fail_cleanup(h.env.store()).unwrap();
    let (st, result) = h.call("POST", "/channels/cleanup", Some(run)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(result["decisions"], 2);
    assert_eq!(
        inspect::decisions::count_decisions_by_ids(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::messages::count_chan_messages_count_3(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::signals::count_ext_events_count_4(h.env.store()).unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_decision_and_changed_preview_are_not_deleted() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    seed::decisions::seed_shared_decision_and_changed_preview_are_not_deleted(h.env.store())
        .unwrap();
    let req = json!({"connection_id":con,"level":"all"});
    let (_, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(preview["decisions"], 0);
    seed::messages::clear_message_decision_id(h.env.store()).unwrap();
    let mut run = req;
    run["confirm"] = json!(true);
    run["decision_ids"] = preview["decision_ids"].clone();
    let (st, _) = h.call("POST", "/channels/cleanup", Some(run)).await;
    assert_ne!(st, 200);
    assert_eq!(
        inspect::decisions::count_decisions_by_shared(h.env.store()).unwrap(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_workspace_and_item_decisions_remain() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    seed::decisions::seed_decision_foreign(h.env.store()).unwrap();
    let req = json!({"connection_id":con,"level":"all","confirm":true});
    let (st, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!(preview["decisions"], 0);
    let (st, result) = h.call("POST", "/channels/cleanup", Some(req)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        inspect::decisions::count_decisions_by_ids_count(h.env.store()).unwrap(),
        2
    );
    assert_eq!(
        inspect::spaces::count_spaces_by_ids(h.env.store()).unwrap(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_workspace_cannot_preview_or_delete() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    for path in ["/channels/cleanup/preview", "/channels/cleanup"] {
        let (st, _) = h
            .call_in_workspace(
                "POST",
                path,
                Some(json!({"connection_id":con,"level":"all","confirm":true})),
                "other",
            )
            .await;
        assert_ne!(st, 200);
    }
    assert_eq!(
        inspect::messages::count_chan_messages_by_m1(h.env.store()).unwrap(),
        1
    );
}

fn seed_output_records(h: &H, con: &str) {
    seed::delivery::seed_seed_output_records(h.env.store(), &con).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn output_intents_follow_text_purge_without_losing_fingerprints_or_receipts() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    seed_output_records(&h, &con);
    let (st, result) = h
        .call(
            "POST",
            "/channels/cleanup",
            Some(json!({"connection_id":con,"level":"text","confirm":true})),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        inspect::delivery::output_delivery_content_by_output_delete(h.env.store()).unwrap(),
        "{}"
    );
    assert_eq!(
        inspect::delivery::output_delivery_fingerprint_by_output_delete(h.env.store()).unwrap(),
        "original-hash"
    );
    assert_eq!(
        inspect::delivery::count_output_deliveries_by_output_delete(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::delivery::output_delivery_markdown_by_output_keep(h.env.store()).unwrap(),
        "retained",
        "referenced messages remain protected"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_foreign_keys_follow_full_cleanup_and_rollback() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    seed_output_records(&h, &con);
    let req = json!({"connection_id":con,"level":"all","confirm":true});
    seed::harness::install_fail_output_cleanup(h.env.store()).unwrap();
    assert_ne!(
        h.call("POST", "/channels/cleanup", Some(req.clone()))
            .await
            .0,
        200
    );
    assert_eq!(
        inspect::delivery::count_output_deliveries(h.env.store()).unwrap(),
        2
    );
    assert_eq!(
        inspect::output_channel_preferences::count_output_channel_preferences(h.env.store())
            .unwrap(),
        2
    );
    seed::harness::remove_fail_output_cleanup(h.env.store()).unwrap();
    let (st, result) = h.call("POST", "/channels/cleanup", Some(req)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        inspect::delivery::count_output_deliveries_by_output_delete_count(h.env.store()).unwrap(),
        0
    );
    assert_eq!(
        inspect::delivery::count_output_deliveries_by_output_keep(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::output_channel_preferences::count_output_channel_preferences_count(h.env.store())
            .unwrap(),
        0
    );
    assert_eq!(
        inspect::output_channel_preferences::count_output_channel_preferences_count_2(
            h.env.store()
        )
        .unwrap(),
        1
    );
    assert_eq!(
        inspect::pragma_foreign_key_check::count_pragma_foreign_key_check(h.env.store()).unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_or_failed_backup_capability_prevents_cleanup() {
    for fail in [None, Some(true)] {
        let (h, con, acc) = setup_with_backup(fail).await;
        h.seed("", &con, &acc);
        let request = json!({"connection_id":con,"level":"all","confirm":true});
        let (status, before) = h
            .call("POST", "/channels/cleanup/preview", Some(request.clone()))
            .await;
        assert_eq!(status, 200, "{before}");
        assert!(before["spaces"].as_u64().unwrap() > 0);
        let (status, result) = h
            .call("POST", "/channels/cleanup", Some(request.clone()))
            .await;
        assert_eq!(status, 500, "{result}");
        let (status, after) = h
            .call("POST", "/channels/cleanup/preview", Some(request))
            .await;
        assert_eq!(status, 200);
        assert_eq!(before, after, "backup must fail before any deletion");
        assert!(!h.env.dir.path().join("backups").exists());
    }
}
