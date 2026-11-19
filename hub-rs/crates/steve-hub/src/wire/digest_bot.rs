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
            tx.publish(steve_types::DomainEvent::from(steve_types::ConfigEvent::Settings {
                id: who.ws.clone().into(), ws: who.ws.clone().into(),
            }));
            Ok::<_, ApiError>(())
        })?;
        flow.wake();
        Ok(Json(json!({"schedule_id": b.schedule_id})))
    }).await
}

/// The ordinary flow scheduler remains the Bot's execution and policy boundary.
struct Runner(Arc<FlowCx>);

fn runnable(c: &rusqlite::Connection, ws: &str) -> ApiResult<Option<(String, i64)>> {
    let Some(id) = bot::schedule(c, ws)? else {
        return Ok(None);
    };
    let Some(s) = schedules::get(c, ws, &id)? else {
        return Ok(None);
    };
    if !s.enabled
        || !matches!(
            s.spec.mode,
            schedules::Mode::Item | schedules::Mode::ItemAuto
        )
        || s.spec.overlap != OverlapPolicy::Skip
        || s.spec.requester.is_none()
        || s.spec.requester != conv::owner(c, ws)?
    {
        return Ok(None);
    }
    if let Err(e) = schedules::check_refs(c, ws, &s.spec) {
        tracing::warn!(workspace = ws, "digest Bot binding: {e}");
        return Ok(None);
    }
    let busy: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM flow_runs r LEFT JOIN items i ON i.id = r.work_item_id
        WHERE r.workspace_id = ? AND r.flow_id = ? AND (r.status IN ('running', 'waiting', 'retry')
          OR (i.id IS NOT NULL AND i.archived = 0 AND i.status NOT IN ('done', 'drop'))))",
        params![ws, id],
        |r| r.get(0),
    )?;
    if busy {
        return Ok(None);
    }
    // A skipped/failed/empty run must not immediately start again while packets still wait.
    // Keep the old one-minute retry ceiling, using the existing durable run history.
    let last: Option<i64> = c.query_row(
        "SELECT max(started) FROM flow_runs WHERE workspace_id = ? AND flow_id = ?",
        params![ws, id],
        |r| r.get(0),
    )?;
    Ok(Some((
        id,
        last.map_or(i64::MIN, |at| at.saturating_add(60_000)),
    )))
}

impl queue::BotRunner for Runner {
    fn ready(
        &self,
        c: &rusqlite::Connection,
        ws: &str,
    ) -> Result<Option<i64>, steve_store::StoreError> {
        runnable(c, ws)
            .map(|s| s.map(|(_, at)| at))
            .map_err(|e| steve_store::StoreError::Hook(e.to_string()))
    }
    fn start(&self, ws: &str, now: i64) -> Result<(), steve_store::StoreError> {
        let due = self
            .0
            .store()
            .reader(|c| runnable(c, ws))
            .map_err(|e| steve_store::StoreError::Hook(e.to_string()))?;
        if let Some((id, at)) = due
            && at <= now
        {
            let (run, go) = steve_flow::scheduler::start_manual(&self.0, ws, &id)
                .map_err(|e| steve_store::StoreError::Hook(e.to_string()))?;
            if go {
                drop(steve_flow::scheduler::spawn_drive(self.0.clone(), run));
            }
        }
        Ok(())
    }
}

/// Packet-count/age and lease deadlines, woken by committed domain events.
pub async fn run(m: Modules, flow: Arc<FlowCx>, wake: Arc<tokio::sync::Notify>) {
    let source = Arc::new(queue::BotSource::new(&m, Runner(flow)));
    steve_runtime::due::due_loop(
        source,
        m.clock.clone(),
        wake,
        steve_runtime::policy::Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2,
            jitter: steve_runtime::policy::Jitter::None,
            attempts: None,
            reset_after: None,
        },
    )
    .await;
}

/// Queue/configuration and ordinary schedule completion can all make a Bot runnable.
pub fn wakes(event: &steve_types::BusEvent) -> bool {
    use steve_types::{AutomationEvent, ChannelsEvent, ConfigEvent, DomainEvent, WorkEvent};
    matches!(
        event.as_domain(),
        Some(DomainEvent::Channels(
            ChannelsEvent::Digest { .. }
                | ChannelsEvent::Space { .. }
                | ChannelsEvent::Connection { .. }
        )) | Some(DomainEvent::Config(
            ConfigEvent::Settings { .. } | ConfigEvent::Workspace { .. }
        )) | Some(DomainEvent::Automation(
            AutomationEvent::Flow { .. } | AutomationEvent::Run { .. }
        )) | Some(DomainEvent::Work(WorkEvent::Item { .. }))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_flow::testkit::FlowEnv;
    use steve_modules::testkit::{ME, OTHER, WS, call, caller};

    async fn bound() -> (FlowEnv, String) {
        let e = FlowEnv::new();
        let (st, schedule) = call(
            &e.router(),
            Some(&caller(WS, ME)),
            "POST",
            "/schedules",
            Some(&json!({
                "name":"Digest", "cron":"*/30 * * * *", "what":"Organize", "bot":"steve",
                "mode":"item_auto", "overlap":"skip", "requester":ME,
            })),
        )
        .await;
        assert_eq!(st, 200, "{schedule}");
        let id = schedule["id"].as_str().unwrap().to_string();
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        e.env.take_events();
        let (st, v) = call(
            &api,
            Some(&caller(WS, ME)),
            "PUT",
            "/digest/bot",
            Some(&json!({"schedule_id":id})),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        assert!(e.env.take_events().iter().any(|event| matches!(
            event.as_domain(),
            Some(steve_types::DomainEvent::Config(
                steve_types::ConfigEvent::Settings { .. }
            ))
        )));
        (e, id)
    }

    #[tokio::test]
    async fn runnable_excludes_disabled_busy_and_ownerless_bindings_and_spaces_restarts() {
        let (e, id) = bound().await;
        let read = || e.env.store().reader(|c| runnable(c, WS)).unwrap();
        assert_eq!(read(), Some((id.clone(), i64::MIN)));
        e.env
            .store()
            .with_conn(|c| {
                c.execute("UPDATE flows SET enabled = 0 WHERE id = ?", [&id])?;
                Ok(())
            })
            .unwrap();
        assert!(read().is_none());
        e.env.store().with_conn(|c| {
            c.execute("UPDATE flows SET enabled = 1 WHERE id = ?", [&id])?;
            c.execute("INSERT INTO flow_runs (id,workspace_id,flow_id,flow_version,trigger_kind,status,started)
                VALUES ('digest_busy',?, ?,1,'manual','running',1000)", params![WS,id])?;
            Ok(())
        }).unwrap();
        assert!(read().is_none());
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE flow_runs SET status = 'done' WHERE id = 'digest_busy'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            read(),
            Some((id, 61_000)),
            "an empty/failed start cannot spin on queued packets"
        );
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                    params![WS, ME],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(read().is_none());
    }

    #[test]
    fn only_relevant_committed_events_wake_the_bot() {
        use steve_types::{BusEvent, ChannelsEvent, ConfigEvent, DomainEvent};
        for event in [
            DomainEvent::from(ChannelsEvent::Digest {
                id: "p".into(),
                ws: WS.into(),
            }),
            DomainEvent::from(ConfigEvent::Settings {
                id: WS.into(),
                ws: WS.into(),
            }),
            DomainEvent::from(steve_types::AutomationEvent::Run {
                id: "r".into(),
                flow_id: "f".into(),
                ws: WS.into(),
            }),
        ] {
            assert!(wakes(&BusEvent::from(event)));
        }
        assert!(wakes(&BusEvent::from(DomainEvent::from(
            steve_types::WorkEvent::Item {
                id: "item".into(),
                ws: WS.into(),
            }
        ))));
        assert!(!wakes(&BusEvent::from(DomainEvent::from(
            steve_types::WorkEvent::Message {
                id: "message".into(),
                ws: WS.into(),
            }
        ))));
        assert!(!wakes(&BusEvent::from(DomainEvent::from(
            ChannelsEvent::ChanMessage {
                id: "m".into(),
                space_id: "s".into(),
                ws: WS.into(),
            }
        ))));
    }

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
