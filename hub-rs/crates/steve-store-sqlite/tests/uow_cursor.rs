#[path = "uow_support.rs"]
mod support;
use steve_storage_api::{cursor::*, fact::FactRecord, uow::*, *};
use support::*;
#[test]
fn cursor_does_not_read_or_write_recorded_ledger_and_facts_remain_single() {
    let (c, runner, _, _) = fixture();
    let original = unit("shared-key", vec![insert("recorded", 1)]);
    commit(&c, &runner, original.clone()).unwrap();
    let mut u = unit(
        "shared-key",
        vec![
            insert("cursor", 1),
            Mutation::AdvanceCursor {
                name: CursorName("consumer".into()),
                to: Position(CursorKey::Int(5)),
            },
        ],
    );
    u.idempotency = Idempotency::Cursor;
    u.preconditions = vec![Precondition::Cursor {
        name: CursorName("consumer".into()),
        at: None,
    }];
    u.facts.push(FactRecord {
        fact_id: "once".into(),
        topic: "verify.core.fact".into(),
        key: "key".into(),
        payload: RawJson::from_str("{}").unwrap(),
    });
    sign(&mut u);
    let before = count(&c, "uow_applied");
    commit(&c, &runner, u.clone()).unwrap();
    let Err(CommitError::Conflict(failures)) = commit(&c, &runner, u) else {
        panic!("cursor replay must not use recorded receipt");
    };
    assert_eq!(
        failures[0].actual_position,
        Some(Position(CursorKey::Int(5)))
    );
    assert_eq!(count(&c, "outbox_events"), 1);
    assert_eq!(count(&c, "uow_applied"), before);
    assert!(commit(&c, &runner, original).unwrap().replayed);
    let mut next = unit(
        "shared-key",
        vec![Mutation::AdvanceCursor {
            name: CursorName("consumer".into()),
            to: Position(CursorKey::Int(10)),
        }],
    );
    next.idempotency = Idempotency::Cursor;
    next.preconditions = vec![Precondition::Cursor {
        name: CursorName("consumer".into()),
        at: Some(Position(CursorKey::Int(5))),
    }];
    sign(&mut next);
    commit(&c, &runner, next).unwrap();
    assert_eq!(count(&c, "uow_applied"), before);
}
