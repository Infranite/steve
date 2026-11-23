use steve_types::{DeliveryBaseline, DeliveryRef};

use crate::cycle::Policy;
use crate::testkit::Kit;

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
        store
            .write(|w| steve_store::item_delivery::declare(w, crate::repo::WS, "me", &manifest))
            .unwrap();
        let closing = k
            .core
            .tx()
            .lc_request_complete(
                &item,
                "dev",
                Policy::Automatic,
                true,
                "delivered",
                None,
                vec![],
            )
            .unwrap();
        manifest.revision = closing.revision;
        assert_eq!(k.phase(&item), "closing");
        let mut stale = manifest.clone();
        stale.revision -= 1;
        k.core.delivery_verified(&stale).unwrap();
        let mut partial = manifest.clone();
        partial.refs.pop();
        k.core.delivery_verified(&partial).unwrap();
        assert_eq!(k.phase(&item), "closing");
        if owner_first {
            k.finish(&item).unwrap();
        }
        k.core.delivery_verified(&manifest).unwrap();
        let closed = store.items().get(&item).unwrap().unwrap();
        assert_eq!(closed.phase, "closed");
        assert_eq!(
            closed.acceptance.as_deref(),
            Some(if owner_first { "owner" } else { "automatic" }),
        );
        let restarted = k.reopened();
        restarted.delivery_verified(&manifest).unwrap();
        let cycles = store.items().cycles(&item).unwrap();
        assert_eq!(cycles.len(), 1);
        assert_eq!(store.items().get(&item).unwrap().unwrap(), closed);
    }
}
