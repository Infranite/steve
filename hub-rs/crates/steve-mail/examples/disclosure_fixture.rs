//! Manual, loopback-only mail API fixture: real draft/approval/send/C5 paths, scripted model and
//! fake TLS IMAP/SMTP. No real accounts, harnesses or external services. Run with:
//! `cargo run --manifest-path hub-rs/Cargo.toml -p steve-mail --example disclosure_fixture --features fake`.
//! The printed URL exposes fixture controls without authentication; never expose it off loopback.

#[path = "../tests/support/mod.rs"]
mod support;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Extension, Json, Router,
    extract::Path,
    routing::{get, post},
};
use serde_json::{Value, json};
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_mail::{DraftHooks, mcp, repo};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::outbound::{Audience, CheckCx, CheckResult, KnowledgeUse, OutboundCheck};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::{ApiError, ApiResult, GatewayOut};

use support::*;

#[derive(Default)]
struct Controls {
    yolo: AtomicBool,
    hang: AtomicBool,
}
impl DraftHooks for Controls {
    fn yolo_on(&self, _: &str, _: Option<&str>) -> bool {
        self.yolo.load(Ordering::SeqCst)
    }
}
struct Check {
    real: Disclosure,
    controls: Arc<Controls>,
}
#[async_trait::async_trait]
impl OutboundCheck for Check {
    async fn check(&self, cx: &CheckCx, a: &Audience, t: &str, k: &[KnowledgeUse]) -> CheckResult {
        if !cx.skip_model && self.controls.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.real.check(cx, a, t, k).await
    }
}

struct Fixture {
    h: H,
    controls: Arc<Controls>,
    conn: String,
    thread: String,
}
type F = Extension<Arc<Fixture>>;

async fn state(Extension(f): F) -> ApiResult<Json<Value>> {
    let drafts = f.h.env.store().reader(|c| -> ApiResult<Vec<Value>> {
        let mut stmt = c.prepare("SELECT id FROM mail_drafts ORDER BY rowid")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                let d = repo::draft(c, id)
                    .map_err(|e| ApiError::Internal(e.to_string()))?
                    .unwrap();
                let out = steve_mail::drafts::draft_out(c, &d)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                let mut v = json!(out);
                let mut ds =
                    c.prepare("SELECT id FROM decisions WHERE subject_id=? ORDER BY rowid")?;
                let decisions = ds
                    .query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v["decisions"] = json!(
                    decisions
                        .iter()
                        .map(|id| steve_store::base::decisions::get(c, id).unwrap())
                        .collect::<Vec<_>>()
                );
                Ok(v)
            })
            .collect()
    })?;
    let sent: Vec<Value> = f.h.smtp.sent().iter().map(|s| {
        let parsed = steve_mail::parse::parse(&s.data, false).unwrap();
        json!({"from":s.from,"rcpt":s.rcpt,"subject":parsed.subject,"body":parsed.text,"message_id":parsed.message_id})
    }).collect();
    Ok(Json(json!({"connection_id":f.conn,"thread_id":f.thread,
        "yolo":f.controls.yolo.load(Ordering::SeqCst),"drafts":drafts,"smtp":sent,
        "check_prompts":f.h.env.gateway.prompts.lock().unwrap().iter().filter(|p|p.starts_with(CHECK_HEAD)).collect::<Vec<_>>()})))
}

async fn model(Extension(f): F, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let mode = b["model"].as_str().unwrap_or("pass");
    if !["pass", "error", "unreadable", "leak", "same", "hang"].contains(&mode) {
        return Err(ApiError::invalid("model"));
    }
    f.controls.yolo.store(b["yolo"] == true, Ordering::SeqCst);
    f.controls.hang.store(mode == "hang", Ordering::SeqCst);
    f.h.env.gateway.unscript(CHECK_HEAD);
    if mode == "error" {
        f.h.env
            .gateway
            .script_fail(CHECK_HEAD, "fixture model unavailable");
    } else {
        let text = match mode {
            "unreadable" => "no readable verdict",
            "leak" => r#"{"verdict":"leak","spans":[]}"#,
            _ => r#"{"verdict":"pass","spans":[]}"#,
        };
        f.h.env.gateway.script_out(
            CHECK_HEAD,
            GatewayOut {
                text: text.into(),
                model: if mode == "same" { "fake" } else { "reviewer" }.into(),
                ..Default::default()
            },
        );
    }
    Ok(Json(json!({"model":mode,"yolo":b["yolo"]==true})))
}

async fn decide(
    Extension(f): F,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    let who = caller(WS, if b["yolo"] == true { "yolo" } else { ME });
    let decision = steve_modules::decisions::dispatch(
        &f.h.env.modules,
        &who,
        &id,
        b["choice"].as_str().unwrap_or("send"),
        b["text"].as_str().unwrap_or(""),
    )
    .ok_or_else(|| ApiError::not_found("decision"))??;
    Ok(Json(json!(decision)))
}

async fn tick(Extension(f): F) -> Result<Json<Value>, steve_mail::error::MailError> {
    f.h.env.clock.advance(10_000);
    Ok(Json(json!(f.h.mail.send_due().await?)))
}

async fn mcp_draft(Extension(f): F, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "mail".into(),
        item_id: Some("ST-1".into()),
        session_id: Some("ses_mail".into()),
    };
    let result = mcp::call(&f.h.mail, &cx, mcp::DRAFT, &b)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(json!({"result":result})))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let controls = Arc::new(Controls::default());
    let disclosure = Disclosure::with_models(vec!["reviewer".into()]);
    let h = setup_with(Opts {
        check: Some(Arc::new(Check {
            real: disclosure.clone(),
            controls: controls.clone(),
        })),
        hooks: Some(controls.clone()),
        ..Default::default()
    })
    .await;
    disclosure.bind(h.env.store().clone(), h.env.gateway.clone());
    h.env.gateway.script_out(
        CHECK_HEAD,
        GatewayOut {
            text: r#"{"verdict":"pass","spans":[]}"#.into(),
            model: "reviewer".into(),
            ..Default::default()
        },
    );
    h.env.gateway.script(steve_mail::compose::SAFETY,
        r#"{"body":"Compose fixture draft","reply_all":false,"needs_attention":false,"why":"Fixture reply","cites":[]}"#);
    h.inject("INBOX", steve_mail::fake::fixtures::thread()[0]);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let thread = h.by_subject(&conn, "预算审批流程").thread_id;
    h.env.store().with_conn(|c| {
        c.execute_batch("INSERT INTO bots (id,workspace_id,name,harness,model,updated) VALUES ('mail','default','Mail','fixture','writer',1);
            INSERT INTO items (id,workspace_id,no,kind,title,status,stage,created,updated,requested_by) VALUES ('ST-1','default',1,'task','Mail','open','',1,1,'usr_dev');
            INSERT INTO sessions (id,workspace_id,item_id,bot_id,harness,model,cwd,status,created,updated) VALUES ('ses_mail','default','ST-1','mail','fixture','writer','','running',1,1);")?;
        c.execute("INSERT INTO links (workspace_id,subject_type,subject_id,predicate,object_type,object_id,source,created) VALUES ('default','item','ST-1','about','mail_thread',?,'fixture',1)", [&thread])?;
        Ok(())
    })?;
    let modules = h.env.modules.clone();
    let routes = Router::new()
        .nest("/api", steve_mail::api::routes(h.mail.clone()))
        .route("/fixture/state", get(state))
        .route("/fixture/model", post(model))
        .route("/fixture/decisions/{id}", post(decide))
        .route("/fixture/tick", post(tick))
        .route("/fixture/mcp-draft", post(mcp_draft))
        .layer(Extension(Arc::new(Fixture {
            h,
            controls,
            conn,
            thread,
        })))
        .layer(Extension(me()))
        .with_state(modules);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}", listener.local_addr()?);
    axum::serve(listener, routes).await?;
    Ok(())
}
