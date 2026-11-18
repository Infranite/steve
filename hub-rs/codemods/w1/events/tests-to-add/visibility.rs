//! Pending W1.3a integration test. Move to steve-hub/tests/visibility.rs AFTER Bus::publish accepts
//! Vec<DomainEvent> and Bus::live accepts LiveEvent. This preparation file is deliberately not built.
//! Calls the actual /api/stream handler and reads its SSE body, including coalescing/filtering.
//! No copied visibility predicate, running Hub, database, real hosts, adapters or model calls.
use axum::{
    Extension,
    body::Body,
    extract::{Query, State},
    http::HeaderMap,
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use steve_hub::{
    api::{
        AppState, Ctx,
        stream::{self, StreamQuery},
    },
    backend::{BResult, Backend, NoDomain, Repo},
    bus::Bus,
    config::Config,
};
use steve_types::{DevicesEvent, PeopleEvent, RawJson, WorkLiveEvent};

struct NoStorage;
impl Backend for NoStorage {
    fn reader(&self) -> BResult<Box<dyn Repo + '_>> {
        panic!("SSE must not read storage for scope")
    }
    fn writer(&self) -> BResult<Box<dyn Repo + '_>> {
        panic!("SSE must not write storage for scope")
    }
}

async fn read_until_barrier(body: Body) -> BTreeSet<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut body = body.into_data_stream();
        let mut buffer = String::new();
        let mut seen = BTreeSet::new();
        while let Some(chunk) = body.next().await {
            buffer.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            while let Some(end) = buffer.find("\n\n") {
                let frame = buffer[..end].to_string();
                buffer.drain(..end + 2);
                for line in frame.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data: Value = serde_json::from_str(data).unwrap();
                        let kind = data["type"].as_str().unwrap();
                        if data["id"] == "barrier" {
                            return seen;
                        }
                        seen.insert(kind.to_owned());
                    }
                }
            }
        }
        panic!("stream ended before the Hub-scoped barrier")
    })
    .await
    .expect("ordered barrier delivered")
}

#[tokio::test]
async fn actual_sse_filter_preserves_workspace_user_and_hub_audiences() {
    let home = tempfile::tempdir().unwrap();
    let vars = [
        ("HOME", home.path().display().to_string()),
        (
            "STEVE_HOME",
            home.path().join("steve").display().to_string(),
        ),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PORT", "18440".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
    ];
    let cfg =
        Config::from_vars(&|key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()))
            .unwrap();
    let bus = Bus::new();
    let state = AppState::new(
        Arc::new(cfg),
        Arc::new(NoStorage),
        Arc::new(NoDomain),
        bus.clone(),
    );
    let mut streams = vec![];
    for (ws, user, owner, sees_ws, sees_user) in [
        ("workspace-a", "user-a", false, true, true),
        ("workspace-b", "user-a", false, false, true),
        ("workspace-a", "user-b", false, true, false),
        ("workspace-b", "owner", true, false, true),
        ("workspace-b", "", false, false, false),
    ] {
        let ctx = Ctx {
            ws: ws.into(),
            me: user.into(),
            client: None,
            user: json!({"role": if owner { "owner" } else { "member" }})
                .as_object()
                .unwrap()
                .clone(),
        };
        let response = stream::stream(
            State(state.clone()),
            Extension(ctx),
            Query(StreamQuery::default()),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        streams.push((response.into_body(), sees_ws, sees_user));
    }
    bus.publish(vec![
        PeopleEvent::Party {
            id: "party-a".into(),
            ws: "workspace-a".into(),
        }
        .into(),
        DevicesEvent::Node {
            id: "node-a".into(),
            user: "user-a".into(),
        }
        .into(),
        DevicesEvent::Client {
            id: "client-a".into(),
            user: "user-a".into(),
        }
        .into(),
        DevicesEvent::ReleaseHold {
            hold: Some(RawJson::from_str(r#"{"item":"item-a","running":1,"since":7}"#).unwrap()),
        }
        .into(),
    ]);
    bus.live(
        WorkLiveEvent::SteveThinking {
            on: true,
            ws: "workspace-a".into(),
            phase: None,
        }
        .into(),
    );
    bus.publish(vec![
        DevicesEvent::Device {
            id: "barrier".into(),
        }
        .into(),
    ]);
    for (body, sees_ws, sees_user) in streams {
        let seen = read_until_barrier(body).await;
        let mut expected = BTreeSet::from(["release.hold".to_owned()]);
        if sees_ws {
            expected.extend(["party".to_owned(), "steve.thinking".to_owned()]);
        }
        if sees_user {
            expected.extend(["node".to_owned(), "client".to_owned()]);
        }
        assert_eq!(seen, expected);
    }
}
