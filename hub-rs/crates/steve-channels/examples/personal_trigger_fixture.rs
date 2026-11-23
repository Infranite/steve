//! Manual loopback acceptance server for personal triggers. No production messages or services.
use axum::{
    Extension, Json, Router,
    extract::{Path, Query},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use steve_channels::{By, Mention, api};
use steve_modules::testkit::{ME, WS, caller};

#[path = "support/personal_triggers.rs"]
mod support;
use support::Fixture;
type F = Extension<Arc<Fixture>>;

async fn state(Extension(f): F) -> Json<Value> {
    let outbound=f.env.store().reader(|c| -> steve_modules::ApiResult<Vec<Value>> {
        let mut st=c.prepare("SELECT id,status,text FROM chan_messages WHERE direction='out' ORDER BY rowid")?;
        Ok(st.query_map([], |r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).unwrap();
    Json(
        json!({"connections":{"me":f.me,"second":f.second,"other":f.other,"bot":f.bot},"sends":f.sends(),"outbound":outbound}),
    )
}
async fn mode(Extension(f): F, Json(v): Json<Value>) -> Json<Value> {
    f.reply(v["approval"] == true);
    if v["skip"] == true {
        f.env.gateway.unscript(steve_channels::compose::H_SAFETY);
        f.env.gateway.script(
            steve_channels::compose::H_SAFETY,
            r#"{"outcome":"skip","text":"No action needed","why":"No explicit request","needs_approval":false}"#,
        );
    }
    *f.fake.block_target_send.lock().unwrap() =
        (v["blocked"] == true).then(|| "target_forbidden: synthetic fixture policy".into());
    *f.fake.fail_send.lock().unwrap() =
        (v["failed"] == true).then(|| "synthetic fixture transport failure".into());
    *f.fake.lose_answer.lock().unwrap() = v["unknown"] == true;
    *f.fake.fail_reactions.lock().unwrap() = v["reaction_failure"] == true;
    Json(json!({"ok":true}))
}
async fn inbound(Extension(f): F, Json(v): Json<Value>) -> Json<Value> {
    let conn = match v["connection"].as_str() {
        Some("second") => &f.second,
        Some("other") => &f.other,
        Some("bot") => &f.bot,
        _ => &f.me,
    };
    let id = v["id"].as_str().unwrap_or("synthetic");
    let mut m = f.message(
        id,
        v["from"].as_str().unwrap_or("owner-one"),
        v["text"].as_str().unwrap_or("/bot synthetic request"),
    );
    m.mine = v["mine"] == true;
    m.sender.is_bot = v["bot"] == true;
    m.thread_key = v["thread"].as_str().map(String::from);
    if v["history"] == true {
        m.created = 1;
    }
    if v["mention"] == true {
        m.mentions.push(Mention {
            handle: None,
            name: "Fixture Person".into(),
            is_connection: true,
        });
    }
    let a = f.receive(conn, m).await;
    Json(
        json!({"message_id":a.message_id,"space_id":a.space_id,"duplicate":a.duplicate,
        "trigger":matches!(a.response.as_ref().map(|r|&r.by),Some(By::Trigger{..})),
        "label":a.response.as_ref().map(|r|&r.label),"out_message_id":a.response.as_ref().and_then(|r|r.out_message_id.as_ref()),
        "decision_id":a.response.as_ref().and_then(|r|r.decision_id.as_ref()),"record":f.record(&a.space_id)}),
    )
}

async fn record(
    Extension(f): F,
    Path(conv): Path<String>,
    Query(q): Query<steve_channels::conv_record::RecordQuery>,
) -> steve_modules::ApiResult<Json<Value>> {
    let viewer = f.env.modules.viewer(&caller(WS, ME))?;
    Ok(Json(steve_channels::conv_record::record(
        f.env.store(),
        WS,
        &viewer,
        &conv,
        &q,
    )?))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let f = Arc::new(Fixture::new().await);
    let modules = f.env.modules.clone();
    let ch = f.ch.clone();
    let routes = Router::new()
        .route("/fixture/state", get(state))
        .route("/fixture/mode", post(mode))
        .route("/fixture/inbound", post(inbound))
        .nest(
            "/api",
            api::routes(ch.clone())
                .merge(steve_channels::channels_api::routes(ch))
                .route("/conversations/{space}/record", get(record)),
        )
        .layer(Extension(f))
        .layer(Extension(caller(WS, ME)))
        .with_state(modules);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("fixture pid: {}", std::process::id());
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, routes).await?;
    Ok(())
}
