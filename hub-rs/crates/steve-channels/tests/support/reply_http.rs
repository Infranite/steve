//! Manual loopback fixture: real Channels, SQLite, Outbox and decision dispatch, fake model/provider.
//! Run with `cargo test -p steve-channels --test im_conversations serve_reply_review -- --ignored --nocapture`.
use super::*;
use axum::{
    Extension, Json, Router,
    extract::Path,
    routing::{get, post},
};
use steve_modules::ApiError;
use tower::ServiceExt;

type F = Extension<Arc<Mutex<Option<Arc<H>>>>>;
fn active(f: &F) -> ApiResult<Arc<H>> {
    f.0.lock()
        .unwrap()
        .clone()
        .ok_or_else(|| ApiError::not_found("fixture request"))
}

async fn request(f: F, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    // Each scenario owns a clean temporary database and fake provider, never an installed home.
    let h = Arc::new(setup(true).await);
    let answer = json!({"outcome":body["outcome"].as_str().unwrap_or("reply"),
        "text":body["text"].as_str().unwrap_or("我是 Steve，值班助手。有什么可以帮你处理的？"),
        "needs_approval":body["needs_approval"] == true});
    if body["approval"] == true {
        set_rules(
            &h,
            &h.bot,
            json!([{"when":{"kind":"mention_or_dm"},"act":"draft"}]),
        );
    }
    if body["refused"] == true {
        *h.fake.fail_send.lock().unwrap() = Some("fixture: provider refused target".into());
    }
    if body["own"] == true {
        let owner = h
            .env
            .store()
            .read::<_, steve_store::StoreError>(|c| Ok(steve_channels::personas::owner_name(c, WS)))
            .unwrap()?;
        let identity = format!("我是{owner}。");
        h.env.gateway.unscript(H_SAFETY);
        h.env.gateway.script(
            H_SAFETY,
            json!({"outcome":"reply","text":identity,"needs_approval":false}).to_string(),
        );
        let source = push(
            &h,
            &h.me,
            msg(
                "D_LI",
                "http-own",
                "ulilei",
                body["input"].as_str().unwrap_or("你是谁啊"),
                false,
                false,
            ),
        )
        .await;
        let acted = source
            .response
            .as_ref()
            .ok_or_else(|| ApiError::not_found("own approval"))?;
        let result = json!({"source":source.message_id,"identity":identity,"delivered":{"outbound":acted.out_message_id,"decision":acted.decision_id}});
        *f.0.lock().unwrap() = Some(h.clone());
        return Ok(Json(result));
    }
    let (turn, reply, source) = if body["own_command_error"] == true {
        own_trigger_words(&h).await;
        let turn = your_trigger(&h, "http-trigger", "/ai 查看状态").await;
        let reply = failed_answer(&h, &turn, "fixture model failed; private details stay here");
        (turn, reply, None)
    } else {
        let (turn, reply, source) =
            channel_answer_for(&h, answer, body["input"].as_str().unwrap_or("你是谁啊")).await;
        (turn, reply, Some(source.message_id))
    };
    if body["missing_generation"] == true {
        seed::conversations::clear_conversation_generation(h.env.store(), &turn)?;
    }
    *f.0.lock().unwrap() = Some(h.clone());
    let delivered = if body["replay_race"] == true {
        let (first, second) = delivery_race(&h, &reply, body["own_command_error"] == true).await;
        json!({"first":format!("{first:?}"),"second":format!("{second:?}")})
    } else {
        let (cold, _) = cold_runtime(&h);
        let d = cold.conv_deliver(WS, &reply).await?;
        json!({"result":format!("{d:?}"),"outbound":d.as_ref().and_then(|d|d.message_id()),"decision":d.as_ref().and_then(|d|d.decision_id())})
    };
    Ok(Json(
        json!({"turn":turn,"reply":reply,"source":source,"delivered":delivered}),
    ))
}

async fn snapshot(f: F) -> ApiResult<Json<Value>> {
    let h = active(&f)?;
    let decisions = inspect::decisions::approvals(h.env.store())?;
    let messages = steve_store::testkit::channels::outbound(h.env.store())?
        .into_iter()
        .map(|m| {
            json!({"id":m.id, "status":m.status, "text":m.text,
            "decision":m.decision.map(|d| d.id), "external_id":m.external_id})
        })
        .collect::<Vec<_>>();
    let receipts=h.fake.sent.lock().unwrap().iter().map(|(target,content,receipt)|json!({"target":format!("{target:?}"),"text":content.text,"provider_message_id":receipt})).collect::<Vec<_>>();
    let items = inspect::items::workspace_items(h.env.store(), WS)?;
    Ok(Json(
        json!({"decisions":decisions,"messages":messages,"receipts":receipts,"items":items,
        "provider_attempts":h.fake.send_calls.lock().unwrap().len(),"environment":"isolated FakeChannel; not a real messaging platform"}),
    ))
}

async fn decide(f: F, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let h = active(&f)?;
    let d = steve_modules::decisions::dispatch(
        &h.env.modules,
        &caller(WS, ME),
        &id,
        body["choice"].as_str().unwrap_or(""),
        body["text"].as_str().unwrap_or(""),
    )
    .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(serde_json::to_value(d)?))
}
async fn tick(f: F, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let h = active(&f)?;
    if body.get("refused").is_some() {
        *h.fake.fail_send.lock().unwrap() = if body["refused"] == true {
            Some("fixture: provider refused target".into())
        } else {
            None
        };
    }
    h.env.clock.advance(11_000);
    let (cold, out) = cold_runtime(&h);
    Ok(Json(json!({"sent":out.send_due(&cold).await})))
}
async fn replay(f: F, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let h = active(&f)?;
    let reply = body["reply"]
        .as_str()
        .ok_or_else(|| ApiError::invalid("reply"))?;
    if body["forget_mark"] == true {
        forget_mark(&h, reply);
    }
    let (cold, _) = cold_runtime(&h);
    Ok(Json(
        json!({"result":format!("{:?}",cold.conv_deliver(WS,reply).await?)}),
    ))
}

async fn api_fallback(f: F, request: axum::extract::Request) -> axum::response::Response {
    let h = active(&f).unwrap();
    steve_channels::api::routes(h.ch.clone())
        .layer(Extension(caller(WS, ME)))
        .with_state(h.env.modules.clone())
        .oneshot(request)
        .await
        .unwrap()
}

async fn record(
    f: F,
    Path(conv): Path<String>,
    axum::extract::Query(q): axum::extract::Query<steve_channels::conv_record::RecordQuery>,
) -> ApiResult<Json<Value>> {
    let h = active(&f)?;
    Ok(Json(steve_channels::conv_record::record(
        h.env.store(),
        WS,
        &Viewer::bare(WS, ME),
        &conv,
        &q,
    )?))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual isolated HTTP fixture; stop the PID printed by the caller"]
async fn serve_reply_review() {
    let fixture: Arc<Mutex<Option<Arc<H>>>> = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/fixture/request", post(request))
        .route("/fixture/state", get(snapshot))
        .route("/fixture/tick", post(tick))
        .route("/fixture/replay", post(replay))
        .route("/api/conversations/{conv}/record", get(record))
        .route("/api/decisions/{id}", post(decide))
        .nest("/api", Router::new().fallback(api_fallback))
        .layer(Extension(fixture));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("REPLY_FIXTURE=http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
