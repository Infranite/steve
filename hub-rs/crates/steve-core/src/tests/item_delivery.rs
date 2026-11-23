use steve_types::{DeliveryBaseline, DeliveryProof, DeliveryRef, PullProof};

use crate::cycle::ClosePolicy;
use crate::testkit::Kit;

fn proof(baseline: &DeliveryBaseline) -> DeliveryProof {
    DeliveryProof {
        baseline: baseline.clone(),
        pulls: baseline
            .refs
            .iter()
            .map(|pr| PullProof {
                repository: pr.repository.clone(),
                number: pr.number,
                target_head: "c".repeat(40),
            })
            .collect(),
    }
}

#[test]
fn automatic_finish_requires_the_whole_current_manifest_and_owner_finish_wins_over_late_proof() {
    for owner_first in [false, true] {
        let k = Kit::new();
        let item = k.item("run", "work", None);
        let store = k.db().store();
        let id = steve_types::ItemId::from(item.clone());
        let refs = ["repo-a", "repo-b"]
            .into_iter()
            .map(|repository| DeliveryRef {
                repository: repository.into(),
                number: 1,
                target: "main".into(),
                head: "a".repeat(40),
                base: Some("b".repeat(40)),
                required: true,
            })
            .collect();
        let mut manifest = DeliveryBaseline {
            item: id,
            cycle: 1,
            revision: 0,
            scope: store
                .read(|r| steve_store::item_delivery::scope(r, &item))
                .unwrap(),
            refs,
        };
        let current = store.items().get(&item).unwrap().unwrap();
        store
            .write(|w| {
                steve_store::item_delivery::declare(
                    w,
                    crate::repo::WS,
                    "me",
                    &steve_store::item_delivery::DeliveryDeclaration {
                        item: &item,
                        cycle: current.cycle,
                        expected_revision: current.cas_revision,
                        scope: manifest.scope,
                        refs: &manifest.refs,
                    },
                )
            })
            .unwrap();
        let closing = k
            .core
            .tx()
            .request_delivery_close(
                &item,
                "dev",
                ClosePolicy::Automatic,
                true,
                "delivered",
                None,
                vec![],
            )
            .unwrap();
        manifest.revision = closing.delivery_revision;
        assert_eq!(k.phase(&item), "closing");
        let mut stale = manifest.clone();
        stale.revision -= 1;
        k.core.delivery_verified(&proof(&stale)).unwrap();
        let mut partial = manifest.clone();
        partial.refs.pop();
        k.core.delivery_verified(&proof(&partial)).unwrap();
        assert_eq!(k.phase(&item), "closing");
        if owner_first {
            k.finish(&item).unwrap();
        }
        k.core.delivery_verified(&proof(&manifest)).unwrap();
        let closed = store.items().get(&item).unwrap().unwrap();
        assert_eq!(closed.phase.as_str(), "closed");
        assert_eq!(
            closed.acceptance.as_deref(),
            Some(if owner_first { "owner" } else { "automatic" }),
        );
        let restarted = k.reopened();
        restarted.delivery_verified(&proof(&manifest)).unwrap();
        let cycles = store.items().cycles(&item).unwrap();
        assert_eq!(cycles.len(), 1);
        assert_eq!(store.items().get(&item).unwrap().unwrap(), closed);
    }
}

#[test]
fn mail_receipt_closes_only_its_original_delivery_version() {
    let k = Kit::new();
    let item = k.item("run", "work", None);
    let ws = crate::repo::WS;
    let store = k.db().store();
    let version = || {
        store
            .read(|r| steve_store::item_delivery::open_version(r, ws, &item))
            .unwrap()
    };
    let original = version();
    assert!(original.is_some());
    let closed = k.finish(&item).unwrap();
    k.core
        .lifecycle_command(
            &item,
            "new_cycle",
            closed.cycle,
            closed.cas_revision,
            None,
            None,
            "me",
        )
        .unwrap();
    for stale in [None, original] {
        k.core
            .module_complete(ws, &item, stale, "mail.sent")
            .unwrap();
        assert_eq!(k.phase(&item), "ready");
        assert_eq!(store.items().cycles(&item).unwrap().len(), 1);
    }

    k.core
        .module_complete(ws, &item, version(), "mail.sent")
        .unwrap();
    let cycles = store.items().cycles(&item).unwrap();
    assert_eq!(cycles.len(), 2);
    assert_eq!(cycles[0].acceptance.as_deref(), Some("owner"));
    assert_eq!(cycles[1].acceptance.as_deref(), Some("automatic"));
}
