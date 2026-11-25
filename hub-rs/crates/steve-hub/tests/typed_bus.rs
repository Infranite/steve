use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use steve_hub::{
    bus::{Bus, Scope},
    wire::store::BusPublisher,
};
use steve_store::{Store, StoreError};
use steve_types::*;
mod bus_cases;

async fn next(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<steve_hub::bus::Delivery>>,
) -> (Arc<steve_hub::bus::Delivery>, Value) {
    let frame = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let encoded = steve_hub::api::stream::Frame::from(frame.as_ref());
    let value = serde_json::from_slice(&encoded.bytes[6..encoded.bytes.len() - 2]).unwrap();
    (frame, value)
}
fn item(id: &str) -> DomainEvent {
    WorkEvent::Item {
        id: id.into(),
        ws: "a".into(),
    }
    .into()
}
fn project(id: &str) -> DomainEvent {
    ConfigEvent::Project {
        id: id.into(),
        ws: "a".into(),
    }
    .into()
}
fn thinking() -> LiveEvent {
    WorkLiveEvent::SteveThinking {
        on: true,
        ws: "a".into(),
        phase: None,
    }
    .into()
}

#[tokio::test]
async fn committed_batches_share_rev_and_rollback_does_not_allocate_one() {
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let store = Store::open(dir.path().join("steve.sqlite3"), BusPublisher(bus.clone())).unwrap();
    let mut rx = bus.subscribe();
    let start = bus.cursor();
    store
        .write(|tx| {
            tx.emit(project("first"));
            tx.scope(|w| {
                w.emit(project("nested"));
                Ok::<_, StoreError>(())
            })?;
            tx.emit(thinking());
            tx.emit(project("third"));
            Ok::<_, StoreError>(())
        })
        .unwrap();
    for id in ["first", "nested"] {
        let (_, v) = next(&mut rx).await;
        assert_eq!(v, json!({"type":"project", "id":id, "ws":"a", "rev":1}));
    }
    assert_eq!(next(&mut rx).await.1["rev"], 1);
    assert_eq!(next(&mut rx).await.1["rev"], 1);
    let rolled_back: Result<(), StoreError> = store.write(|tx| {
        tx.emit(project("rolled-back"));
        Err(StoreError::NotFound("test".into()))
    });
    assert!(rolled_back.is_err());
    store
        .write(|tx| {
            tx.emit(project("second"));
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(next(&mut rx).await.1["rev"], 2);
    store.publish(thinking());
    assert_eq!(next(&mut rx).await.1["rev"], 2);
    store.publish(project("outside"));
    assert_eq!(next(&mut rx).await.1["rev"], 3);
    assert_eq!(
        bus.cursor().n,
        start.n + 7,
        "rev never replaces the per-frame cursor"
    );
}

#[tokio::test]
async fn every_typed_wire_variant_uses_scoped_visibility_and_keeps_typed_taps() {
    let tags = steve_types::WIRE_TYPES;
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let mut rx = bus.subscribe();
    let mut tap = bus.tap(|_| true);
    for tag in tags {
        let event = bus_cases::typed(tag, "a");
        let replaced = steve_hub::projection::replaced(&event);
        bus.publish(event);
        let tap_value = tokio::time::timeout(Duration::from_secs(10), tap.recv())
            .await
            .unwrap()
            .unwrap();
        if replaced {
            // internal subscribers get it; streams get its views as patches instead
            assert_eq!(tap_value.event.wire_type(), tag);
            continue;
        }
        let (frame, value) = next(&mut rx).await;
        assert_eq!(serde_json::to_value(&*tap_value).unwrap(), value, "{tag}");
        assert!(value["rev"].is_number());
        assert_eq!(value["type"], tag);
        let hub = matches!(tag, "device" | "release.hold" | "node" | "client");
        for ws in ["a", "b", ""] {
            assert_eq!(
                frame.visible_to(ws),
                hub || ws == "a",
                "{tag}: workspace {ws:?}"
            );
        }
        assert_eq!(
            frame.envelope.scope(),
            if hub {
                Scope::Hub
            } else {
                Scope::Ws("a".into())
            }
        );
    }
}

#[tokio::test]
async fn the_internal_item_event_retains_its_wire_shape() {
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let mut rx = bus.subscribe();
    let mut tap = bus.tap(|_| true);
    bus.publish(item("entity-1"));
    let envelope = tap.recv().await.unwrap();
    assert!(rx.try_recv().is_err(), "the thin event must stay internal");
    let golden = include_str!("../../steve-types/tests/fixtures/wire/item.json");
    let golden = golden.trim_end();
    assert_eq!(
        serde_json::to_string(&*envelope).unwrap(),
        format!("{},\"ws\":\"a\",\"rev\":1}}", &golden[..golden.len() - 1])
    );
}

#[tokio::test]
async fn typed_transcript_preserves_slim_sse_and_full_tap_payloads() {
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let mut rx = bus.subscribe();
    let mut tap = bus.tap(|e| {
        matches!(
            e.as_live(),
            Some(LiveEvent::Work(WorkLiveEvent::Event { .. }))
        )
    });
    let data = json!({
        "tool": {
            "id": "call_1",
            "title": "Read",
            "kind": "read",
            "status": "completed",
            "action": {
                "kind": "tool",
                "name": "Read",
                "args": {"img": {"type": "image", "data": "base64"}}
            },
            "result": [{"type": "text", "text": "x".repeat(5000)}]
        }
    });
    bus.live(
        WorkLiveEvent::Event {
            session_id: "s".into(),
            seq: Seq(1),
            kind: "tool".into(),
            data: RawJson::from_str(&data.to_string()).unwrap(),
            ws: "a".into(),
        }
        .into(),
    );
    let (_, typed) = next(&mut rx).await;
    assert_eq!(
        serde_json::to_value(&*tap.recv().await.unwrap()).unwrap()["data"],
        data
    );
    assert_eq!(typed["data"]["tool"]["title"], "Read");
    assert_eq!(
        typed["data"]["tool"]["action"]["args"]["img"],
        json!({"type": "image"})
    );
    assert_eq!(
        typed["data"]["tool"]["result"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        4001
    );
}

/// Workspace events without a workspace reach nobody, including connections without a workspace.
/// Device notifications remain Hub-wide even when their user payload is empty.
#[tokio::test]
async fn missing_workspaces_reach_nobody_and_devices_without_users_remain_hub_wide() {
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let mut rx = bus.subscribe();
    let workspaces = ["", "a", "b"];
    for tag in [
        "project",
        "steve.tool",
        "steve.draft",
        "steve.thinking",
        "event",
        "kb",
        "mail",
    ] {
        bus.publish(bus_cases::typed(tag, ""));
        let (frame, _) = next(&mut rx).await;
        for ws in workspaces {
            assert!(!frame.visible_to(ws), "{tag}: workspace {ws:?}");
        }
    }
    for event in [
        steve_types::DevicesEvent::Node {
            id: "n".into(),
            user: "".into(),
        },
        steve_types::DevicesEvent::Client {
            id: "c".into(),
            user: "".into(),
        },
    ] {
        bus.publish(steve_types::DomainEvent::from(event));
        let (frame, _) = next(&mut rx).await;
        assert_eq!(frame.envelope.scope(), Scope::Hub);
        for ws in workspaces {
            assert!(frame.visible_to(ws), "workspace {ws:?}");
        }
    }
}
