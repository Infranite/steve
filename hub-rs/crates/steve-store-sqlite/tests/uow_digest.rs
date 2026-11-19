#[path = "uow_support.rs"]
mod support;
use steve_storage_api::{ops::comms::digest::*, uow::*, *};
use support::*;
fn source(id: &str, conv: &str, status: &str, at: i64) -> Mutation {
    Mutation::Upsert {
        entity: "verify_digest_source".into(),
        row: Row(vec![
            ("source".into(), Cell::Text("chan_message".into())),
            ("id".into(), Cell::Text(id.into())),
            ("workspace_id".into(), Cell::Text("w".into())),
            ("conv".into(), Cell::Text(conv.into())),
            ("status".into(), Cell::Text(status.into())),
            ("at".into(), Cell::Int(at)),
        ]),
    }
}
fn sync(id: &str) -> Mutation {
    Mutation::Op(
        SyncPendingMembership::enqueue_digestible(Source::ChanMessage, id.into(), "w".into())
            .unwrap()
            .to_wire()
            .unwrap(),
    )
}
fn command(id: &str, m: Vec<Mutation>) -> UnitOfWork {
    let mut u = unit(id, m);
    u.domain = Domain::Comms;
    sign(&mut u);
    u
}
#[test]
fn digest_fixture_moves_purges_receipts_and_fifo_positions_follow_reference() {
    let (c, r, _, _) = fixture();
    commit(
        &c,
        &r,
        command(
            "a",
            vec![source("a", "space:one", "received", 10), sync("a")],
        ),
    )
    .unwrap();
    commit(
        &c,
        &r,
        command("b", vec![source("b", "space:one", "sent", 20), sync("b")]),
    )
    .unwrap();
    let first: i64 = c
        .query_row(
            "SELECT position FROM verify_digest_pending WHERE message_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    commit(&c, &r, command("again", vec![sync("a")])).unwrap();
    assert_eq!(
        first,
        c.query_row(
            "SELECT position FROM verify_digest_pending WHERE message_id='a'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
    );
    let stats: (i64, i64, i64) = c
        .query_row(
            "SELECT messages,oldest,newest FROM verify_digest_convs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stats, (2, 10, 20));
    commit(
        &c,
        &r,
        command(
            "move",
            vec![source("a", "space:two", "received", 30), sync("a")],
        ),
    )
    .unwrap();
    let moved: i64 = c
        .query_row(
            "SELECT position FROM verify_digest_pending WHERE message_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(moved > first);
    let ack = SyncPendingMembership::dequeue_on_receipt("space:two", "a", "w".into())
        .unwrap()
        .to_wire()
        .unwrap();
    assert!(commit(&c, &r, command("bad-ack", vec![Mutation::Op(ack.clone())])).is_err());
    let receipt = Mutation::Insert {
        entity: "verify_digest_receipt".into(),
        row: Row(vec![
            ("workspace_id".into(), Cell::Text("w".into())),
            ("conv".into(), Cell::Text("space:two".into())),
            ("message_id".into(), Cell::Text("a".into())),
        ]),
    };
    commit(&c, &r, command("ack", vec![receipt, Mutation::Op(ack)])).unwrap();
    commit(&c, &r, command("no-reenqueue", vec![sync("a")])).unwrap();
    assert_eq!(count(&c, "verify_digest_pending"), 1);
    commit(
        &c,
        &r,
        command(
            "purge",
            vec![source("b", "space:one", "purged", 20), sync("b")],
        ),
    )
    .unwrap();
    assert_eq!(count(&c, "verify_digest_pending"), 0);
    assert_eq!(count(&c, "verify_digest_convs"), 0);
    commit(
        &c,
        &r,
        command(
            "new",
            vec![source("c", "space:one", "received", 5), sync("c")],
        ),
    )
    .unwrap();
    assert!(
        c.query_row("SELECT position FROM verify_digest_pending", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            > moved
    );
}
