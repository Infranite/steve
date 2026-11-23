//! Loopback-only acceptance fixture: real accounts/connection HTTP routes and ext_ws adapter,
//! isolated SQLite/vault, fake channel service. Fixture writes bypass the UI's normal approval
//! flow deliberately to exercise the final adapter boundary. Never use real account credentials.
use axum::{
    Extension, Json, Router,
    extract::Path,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use steve_channels::{
    Channels,
    adapters::ext_ws::{self, ExtWs, ExtWsHook},
    channel::{ActAsUser, ActTarget, MessageRef, OutContent, ReactionKind, SendKey},
    fake::ext_ws::{FakeExtWs, TOKEN},
    history::HistoryRead,
    lifecycle::ConnectionRuntime,
};
use steve_flow::{FlowSetup, LinearExecutor, testkit::FakeCore};
use steve_modules::{
    Clock,
    accounts::{
        connections::{self, Connections, ConnectionsConfig},
        service::ensure_seeds,
    },
    testkit::{ME, TestEnv, WS, caller},
    vault::{Vault, keystore::FileKeyStore},
};
use zeroize::Zeroizing;

struct Fixture {
    env: TestEnv,
    _keys: tempfile::TempDir,
    fake: FakeExtWs,
    ext: Arc<ExtWs>,
    conns: Connections,
    conn: String,
}
type F = Extension<Arc<Fixture>>;

async fn state(Extension(f): F) -> Json<Value> {
    let conn = f.conns.get(&caller(WS, ME), &f.conn).unwrap();
    let inbound = f.env.store().reader(|c| -> steve_modules::ApiResult<Vec<Value>> {
        let mut st = c.prepare("SELECT m.text,m.history,e.outcome_label FROM chan_messages m LEFT JOIN ext_events e ON e.id=m.event_id WHERE direction='in' ORDER BY m.rowid")?;
        Ok(st.query_map([], |r| Ok(json!({"text":r.get::<_,Option<String>>(0)?,"history":r.get::<_,Option<String>>(1)?,"label":r.get::<_,Option<String>>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).unwrap();
    Json(
        json!({"connection":conn,"writes":f.fake.with(|w|w.writes.clone()),
        "write_calls":f.fake.with(|w|w.log.iter().filter(|(m,_)|m.starts_with("messages.") && m != "messages.history" && m != "messages.get" || m.starts_with("reactions.") || m == "connector.call").cloned().collect::<Vec<_>>()),
        "acked":f.fake.acked(),"inbound":inbound,"now":f.env.clock.now()}),
    )
}

async fn policy(Extension(f): F, Json(body): Json<Value>) -> Json<Value> {
    let policy = body["policy"].as_str().unwrap_or("allowed");
    assert!(["allowed", "restricted", "forbidden"].contains(&policy));
    f.fake.with(|w| {
        w.write_policy = policy.into();
        if body["target_blocked"] == true {
            w.forbidden_targets.insert("fixture-chat".into());
        } else {
            w.forbidden_targets.remove("fixture-chat");
        }
    });
    f.fake.notify(
        "session.status",
        json!({"status":"ok","write_policy":policy}),
    );
    Json(json!({"policy":policy,"target_blocked":body["target_blocked"] == true}))
}

async fn write(Extension(f): F, Path(method): Path<String>) -> Json<Value> {
    let cx = f.conns.cx(WS, &f.conn).unwrap();
    let target = MessageRef::platform("fixture-chat", "m_target");
    let key = SendKey(steve_store::uid("manual"));
    let result = match method.as_str() {
        "send" => f
            .ext
            .send(
                &cx,
                &ActTarget::Space {
                    ext_id: "fixture-chat".into(),
                },
                &OutContent::text("explicit fixture send"),
                &key,
            )
            .await
            .map(|_| ()),
        "reply" => f
            .ext
            .send(
                &cx,
                &ActTarget::Reply {
                    to: target,
                    in_thread: true,
                },
                &OutContent::text("explicit fixture reply"),
                &key,
            )
            .await
            .map(|_| ()),
        "recall" => f.ext.recall(&cx, &target).await,
        "add" => f
            .ext
            .add_reaction(&cx, &target, ReactionKind::Ack)
            .await
            .map(|_| ()),
        "remove" => f.ext.remove_reaction(&cx, &target, "r_target").await,
        _ => return Json(json!({"ok":false,"error":"unknown fixture operation"})),
    };
    Json(match result {
        Ok(()) => json!({"ok":true}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    })
}

async fn read(Extension(f): F) -> Json<Value> {
    let cx = f.conns.cx(WS, &f.conn).unwrap();
    let result = f
        .ext
        .read(&cx, "fixture-chat", 0, f.env.clock.now(), 10)
        .await;
    Json(json!({"ok":result.is_ok()}))
}

async fn message(Extension(f): F) -> Json<Value> {
    // Written before the next switch enable, arriving later through the running subscription.
    let seq = f.fake.push_message(steve_channels::fake::ext_ws::message(
        "fixture-chat",
        &steve_store::uid("old"),
        "u_other",
        "old message before enabling",
        f.env.clock.now() - 3 * 60_000,
    ));
    Json(json!({"seq":seq}))
}

async fn advance(Extension(f): F) -> Json<Value> {
    f.env.clock.advance(5 * 60_000);
    Json(json!({"now":f.env.clock.now()}))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeExtWs::start().await?;
    let setup = FlowSetup::new(Arc::new(FakeCore::default()));
    let env = TestEnv::new(|r| {
        setup.register(r);
        connections::register(r);
    });
    fake.with(|w| w.now = Some(env.clock.now()));
    env.store().transaction(|tx| ensure_seeds(tx, WS))?;
    env.store().write(|w| {
        steve_store::Systems(w).upsert_declared(
            ext_ws::SYSTEM,
            ext_ws::SYSTEM_LABEL,
            "ext_ws",
            steve_store::now(),
        )
    })?;
    let keys = tempfile::tempdir()?;
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path())?),
    );
    vault.init(WS, ME)?;
    let ext = Arc::new(ExtWs::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(ext.clone())
    .build();
    let flow = setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .decl(ext_ws::decl())
        .build();
    let mut fields = BTreeMap::new();
    fields.insert(ext_ws::URL.into(), Zeroizing::new(fake.url()));
    fields.insert(ext_ws::TOKEN.into(), Zeroizing::new(TOKEN.into()));
    let conn = conns
        .create(&caller(WS, ME), ext_ws::ADAPTER, &fields, json!({}))
        .await?
        .id;
    conns.patch(
        &caller(WS, ME),
        &conn,
        None,
        Some(vec!["msg.receive".into()]),
    )?;
    let rt = ConnectionRuntime::builder(ch.clone())
        .hook(
            ext_ws::ADAPTER,
            Some(ext_ws::decl()),
            Arc::new(ExtWsHook::new(ch.clone(), ext.clone())),
        )
        .build();
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    drop(rt.start(rx));
    let modules = env.modules.clone();
    let fixture = Arc::new(Fixture {
        env,
        _keys: keys,
        fake,
        ext,
        conns: conns.clone(),
        conn,
    });
    let events = fixture.clone();
    tokio::spawn(async move {
        loop {
            for event in events.env.take_events() {
                let _ = tx.send(steve_modules::testkit::notification(event)).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let api = connections::routes(conns)
        .merge(steve_modules::accounts::api::routes())
        .merge(steve_channels::channels_api::routes(ch.clone()))
        .merge(steve_channels::api::routes(ch));
    let app = Router::new()
        .nest("/api", api)
        .route("/fixture/state", get(state))
        .route("/fixture/policy", post(policy))
        .route("/fixture/write/{method}", post(write))
        .route("/fixture/read", get(read))
        .route("/fixture/message", post(message))
        .route("/fixture/advance", post(advance))
        .layer(Extension(fixture))
        .layer(Extension(caller(WS, ME)))
        .with_state(modules);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
