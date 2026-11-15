//! Caller-scoped reading APIs. Never emits personal entry/read data to workspace SSE.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::{StoreError, feed};

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/me/feed", get(list))
        .route("/me/feed/read", post(read))
        .route("/me/feed/{id}", get(detail))
}
fn error(e: StoreError) -> ApiError {
    match e {
        StoreError::Conflict(s) => ApiError::invalid(s),
        StoreError::NotFound(s) => ApiError::not_found(s),
        e => e.into(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    kind: Option<String>,
    source: Option<String>,
    unread: Option<bool>,
    cursor: Option<String>,
    limit: Option<usize>,
}
async fn list(
    State(m): State<Modules>,
    who: Caller,
    Query(q): Query<List>,
) -> ApiResult<Json<feed::Page>> {
    m.blocking(move |m| {
        let filter = feed::Filter {
            kind: q.kind,
            source: q.source,
            unread: q.unread.unwrap_or(false),
        };
        m.store
            .transaction(|tx| {
                feed::list(
                    tx,
                    &who.ws,
                    &who.me,
                    &filter,
                    q.cursor.as_deref(),
                    q.limit.unwrap_or(50),
                    m.clock.now(),
                )
            })
            .map(Json)
            .map_err(error)
    })
    .await
}
async fn detail(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<feed::Entry>> {
    m.blocking(move |m| {
        m.store
            .reader(|c| feed::detail(c, &who.ws, &who.me, &id))
            .map(Json)
            .map_err(error)
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, untagged)]
enum Read {
    Ids {
        ids: Vec<String>,
    },
    Snapshot {
        snapshot: String,
        kind: Option<String>,
        source: Option<String>,
        unread: Option<bool>,
    },
}
async fn read(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<Read>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let updated = m
            .store
            .transaction(|tx| match b {
                Read::Ids { ids } => feed::read_ids(tx, &who.ws, &who.me, &ids, m.clock.now()),
                Read::Snapshot {
                    snapshot,
                    kind,
                    source,
                    unread,
                } => feed::read_snapshot(
                    tx,
                    &who.ws,
                    &who.me,
                    &feed::Filter {
                        kind,
                        source,
                        unread: unread.unwrap_or(false),
                    },
                    &snapshot,
                    m.clock.now(),
                ),
            })
            .map_err(error)?;
        Ok(Json(json!({"updated":updated})))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
    fn env() -> TestEnv {
        let e = TestEnv::new(|_| {});
        e.store().transaction(|tx|->steve_store::Result<()> {
            feed::publish_report(tx,WS,"flow","run","n",&feed::Content{title:"Report".into(),markdown:"Markdown".into(),..Default::default()},&json!({"text":"Markdown","digest":0}),10)?;
            tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created) VALUES('private',?1,?2,'event','key','log','Private title\nPrivate body','held',10)",rusqlite::params![WS,ME])?;
            Ok(())
        }).unwrap();
        e.take_events();
        e
    }
    #[tokio::test]
    async fn feed_json_contract_scoping_invalid_parameters_and_no_sse_or_model() {
        let e = env();
        let router = routes().with_state(e.modules.clone());
        let me = caller(WS, ME);
        let (status, page) = call(&router, Some(&me), "GET", "/me/feed?limit=1", None).await;
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        assert_eq!(page["entries"][0]["id"], "feed_private");
        assert_eq!(page["entries"][0]["body"], Value::Null);
        assert_eq!(page["entries"][0]["purged"], false);
        assert_eq!(page["unread_count"], 2);
        assert!(page["next_cursor"].is_string());
        assert!(page["snapshot"].is_string());
        let (s, detail) = call(&router, Some(&me), "GET", "/me/feed/feed_private", None).await;
        assert_eq!(s, 200);
        assert_eq!(detail["body"]["markdown"], "Private title\nPrivate body");
        assert_eq!(detail["read_at"], Value::Null);
        let other = caller(WS, "usr_other");
        assert_eq!(
            call(&router, Some(&other), "GET", "/me/feed/feed_private", None)
                .await
                .0,
            404
        );
        assert_eq!(
            call(
                &router,
                Some(&caller(WS2, ME)),
                "GET",
                "/me/feed/feed_private",
                None
            )
            .await
            .0,
            404
        );
        for path in [
            "/me/feed?kind=invalid",
            "/me/feed?source=invalid",
            "/me/feed?limit=0",
            "/me/feed?cursor=invalid",
            "/me/feed?user_id=usr_other",
        ] {
            assert_eq!(
                call(&router, Some(&me), "GET", path, None).await.0,
                400,
                "{path}"
            );
        }
        for who in [&other, &caller(WS2, ME)] {
            assert_eq!(
                call(
                    &router,
                    Some(who),
                    "POST",
                    "/me/feed/read",
                    Some(&json!({"snapshot":page["snapshot"]}))
                )
                .await
                .0,
                400
            );
        }
        for body in [
            json!({"ids":["feed_private","foreign"]}),
            json!({"ids":["feed_private"],"snapshot":page["snapshot"]}),
            json!({"snapshot":page["snapshot"],"unread":true}),
            json!({"ids":[]}),
        ] {
            assert!(matches!(
                call(&router, Some(&me), "POST", "/me/feed/read", Some(&body))
                    .await
                    .0,
                400 | 404
            ));
        }
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/feed/read",
                Some(&json!({"ids":["feed_private"]}))
            )
            .await
            .1["updated"],
            1
        );
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/feed/read",
                Some(&json!({"ids":["feed_private"]}))
            )
            .await
            .1["updated"],
            0
        );
        assert_eq!(
            call(
                &router,
                Some(&me),
                "POST",
                "/me/feed/read",
                Some(&json!({"snapshot":page["snapshot"]}))
            )
            .await
            .1["updated"],
            1
        );
        e.store()
            .transaction(|tx| -> steve_store::Result<()> {
                tx.execute(
                    "DELETE FROM workspace_members WHERE user_id=? AND workspace_id=?",
                    rusqlite::params![ME, WS],
                )?;
                Ok(())
            })
            .unwrap();
        for (method, path, body) in [
            ("GET", "/me/feed", None),
            ("GET", "/me/feed/feed_private", None),
            (
                "POST",
                "/me/feed/read",
                Some(json!({"ids":["feed_private"]})),
            ),
        ] {
            assert_eq!(
                call(&router, Some(&me), method, path, body.as_ref())
                    .await
                    .0,
                404
            );
        }
        assert!(e.gateway.calls.lock().unwrap().is_empty());
        assert!(e.take_events().is_empty());
    }
}
