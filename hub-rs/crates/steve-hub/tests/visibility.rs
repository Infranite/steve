//! Exercises typed scope through the actual SSE boundary.
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
    bus::Bus,
    config::Config,
    wire::{core::CoreDomain, store::StoreBackend},
};
use steve_types::{DevicesEvent, PeopleEvent, RawJson, WorkLiveEvent};

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
async fn actual_sse_filter_preserves_workspace_and_hub_audiences() {
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
    let cfg = Config::from_vars(&|key: &str| {
        vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
    })
    .unwrap();
    let cfg = Arc::new(cfg);
    let store =
        steve_store::Store::open(home.path().join("unused.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let state = AppState::new(
        cfg.clone(),
        StoreBackend(store.clone()),
        CoreDomain::for_test(&cfg, store, &bus, None),
        bus.clone(),
    );
    let mut streams = vec![];
    for (ws, user, owner, sees_ws) in [
        ("workspace-a", "user-a", false, true),
        ("workspace-b", "user-a", false, false),
        ("workspace-a", "user-b", false, true),
        ("workspace-b", "owner", true, false),
        ("workspace-b", "", false, false),
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
        streams.push((response.into_body(), sees_ws));
    }
    bus.publish_batch(vec![
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
    bus.publish_batch(vec![
        DevicesEvent::Device {
            id: "barrier".into(),
        }
        .into(),
    ]);
    for (body, sees_ws) in streams {
        let seen = read_until_barrier(body).await;
        let mut expected = BTreeSet::from([
            "release.hold".to_owned(),
            "node".to_owned(),
            "client".to_owned(),
        ]);
        if sees_ws {
            expected.extend(["party".to_owned(), "steve.thinking".to_owned()]);
        }
        assert_eq!(seen, expected);
    }
}
