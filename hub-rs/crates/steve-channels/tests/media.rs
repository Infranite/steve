//! Message media end to end on the fake Feishu (CONTRACT.md of the message-media task): a card,
//! an image and a post with media arrive over the bot's long connection; the space's messages
//! show them without provider keys; `GET /chan-messages/:id/resources/:n` fetches only what the
//! stored message recorded, as the connection that received it, with the guards (workspace, space
//! visibility, membership, purge — also while the bytes are coming), the size limit, provider
//! refusals and safe headers; an old card is read again from its own raw event.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use steve_channels::Channels;
use steve_channels::adapters::feishu::FeishuApp;
use steve_channels::adapters::feishu::inbound::LongConnection;
use steve_channels::adapters::feishu::ws::WsState;
use steve_channels::fake::feishu::model::Resource;
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, WsClientConfig, fixture, ids};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const T: Duration = Duration::from_secs(10);
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRfake";
const IMG_MSG: &str = "om_image00000000000000000000000001";
const IMG_KEY: &str = "img_v3_fake0000-0000-0000-0000-000000000001g";

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    ch: Channels,
    con: String,
    api: Router,
    _lc: LongConnection,
}

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    // each test says which resources Feishu has
    fake.world(|w| w.resources.clear());
    fake.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
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
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .adapter(app)
        .build();
    let capi: Router = connections::routes(conns).with_state(env.modules.clone());
    let body = json!({
        "adapter": "feishu_app",
        "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
        "config": {"api_base": fake.base_url()},
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/connections")
        .header(header::HOST, "hub.test:8440")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    req.extensions_mut().insert(caller(WS, ME));
    let res = capi.oneshot(req).await.expect("infallible");
    assert_eq!(res.status().as_u16(), 200);
    let v: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    let con = v["id"].as_str().expect("id").to_string();
    let lc = LongConnection::start(&ch, WS, &con)
        .await
        .expect("long connection");
    lc.ws
        .wait_for(T, |s| matches!(s, WsState::Connected { .. }))
        .await
        .expect("connected");
    let api = steve_channels::api::routes(ch.clone()).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        fake,
        ch,
        con,
        api,
        _lc: lc,
    }
}

fn sql(h: &H, s: &str) {
    let s = s.to_string();
    h.env
        .store()
        .transaction(move |tx| -> Result<(), steve_store::StoreError> {
            tx.execute_batch(&s)?;
            Ok(())
        })
        .expect("sql");
}

fn one(h: &H, s: &str) -> Value {
    let s = s.to_string();
    h.env
        .store()
        .reader(move |c| {
            Ok::<_, steve_store::StoreError>(c.query_row(&s, [], |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
            })?)
        })
        .expect("query")
}

/// Pushes an envelope over the long connection and waits for its message; its `cm_…` id.
async fn push(h: &H, env: Value) -> String {
    let ext = env["event"]["message"]["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = h.fake.ws_push_envelope(env);
    h.fake.ws_wait_acked(&id, T).await.expect("acked");
    let t0 = std::time::Instant::now();
    loop {
        let v = one(
            h,
            &format!(
                "SELECT id FROM chan_messages WHERE external_id = '{ext}' AND direction = 'in'"
            ),
        );
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
        assert!(t0.elapsed() < T, "message {ext} never stored");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// An envelope of the p2p image fixture with another type and content.
fn envelope(ext: &str, ty: &str, content: &Value, event_id: &str) -> Value {
    let mut e = fixture("message_image").unwrap();
    e["header"]["event_id"] = json!(event_id);
    e["event"]["message"]["message_id"] = json!(ext);
    e["event"]["message"]["message_type"] = json!(ty);
    e["event"]["message"]["content"] = json!(content.to_string());
    e
}

fn space_of(h: &H, msg: &str) -> String {
    one(
        h,
        &format!("SELECT space_id FROM chan_messages WHERE id = '{msg}'"),
    )
    .as_str()
    .unwrap()
    .to_string()
}

async fn shown(h: &H, msg: &str) -> Value {
    let space = space_of(h, msg);
    let (st, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{space}/messages"),
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v.as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == msg)
        .cloned()
        .expect("listed")
}

async fn get(h: &H, who: (&str, &str), path: &str) -> (u16, HeaderMap, Vec<u8>) {
    get_on(&h.api, who, path).await
}

async fn get_on(api: &Router, who: (&str, &str), path: &str) -> (u16, HeaderMap, Vec<u8>) {
    let mut req = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request");
    req.extensions_mut().insert(caller(who.0, who.1));
    let res = api.clone().oneshot(req).await.expect("infallible");
    let st = res.status().as_u16();
    let hd = res.headers().clone();
    let b = axum::body::to_bytes(res.into_body(), 64 << 20)
        .await
        .expect("body")
        .to_vec();
    (st, hd, b)
}

fn reason(b: &[u8]) -> String {
    serde_json::from_slice::<Value>(b).unwrap_or_else(|_| {
        panic!(
            "not json: {:?}",
            String::from_utf8_lossy(&b[..b.len().min(200)])
        )
    })["reason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn add_resource(h: &H, msg: &str, key: &str, ct: &str, bytes: &[u8], name: Option<&str>) {
    h.fake.world(|w| {
        w.resources.push(Resource {
            message_id: msg.into(),
            file_key: key.into(),
            content_type: ct.into(),
            file_name: name.map(str::to_string),
            bytes: bytes.to_vec(),
        })
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_is_shown_and_fetched_through_its_own_message_only() {
    let h = setup().await;
    let msg = push(&h, fixture("message_image").unwrap()).await;
    add_resource(&h, IMG_MSG, IMG_KEY, "image/png", PNG, None);

    // the API: no provider key, a url instead; the stored row still has the key
    let m = shown(&h, &msg).await;
    let url = format!("/api/chan-messages/{msg}/resources/0");
    assert_eq!(
        m["attachments"],
        json!([{"type": "image", "source": "message", "url": url}])
    );
    assert!(!m.to_string().contains(IMG_KEY));
    assert!(
        one(
            &h,
            &format!("SELECT attachments FROM chan_messages WHERE id = '{msg}'")
        )
        .as_str()
        .unwrap()
        .contains(IMG_KEY)
    );

    // fetched inline, as the bot, with the type and key of the stored entry
    let path = url.trim_start_matches("/api");
    h.fake.clear_log();
    let (st, hd, b) = get(&h, (WS, ME), path).await;
    assert_eq!(st, 200);
    assert_eq!(b, PNG);
    assert_eq!(hd[header::CONTENT_TYPE], "image/png");
    assert!(
        hd[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("inline;")
    );
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    assert_eq!(hd[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
        hd[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; sandbox"
    );
    let reqs = h.fake.requests_to(Endpoint::GetResource);
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].path,
        format!("/open-apis/im/v1/messages/{IMG_MSG}/resources/{IMG_KEY}")
    );
    assert_eq!(reqs[0].query, "type=image");
    assert!(
        reqs[0].token.as_deref().unwrap_or("").starts_with("t-"),
        "tenant token"
    );

    // ?download=1 is an attachment
    let (_, hd, _) = get(&h, (WS, ME), &format!("{path}?download=1")).await;
    assert!(
        hd[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );

    // another member who sees the space may; another workspace, a bad index, a non-number may not
    assert_eq!(get(&h, (WS, OTHER), path).await.0, 200);
    let (st, _, b) = get(&h, (WS2, ME), path).await;
    assert_eq!((st, reason(&b).as_str()), (404, "not_found"));
    for bad in ["1", "abc", "-1", "99999999999999999999"] {
        let (st, _, _) = get(
            &h,
            (WS, ME),
            &format!("/chan-messages/{msg}/resources/{bad}"),
        )
        .await;
        assert_eq!(st, 404, "{bad}");
    }
    assert_eq!(
        get(&h, (WS, ME), "/chan-messages/cm_nope/resources/0")
            .await
            .0,
        404
    );
    // nothing reached Feishu for the refused ones
    assert_eq!(h.fake.requests_to(Endpoint::GetResource).len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_refusals_limits_and_unsafe_types() {
    let h = setup().await;
    let msg = push(&h, fixture("message_image").unwrap()).await;
    let path = format!("/chan-messages/{msg}/resources/0");

    // not in the message at Feishu
    let (st, hd, b) = get(&h, (WS, ME), &path).await;
    assert_eq!((st, reason(&b).as_str()), (502, "gone"));
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    assert!(
        !String::from_utf8_lossy(&b).contains("File not in"),
        "provider msg not passed on"
    );

    for (status, code, want, http) in [
        (400u16, 234040, "invisible", 502u16),
        (400, 234038, "restricted", 502),
        (400, 234004, "not_in_chat", 502),
        (400, 234037, "too_large", 413),
        (400, 99991672, "no_permission", 502),
        (429, 99991400, "rate_limited", 429),
        (500, 1, "provider", 502),
        // a 200 with an error envelope is a refusal, not the file
        (200, 99991672, "no_permission", 502),
    ] {
        let body = json!({"code": code, "msg": "secret detail"})
            .to_string()
            .into_bytes();
        h.fake
            .resource_answer(Some((status, "application/json; charset=utf-8", body)));
        let (st, _, b) = get(&h, (WS, ME), &path).await;
        assert_eq!((st, reason(&b).as_str()), (http, want), "{status} {code}");
        assert!(!String::from_utf8_lossy(&b).contains("secret detail"));
    }
    // an endless error body is not read whole
    h.fake
        .resource_answer(Some((400, "application/json", vec![b' '; 2 << 20])));
    let (st, _, b) = get(&h, (WS, ME), &path).await;
    assert_eq!((st, reason(&b).as_str()), (502, "provider"));

    // a JSON *file* (code 0 / no code) is the file, as a download
    h.fake
        .resource_answer(Some((200, "application/json", br#"{"a":1}"#.to_vec())));
    let (st, hd, b) = get(&h, (WS, ME), &path).await;
    assert_eq!((st, b.as_slice()), (200, br#"{"a":1}"#.as_slice()));
    assert_eq!(hd[header::CONTENT_TYPE], "application/octet-stream");

    // too large
    h.fake
        .resource_answer(Some((200, "image/png", vec![0u8; 25 * 1024 * 1024 + 1])));
    let (st, _, b) = get(&h, (WS, ME), &path).await;
    assert_eq!((st, reason(&b).as_str()), (413, "too_large"));

    // svg / html never inline, whatever Feishu calls them
    for body in [
        &b"<svg xmlns='http://www.w3.org/2000/svg'><script>x</script></svg>"[..],
        b"<html><script>x</script>",
    ] {
        h.fake
            .resource_answer(Some((200, "image/svg+xml", body.to_vec())));
        let (st, hd, _) = get(&h, (WS, ME), &path).await;
        assert_eq!(st, 200);
        assert_eq!(hd[header::CONTENT_TYPE], "application/octet-stream");
        assert!(
            hd[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
    }
}

/// Starts a download as `who` and returns once the fake Feishu has the request (it then holds it
/// for the delay), so what the test changes next happens while the bytes are coming.
async fn started(
    h: &H,
    who: (&'static str, &'static str),
    path: &str,
) -> tokio::task::JoinHandle<(u16, HeaderMap, Vec<u8>)> {
    let before = h.fake.requests_to(Endpoint::GetResource).len();
    let api = h.api.clone();
    let p = path.to_string();
    let req = tokio::spawn(async move { get_on(&api, who, &p).await });
    let t0 = std::time::Instant::now();
    while h.fake.requests_to(Endpoint::GetResource).len() == before {
        assert!(t0.elapsed() < T, "the download never reached Feishu");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    req
}

#[tokio::test(flavor = "multi_thread")]
async fn a_purge_a_removal_or_a_move_while_downloading_drops_the_bytes() {
    let h = setup().await;
    let msg = push(&h, fixture("message_image").unwrap()).await;
    add_resource(&h, IMG_MSG, IMG_KEY, "image/png", PNG, None);
    let path = format!("/chan-messages/{msg}/resources/0");
    let restore = format!(
        "UPDATE chan_messages SET text = '', attachments = (SELECT json_array(json_object('type','image','source','message','key','{IMG_KEY}'))), purged = NULL WHERE id = '{msg}'"
    );
    h.fake.resource_delay(Duration::from_secs(1));

    // purged mid-way: 410, no bytes
    let req = started(&h, (WS, ME), &path).await;
    sql(
        &h,
        &format!(
            "UPDATE chan_messages SET text = NULL, attachments = '[]', purged = 1 WHERE id = '{msg}'"
        ),
    );
    let (st, _, b) = req.await.unwrap();
    assert_eq!((st, reason(&b).as_str()), (410, "purged"));
    assert!(!b.starts_with(PNG));
    // and afterwards: 410 at once, the API shows nothing of it
    let n = h.fake.requests_to(Endpoint::GetResource).len();
    assert_eq!(get(&h, (WS, ME), &path).await.0, 410);
    assert_eq!(
        h.fake.requests_to(Endpoint::GetResource).len(),
        n,
        "not even asked"
    );
    let m = shown(&h, &msg).await;
    assert_eq!(
        (m["text"].clone(), m["attachments"].clone()),
        (Value::Null, json!([]))
    );

    // moved to another space the viewer also sees: 404
    sql(&h, &restore);
    let group = push(&h, fixture("message_group_at_bot").unwrap()).await;
    let (home, other) = (space_of(&h, &msg), space_of(&h, &group));
    assert_ne!(home, other);
    let req = started(&h, (WS, ME), &path).await;
    sql(
        &h,
        &format!("UPDATE chan_messages SET space_id = '{other}' WHERE id = '{msg}'"),
    );
    let (st, _, b) = req.await.unwrap();
    assert_eq!((st, reason(&b).as_str()), (404, "not_found"));
    sql(
        &h,
        &format!("UPDATE chan_messages SET space_id = '{home}' WHERE id = '{msg}'"),
    );

    // a member removed mid-way: 404
    let req = started(&h, (WS, OTHER), &path).await;
    sql(
        &h,
        &format!(
            "DELETE FROM workspace_members WHERE workspace_id = '{WS}' AND user_id = '{OTHER}'"
        ),
    );
    let (st, _, b) = req.await.unwrap();
    assert_eq!((st, reason(&b).as_str()), (404, "not_found"));

    // ME still can
    h.fake.resource_delay(Duration::ZERO);
    let (st, _, b) = get(&h, (WS, ME), &path).await;
    assert_eq!((st, b.as_slice()), (200, PNG));
}

#[tokio::test(flavor = "multi_thread")]
async fn purged_rows_left_by_an_older_sweep_show_nothing() {
    let h = setup().await;
    let msg = push(&h, fixture("message_image").unwrap()).await;
    // what the sweep before this change did: text gone, attachments kept
    sql(
        &h,
        &format!("UPDATE chan_messages SET text = NULL, purged = 5 WHERE id = '{msg}'"),
    );
    let m = shown(&h, &msg).await;
    assert_eq!(m["attachments"], json!([]));
    assert_eq!(
        get(&h, (WS, ME), &format!("/chan-messages/{msg}/resources/0"))
            .await
            .0,
        410
    );
    // the next sweep clears what was left
    steve_channels::messages::sweep(h.ch.modules(), 10).unwrap();
    assert_eq!(
        one(
            &h,
            &format!("SELECT attachments FROM chan_messages WHERE id = '{msg}'")
        ),
        json!("[]")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_card_and_a_post_with_media_arrive_readable() {
    let h = setup().await;
    let dsl = json!({
        "schema": "2.0",
        "header": {"title": {"tag": "plain_text", "content": "发布审批"}},
        "body": {"elements": [
            {"tag": "markdown", "content": "请确认 **v0.2.3**"},
            {"tag": "img", "img_key": "img_card_1", "alt": {"tag": "plain_text", "content": "截图"}},
            {"tag": "button", "text": {"tag": "plain_text", "content": "同意"}, "behaviors": [{"type": "callback", "value": {"ok": 1}}]}
        ]}
    });
    let card = push(
        &h,
        envelope(
            "om_card0000000000000000000000000001",
            "interactive",
            &json!({"user_dsl": dsl.to_string()}),
            "ev_card_1",
        ),
    )
    .await;
    let m = shown(&h, &card).await;
    assert_eq!(
        m["text"],
        "发布审批\n请确认 **v0.2.3**\n[图片：截图]\n[同意]"
    );
    let a = &m["attachments"][0];
    assert_eq!(
        (a["type"].clone(), a["format"].clone(), a["schema"].clone()),
        (json!("card"), json!("user_dsl"), json!(2))
    );
    assert_eq!(a["blocks"][1], json!({"t": "image", "alt": "截图"}));
    assert!(!m.to_string().contains("img_card_1"));
    assert_eq!(
        get(&h, (WS, ME), &format!("/chan-messages/{card}/resources/0"))
            .await
            .0,
        404,
        "a card is no resource"
    );

    let post = json!({
        "title": "日报",
        "content": [
            [{"tag": "text", "text": "见图"}, {"tag": "img", "image_key": "img_post_1"}],
            [{"tag": "media", "file_key": "file_vid_1", "image_key": "img_cover"}],
            [{"tag": "img", "image_key": "img_post_1"}]
        ],
        "files": [{"file_key": "file_zone_1", "file_name": "数据.xlsx"}, {"file_key": "fold_1", "file_name": "资料", "is_folder": true}]
    });
    let pm = push(
        &h,
        envelope(
            "om_post0000000000000000000000000001",
            "post",
            &post,
            "ev_post_1",
        ),
    )
    .await;
    add_resource(
        &h,
        "om_post0000000000000000000000000001",
        "file_zone_1",
        "application/vnd.ms-excel",
        b"PK\x03\x04",
        Some("数据.xlsx"),
    );
    let m = shown(&h, &pm).await;
    assert_eq!(m["text"], "日报\n见图[图片]\n[视频]\n[图片]");
    let atts = m["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 4, "the repeated image once: {atts:?}");
    assert_eq!(atts[0]["source"], "post");
    assert_eq!(atts[1]["kind"], "video");
    assert_eq!(atts[2]["name"], "数据.xlsx");
    assert_eq!(
        (atts[3]["kind"].clone(), atts[3]["unavailable"].clone()),
        (json!("folder"), json!("folder"))
    );
    assert!(atts[3].get("url").is_none());
    let (st, hd, b) = get(&h, (WS, ME), &format!("/chan-messages/{pm}/resources/2")).await;
    assert_eq!((st, b.as_slice()), (200, b"PK\x03\x04".as_slice()));
    assert_eq!(hd[header::CONTENT_TYPE], "application/octet-stream");
    assert!(
        hd[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("%E6%95%B0%E6%8D%AE.xlsx")
    );
    assert_eq!(
        h.fake
            .requests_to(Endpoint::GetResource)
            .last()
            .unwrap()
            .query,
        "type=file"
    );
    let (st, _, b) = get(&h, (WS, ME), &format!("/chan-messages/{pm}/resources/3")).await;
    assert_eq!((st, reason(&b).as_str()), (409, "folder"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_card_is_read_again_from_its_own_raw_event_only() {
    let h = setup().await;
    let content = json!({"title": "旧卡片", "elements": [[{"tag": "text", "text": "正文"}, {"tag": "img", "image_key": "img_1"}]]});
    let msg = push(
        &h,
        envelope(
            "om_old00000000000000000000000000001",
            "interactive",
            &content,
            "ev_old_1",
        ),
    )
    .await;
    // as it was stored before cards were read
    sql(
        &h,
        &format!(
            "UPDATE chan_messages SET text = '[interactive消息]', attachments = '[]' WHERE id = '{msg}'"
        ),
    );
    let m = shown(&h, &msg).await;
    assert_eq!(m["text"], "旧卡片\n正文\n[图片：图片]");
    assert_eq!(m["attachments"][0]["format"], "flattened");
    // nothing was written back
    assert_eq!(
        one(
            &h,
            &format!("SELECT text FROM chan_messages WHERE id = '{msg}'")
        ),
        json!("[interactive消息]")
    );

    // an event of another message is not borrowed
    let other = push(&h, fixture("message_image").unwrap()).await;
    let ev = one(
        &h,
        &format!("SELECT event_id FROM chan_messages WHERE id = '{other}'"),
    );
    let ev_old = one(
        &h,
        &format!("SELECT event_id FROM chan_messages WHERE id = '{msg}'"),
    );
    sql(
        &h,
        &format!(
            "UPDATE chan_messages SET event_id = '{}' WHERE id = '{msg}'",
            ev.as_str().unwrap()
        ),
    );
    assert_eq!(shown(&h, &msg).await["text"], "[interactive消息]");
    sql(
        &h,
        &format!(
            "UPDATE chan_messages SET event_id = '{}' WHERE id = '{msg}'",
            ev_old.as_str().unwrap()
        ),
    );

    // the raw payload purged: stays as it was
    sql(
        &h,
        &format!(
            "UPDATE ext_events SET payload = NULL, payload_purged = 1 WHERE id = '{}'",
            ev_old.as_str().unwrap()
        ),
    );
    let m = shown(&h, &msg).await;
    assert_eq!(
        (m["text"].clone(), m["attachments"].clone()),
        (json!("[interactive消息]"), json!([]))
    );
    let _ = &h.con;
}
