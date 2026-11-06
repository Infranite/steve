//! Cleaning up what an identity brought in (`steve_channels::cleanup`): the dry run, the text
//! purge, the full cleanup with everything referenced kept, a disconnected identity by its
//! account, the backup taken first, the leftovers list, and the confirmation.

use std::sync::Arc;

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
use steve_modules::testkit::{ME, TestEnv, WS, caller};
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
    let fake = FakeFeishu::start().await.expect("fake feishu");
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        space_decisions::register(r);
    });
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1);",
            )?;
            Ok(())
        })
        .expect("bots");
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
    let api = connections::routes(conns)
        .merge(channels_api::routes(ch))
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
    let acc = h.one(&format!(
        "SELECT account_id FROM connections WHERE id = '{con}'"
    ));
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

    fn n(&self, sql: &str) -> i64 {
        let sql = sql.to_string();
        self.env
            .store()
            .reader(move |c| Ok::<_, steve_store::StoreError>(c.query_row(&sql, [], |r| r.get(0))?))
            .expect("count")
    }

    fn one(&self, sql: &str) -> String {
        let sql = sql.to_string();
        self.env
            .store()
            .reader(move |c| Ok::<_, steve_store::StoreError>(c.query_row(&sql, [], |r| r.get(0))?))
            .expect("row")
    }

    fn exec(&self, sql: &str) {
        let sql = sql.to_string();
        self.env
            .store()
            .with_conn(move |c| {
                c.execute_batch(&sql)?;
                Ok(())
            })
            .expect("exec");
    }

    /// Spaces of `con` (or, `con` empty, of disconnected account `acc`): 客户群 with an auto
    /// contact (Ann), a confirmed one (Bob) and a shared auto one (Cat); 项目群 whose one message
    /// an item points at (Cat is there too), and 空群.
    fn seed(&self, p: &str, con: &str, acc: &str) {
        let conn = if con.is_empty() {
            "NULL".to_string()
        } else {
            format!("'{con}'")
        };
        self.exec(&format!(
            "INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES
               ('{p}ann', 'default', 'contact', 'Ann', 'unconfirmed', 'identify', 'usr_dev', 1, 1),
               ('{p}bob', 'default', 'contact', 'Bob', NULL, 'manual', 'usr_dev', 1, 1),
               ('{p}cat', 'default', 'contact', 'Cat', 'unconfirmed', 'identify', 'usr_dev', 1, 1);
             INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
               ('{p}a_ann', 'default', 'feishu', 'feishu', '{p}ann', 'personal', '{p}on_ann', 'Ann', 'identify', 1, 1),
               ('{p}a_bob', 'default', 'feishu', 'feishu', '{p}bob', 'personal', '{p}on_bob', 'Bob', 'identify', 1, 1),
               ('{p}a_cat', 'default', 'feishu', 'feishu', '{p}cat', 'personal', '{p}on_cat', 'Cat', 'identify', 1, 1);
             INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES
               ('default', 'feishu', '{p}on_ann', '{p}a_ann', 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES
               ('default', 'party', '{p}ann', 'works_at', 'party', '{p}bob', 'infer', 1);
             INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated, account_id) VALUES
               ('{p}sp_a', 'default', 'feishu', {conn}, 'group', '{p}oc_a', '客户群', 'active', 1, 1, '{acc}'),
               ('{p}sp_b', 'default', 'feishu', {conn}, 'group', '{p}oc_b', '项目群', 'active', 1, 1, '{acc}'),
               ('{p}sp_c', 'default', 'feishu', {conn}, 'group', '{p}oc_c', '空群', 'active', 1, 1, '{acc}');
             INSERT INTO space_members (workspace_id, space_id, account_id, joined) VALUES
               ('default', '{p}sp_a', '{p}a_ann', 1), ('default', '{p}sp_a', '{p}a_bob', 1), ('default', '{p}sp_a', '{p}a_cat', 1),
               ('default', '{p}sp_b', '{p}a_cat', 1);
             INSERT INTO chan_messages (id, workspace_id, space_id, direction, sender_account_id, sender_party_id, text, attachments, work_item_id, created) VALUES
               ('{p}m1', 'default', '{p}sp_a', 'in', '{p}a_ann', '{p}ann', '合同下周签', '[{{\"file\":\"fil_1\"}}]', NULL, 1),
               ('{p}m2', 'default', '{p}sp_a', 'in', '{p}a_bob', '{p}bob', '好的', '[]', NULL, 2),
               ('{p}m3', 'default', '{p}sp_b', 'in', '{p}a_cat', '{p}cat', '帮我跟进报价', '[]', 'it_1', 3),
               ('{p}m4', 'default', '{p}sp_b', 'in', '{p}a_cat', '{p}cat', '顺便问下', '[]', NULL, 4);
             INSERT INTO ext_events (id, workspace_id, connection_id, source, type, dedupe_key, space_id, message_id, occurred, received, summary, payload) VALUES
               ('{p}ev1', 'default', {conn}, 'im', 'im.message', '{p}d1', '{p}sp_a', '{p}m1', 1, 1, 'Ann：合同下周签', '{{\"text\":\"合同下周签\"}}');"
        ));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_then_text_purge() {
    let (h, con, _) = setup().await;
    let base = h.n(&format!(
        "SELECT count(*) FROM spaces WHERE connection_id = '{con}'"
    ));
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
        h.n("SELECT count(*) FROM chan_messages WHERE text IS NULL"),
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
    let backup = v["backup"].as_str().expect("backup").to_string();
    assert!(backup.starts_with("pre-purge-"), "{backup}");
    assert!(h.env.dir.path().join("backups").join(&backup).is_file());
    assert_eq!(
        h.n("SELECT count(*) FROM chan_messages WHERE text IS NULL AND attachments = '[]' AND purged IS NOT NULL"),
        3
    );
    assert_eq!(
        h.one("SELECT text FROM chan_messages WHERE id = 'm3'"),
        "帮我跟进报价"
    );
    assert_eq!(h.n("SELECT count(*) FROM chan_messages"), 4, "rows stay");
    assert_eq!(
        h.n("SELECT count(*) FROM ext_events WHERE id = 'ev1' AND payload IS NULL AND payload_purged IS NOT NULL AND summary <> ''"),
        1
    );
    assert_eq!(
        h.n("SELECT count(*) FROM audit WHERE action = 'channel.cleanup' AND detail LIKE '%pre-purge-%'"),
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
    h.exec(
        "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
           ('a_other', 'default', 'feishu', 'feishu', 'bob', 'app_bot', 'cli_other', 'Other', 'connect', 1, 1);
         INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated, account_id) VALUES
           ('sp_elsewhere', 'default', 'feishu', NULL, 'group', 'oc_e', '别处', 'left', 1, 1, 'a_other');
         INSERT INTO space_members (workspace_id, space_id, account_id, joined) VALUES ('default', 'sp_elsewhere', 'a_cat', 1);",
    );
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
        h.n("SELECT count(*) FROM spaces WHERE id IN ('sp_a', 'sp_c')"),
        0
    );
    assert_eq!(h.n("SELECT count(*) FROM spaces WHERE id = 'sp_b'"), 1);
    assert_eq!(
        h.n("SELECT count(*) FROM chan_messages WHERE space_id = 'sp_a'"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM space_members WHERE space_id = 'sp_a'"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM ext_events WHERE id = 'ev1'"),
        0,
        "an unused signal goes"
    );
    assert_eq!(
        h.one("SELECT text FROM chan_messages WHERE id = 'm3'"),
        "帮我跟进报价"
    );
    assert_eq!(
        h.n("SELECT count(*) FROM chan_messages WHERE id = 'm4' AND text IS NULL"),
        1
    );
    assert_eq!(
        h.n("SELECT count(*) FROM parties WHERE id = 'ann'"),
        0,
        "auto contact deleted"
    );
    assert_eq!(h.n("SELECT count(*) FROM accounts WHERE id = 'a_ann'"), 0);
    assert_eq!(
        h.n("SELECT count(*) FROM account_handles WHERE account_id = 'a_ann'"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM links WHERE subject_id = 'ann'"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM parties WHERE id IN ('bob', 'cat')"),
        2
    );
    // the identity itself is untouched
    assert_eq!(
        h.n(&format!("SELECT count(*) FROM accounts WHERE id = '{acc}'")),
        1
    );
    assert_eq!(
        h.n(&format!(
            "SELECT count(*) FROM connections WHERE id = '{con}'"
        )),
        1
    );
    assert!(
        h.env
            .dir
            .path()
            .join("backups")
            .join(v["backup"].as_str().expect("b"))
            .is_file()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnected_identity_by_its_account() {
    let (h, _con, _) = setup().await;
    h.exec(
        "INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES
           ('old_bot', 'default', 'contact', '旧机器人', 'bot', 'connect', 'usr_dev', 1, 1);
         INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, display_name, source, created, updated) VALUES
           ('acc_old', 'default', 'feishu_app', 'feishu_app', 'old_bot', 'app_bot', 'cli_old', 'cli_old', '旧应用', 'connect', 1, 1);",
    );
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
        h.n("SELECT count(*) FROM spaces WHERE account_id = 'acc_old'"),
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
    let base = h.n(&format!(
        "SELECT count(*) FROM spaces WHERE connection_id = '{con}'"
    ));
    h.seed("", &con, "");
    h.exec(&format!(
        "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
           VALUES ('acc_you', 'default', 'feishu', 'feishu', 'bob', 'personal', 'on_you', '你', 'connect', 1, 1);
         INSERT INTO connections (id, workspace_id, account_id, adapter, auth, config, created_by, created, updated)
           VALUES ('con_you', 'default', 'acc_you', 'feishu_user', 'oauth', '{{\"app_connection\":\"{con}\"}}', 'usr_dev', 1, 1);
         INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES
           ('sp_a', '{con}', 'default', 1), ('sp_a', 'con_you', 'default', 1),
           ('sp_c', '{con}', 'default', 1), ('sp_c', 'con_gone', 'default', 1);
         UPDATE space_identities SET left_at = 2 WHERE connection_id = 'con_gone';
         INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated) VALUES
           ('sp_d', 'default', 'feishu', NULL, 'group', 'oc_d', '先由旧连接看到', 'active', 1, 1);
         INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES ('sp_d', '{con}', 'default', 1);"
    ));
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
        h.n("SELECT count(*) FROM spaces WHERE id IN ('sp_c', 'sp_d')"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM space_identities WHERE space_id IN ('sp_c', 'sp_d')"),
        0
    );
    assert_eq!(h.n("SELECT count(*) FROM spaces WHERE id = 'sp_a'"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_decisions_are_previewed_and_deleted_atomically() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    h.exec("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('d1','default','reply','chan_message','m1','reply?','open','usr_dev',1),
      ('d2','default','reply','chan_message','m2','reply?','resolved','usr_dev',1);
      UPDATE chan_messages SET decision_id='d1' WHERE id='m1';
      UPDATE ext_events SET decision_id='d1' WHERE message_id='m1';");
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
        h.n("SELECT count(*) FROM decisions WHERE id IN ('d1','d2')"),
        2
    );
    run["decision_ids"] = preview["decision_ids"].clone();
    h.exec("CREATE TRIGGER fail_cleanup BEFORE DELETE ON spaces BEGIN SELECT RAISE(ABORT, 'test rollback'); END;");
    let (st, _) = h.call("POST", "/channels/cleanup", Some(run.clone())).await;
    assert_ne!(st, 200);
    assert_eq!(
        h.n("SELECT count(*) FROM decisions WHERE id IN ('d1','d2')"),
        2
    );
    assert_eq!(
        h.one("SELECT decision_id FROM chan_messages WHERE id='m1'"),
        "d1"
    );
    h.exec("DROP TRIGGER fail_cleanup;");
    let (st, result) = h.call("POST", "/channels/cleanup", Some(run)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(result["decisions"], 2);
    assert_eq!(
        h.n("SELECT count(*) FROM decisions WHERE id IN ('d1','d2')"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM chan_messages WHERE decision_id IN ('d1','d2')"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM ext_events WHERE decision_id IN ('d1','d2')"),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_decision_and_changed_preview_are_not_deleted() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    h.exec("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('shared','default','reply','chan_message','m1','reply?','open','usr_dev',1);
      UPDATE chan_messages SET decision_id='shared' WHERE id IN ('m1','m3');");
    let req = json!({"connection_id":con,"level":"all"});
    let (_, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(preview["decisions"], 0);
    h.exec("UPDATE chan_messages SET decision_id=NULL WHERE id='m3';");
    let mut run = req;
    run["confirm"] = json!(true);
    run["decision_ids"] = preview["decision_ids"].clone();
    let (st, _) = h.call("POST", "/channels/cleanup", Some(run)).await;
    assert_ne!(st, 200);
    assert_eq!(h.n("SELECT count(*) FROM decisions WHERE id='shared'"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_workspace_and_item_decisions_remain() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    h.exec("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, requested_by, created) VALUES
      ('foreign','other','reply','chan_message','m1',NULL,'reply?','open','usr_dev',1),
      ('item_decision','default','reply','space','sp_c','it_1','reply?','open','usr_dev',1);");
    let req = json!({"connection_id":con,"level":"all","confirm":true});
    let (st, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!(preview["decisions"], 0);
    let (st, result) = h.call("POST", "/channels/cleanup", Some(req)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        h.n("SELECT count(*) FROM decisions WHERE id IN ('foreign','item_decision')"),
        2
    );
    assert_eq!(
        h.n("SELECT count(*) FROM spaces WHERE id IN ('sp_a','sp_c')"),
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
    assert_eq!(h.n("SELECT count(*) FROM chan_messages WHERE id='m1'"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_yolo_history_does_not_pin_channel_decisions() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    // Production shape: resolved channel approve/fail decisions, with cancelled or
    // decided YOLO history, sometimes carrying an unused undo description.
    h.exec("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created, resolved) VALUES
      ('d1','default','approve','chan_message','m1','send?','resolved','steve',1,2),
      ('d2','default','fail','chan_message','m2','retry?','resolved','steve',1,2);
      UPDATE chan_messages SET decision_id='d1' WHERE id='m1';
      INSERT INTO yolo_actions (id,workspace_id,decision_id,kind,subject_type,rule,outcome,reason,undo,created) VALUES
      ('ya1','default','d1','approve','chan_message','channel.send','decided','sent','{\"kind\":\"recall\"}',1),
      ('ya2','default','d2','fail','chan_message','channel.retry','cancelled','cancelled','{\"kind\":\"decision\"}',1),
      ('ya3','default','d2','fail','chan_message','channel.retry','decided','done',NULL,1);");
    let mut req = json!({"connection_id":con,"level":"all","confirm":true});
    let (st, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!(preview["decisions"], 2);
    req["decision_ids"] = preview["decision_ids"].clone();
    h.exec("CREATE TRIGGER fail_yolo_cleanup BEFORE DELETE ON decisions BEGIN SELECT RAISE(ABORT, 'rollback'); END;");
    let (st, _) = h.call("POST", "/channels/cleanup", Some(req.clone())).await;
    assert_ne!(st, 200);
    assert_eq!(h.n("SELECT count(*) FROM yolo_actions WHERE id='ya1' AND decision_id='d1' AND undo IS NOT NULL"), 1);
    h.exec("DROP TRIGGER fail_yolo_cleanup;");
    let (st, result) = h.call("POST", "/channels/cleanup", Some(req)).await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        h.n("SELECT count(*) FROM decisions WHERE id IN ('d1','d2')"),
        0
    );
    assert_eq!(
        h.n("SELECT count(*) FROM yolo_actions WHERE id IN ('ya1','ya2','ya3')"),
        3
    );
    assert_eq!(h.n("SELECT count(*) FROM yolo_actions WHERE id IN ('ya1','ya2','ya3') AND (decision_id IS NOT NULL OR undo IS NOT NULL)"), 0);
    assert_eq!(
        h.one("SELECT reason FROM yolo_actions WHERE id='ya1'"),
        "sent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_yolo_action_and_new_execution_after_preview_are_protected() {
    let (h, con, _) = setup().await;
    h.seed("", &con, "");
    h.exec("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('d1','default','approve','chan_message','m1','send?','open','steve',1);
      INSERT INTO yolo_actions (id,workspace_id,decision_id,kind,subject_type,rule,outcome,reason,created) VALUES
      ('ya1','default','d1','approve','chan_message','channel.send','scheduled','waiting',1);");
    let mut req = json!({"connection_id":con,"level":"all","confirm":true});
    let (_, blocked) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(blocked["decisions"], 0);
    h.exec("UPDATE yolo_actions SET outcome='cancelled' WHERE id='ya1';");
    let (_, preview) = h
        .call("POST", "/channels/cleanup/preview", Some(req.clone()))
        .await;
    assert_eq!(preview["decisions"], 1);
    req["decision_ids"] = preview["decision_ids"].clone();
    // Any nonterminal/unknown execution state must fail closed.
    h.exec("UPDATE yolo_actions SET outcome='running' WHERE id='ya1';");
    let (st, _) = h.call("POST", "/channels/cleanup", Some(req)).await;
    assert_ne!(st, 200);
    assert_eq!(
        h.one("SELECT decision_id FROM yolo_actions WHERE id='ya1'"),
        "d1"
    );
    assert_eq!(h.n("SELECT count(*) FROM decisions WHERE id='d1'"), 1);
}
