//! Authenticated PERSONAL attention projection. No private SSE or duplicate business storage.
//! `cursor` is snapshot pagination, NOT an incremental event cursor. Complete every page before
//! establishing/replacing a baseline; changed source membership returns 409 and requires restart.
//! `if_snapshot` (single category) returns `unchanged=true` without retransmitting its entries.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::{StoreError, attention};

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/me/attention", get(list))
        .route("/me/attention/read", post(read))
        .route("/me/attention/replies/{id}", get(reply))
}
fn error(e: StoreError) -> ApiError {
    match e {
        StoreError::Conflict(s) => ApiError::conflict(s),
        StoreError::NotFound(s) => ApiError::not_found(s),
        e => e.into(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    kind: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    if_snapshot: Option<String>,
}
async fn list(
    State(m): State<Modules>,
    who: Caller,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=100).contains(&limit)
        || q.kind
            .as_deref()
            .is_some_and(|k| !attention::KINDS.contains(&k))
        || (q.kind.is_none() && (q.cursor.is_some() || q.if_snapshot.is_some()))
        || (q.cursor.is_some() && q.if_snapshot.is_some())
    {
        return Err(ApiError::invalid(
            "attention kind, limit or cursor combination",
        ));
    }
    m.blocking(move |m| {
        m.store.transaction(|tx| -> ApiResult<_> {
            steve_store::feed::member(tx,&who.ws,&who.me).map_err(error)?;
            let viewer=steve_store::base::Viewer::bare(&who.ws,&who.me);
            let now=m.clock.now();
            attention::cleanup(tx,now).map_err(error)?;
            let mut categories=vec![];
            for kind in attention::KINDS {
                if q.kind.as_deref().is_some_and(|k|k!=kind) { continue; }
                if kind=="feed" {
                    categories.push(attention::feed_page(tx,&who.ws,&who.me,q.cursor.as_deref(),q.if_snapshot.as_deref(),limit,now).map_err(error)?);
                    continue;
                }
                let (entries,upper)=match kind {
                    "replies"=>attention::replies(tx,&who.ws,&who.me).map_err(error)?,
                    "pending"=>(attention::pending(tx,&who.ws,&who.me,now).map_err(error)?,0),
                    _=>(steve_channels::conversations::attention(tx,&who.ws,&viewer,now)?,0),
                };
                categories.push(attention::page(tx,&who.ws,&who.me,kind,entries,upper,q.cursor.as_deref(),q.if_snapshot.as_deref(),limit,now).map_err(error)?);
            }
            Ok(Json(json!({"v":1,"epoch":attention::epoch(tx).map_err(error)?,"user_id":who.me,"workspace_id":who.ws,
                "history_policy":"since_attention_v1","categories":categories})))
        })
    }).await
}
async fn reply(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let (id, created) = m
            .store
            .reader(|c| attention::reply(c, &who.ws, &who.me, &id))
            .map_err(error)?;
        Ok(Json(json!({"id":id,"created":created})))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    snapshot: String,
    through_id: String,
}
async fn read(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<Read>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let updated = m
            .store
            .transaction(|tx| {
                attention::read_replies(
                    tx,
                    &who.ws,
                    &who.me,
                    &b.snapshot,
                    &b.through_id,
                    m.clock.now(),
                )
            })
            .map_err(error)?;
        Ok(Json(json!({"updated":updated})))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};

    fn final_reply(
        e: &TestEnv,
        ws: &str,
        user: &str,
        suffix: &str,
        status: &str,
        space: &str,
    ) -> String {
        let msg = format!("question_{suffix}");
        let reply = format!("reply_{suffix}");
        let run = format!("run_{suffix}");
        e.modules.store.transaction(|tx| -> steve_store::Result<()> {
            tx.execute("INSERT INTO messages(id,workspace_id,space,author,text,created) VALUES(?,?,?,'me','question',10)",params![msg,ws,space])?;
            tx.execute("INSERT INTO thread_runs(id,workspace_id,message_id,user_id,status,created) VALUES(?,?,?,?,'running',10)",params![run,ws,msg,user])?;
            tx.execute("INSERT INTO messages(id,workspace_id,space,author,text,meta,created) VALUES(?,?,?,'steve','final',?,20)",params![reply,ws,space,json!({"run":run,"origin":msg,"run_receipt":true}).to_string()])?;
            tx.execute("UPDATE thread_runs SET status=?,reply_id=?,ended=20 WHERE id=?",params![status,reply,run])?;
            Ok(())
        }).unwrap();
        reply
    }
    fn category<'a>(v: &'a Value, k: &str) -> &'a Value {
        v["categories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["kind"] == k)
            .unwrap()
    }
    #[tokio::test]
    async fn terminal_failure_budget_and_stopped_receipts_are_final_but_drafts_are_not() {
        let e = TestEnv::new(|_| {});
        let router = routes().with_state(e.modules.clone());
        let me = caller(WS, ME);
        for status in [
            "done",
            "stopped",
            "budget",
            "violated",
            "failed",
            "queued",
            "running",
            "interrupted",
        ] {
            final_reply(&e, WS, ME, status, status, "steve");
        }
        final_reply(&e, WS, ME, "unbound", "running", "steve");
        e.store()
            .with_conn(|c| {
                c.execute_batch(
                    "UPDATE messages SET meta='{}' WHERE id='reply_unbound';
              UPDATE thread_runs SET status='failed' WHERE id='run_unbound';",
                )?;
                Ok(())
            })
            .unwrap();
        let (s, v) = call(
            &router,
            Some(&me),
            "GET",
            "/me/attention?kind=replies",
            None,
        )
        .await;
        assert_eq!(s, 200);
        let cat = category(&v, "replies");
        assert_eq!(cat["count"], 5);
        let mut ids = cat["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            [
                "reply_budget",
                "reply_done",
                "reply_failed",
                "reply_stopped",
                "reply_violated"
            ]
        );
        for id in ids {
            assert_eq!(
                call(
                    &router,
                    Some(&me),
                    "GET",
                    &format!("/me/attention/replies/{id}"),
                    None
                )
                .await
                .0,
                200
            );
        }
        assert_eq!(
            call(
                &router,
                Some(&me),
                "GET",
                "/me/attention/replies/reply_unbound",
                None
            )
            .await
            .0,
            404
        );
    }

    #[tokio::test]
    async fn personal_attention_contract_paging_reads_privacy_and_no_sse() {
        let e = TestEnv::new(|_| {});
        let router = routes().with_state(e.modules.clone());
        let me = caller(WS, ME);
        let other = caller(WS, "usr_other");
        let (s, baseline) = call(
            &router,
            Some(&me),
            "GET",
            "/me/attention?kind=replies",
            None,
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(category(&baseline, "replies")["count"], 0);
        let baseline_snapshot = category(&baseline, "replies")["snapshot"].as_str().unwrap();
        final_reply(&e, WS, ME, "a", "done", "steve");
        let b = final_reply(&e, WS, ME, "b", "done", "steve");
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":baseline_snapshot,"through_id":b}))
            )
            .await
            .0,
            409
        );
        final_reply(&e, WS, "usr_other", "other", "done", "steve");
        final_reply(&e, WS2, ME, "workspace", "done", "steve");
        final_reply(&e, WS, ME, "draft", "running", "steve");
        final_reply(&e, WS, ME, "channel", "done", "chan:private");
        e.take_events();
        let (s, v) = call(&router, Some(&me), "GET", "/me/attention?limit=1", None).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["v"], 1);
        assert!(v["epoch"].is_string());
        assert_eq!(v["categories"].as_array().unwrap().len(), 4);
        let r = category(&v, "replies");
        assert_eq!(r["count"], 2);
        assert_eq!(r["unit"], "messages");
        assert_eq!(r["entries"][0]["id"], b);
        assert_eq!(r["entries"][0]["created"], 20);
        assert!(
            r.get("snapshot").is_none(),
            "incomplete traversal issues no snapshot"
        );
        for bad_snapshot in [category(&v, "feed")["snapshot"].as_str().unwrap(), "forged"] {
            assert_eq!(
                call(
                    &router,
                    Some(&me),
                    "POST",
                    "/me/attention/read",
                    Some(&json!({"snapshot":bad_snapshot,"through_id":b}))
                )
                .await
                .0,
                409
            );
        }
        assert_eq!(
            call(
                &router,
                Some(&caller(WS, "stranger")),
                "GET",
                "/me/attention",
                None
            )
            .await
            .0,
            404
        );
        let cursor = r["next_cursor"].as_str().unwrap();
        for (method, path, body) in [
            (
                "GET",
                format!("/me/attention?kind=replies&if_snapshot={cursor}"),
                None,
            ),
            (
                "POST",
                "/me/attention/read".into(),
                Some(json!({"snapshot":cursor,"through_id":b})),
            ),
        ] {
            assert_eq!(
                call(&router, Some(&me), method, &path, body.as_ref())
                    .await
                    .0,
                409
            );
        }
        let (s, p) = call(
            &router,
            Some(&me),
            "GET",
            &format!("/me/attention?kind=replies&limit=1&cursor={cursor}"),
            None,
        )
        .await;
        assert_eq!(s, 200, "{p}");
        assert_eq!(category(&p, "replies")["entries"][0]["id"], "reply_a");
        let snapshot = category(&p, "replies")["snapshot"].as_str().unwrap();
        assert!(category(&p, "replies")["next_cursor"].is_null());
        let (s, u) = call(
            &router,
            Some(&me),
            "GET",
            &format!("/me/attention?kind=replies&if_snapshot={snapshot}"),
            None,
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(category(&u, "replies")["unchanged"], true);
        assert_eq!(category(&u, "replies")["entries"], json!([]));
        for body in [
            json!({"snapshot":snapshot,"through_id":"reply_other"}),
            json!({"snapshot":snapshot,"through_id":"reply_workspace"}),
            json!({"snapshot":snapshot,"through_id":"reply_draft"}),
        ] {
            assert_eq!(
                call(
                    &router,
                    Some(&me),
                    "POST",
                    "/me/attention/read",
                    Some(&body)
                )
                .await
                .0,
                409
            );
        }
        assert_eq!(
            call(
                &router,
                Some(&other),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":snapshot,"through_id":b}))
            )
            .await
            .0,
            409
        );
        assert_eq!(
            call(
                &router,
                Some(&caller(WS2, ME)),
                "GET",
                &format!("/me/attention?kind=replies&cursor={cursor}"),
                None
            )
            .await
            .0,
            409
        );
        let c = final_reply(&e, WS, ME, "later", "done", "steve");
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":snapshot,"through_id":c}))
            )
            .await
            .0,
            409
        );
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":snapshot,"through_id":"reply_a"}))
            )
            .await
            .0,
            200
        );
        let (_, partial) = call(
            &router,
            Some(&me),
            "GET",
            "/me/attention?kind=replies",
            None,
        )
        .await;
        assert_eq!(
            category(&partial, "replies")["count"],
            2,
            "viewing the older reply cannot acknowledge a newer off-screen reply"
        );
        for (id, updated) in [(b.as_str(), true), ("reply_a", false), (b.as_str(), false)] {
            let (s, body) = call(
                &router,
                Some(&me),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":snapshot,"through_id":id})),
            )
            .await;
            assert_eq!(s, 200);
            assert_eq!(body["updated"], updated);
        }
        let (_, v) = call(
            &router,
            Some(&me),
            "GET",
            "/me/attention?kind=replies",
            None,
        )
        .await;
        assert_eq!(category(&v, "replies")["count"], 1);
        assert_eq!(category(&v, "replies")["entries"][0]["id"], c);
        let path = format!("/me/attention/replies/{b}");
        let (status, lookup) = call(&router, Some(&me), "GET", &path, None).await;
        assert_eq!(status, 200);
        assert_eq!(lookup, json!({"id":b,"created":20}));
        for viewer in [&other, &caller(WS2, ME)] {
            assert_eq!(call(&router, Some(viewer), "GET", &path, None).await.0, 404);
        }
        for id in ["unknown", "reply_draft", "reply_failed", "reply_channel"] {
            assert_eq!(
                call(
                    &router,
                    Some(&me),
                    "GET",
                    &format!("/me/attention/replies/{id}"),
                    None
                )
                .await
                .0,
                404
            );
        }
        assert_eq!(
            call(
                &router,
                Some(&me),
                "GET",
                &format!("/me/attention?kind=replies&cursor={cursor}"),
                None
            )
            .await
            .0,
            409
        );
        assert!(
            e.take_events().is_empty(),
            "personal reads must not broadcast"
        );
        for path in [
            "/me/attention?limit=0",
            "/me/attention?limit=101",
            "/me/attention?kind=bad",
            "/me/attention?cursor=fake",
            "/me/attention?user_id=other",
        ] {
            assert_eq!(
                call(&router, Some(&me), "GET", path, None).await.0,
                400,
                "{path}"
            );
        }
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/attention/read",
                Some(&json!({"snapshot":snapshot}))
            )
            .await
            .0,
            400
        );
        assert_eq!(
            call(&router, None, "GET", "/me/attention", None).await.0,
            401
        );
    }
}
