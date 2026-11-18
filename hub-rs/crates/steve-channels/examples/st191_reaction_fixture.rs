//! Loopback-only manual acceptance server. Isolated SQLite, fake channel and deterministic model;
//! no real credentials, provider writes, installed Hub or services. Also serves the real channel API.
#[path = "../tests/support/owner_presence.rs"]
#[allow(unused_imports, dead_code)]
mod support;
use axum::{
    Extension, Json,
    extract::Path,
    routing::{get, post},
};
use steve_channels::{ReactionEvent, ReactionOrigin};
use steve_modules::ApiError;
use support::*;

type F = Extension<Arc<H>>;

fn ev(h: &H, v: &Value) -> ReactionEvent {
    ReactionEvent {
        space_ext_id: Some(v["space"].as_str().unwrap_or("D_LI").into()),
        message_ext_id: Some(v["target"].as_str().unwrap_or("request").into()),
        message_source_id: None,
        actor: FakeChannel::handle(v["actor"].as_str().unwrap_or("upx")),
        actor_name: None,
        origin: match v["origin"].as_str() {
            Some("manual") => ReactionOrigin::Manual,
            Some("automated") => ReactionOrigin::Automated,
            _ => ReactionOrigin::Unknown,
        },
        emoji: v["emoji"].as_str().unwrap_or("👍").into(),
        at: v["at"].as_i64().unwrap_or(h.ch.now()),
        removed: v["removed"].as_bool().unwrap_or(false),
    }
}

async fn reaction(Extension(h): F, Json(v): Json<Value>) -> ApiResult<Json<Value>> {
    h.ch.on_reaction(WS, &h.me, ev(&h, &v))?;
    Ok(Json(json!({"stored":true})))
}

async fn message(Extension(h): F, Json(v): Json<Value>) -> ApiResult<Json<Value>> {
    let id = v["id"].as_str().unwrap_or("request");
    let space = v["space"].as_str().unwrap_or("D_LI");
    h.fake.space(
        space,
        "Fixture chat",
        if space.starts_with("D_") {
            SpaceKind::Dm
        } else {
            SpaceKind::Group
        },
        false,
        &[("ulilei", "Peer"), ("uhan", "Other")],
    );
    if let Some(during) = v.get("during_reaction") {
        let (ch, conn, event) = (h.ch.clone(), h.me.clone(), ev(&h, during));
        *h.model.during.lock().unwrap() =
            Some(Box::new(move || ch.on_reaction(WS, &conn, event).unwrap()));
    }
    let mut m = msg(
        &h,
        v["space"].as_str().unwrap_or("D_LI"),
        id,
        v["from"].as_str().unwrap_or("ulilei"),
        v["text"].as_str().unwrap_or("请确认这件事"),
        0,
    );
    m = at_you(m);
    let stored = if v["record_only"] == true {
        h.ch.on_inbound(WS, &h.me, m).await?
    } else {
        h.ch.on_message(WS, &h.me, m).await?
    };
    checked(&h).await;
    Ok(Json(
        json!({"message":stored.message_id,"space":stored.space_id,
        "duplicate":stored.duplicate,"response":stored.response.map(|r|json!({"label":r.label,"draft":r.out_message_id,"decision":r.decision_id}))}),
    ))
}

async fn decide(
    Extension(h): F,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    let user = if v["yolo"] == true { YOLO_ACTOR } else { ME };
    let result = steve_modules::decisions::dispatch(
        &h.env.modules,
        &caller(WS, user),
        &id,
        v["choice"].as_str().unwrap_or("yes"),
        "",
    )
    .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(serde_json::to_value(result)?))
}

async fn tick(Extension(h): F, Json(v): Json<Value>) -> Json<Value> {
    h.env.clock.advance(v["ms"].as_i64().unwrap_or(11_000));
    Json(json!({"sent":h.outbox.send_due(&h.ch).await,"now":h.ch.now()}))
}

async fn state(Extension(h): F) -> ApiResult<Json<Value>> {
    let rows = h.env.store().reader(|c| -> ApiResult<Vec<Value>> {
        let mut stmt = c.prepare("SELECT id FROM chan_messages ORDER BY created,rowid")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                let message = steve_channels::messages::get(c, &id)?;
                let gate = presence::stop(c, WS, &id, h.ch.now())?;
                Ok(json!({"message":message,"gate":gate}))
            })
            .collect()
    })?;
    Ok(Json(json!({"now":h.ch.now(),"rows":rows,
        "sends":h.fake.sent.lock().unwrap().len(),"send_attempts":h.fake.send_calls.lock().unwrap().len(),
        "composer_calls":h.model.prompts.lock().unwrap().len()})))
}

async fn record(Extension(h): F, Path(space): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(h.env.store().reader(|c| {
        steve_channels::conv_record::record(
            c,
            WS,
            &Viewer::bare(WS, ME),
            &format!("chan:{space}"),
            &Default::default(),
        )
    })?))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let h = Arc::new(setup().await);
    let routes = Router::new()
        .route("/fixture/message", post(message))
        .route("/fixture/reaction", post(reaction))
        .route("/fixture/decision/{id}", post(decide))
        .route("/fixture/tick", post(tick))
        .route("/fixture/state", get(state))
        .route("/fixture/record/{space}", get(record))
        .nest("/api", api::routes(h.ch.clone()))
        .layer(Extension(h.clone()))
        .layer(Extension(caller(WS, ME)))
        .with_state(h.env.modules.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, routes).await?;
    Ok(())
}
