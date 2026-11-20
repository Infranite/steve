//! Named checks for the provisional cursor/retry/watch contracts.
use super::*;
use crate::retry::{Attempt, retry_on_conflict};
use std::{
    future::Future,
    task::{Context, Waker},
};

pub(super) fn cursor_unit(h: &dyn Harness) -> UnitOfWork {
    let mut unit = uow(
        h,
        Domain::Core,
        "cursor-intent",
        vec![
            insert(CORE.name, "cursor-row", 7),
            Mutation::AdvanceCursor {
                name: CursorName("idempotent".into()),
                to: Position(CursorKey::Int(10)),
            },
        ],
    );
    unit.idempotency = Idempotency::Cursor;
    unit.preconditions.push(Precondition::Cursor {
        name: CursorName("idempotent".into()),
        at: None,
    });
    unit.facts.push(fact("cursor-fact"));
    sign(&mut unit);
    unit
}
fn conflict_at(store: &dyn DomainStore, unit: UnitOfWork, position: i64) {
    let Err(CommitError::Conflict(failures)) = block_on(store.commit(unit)) else {
        panic!("cursor replay must conflict");
    };
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].actual_position,
        Some(Position(CursorKey::Int(position)))
    );
}
pub(super) fn cursor_idempotency_replay(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let unit = cursor_unit(h);
    commit(store.as_ref(), unit.clone());
    conflict_at(store.as_ref(), unit.clone(), 10);
    let mut changed = unit.clone();
    if let Mutation::AdvanceCursor { to, .. } = &mut changed.mutations[1] {
        *to = Position(CursorKey::Int(11));
    }
    sign(&mut changed);
    conflict_at(store.as_ref(), changed, 10);
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "advance-further",
            vec![Mutation::AdvanceCursor {
                name: CursorName("idempotent".into()),
                to: Position(CursorKey::Int(20)),
            }],
        ),
    );
    conflict_at(store.as_ref(), unit, 20);
    assert_eq!(page(store.as_ref(), Domain::Core, CORE.name).rows.len(), 1);
    probe_fact(h, store.as_ref(), "cursor-fact", true);
}
fn receipt_count(store: &dyn DomainStore) -> usize {
    // Export carries the ledger as typed contract state in both backends. No private
    // SQL or backend-only query is needed to count its exact durable entries.
    #[derive(Deserialize)]
    struct Ledger {
        receipts: Vec<serde::de::IgnoredAny>,
    }
    let session = block_on(store.begin_export(Domain::Core)).unwrap();
    let batch = block_on(store.export(&session, ExportCursor::default())).unwrap();
    decode::<Ledger>(&batch.state).unwrap().receipts.len()
}
pub(super) fn cursor_idempotency_no_receipts(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let recorded = uow(h, Domain::Core, "cursor-intent", vec![]);
    let saved = commit(store.as_ref(), recorded.clone());
    let before = receipt_count(store.as_ref());
    let unit = cursor_unit(h);
    commit(store.as_ref(), unit.clone());
    conflict_at(store.as_ref(), unit, 10);
    assert_eq!(receipt_count(store.as_ref()), before);
    let replay = commit(store.as_ref(), recorded);
    assert!(replay.replayed);
    assert_eq!(replay.rev, saved.rev);
}
pub(super) fn cursor_idempotency_shape(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let valid = cursor_unit(h);
    for i in 0..8 {
        let mut unit = valid.clone();
        match i {
            0 => unit.preconditions.clear(),
            1 => unit.preconditions.push(unit.preconditions[0].clone()),
            2 => {
                unit.mutations.pop();
            }
            3 => unit.mutations.push(unit.mutations[1].clone()),
            4 => {
                if let Mutation::AdvanceCursor { name, .. } = &mut unit.mutations[1] {
                    name.0 = "other".into();
                }
            }
            5..=7 => {
                if let Precondition::Cursor { at, .. } = &mut unit.preconditions[0] {
                    *at = Some(Position(CursorKey::Int(10)));
                }
                if let Mutation::AdvanceCursor { to, .. } = &mut unit.mutations[1] {
                    *to = Position(if i == 7 {
                        CursorKey::Text("11".into())
                    } else {
                        CursorKey::Int(if i == 5 { 10 } else { 9 })
                    });
                }
            }
            _ => unreachable!(),
        }
        sign(&mut unit);
        assert!(matches!(
            block_on(store.commit(unit)),
            Err(CommitError::Fatal(Error {
                code: Code::Invalid,
                retry: steve_types::Retry::Never,
                ..
            }))
        ));
        assert!(get(store.as_ref(), "cursor-row").is_none());
    }
    commit(store.as_ref(), valid);
}
pub(super) fn retry_conflict_same_key_new_digest(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let before = receipt_count(store.as_ref());
    let mut count = 0;
    let result = block_on(retry_on_conflict(store.as_ref(), || {
        count += 1;
        let mut unit = uow(
            h,
            Domain::Core,
            "stable-intent",
            vec![insert(CORE.name, "retry-row", count)],
        );
        if count == 1 {
            unit.preconditions.push(Precondition::Present {
                entity: CORE.name.into(),
                key: vec![Cell::Text("retry-row".into())],
            });
        }
        sign(&mut unit);
        Box::pin(async move { Ok(Attempt::Commit(unit, count)) })
    }))
    .unwrap();
    assert_eq!(result, 2);
    assert_eq!(count, 2);
    assert_eq!(value(&get(store.as_ref(), "retry-row").unwrap().0), 2);
    assert_eq!(receipt_count(store.as_ref()), before + 1);
}
pub(super) fn retry_conflict_policy(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let mut count = 0;
    let exhausted = block_on(retry_on_conflict::<()>(store.as_ref(), || {
        count += 1;
        let mut unit = uow(h, Domain::Core, "always-races", vec![]);
        unit.preconditions.push(Precondition::Present {
            entity: CORE.name.into(),
            key: vec![Cell::Text("missing".into())],
        });
        sign(&mut unit);
        Box::pin(async move { Ok(Attempt::Commit(unit, ())) })
    }))
    .unwrap_err();
    assert_eq!(count, steve_types::NOW_MAX_ATTEMPTS);
    assert_eq!(exhausted.code, Code::Conflict);
    assert!(matches!(exhausted.retry, steve_types::Retry::Later { .. }));
    let e = block_on(retry_on_conflict::<()>(store.as_ref(), || {
        Box::pin(async { Ok(Attempt::Reject(error(Code::Invalid, "state advanced"))) })
    }))
    .unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    assert_eq!(e.retry, steve_types::Retry::Never);
    assert_eq!(
        block_on(retry_on_conflict(store.as_ref(), || Box::pin(async {
            Ok(Attempt::Done(7))
        })))
        .unwrap(),
        7
    );
    let mut attempts = 0;
    let mismatch = block_on(retry_on_conflict::<()>(store.as_ref(), || {
        attempts += 1;
        let mut unit = uow(h, Domain::Core, format!("changed-key-{attempts}"), vec![]);
        unit.preconditions.push(Precondition::Present {
            entity: CORE.name.into(),
            key: vec![Cell::Text("missing".into())],
        });
        sign(&mut unit);
        Box::pin(async move { Ok(Attempt::Commit(unit, ())) })
    }))
    .unwrap_err();
    assert_eq!(attempts, 2);
    assert_eq!(mismatch.code, Code::Invalid);
    let mut fatal_attempts = 0;
    let fatal = block_on(retry_on_conflict::<()>(store.as_ref(), || {
        fatal_attempts += 1;
        Box::pin(async { Err(error(Code::Unavailable, "backend offline")) })
    }))
    .unwrap_err();
    assert_eq!(fatal_attempts, 1);
    assert_eq!(fatal.code, Code::Unavailable);
}
fn ready(watch: &CommitWatch) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    let mut changed = std::pin::pin!(watch.changed());
    changed.as_mut().poll(&mut cx).is_ready()
}
pub(super) fn commit_watch(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let watch = store.watch(&[CORE.name.into()]);
    assert!(!ready(&watch));
    let unit = uow(
        h,
        Domain::Core,
        "watched",
        vec![insert(CORE.name, "watched", 1)],
    );
    commit(store.as_ref(), unit.clone());
    assert!(ready(&watch), "commit_hint_1: successful commit must wake");
    assert!(!ready(&watch));
    commit(store.as_ref(), unit);
    assert!(!ready(&watch));
    let failed = uow(
        h,
        Domain::Core,
        "rolled-back",
        vec![
            insert(CORE.name, "rollback", 1),
            insert(CORE.name, "rollback", 2),
        ],
    );
    assert!(block_on(store.commit(failed)).is_err());
    assert!(
        !ready(&watch),
        "commit_hint_3: rolled-back UOW must not wake"
    );
    commit(
        store.as_ref(),
        uow(h, Domain::Core, "coalesced-1", vec![update("watched", 2)]),
    );
    commit(
        store.as_ref(),
        uow(h, Domain::Core, "coalesced-2", vec![update("watched", 3)]),
    );
    assert!(ready(&watch));
    assert!(!ready(&watch));
}
