//! Queue-driven starts reuse the ordinary schedule engine and item/ACP execution path. No model
//! gateway, process, file or backend-specific call belongs here.
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_digest::{bot, conv, queue};
use steve_flow::{FlowCx, policy::OverlapPolicy, schedules};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};

pub fn routes(flow: Arc<FlowCx>) -> Router<Modules> {
    Router::new()
        .route("/digest/bot", get(read).put(bind))
        .layer(Extension(flow))
}
fn owner(m: &Modules, who: &Caller) -> ApiResult<()> {
    if m.store.reader(|c| conv::owner(c, &who.ws))?.as_deref() != Some(who.me.as_str()) {
        return Err(ApiError::Forbidden(
            "Only the digest data owner can configure its Bot".into(),
        ));
    }
    Ok(())
}
async fn read(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        Ok(Json(json!({"schedule_id": m.store.reader(|c| bot::schedule(c, &who.ws))?, "template": bot::template()})))
    }).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    schedule_id: Option<String>,
}
async fn bind(
    State(m): State<Modules>,
    Extension(flow): Extension<Arc<FlowCx>>,
    who: Caller,
    Body(b): Body<Bind>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        m.store.transaction(|tx| {
            if let Some(id) = &b.schedule_id {
                let s = schedules::own(tx, &who.ws, id)?;
                schedules::check_refs(tx, &who.ws, &s.spec)?;
                if !matches!(s.spec.mode, schedules::Mode::Item | schedules::Mode::ItemAuto)
                    || s.spec.requester.as_deref() != Some(who.me.as_str()) || s.spec.overlap != OverlapPolicy::Skip {
                    return Err(ApiError::rule("Bind an item-mode schedule with requester set to your user id and overlap=skip"));
                }
                tx.execute("INSERT OR REPLACE INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)", params![who.ws, bot::SCHEDULE, id, m.clock.now()])?;
            } else {
                tx.execute("DELETE FROM settings WHERE workspace_id = ? AND key = ?", params![who.ws, bot::SCHEDULE])?;
            }
            Ok::<_, ApiError>(())
        })?;
        flow.wake();
        Ok(Json(json!({"schedule_id": b.schedule_id})))
    }).await
}

/// One lightweight queue check per minute; only due work starts a Bot session. Schedule enabled,
/// overlap, budget and requester membership keep their ordinary meaning. Failed packet attempts
/// are retried by the queue; an active item is never restarted by this loop.
pub async fn run(m: Modules, flow: Arc<FlowCx>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        let f = flow.clone();
        let starts = m.blocking(move |m| {
            let mut starts = Vec::new();
            for ws in m.store.workspaces().all()? {
                let start = (|| -> ApiResult<Option<String>> {
                let due = m.store.transaction(|tx| {
                    queue::release_expired(tx, &ws.id, m.clock.now())?;
                    let Some(id) = bot::schedule(tx, &ws.id)? else { return Ok(None) };
                    let Some(s) = schedules::get(tx, &ws.id, &id)? else { return Ok(None) };
                    let b = queue::batching(tx, &ws.id)?;
                    if !s.enabled || !b.enabled || !matches!(s.spec.mode, schedules::Mode::Item | schedules::Mode::ItemAuto)
                        || s.spec.overlap != OverlapPolicy::Skip || s.spec.requester != conv::owner(tx, &ws.id)?
                        || s.spec.requester.is_none() || queue::due(tx, &ws.id, m.clock.now(), &b)?.is_none() {
                        return Ok(None);
                    }
                    schedules::check_refs(tx, &ws.id, &s.spec)?;
                    let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM flow_runs r LEFT JOIN items i ON i.id = r.work_item_id
                        WHERE r.workspace_id = ? AND r.flow_id = ? AND (r.status IN ('running', 'waiting', 'retry')
                          OR (i.id IS NOT NULL AND i.archived = 0 AND i.status NOT IN ('done', 'drop'))))", params![ws.id, id], |r| r.get(0))?;
                    if busy { return Ok(None) }
                    Ok::<_, ApiError>(Some(id))
                })?;
                if let Some(id) = due {
                    let (run, go) = steve_flow::scheduler::start_manual(&f, &ws.id, &id)?;
                    return Ok(go.then_some(run));
                }
                Ok(None)
                })();
                match start {
                    Ok(Some(run)) => starts.push(run),
                    Ok(None) => {},
                    Err(e) => tracing::warn!(workspace = %ws.id, "digest Bot queue check: {e}"),
                }
            }
            Ok(starts)
        }).await;
        match starts {
            Ok(starts) => {
                for id in starts {
                    drop(steve_flow::scheduler::spawn_drive(flow.clone(), id));
                }
            }
            Err(e) => tracing::warn!("digest Bot queue check: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_flow::testkit::FlowEnv;
    use steve_modules::testkit::{ME, OTHER, WS, call, caller};

    #[tokio::test]
    async fn binding_requires_owner_and_explicit_personal_item_schedule() {
        let e = FlowEnv::new();
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        let (st, v) = call(&api, Some(&caller(WS, ME)), "GET", "/digest/bot", None).await;
        assert_eq!(st, 200);
        assert!(
            v["template"]["bot"]["rules"]
                .as_str()
                .unwrap()
                .contains("steve_tool_call")
        );
        assert_eq!(
            call(&api, Some(&caller(WS, OTHER)), "GET", "/digest/bot", None)
                .await
                .0,
            403
        );
        for requester in [None, Some(ME)] {
            let mut body = json!({"name": "Digest", "cron": "*/30 * * * *", "what": "Organize", "bot": "steve", "mode": "item_auto", "overlap": "skip"});
            if let Some(user) = requester {
                body["requester"] = json!(user);
            }
            let (st, schedule) = call(
                &e.router(),
                Some(&caller(WS, ME)),
                "POST",
                "/schedules",
                Some(&body),
            )
            .await;
            assert_eq!(st, 200, "{schedule}");
            let bind = json!({"schedule_id": schedule["id"]});
            let (st, _) = call(
                &api,
                Some(&caller(WS, ME)),
                "PUT",
                "/digest/bot",
                Some(&bind),
            )
            .await;
            assert_eq!(st, if requester.is_some() { 200 } else { 400 });
            assert_eq!(
                call(
                    &api,
                    Some(&caller(WS, OTHER)),
                    "PUT",
                    "/digest/bot",
                    Some(&bind)
                )
                .await
                .0,
                403
            );
        }
        assert!(
            e.env
                .store()
                .reader(|c| bot::schedule(c, WS))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            call(
                &api,
                Some(&caller(WS, ME)),
                "PUT",
                "/digest/bot",
                Some(&json!({"schedule_id": null}))
            )
            .await
            .0,
            200
        );
        assert!(
            e.env
                .store()
                .reader(|c| bot::schedule(c, WS))
                .unwrap()
                .is_none()
        );
    }
}
