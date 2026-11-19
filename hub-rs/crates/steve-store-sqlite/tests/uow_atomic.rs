#[path = "uow_support.rs"]
mod support;
use steve_storage_api::{append::*, cursor::*, uow::*, *};
use steve_store_sqlite::uow::WallClock;
use steve_store_sqlite::{exec::BatchRunner, publish::Attached, uow::cursor};
use support::*;

#[test]
fn idempotency_replays_full_result_checks_digest_age_and_future_before_lookup() {
    let (c, r, time, events) = fixture();
    let u = unit(
        "append",
        vec![Mutation::Append {
            stream: StreamName("verify_core_stream".into()),
            records: vec![Row(vec![
                ("id".into(), Cell::Text("a".into())),
                ("value".into(), Cell::Int(1)),
            ])],
        }],
    );
    let first = commit(&c, &r, u.clone()).unwrap();
    assert_eq!(first.seqs[0].1, vec![Seq(1)]);
    let mut same = commit(&c, &r, u.clone()).unwrap();
    assert!(same.replayed);
    same.replayed = false;
    assert_eq!(same, first);
    assert_eq!(events.lock().unwrap().len(), 1);
    let mut changed = u.clone();
    changed.mutations.clear();
    sign(&mut changed);
    assert_eq!(
        commit(&c, &r, changed),
        Err(CommitError::IdempotencyMismatch)
    );
    let mut forged = u.clone();
    forged.digest = RequestDigest([1; 32]);
    assert!(matches!(
        commit(&c, &r, forged),
        Err(CommitError::Fatal(Error {
            code: steve_types::Code::Invalid,
            ..
        }))
    ));
    time.add(30 * 86_400_000);
    assert!(commit(&c, &r, u.clone()).unwrap().replayed);
    // Removing the receipt must not make an expired intent execute again.
    time.add(1);
    c.execute("DELETE FROM uow_applied", []).unwrap();
    assert_eq!(commit(&c, &r, u.clone()), Err(CommitError::Expired));
    let mut future = unit("future", vec![]);
    future.id.issued_at = Millis(time.now().0 + 300_001);
    assert!(matches!(
        commit(&c, &r, future.clone()),
        Err(CommitError::Fatal(_))
    ));
    future.id.issued_at.0 -= 1;
    assert!(commit(&c, &r, future).is_ok());
    assert_eq!(count(&c, "verify_core_stream"), 1);
}
#[test]
fn intent_identity_includes_domain_issued_at_and_key_without_delimiter_collisions() {
    let (c, r, _, _) = fixture();
    let a = unit("same", vec![]);
    commit(&c, &r, a.clone()).unwrap();
    let mut b = a.clone();
    b.id.issued_at.0 += 1;
    commit(&c, &r, b).unwrap();
    let mut b = a;
    b.domain = Domain::Comms;
    sign(&mut b);
    commit(&c, &r, b).unwrap();
    assert_eq!(count(&c, "uow_applied"), 3);
}
#[test]
fn savepoint_isolates_cas_unique_and_fact_failures_with_successful_siblings() {
    let (c, r, _, events) = fixture();
    let first = commit(&c, &r, unit("initial", vec![insert("a", 1)])).unwrap();
    events.lock().unwrap().clear();
    let mut cas = unit("bad-cas", vec![insert("must-rollback", 5), update("a", 2)]);
    cas.preconditions.push(Precondition::Rev {
        entity: "verify_core".into(),
        key: vec![Cell::Text("a".into())],
        rev: Rev(first.rev.0 + 1),
    });
    sign(&mut cas);
    let mut x = row("unique", 2);
    set(&mut x, "label", Cell::Text("collision".into()));
    let mut y = x.clone();
    set(&mut y, "id", Cell::Text("other".into()));
    let units = vec![
        unit("ok-before", vec![insert("before", 1)]),
        cas,
        unit(
            "unique",
            vec![Mutation::Insert {
                entity: "verify_core".into(),
                row: x,
            }],
        ),
        unit(
            "unique-bad",
            vec![
                insert("discard", 1),
                Mutation::Insert {
                    entity: "verify_core".into(),
                    row: y,
                },
            ],
        ),
        unit("ok-after", vec![update("a", 3)]),
    ];
    let results = r.run(
        &c,
        units
            .into_iter()
            .map(|u| Attached::new(u, vec!["committed".into()]))
            .collect(),
    );
    assert!(results[0].is_ok());
    assert!(matches!(results[1], Err(CommitError::Conflict(_))));
    assert!(results[2].is_ok());
    assert!(matches!(&results[3],Err(CommitError::Unique{index,..}) if index=="label"));
    assert!(results[4].is_ok());
    assert_eq!(value(&c, "must-rollback"), None);
    assert_eq!(value(&c, "discard"), None);
    assert_eq!(value(&c, "a"), Some(3));
    assert!(c.is_autocommit());
    assert_eq!(events.lock().unwrap().len(), 3);
    let mut good = unit("cas-ok", vec![update("a", 7)]);
    good.preconditions.push(Precondition::Rev {
        entity: "verify_core".into(),
        key: vec![Cell::Text("a".into())],
        rev: results[4].as_ref().unwrap().rev,
    });
    sign(&mut good);
    commit(&c, &r, good).unwrap();
    assert_eq!(value(&c, "a"), Some(7));
}
#[test]
fn natural_single_insert_is_replayed_without_recorded_intents_and_unsafe_shapes_fail() {
    let (c, r, _, _) = fixture();
    let mut row = row("natural", 7);
    set(&mut row, "label", Cell::Text("stable".into()));
    let mut u = unit(
        "n",
        vec![Mutation::Insert {
            entity: "verify_core".into(),
            row,
        }],
    );
    u.idempotency = Idempotency::Natural {
        entity: "verify_core".into(),
        index: "label".into(),
    };
    sign(&mut u);
    let first = commit(&c, &r, u.clone()).unwrap();
    u.id.key = "retry".into();
    let again = commit(&c, &r, u.clone()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.rev, first.rev);
    assert_eq!(count(&c, "uow_applied"), 0);
    if let Mutation::Insert { row, .. } = &mut u.mutations[0] {
        set(row, "value", Cell::Int(8));
    }
    sign(&mut u);
    assert_eq!(
        commit(&c, &r, u.clone()),
        Err(CommitError::IdempotencyMismatch)
    );
    u.mutations.push(insert("extra", 1));
    sign(&mut u);
    assert!(matches!(
        commit(&c, &r, u),
        Err(CommitError::Fatal(Error {
            code: steve_types::Code::Invalid,
            ..
        }))
    ));
    assert_eq!(count(&c, "verify_core"), 1);
}
#[test]
fn cursor_conditions_shape_and_backward_rejection_are_atomic() {
    let (c, r, _, _) = fixture();
    let name = CursorName("consumer".into());
    assert_eq!(cursor(&c, Domain::Core, &name).unwrap(), None);
    let mut u = unit(
        "cursor",
        vec![
            Mutation::AdvanceCursor {
                name: name.clone(),
                to: Position(CursorKey::Int(10)),
            },
            insert("cursor-side", 1),
        ],
    );
    u.preconditions.push(Precondition::Cursor {
        name: name.clone(),
        at: None,
    });
    sign(&mut u);
    commit(&c, &r, u.clone()).unwrap();
    u.id.key = "again".into();
    assert!(matches!(commit(&c, &r, u), Err(CommitError::Conflict(_))));
    for to in [CursorKey::Int(9), CursorKey::Text("11".into())] {
        let u = unit(
            "bad",
            vec![
                insert("discard", 1),
                Mutation::AdvanceCursor {
                    name: name.clone(),
                    to: Position(to),
                },
            ],
        );
        assert!(commit(&c, &r, u).is_err());
        assert_eq!(value(&c, "discard"), None);
    }
    commit(
        &c,
        &r,
        unit(
            "equal",
            vec![Mutation::AdvanceCursor {
                name: name.clone(),
                to: Position(CursorKey::Int(10)),
            }],
        ),
    )
    .unwrap();
}
#[test]
fn cross_domain_references_are_rejected_even_when_colocated() {
    let (c, r, _, _) = fixture();
    for m in [
        Mutation::Insert {
            entity: "verify_comms".into(),
            row: row("other", 1),
        },
        Mutation::Delete {
            entity: "verify_comms".into(),
            key: vec![Cell::Text("x".into())],
        },
        Mutation::Append {
            stream: StreamName("verify_stream".into()),
            records: vec![],
        },
    ] {
        let u = unit("mixed", vec![insert("sentinel", 1), m]);
        assert!(matches!(
            commit(&c, &r, u),
            Err(CommitError::Fatal(Error {
                code: steve_types::Code::Invalid,
                ..
            }))
        ));
        assert_eq!(value(&c, "sentinel"), None);
    }
}
#[test]
fn facts_use_caller_id_and_explicit_legacy_mapping_and_duplicate_rolls_back() {
    let (c, r, _, _) = fixture();
    let fact = fact::FactRecord {
        fact_id: "stable-fact".into(),
        topic: "verify.core.fact".into(),
        key: "key".into(),
        payload: RawJson::from_str("{ \"b\":2, \"a\":1 }").unwrap(),
    };
    let mut u = unit("fact", vec![insert("a", 1)]);
    u.facts.push(fact.clone());
    sign(&mut u);
    commit(&c, &r, u.clone()).unwrap();
    commit(&c, &r, u).unwrap();
    let (id, text): (String, String) = c
        .query_row("SELECT id,payload FROM outbox_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(id, fact.fact_id);
    assert_eq!(
        serde_json::from_str::<fact::FactRecord>(&text).unwrap(),
        fact
    );
    let mut u = unit("duplicate", vec![insert("discard", 1)]);
    u.facts.push(fact);
    sign(&mut u);
    assert!(matches!(commit(&c, &r, u), Err(CommitError::Unique { .. })));
    assert_eq!(value(&c, "discard"), None);
}

#[test]
fn conditions_refer_to_initial_state_when_a_unit_changes_the_same_row_twice() {
    let (c, r, _, _) = fixture();
    let first = commit(&c, &r, unit("initial", vec![insert("a", 1)])).unwrap();
    let mut u = unit("twice", vec![update("a", 2), update("a", 3)]);
    u.preconditions.push(Precondition::Rev {
        entity: "verify_core".into(),
        key: vec![Cell::Text("a".into())],
        rev: first.rev,
    });
    sign(&mut u);
    commit(&c, &r, u).unwrap();
    assert_eq!(value(&c, "a"), Some(3));
}

#[test]
fn expired_request_never_reads_the_dedup_table() {
    let (c, r, time, _) = fixture();
    let u = unit("old", vec![insert("never", 1)]);
    time.add(30 * 86_400_000 + 1);
    c.execute_batch("DROP TABLE uow_applied").unwrap();
    assert_eq!(commit(&c, &r, u), Err(CommitError::Expired));
    assert_eq!(value(&c, "never"), None);
}

#[test]
fn validation_rejects_unknown_mappings_unsafe_natural_modes_and_mixed_preconditions() {
    let (c, r, _, _) = fixture();
    for index in ["value", "missing", "label"] {
        let mut u = unit(index, vec![insert("n", 1)]);
        u.idempotency = Idempotency::Natural {
            entity: "verify_core".into(),
            index: index.into(),
        };
        sign(&mut u);
        assert!(matches!(
            commit(&c, &r, u),
            Err(CommitError::Fatal(Error {
                code: steve_types::Code::Invalid,
                ..
            }))
        ));
    }
    for p in [
        Precondition::Rev {
            entity: "verify_comms".into(),
            key: vec![Cell::Text("x".into())],
            rev: Rev(1),
        },
        Precondition::Absent {
            entity: "verify_comms".into(),
            key: vec![Cell::Text("x".into())],
        },
        Precondition::Present {
            entity: "verify_comms".into(),
            key: vec![Cell::Text("x".into())],
        },
    ] {
        let mut u = unit("mixed-p", vec![insert("sentinel", 1)]);
        u.preconditions.push(p);
        sign(&mut u);
        assert!(matches!(
            commit(&c, &r, u),
            Err(CommitError::Fatal(Error {
                code: steve_types::Code::Invalid,
                ..
            }))
        ));
    }
    let bad = unit(
        "unknown",
        vec![Mutation::Insert {
            entity: "unknown".into(),
            row: Row::default(),
        }],
    );
    assert!(matches!(
        commit(&c, &r, bad),
        Err(CommitError::Fatal(Error {
            code: steve_types::Code::Unsupported,
            ..
        }))
    ));
    assert_eq!(count(&c, "verify_core"), 0);
}
