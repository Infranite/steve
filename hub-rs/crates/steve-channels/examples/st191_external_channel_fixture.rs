//! Isolated channel consumer for manual ext_ws conformance checks. The producer is a separate
//! process, chosen by loopback URL and a test token file. No platform/gateway implementation lives
//! here: only the public ext_ws adapter, real channel routes, a fresh store and deterministic model.
#[path = "../tests/support/owner_presence.rs"]
#[allow(unused_imports, dead_code)]
mod support;
use axum::{
    Extension, Json,
    extract::Path,
    routing::{get, post},
};
use std::collections::BTreeMap;
use steve_channels::adapters::ext_ws::{self, ExtWs, ExtWsHook};
use steve_channels::lifecycle::ConnectionRuntime;
use steve_modules::{
    ApiError,
    accounts::{repo, service::ensure_seeds},
};
use steve_store::base;
use support::*;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

struct Fixture {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    out: Arc<Outbox>,
    connection: String,
    _runtime: ConnectionRuntime,
    _events: mpsc::Sender<Value>,
}
type F = Extension<Arc<Fixture>>;

async fn state(Extension(f): F) -> ApiResult<Json<Value>> {
    let rows = f.env.store().reader(|c| -> ApiResult<Vec<Value>> {
        let mut st = c.prepare("SELECT id FROM chan_messages ORDER BY created,rowid")?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| {
            let m = steve_channels::messages::get(c,&id)?.unwrap();
            let decision = match &m.decision_id {Some(d)=>base::decisions::get(c,d)?,None=>None};
            Ok(json!({"message":m,"decision":decision,"gate":presence::stop(c,WS,&id,f.ch.now())?}))
        }).collect()
    })?;
    Ok(Json(
        json!({"connection":f.connection,"now":f.ch.now(),"rows":rows}),
    ))
}

async fn decide(
    Extension(f): F,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    let actor = if v["yolo"] == true { YOLO_ACTOR } else { ME };
    let d = steve_modules::decisions::dispatch(
        &f.env.modules,
        &caller(WS, actor),
        &id,
        v["choice"].as_str().unwrap_or("yes"),
        "",
    )
    .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(serde_json::to_value(d)?))
}

async fn tick(Extension(f): F, Json(v): Json<Value>) -> Json<Value> {
    f.env.clock.advance(v["ms"].as_i64().unwrap_or(11_000));
    Json(json!({"sent":f.out.send_due(&f.ch).await,"now":f.ch.now()}))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("CHANNEL_FIXTURE_URL")?;
    let parsed = url::Url::parse(&endpoint)?;
    if !matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    ) {
        return Err("the acceptance producer must be on loopback".into());
    }
    let token = std::fs::read_to_string(std::env::var("CHANNEL_FIXTURE_TOKEN_FILE")?)?;
    let core = Arc::new(FakeCore::default());
    let flows = FlowSetup::new(core.clone());
    let disclosure = Disclosure::with_models(Vec::new());
    let link = OutLink::new();
    let env = TestEnv::new(|r| {
        flows.register(r);
        connections::register(r);
        r.outbound_check(Box::new(disclosure.clone()));
        r.decision(SUBJECT_MESSAGE, link.handler());
    });
    *core.store.lock().unwrap() = Some(env.store().clone());
    disclosure.bind(env.store().clone(), env.gateway.clone());
    link.bind_clock(env.clock.clone());
    env.gateway.script(CHECK_HEAD, PASS);
    env.store().transaction(|tx| -> ApiResult<()> {
        ensure_seeds(tx, WS)?;
        repo::upsert_declared(tx, ext_ws::SYSTEM, ext_ws::SYSTEM_LABEL, "ext_ws")?;
        tx.execute_batch(
            "INSERT INTO bots(id,workspace_id,name,main,harness,persona,role,rules,model,updated)
          VALUES('steve','default','Steve',1,'claude','','','','fixture-model',1);
          INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES('steve','default',1);",
        )?;
        Ok(())
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
    let out = Arc::new(Outbox::new(link).act_as_user(ext.clone()));
    let model = Arc::new(Slow {
        clock: env.clock.clone(),
        advance: AtomicI64::new(0),
        prompts: Mutex::new(Vec::new()),
        during: Mutex::new(None),
    });
    let replies = Arc::new(
        Replies::new()
            .outbound(out.clone())
            .composer(Composer::new().pinned(model)),
    );
    let flow = flows
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .decl(ext_ws::decl())
        .adapter(ext.clone())
        .responder(replies)
        .build();
    let mut fields = BTreeMap::new();
    fields.insert(ext_ws::URL.into(), Zeroizing::new(endpoint));
    fields.insert(
        ext_ws::TOKEN.into(),
        Zeroizing::new(token.trim().to_string()),
    );
    let connection = conns
        .create(&caller(WS, ME), ext_ws::ADAPTER, &fields, json!({}))
        .await?
        .id;
    env.store().transaction(|tx| {
        spaces::set_account_rules(
            tx,
            &Viewer::bare(WS, ME),
            &connection,
            &json!([{"when":{"kind":"all"},"act":"draft"}]),
            ch.now(),
        )
    })?;
    let runtime = ConnectionRuntime::builder(ch.clone())
        .hook(
            ext_ws::ADAPTER,
            Some(ext_ws::decl()),
            Arc::new(ExtWsHook::new(ch.clone(), ext)),
        )
        .build();
    let (tx, rx) = mpsc::channel(256);
    drop(runtime.start(rx));
    for event in env.take_events() {
        let _ = tx.try_send(serde_json::to_value(event)?);
    }
    let modules = env.modules.clone();
    let f = Arc::new(Fixture {
        env,
        _keys: keys,
        ch: ch.clone(),
        out,
        connection,
        _runtime: runtime,
        _events: tx,
    });
    let routes = Router::new()
        .route("/fixture/state", get(state))
        .route("/fixture/decision/{id}", post(decide))
        .route("/fixture/tick", post(tick))
        .nest("/api", api::routes(ch))
        .layer(Extension(f))
        .layer(Extension(caller(WS, ME)))
        .with_state(modules);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, routes).await?;
    Ok(())
}
