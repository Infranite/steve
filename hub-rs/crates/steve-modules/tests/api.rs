//! Error codes, extractors, ownership and decision dispatch, through a small router the way P1/V1/A1
//! routers will be built.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{patch, post};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::decisions::{self, DecisionCx, DecisionHandler};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, call_raw, caller};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules, Registry, own};
use steve_store::base::mems::{self, MemPatch, NewMem};
use steve_store::base::{self, Level, ObjRef, Visibility};
use steve_store::{ChangeEvent, Decision, Tx};

#[derive(Deserialize)]
struct NewNote {
    text: String,
    #[serde(default)]
    level: Option<Level>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    members: Vec<Member>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Member {
    contact_id: String,
}

async fn create(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<NewNote>,
) -> ApiResult<Json<Value>> {
    Check::new()
        .len("text", &b.text, 1, 2000)
        .list("tags", &b.tags, 3, 20)
        .done()?;
    let mem = m
        .blocking(move |m| {
            Ok(m.store.transaction(|tx| {
                mems::create(
                    tx,
                    &who.ws,
                    &NewMem {
                        text: b.text,
                        level: b.level.unwrap_or(Level::Conclusion),
                        created_by: who.me.clone(),
                        about: vec![ObjRef::new("party", "ct_x")],
                        ..Default::default()
                    },
                )
            })?)
        })
        .await?;
    Ok(Json(json!({ "id": mem.id, "members": b.members.len() })))
}

#[derive(Deserialize)]
struct Edit {
    text: Option<String>,
}

async fn edit(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<Edit>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let viewer = m.viewer(&who)?;
        let mem = own(m.store.reader(|c| mems::get(c, &id))?, &viewer, "记忆")?;
        if mem.text == "锁定" {
            return Err(ApiError::rule("这条记忆不能改"));
        }
        if b.text.as_deref() == Some(mem.text.as_str()) {
            return Err(ApiError::conflict("没有变化"));
        }
        let m2 = m.store.transaction(|tx| {
            mems::patch(
                tx,
                &id,
                &MemPatch {
                    text: b.text,
                    ..Default::default()
                },
            )
        })?;
        Ok(Json(serde_json::to_value(m2)?))
    })
    .await
}

fn router(m: Modules) -> Router {
    Router::new()
        .route("/mems", post(create))
        .route("/mems/{id}", patch(edit))
        .with_state(m)
}

/// A memory candidate confirmed with a `confirm` decision (what P3/P4 will register for `mem`).
struct MemConfirm;
impl DecisionHandler for MemConfirm {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        match cx.choice {
            "yes" => {
                mems::patch(
                    tx,
                    &d.subject_id,
                    &MemPatch {
                        status: Some("confirmed".into()),
                        ..Default::default()
                    },
                )?;
            }
            _ => {
                mems::delete(tx, &d.subject_id)?;
            }
        }
        Ok(decisions::choice_result(cx))
    }
    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        mems::patch(
            tx,
            &d.subject_id,
            &MemPatch {
                status: Some("stale".into()),
                ..Default::default()
            },
        )?;
        Ok(())
    }
}

fn env() -> TestEnv {
    TestEnv::new(|r: &mut Registry| {
        r.decision("mem", Box::new(MemConfirm));
    })
}

#[tokio::test]
async fn validation_errors_name_the_fields() {
    let t = env();
    let r = router(t.modules.clone());
    let me = caller(WS, ME);
    let (s, b) = call(&r, Some(&me), "POST", "/mems", Some(&json!({"text": ""}))).await;
    assert_eq!((s, b), (400, json!({"error": "参数不对：text"})));
    let (s, b) = call(
        &r,
        Some(&me),
        "POST",
        "/mems",
        Some(&json!({"text": "", "tags": ["a", "", "c", "d"]})),
    )
    .await;
    assert_eq!(
        (s, b),
        (400, json!({"error": "参数不对：text、tags、tags.1"}))
    );
    let (s, b) = call(&r, Some(&me), "POST", "/mems", Some(&json!({}))).await;
    assert_eq!(
        (s, b),
        (400, json!({"error": "参数不对：text"})),
        "missing field"
    );
    let (s, b) = call(&r, Some(&me), "POST", "/mems", Some(&json!({"text": 3}))).await;
    assert_eq!(
        (s, b),
        (400, json!({"error": "参数不对：text"})),
        "wrong type"
    );
    let (s, b) = call(
        &r,
        Some(&me),
        "POST",
        "/mems",
        Some(&json!({"text": "a", "level": "secret"})),
    )
    .await;
    assert_eq!(
        (s, b),
        (400, json!({"error": "参数不对：level"})),
        "unknown enum code"
    );
    let (s, b) = call(
        &r,
        Some(&me),
        "POST",
        "/mems",
        Some(&json!({"text": "a", "members": [{"contact_id": "ct_1"}, {}]})),
    )
    .await;
    assert_eq!(
        (s, b),
        (400, json!({"error": "参数不对：members.1.contact_id"}))
    );
    let (s, b) = call_raw(&r, &me, "POST", "/mems", "{not json").await;
    assert_eq!((s, b), (400, json!({"error": "参数不对：body"})));
    let (s, b) = call(&r, None, "POST", "/mems", Some(&json!({"text": "a"}))).await;
    assert_eq!((s, b), (401, json!({"error": "unauthenticated"})));
    let (s, b) = call(
        &r,
        Some(&me),
        "POST",
        "/mems",
        Some(&json!({"text": "a", "members": [{"contact_id": "ct_1"}]})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(b["members"], 1);
    assert!(b["id"].as_str().unwrap().starts_with("mm_"));
    assert_eq!(
        t.take_events(),
        [ChangeEvent::Mm {
            id: b["id"].as_str().unwrap().into(),
            ws: WS.into()
        }]
    );
}

#[tokio::test]
async fn ownership_and_domain_errors() {
    let t = env();
    let r = router(t.modules.clone());
    let me = caller(WS, ME);
    let (_, b) = call(
        &r,
        Some(&me),
        "POST",
        "/mems",
        Some(&json!({"text": "只给我"})),
    )
    .await;
    let id = b["id"].as_str().unwrap().to_string();
    let path = format!("/mems/{id}");
    // private: another user of the same workspace gets the same 404 as a missing id
    let (s, b) = call(
        &r,
        Some(&caller(WS, OTHER)),
        "PATCH",
        &path,
        Some(&json!({"text": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个记忆"})));
    let (s, _) = call(
        &r,
        Some(&caller(WS2, ME)),
        "PATCH",
        &path,
        Some(&json!({"text": "x"})),
    )
    .await;
    assert_eq!(s, 404, "other workspace");
    let (s, _) = call(
        &r,
        Some(&me),
        "PATCH",
        "/mems/mm_nope",
        Some(&json!({"text": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, b) = call(
        &r,
        Some(&me),
        "PATCH",
        &path,
        Some(&json!({"text": "只给我"})),
    )
    .await;
    assert_eq!((s, b), (409, json!({"error": "没有变化"})));
    let (s, b) = call(
        &r,
        Some(&me),
        "PATCH",
        &path,
        Some(&json!({"text": "锁定"})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(b["text"], "锁定");
    assert_eq!(b["visibility"], "private");
    assert_eq!(b["level"], "conclusion");
    assert_eq!(b["about"], json!([{"type": "party", "id": "ct_x"}]));
    let (s, b) = call(&r, Some(&me), "PATCH", &path, Some(&json!({"text": "y"}))).await;
    assert_eq!((s, b), (400, json!({"error": "这条记忆不能改"})));

    // workspace-visible memories are reachable by the other user
    let shared = t
        .store()
        .transaction(|tx| {
            mems::create(
                tx,
                WS,
                &NewMem {
                    text: "大家".into(),
                    visibility: Visibility::Workspace,
                    created_by: ME.into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let (s, _) = call(
        &r,
        Some(&caller(WS, OTHER)),
        "PATCH",
        &format!("/mems/{}", shared.id),
        Some(&json!({"text": "改"})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(ApiError::Internal("x".into()).status(), 500);
}

fn candidate(t: &TestEnv) -> (String, Decision) {
    t.store()
        .transaction(|tx| {
            let m = mems::create(
                tx,
                WS,
                &NewMem {
                    text: "喜欢简短的邮件".into(),
                    status: "candidate".into(),
                    writer: "steve".into(),
                    source_type: "steve".into(),
                    created_by: ME.into(),
                    ..Default::default()
                },
            )?;
            let d = base::decisions::open(
                tx,
                &decisions::confirm(WS, "mem", &m.id, "记住：喜欢简短的邮件？", "steve"),
            )?;
            mems::patch(
                tx,
                &m.id,
                &MemPatch {
                    decision_id: Some(Some(d.id.clone())),
                    ..Default::default()
                },
            )?;
            Ok::<_, ApiError>((m.id, d))
        })
        .unwrap()
}

#[tokio::test]
async fn confirm_decisions_dispatch_to_the_module() {
    let t = env();
    let me = caller(WS, ME);
    let (mid, d) = candidate(&t);
    assert_eq!(
        (d.kind.as_str(), d.options.clone(), d.item_id.clone()),
        ("confirm", vec!["yes".to_string(), "no".into()], None)
    );
    let ev = t.take_events();
    assert!(ev.contains(&ChangeEvent::Decision {
        id: d.id.clone(),
        item_id: Some(None)
    }));

    assert_eq!(
        decisions::dispatch(&t.modules, &me, &d.id, "maybe", "")
            .unwrap()
            .unwrap_err(),
        ApiError::invalid("choice")
    );
    assert_eq!(
        decisions::dispatch(&t.modules, &caller(WS2, ME), &d.id, "yes", "")
            .unwrap()
            .unwrap_err(),
        ApiError::not_found("决定")
    );
    let done = decisions::dispatch(&t.modules, &me, &d.id, "yes", "")
        .unwrap()
        .unwrap();
    assert_eq!(
        (done.status.as_str(), done.result.clone()),
        ("resolved", json!({"choice": "yes"}))
    );
    assert_eq!(
        t.store()
            .reader(|c| mems::get(c, &mid))
            .unwrap()
            .unwrap()
            .status,
        "confirmed"
    );
    let again = decisions::dispatch(&t.modules, &me, &d.id, "no", "")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        (again.status(), again.to_string()),
        (409, "这个决定已经处理过了".to_string())
    );
    assert!(
        t.store().reader(|c| mems::get(c, &mid)).unwrap().is_some(),
        "the second answer did nothing"
    );

    // “no” deletes the candidate
    let (mid2, d2) = candidate(&t);
    decisions::dispatch(&t.modules, &me, &d2.id, "no", "不对")
        .unwrap()
        .unwrap();
    assert!(t.store().reader(|c| mems::get(c, &mid2)).unwrap().is_none());
    assert_eq!(
        t.store().decisions().get(&d2.id).unwrap().unwrap().result,
        json!({"choice": "no", "text": "不对"})
    );

    // expiry
    let (mid3, d3) = candidate(&t);
    assert!(decisions::expire(&t.modules, &d3.id).unwrap());
    assert!(!decisions::expire(&t.modules, &d3.id).unwrap());
    assert_eq!(
        t.store().decisions().get(&d3.id).unwrap().unwrap().status,
        "expired"
    );
    assert_eq!(
        t.store()
            .reader(|c| mems::get(c, &mid3))
            .unwrap()
            .unwrap()
            .status,
        "stale"
    );
}

#[tokio::test]
async fn unregistered_subjects_fall_through_to_steve_core() {
    let t = env();
    let d = t
        .store()
        .decisions()
        .create(&steve_store::NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: "ST-1".into(),
            question: "q".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert!(decisions::dispatch(&t.modules, &caller(WS, ME), &d.id, "x", "").is_none());
    assert!(decisions::dispatch(&t.modules, &caller(WS, ME), "dec_nope", "x", "").is_none());
    assert!(t.modules.registry.handles_decision("mem"));
    assert!(!t.modules.registry.handles_decision("item"));
}

#[tokio::test]
async fn store_errors_map_to_codes() {
    let e: ApiError = steve_store::StoreError::NotFound("没有这个项目").into();
    assert_eq!(
        (e.status(), e.to_string()),
        (404, "没有这个项目".to_string())
    );
    let e: ApiError = rusqlite::Error::QueryReturnedNoRows.into();
    assert_eq!(e.status(), 500);
    let e = ApiError::Invalid(vec!["a".into(), "b.0".into()]);
    assert_eq!(e.to_string(), "参数不对：a、b.0");
}
