mod common;
use common::*;
use steve_store::StoreError;
use steve_store::nodes::*;

fn node<'a>(id: &'a str, key: &'a [u8; 32], fp: &'a str) -> NewNode<'a> {
    NewNode {
        node_id: id,
        name: "laptop",
        pubkey: key,
        fingerprint: fp,
    }
}

#[test]
fn a_code_is_claimed_once_and_only_in_time() {
    let t = fresh();
    let s = &t.store;
    s.transaction::<_, StoreError>(|tx| create_pairing(tx, "h1", "u1", 100, 200))
        .unwrap();
    let k = [7u8; 32];
    // expired
    let late = s
        .transaction::<_, StoreError>(|tx| claim_pairing(tx, "h1", &node("n1", &k, "f1"), 200))
        .unwrap();
    assert_eq!(late, None);
    let won = s
        .transaction::<_, StoreError>(|tx| claim_pairing(tx, "h1", &node("n1", &k, "f1"), 150))
        .unwrap();
    assert_eq!(won.as_deref(), Some("u1"));
    let again = s
        .transaction::<_, StoreError>(|tx| {
            claim_pairing(tx, "h1", &node("n2", &[8; 32], "f2"), 151)
        })
        .unwrap();
    assert_eq!(again, None, "used");
    let garbage = s
        .transaction::<_, StoreError>(|tx| {
            claim_pairing(tx, "nope", &node("n3", &[9; 32], "f3"), 151)
        })
        .unwrap();
    assert_eq!(garbage, None);
    let n = s.reader(|c| get(c, "n1")).unwrap().unwrap();
    assert_eq!(
        (n.state.as_str(), n.owner.as_str(), n.generation),
        ("pending", "u1", 0)
    );
    assert_eq!(n.pubkey, k.to_vec());
}

#[test]
fn only_live_nodes_link_and_each_link_is_a_new_generation() {
    let t = fresh();
    let s = &t.store;
    s.transaction::<_, StoreError>(|tx| {
        create_pairing(tx, "h1", "u1", 1, 1000)?;
        claim_pairing(tx, "h1", &node("n1", &[1; 32], "f1"), 2)?;
        Ok(())
    })
    .unwrap();
    let link = |inc| {
        s.transaction::<_, StoreError>(|tx| begin_link(tx, "n1", inc, 5))
            .unwrap()
    };
    assert!(link(1).is_none(), "pending");
    // only the owner confirms
    assert!(
        !s.transaction::<_, StoreError>(|tx| set_state(tx, "n1", "u2", "pending", "active"))
            .unwrap()
    );
    assert!(
        s.transaction::<_, StoreError>(|tx| set_state(tx, "n1", "u1", "pending", "active"))
            .unwrap()
    );
    let a = link(1).unwrap();
    assert_eq!(
        (a.generation, a.paused, a.previous_incarnation),
        (1, false, None)
    );
    let b = link(2).unwrap();
    assert_eq!((b.generation, b.previous_incarnation), (2, Some(1)));
    assert!(
        s.transaction::<_, StoreError>(|tx| set_state(tx, "n1", "u1", "active", "paused"))
            .unwrap()
    );
    assert!(link(2).unwrap().paused);
    assert!(
        !s.transaction::<_, StoreError>(|tx| revoke(tx, "n1", "u2", 9))
            .unwrap()
    );
    assert!(
        s.transaction::<_, StoreError>(|tx| revoke(tx, "n1", "u1", 9))
            .unwrap()
    );
    assert_eq!(
        s.reader(|c| link_state(c, "n1")).unwrap(),
        Some((4, "revoked".into()))
    );
    assert!(link(3).is_none(), "revoked");
    assert!(s.reader(list_live).unwrap().is_empty());
    assert_eq!(s.reader(|c| list_for_owner(c, "u1")).unwrap().len(), 1);
    s.transaction::<_, StoreError>(|tx| audit(tx, Some("n1"), Some("u1"), "revoke", None, 9))
        .unwrap();
}

#[test]
fn node_capacity_is_persistent_owner_configuration_not_registration_state() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create_pairing(tx, "capacity", "u1", 1, 1000)?;
            claim_pairing(tx, "capacity", &node("n1", &[5; 32], "capacity-fp"), 2)?;
            assert_eq!(get(tx, "n1")?.unwrap().max_sessions, 32);
            assert!(!set_max_sessions(tx, "n1", "u2", 64)?);
            assert!(!set_max_sessions(tx, "n1", "u1", 0)?);
            assert!(!set_max_sessions(tx, "n1", "u1", 257)?);
            assert!(set_max_sessions(tx, "n1", "u1", 64)?);
            assert_eq!(steve_store::runtime::session_limit(tx, "n1")?, 64);
            assert!(set_state(tx, "n1", "u1", "pending", "active")?);
            assert_eq!(get(tx, "n1")?.unwrap().max_sessions, 64);
            revoke(tx, "n1", "u1", 4)?;
            assert!(!set_max_sessions(tx, "n1", "u1", 32)?);
            Ok(())
        })
        .unwrap();
}
