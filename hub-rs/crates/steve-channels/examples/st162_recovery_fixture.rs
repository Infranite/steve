//! Isolated manual HTTP fixture for outbound recovery. No production connections or real model.
use axum::{
    Extension, Json, Router,
    extract::Path,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use steve_channels::adapters::ext_ws::{self, ExtWs};
use steve_channels::deliver::{Auth, OutRequest, OutTarget, Purpose};
use steve_channels::disclosure::Disclosure;
use steve_channels::fake::ext_ws::{FakeExtWs, TOKEN};
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::{Channels, HandleRef, InboundMessage, Mention, Outbound, api, spaces};
use steve_flow::{FlowSetup, LinearExecutor, testkit::FakeCore};
use steve_modules::accounts::{
    connections::{self, Connections, ConnectionsConfig},
    repo,
    service::ensure_seeds,
};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::{self, Viewer};
use zeroize::Zeroizing;

#[path = "support/recovery_yolo.rs"]
mod recovery_yolo;

struct Fixture {
    env: TestEnv,
    _keys: tempfile::TempDir,
    fake: FakeExtWs,
    ch: Channels,
    out: Mutex<Arc<Outbox>>,
    engine: Mutex<steve_core::yolo::Engine>,
    conn: String,
}
type F = Extension<Arc<Fixture>>;
struct Record;
#[async_trait::async_trait]
impl steve_channels::respond::Responder for Record {
    async fn act(
        &self,
        _: &Channels,
        _: &steve_channels::respond::Plan,
    ) -> steve_channels::ChannelResult<Option<steve_channels::respond::Acted>> {
        Ok(Some(steve_channels::respond::Acted::default()))
    }
}
async fn state(Extension(f): F) -> ApiResult<Json<Value>> {
    let messages = f.env.store().reader(|c| -> ApiResult<Vec<Value>> {
        let mut stmt = c.prepare("SELECT id,status,text,decision_id,external_id FROM chan_messages WHERE direction='out'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|(id,status,text,d,external)| Ok(json!({"id":id,"status":status,"text":text,"external_id":external,"decision":d.and_then(|id|base::decisions::get(c,&id).ok().flatten())}))).collect()
    })?;
    Ok(Json(
        json!({"messages":messages,"writes":f.fake.with(|w|w.writes.clone()),"calls":f.fake.calls("messages.reply"),
            "yolo": f.engine.lock().unwrap().yolo().status(WS)?}),
    ))
}
async fn request(Extension(f): F, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let source = steve_store::uid("src");
    let mut inbound = InboundMessage::text(
        "fixture-chat",
        source.clone(),
        HandleRef::new(ext_ws::SYSTEM, "u_requester"),
        "manual fixture request",
        f.ch.now(),
    );
    inbound.mentions.push(Mention {
        handle: None,
        name: "fixture bot".into(),
        is_connection: true,
    });
    let stored = f.ch.on_message(WS, &f.conn, inbound).await?;
    let plan = f
        .env
        .store()
        .reader(|c| steve_channels::flow_actions::plan_of(c, WS, &stored.message_id))?;
    let out = f.out.lock().unwrap().clone();
    let delivered = out
        .deliver(
            &f.ch,
            &OutRequest {
                target: OutTarget::reply(&plan),
                text: body["text"]
                    .as_str()
                    .unwrap_or("完整拟发送内容\n第二行")
                    .into(),
                purpose: Purpose::Reply,
                approve: false,
                generation: None,
                auth: Auth::Rule {
                    source: stored.message_id,
                    act: steve_channels::rules::Act::Reply,
                },
            },
        )
        .await?;
    Ok(Json(
        json!({"message_id":delivered.message_id(),"decision_id":delivered.decision_id(),"outcome":format!("{delivered:?}")}),
    ))
}
async fn policy(Extension(f): F, Json(body): Json<Value>) -> Json<Value> {
    let refused = body["refused"] == true;
    f.fake.with(|w| {
        if refused {
            w.forbidden_targets.insert("fixture-chat".into());
        } else {
            w.forbidden_targets.remove("fixture-chat");
        }
    });
    Json(json!({"refused":refused,"authorization_changed_by":"isolated fixture operator"}))
}
async fn decide(
    Extension(f): F,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let d = steve_modules::decisions::dispatch(
        &f.env.modules,
        &caller(WS, ME),
        &id,
        body["choice"].as_str().unwrap_or(""),
        body["text"].as_str().unwrap_or(""),
    )
    .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(serde_json::to_value(d)?))
}
async fn yolo_approve(Extension(f): F, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let d = steve_modules::decisions::dispatch(&f.env.modules, &caller(WS, "yolo"), &id, "yes", "")
        .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(serde_json::to_value(d)?))
}
async fn tick(Extension(f): F, Json(body): Json<Value>) -> Json<Value> {
    f.env
        .clock
        .advance(body["advance_ms"].as_i64().unwrap_or(11_000));
    let actions = f.engine.lock().unwrap().tick();
    let out = f.out.lock().unwrap().clone();
    Json(json!({"sent":out.send_due(&f.ch).await,"actions":actions}))
}
async fn restart(Extension(f): F) -> Json<Value> {
    *f.engine.lock().unwrap() =
        recovery_yolo::engine(&f.env.modules, f.env.dir.path(), f.env.clock.clone());
    let link = OutLink::new();
    link.bind_clock(f.env.clock.clone());
    *f.out.lock().unwrap() = Arc::new(Outbox::new(link));
    Json(json!({"reopened":"Core, SQLite handle, Engine and Outbox; fixture adapter retained"}))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeExtWs::start().await?;
    fake.with(|w| {
        w.identity = Some("bot".into());
        w.account_identity = Some("bot".into());
        w.write_policy = "restricted".into();
        w.forbidden_targets.insert("fixture-chat".into());
    });
    let flow_setup = FlowSetup::new(Arc::new(FakeCore::default()));
    let disclosure = Disclosure::with_models(Vec::new());
    let link = OutLink::new();
    let late = steve_channels::flow_actions::LateChannels::new();
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        r.outbound_check(Box::new(disclosure.clone()));
        r.decision(steve_channels::respond::SUBJECT_MESSAGE, link.handler());
        steve_channels::recovery::register(r, late.clone());
    });
    link.bind_clock(env.clock.clone());
    env.gateway.script(
        steve_channels::disclosure::CHECK_HEAD,
        r#"{"verdict":"pass","spans":[]}"#,
    );
    env.gateway.script(
        steve_channels::disclosure::REWRITE_HEAD,
        r#"{"text":"模型修改后的待审正文"}"#,
    );
    disclosure.bind(env.store().clone(), env.gateway.clone());
    env.store().transaction(|tx| -> ApiResult<()> {
        ensure_seeds(tx, WS)?;
        repo::upsert_declared(tx, ext_ws::SYSTEM, ext_ws::SYSTEM_LABEL, "ext_ws")?;
        Ok(())
    })?;
    let keys = tempfile::tempdir()?;
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path())?),
    );
    vault.init(WS, ME)?;
    let ext = Arc::new(ExtWs::bot());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(ext.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .responder(Arc::new(Record))
        .decl(ext_ws::bot_decl())
        .adapter(ext)
        .build();
    late.set(&ch);
    let mut fields = BTreeMap::new();
    fields.insert(ext_ws::URL.into(), Zeroizing::new(fake.url()));
    fields.insert(ext_ws::TOKEN.into(), Zeroizing::new(TOKEN.into()));
    let conn = conns
        .create(&caller(WS, ME), ext_ws::BOT_ADAPTER, &fields, json!({}))
        .await?
        .id;
    env.store().transaction(|tx| {
        spaces::set_account_rules(
            tx,
            &Viewer::bare(WS, ME),
            &conn,
            &json!([{"when":{"kind":"all"},"act":"reply"}]),
            ch.now(),
        )
    })?;
    let modules = env.modules.clone();
    let engine = recovery_yolo::engine(&modules, env.dir.path(), env.clock.clone());
    recovery_yolo::enable(&engine, false);
    let fixture = Arc::new(Fixture {
        env,
        _keys: keys,
        fake,
        ch: ch.clone(),
        out: Mutex::new(Arc::new(Outbox::new(link))),
        engine: Mutex::new(engine),
        conn,
    });
    let routes = Router::new()
        .route("/fixture/state", get(state))
        .route("/fixture/request", post(request))
        .route("/fixture/policy", post(policy))
        .route("/fixture/decisions/{id}", post(decide))
        .route("/fixture/tick", post(tick))
        .route("/fixture/yolo/decisions/{id}", post(yolo_approve))
        .route("/fixture/restart", post(restart))
        .nest("/api", api::routes(ch))
        .layer(Extension(fixture))
        .layer(Extension(caller(WS, ME)))
        .with_state(modules);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, routes).await?;
    Ok(())
}
