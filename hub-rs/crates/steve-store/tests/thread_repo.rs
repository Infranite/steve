//! Durable thread storage contract (migration 125): submit idempotency & withdrawal tombstones,
//! per-space serial head CAS, fenced commits, boot recovery, single dispatch per origin,
//! outbound delivery refs, and typed notices.

mod common;

use common::*;
use serde_json::json;
use steve_store::thread;
use steve_store::*;

fn with_steve() -> T {
    let t = fresh();
    t.store
        .bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    t
}

fn item(s: &Store, title: &str) -> String {
    s.items()
        .create(
            WS,
            &NewItem {
                tags: common::task_tags(s, WS),
                title: title.into(),
                goal: title.into(),
                bot_id: "steve".into(),
                phase: Some(steve_types::ItemPhase::Active),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id
}

#[test]
fn submit_idempotency_digest_conflict_and_withdrawal_tombstone() {
    let t = with_steve();
    let s = &t.store;

    let first = s
        .write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws: WS,
                    space: thread::MAIN_SPACE,
                    actor_key: "u1",
                    submit_key: "cid_1",
                    payload_digest: "digest_a",
                    text: "hello",
                    item_id: None,
                    meta: &json!({}),
                    user_id: Some("u1"),
                    project_id: None,
                    conv_json: None,
                    to_item: None,
                    to_reply_to: None,
                    file_ids: &[],
                    files_json: None,
                    mentions_json: None,
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })
        .unwrap();
    let SubmitOutcome::Accepted {
        message_id,
        task_id,
        input_seq,
    } = first
    else {
        panic!("expected accepted");
    };
    assert!(task_id.is_some());
    assert_eq!(input_seq, 1);

    // Same key + same digest -> idempotent duplicate
    let dup = s
        .write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws: WS,
                    space: thread::MAIN_SPACE,
                    actor_key: "u1",
                    submit_key: "cid_1",
                    payload_digest: "digest_a",
                    text: "hello",
                    item_id: None,
                    meta: &json!({}),
                    user_id: Some("u1"),
                    project_id: None,
                    conv_json: None,
                    to_item: None,
                    to_reply_to: None,
                    file_ids: &[],
                    files_json: None,
                    mentions_json: None,
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })
        .unwrap();
    assert_eq!(
        dup,
        SubmitOutcome::Duplicate {
            message_id: message_id.clone(),
            task_id,
            input_seq: 1,
        }
    );

    // Same key + different digest -> Conflict
    let conflict = s
        .write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws: WS,
                    space: thread::MAIN_SPACE,
                    actor_key: "u1",
                    submit_key: "cid_1",
                    payload_digest: "digest_b",
                    text: "changed",
                    item_id: None,
                    meta: &json!({}),
                    user_id: Some("u1"),
                    project_id: None,
                    conv_json: None,
                    to_item: None,
                    to_reply_to: None,
                    file_ids: &[],
                    files_json: None,
                    mentions_json: None,
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })
        .unwrap();
    assert_eq!(
        conflict,
        SubmitOutcome::Conflict {
            reason: "payload_mismatch"
        }
    );

    // Withdraw while queued -> hidden tombstone retained and blocks retry forever
    let wd = s
        .write(|w| thread::withdraw_queued(w, WS, &message_id))
        .unwrap();
    assert!(matches!(wd, WithdrawOutcome::Withdrawn { .. }));
    let tomb = s.messages().get(&message_id).unwrap().unwrap();
    assert_eq!(tomb.meta["hidden"], json!(true));
    assert_eq!(tomb.meta["withdrawn"], json!(true));

    let retry_after_wd = s
        .write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws: WS,
                    space: thread::MAIN_SPACE,
                    actor_key: "u1",
                    submit_key: "cid_1",
                    payload_digest: "digest_a",
                    text: "hello",
                    item_id: None,
                    meta: &json!({}),
                    user_id: Some("u1"),
                    project_id: None,
                    conv_json: None,
                    to_item: None,
                    to_reply_to: None,
                    file_ids: &[],
                    files_json: None,
                    mentions_json: None,
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })
        .unwrap();
    assert_eq!(
        retry_after_wd,
        SubmitOutcome::Conflict {
            reason: "withdrawn"
        }
    );
}

#[test]
fn claim_head_serial_fenced_commit_and_boot_recovery() {
    let t = with_steve();
    let s = &t.store;

    let mut mids = Vec::new();
    for (i, key) in ["k1", "k2", "k3"].iter().enumerate() {
        let out = s
            .write(|w| {
                thread::submit_message(
                    w,
                    &SubmitInput {
                        ws: WS,
                        space: thread::MAIN_SPACE,
                        actor_key: "u1",
                        submit_key: key,
                        payload_digest: key,
                        text: &format!("q{i}"),
                        item_id: None,
                        meta: &json!({}),
                        user_id: Some("u1"),
                        project_id: None,
                        conv_json: None,
                        to_item: None,
                        to_reply_to: None,
                        file_ids: &[],
                        files_json: None,
                        mentions_json: None,
                        enqueue_task: true,
                        validate_refs: false,
                    },
                )
            })
            .unwrap();
        let SubmitOutcome::Accepted { message_id, .. } = out else {
            panic!("expected accepted");
        };
        mids.push(message_id);
    }

    // Per-space serial head CAS: only head (k1) can be claimed; second claim returns None.
    let t1 = s
        .write(|w| thread::claim_head(w, WS, thread::MAIN_SPACE, "lease_1", 10))
        .unwrap()
        .expect("head claimed");
    assert_eq!(t1.origin_message_id, mids[0]);
    assert!(
        s.write(|w| thread::claim_head(w, WS, thread::MAIN_SPACE, "lease_2", 11))
            .unwrap()
            .is_none()
    );

    // Wrong lease or stale generation is fenced with 0 writes.
    let fenced = s
        .write(|w| {
            thread::commit_reply(
                w,
                &CommitReplyInput {
                    task_id: &t1.id,
                    lease_token: "wrong_lease",
                    expected_generation: t1.generation,
                    allow_cancelled_stop: false,
                    outcome: "answered",
                    reply_text: Some("stale"),
                    existing_reply_id: None,
                    reply_item_id: None,
                    reply_meta: &json!({}),
                    error_code: None,
                    route_kind: "answer",
                    model_kind: "answer",
                    route_guard: None,
                    route_model: None,
                    route_confidence: Some(0.9),
                    route_signals: &[],
                    route_fallback: false,
                    route_ms: 10,
                    route_outcome: None,
                    item_id: None,
                    dispatch: ReceiptDispatch::NoDispatch,
                    delivery_ref_id: None,
                    model_call_id: None,
                    user_id: Some("u1"),
                    project_id: None,
                    answer_model: None,
                    harness: None,
                    harness_session: None,
                    tools: 0,
                    turns: 0,
                    first_text_ms: None,
                    error_detail: None,
                    record_attention_commit: true,
                },
            )
        })
        .unwrap();
    assert_eq!(fenced, CommitOutcome::Fenced);

    // Valid commit settles t1 and unlocks head t2.
    let committed = s
        .write(|w| {
            thread::commit_reply(
                w,
                &CommitReplyInput {
                    task_id: &t1.id,
                    lease_token: "lease_1",
                    expected_generation: t1.generation,
                    allow_cancelled_stop: false,
                    outcome: "answered",
                    reply_text: Some("a0"),
                    existing_reply_id: None,
                    reply_item_id: None,
                    reply_meta: &json!({}),
                    error_code: None,
                    route_kind: "answer",
                    model_kind: "answer",
                    route_guard: None,
                    route_model: None,
                    route_confidence: Some(0.9),
                    route_signals: &["q".into()],
                    route_fallback: false,
                    route_ms: 12,
                    route_outcome: None,
                    item_id: None,
                    dispatch: ReceiptDispatch::NoDispatch,
                    delivery_ref_id: None,
                    model_call_id: None,
                    user_id: Some("u1"),
                    project_id: None,
                    answer_model: None,
                    harness: Some("claude-code"),
                    harness_session: Some("sess_1"),
                    tools: 2,
                    turns: 1,
                    first_text_ms: Some(50),
                    error_detail: None,
                    record_attention_commit: true,
                },
            )
        })
        .unwrap();
    assert!(matches!(committed, CommitOutcome::Committed { .. }));
    let it = item(s, "转成事项");
    let DispatchClaim::Claimed(d_ov) = s
        .write(|w| thread::claim_dispatch(w, WS, &mids[0], "create_item", false, 19))
        .unwrap()
    else {
        panic!("expected claimed dispatch");
    };
    s.write(|w| thread::settle_dispatch(w, &d_ov.id, "applied", Some(&it), None, 19))
        .unwrap();
    assert!(
        s.write(|w| {
            thread::update_receipt_override(
                w,
                &mids[0],
                "to_item",
                Some(&it),
                ReceiptDispatch::Dispatched(&d_ov.id),
                20,
            )
        })
        .unwrap()
    );
    assert!(
        !s.write(|w| {
            thread::update_receipt_override(
                w,
                &mids[0],
                "to_inline",
                None,
                ReceiptDispatch::NoDispatch,
                21,
            )
        })
        .unwrap()
    );
    let rcpt = s
        .read(|r| thread::receipt_by_origin(r, &mids[0]))
        .unwrap()
        .unwrap();
    assert_eq!(rcpt.route_override.as_deref(), Some("to_item"));
    assert_eq!(rcpt.item_id.as_deref(), Some(it.as_str()));
    assert_eq!(rcpt.dispatch_ref_id.as_deref(), Some(d_ov.id.as_str()));

    // Claim t2, simulate crash -> recovery settles t2 as 'unknown' and leaves t3 'queued'.
    let t2 = s
        .write(|w| thread::claim_head(w, WS, thread::MAIN_SPACE, "lease_crash", 30))
        .unwrap()
        .expect("t2 claimed");
    assert_eq!(t2.origin_message_id, mids[1]);
    let recovered = s
        .write(|w| thread::recover_claimed_tasks(w, "crash unknown", 40))
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].task_id, t2.id);
    let r2 = s
        .read(|r| thread::receipt_by_task(r, &t2.id))
        .unwrap()
        .unwrap();
    assert_eq!(r2.outcome, "unknown");
    let t3 = s
        .write(|w| thread::claim_head(w, WS, thread::MAIN_SPACE, "lease_3", 50))
        .unwrap()
        .expect("t3 still queued");
    assert_eq!(t3.origin_message_id, mids[2]);
}

#[test]
fn dispatch_delivery_and_notice_contracts() {
    let t = with_steve();
    let s = &t.store;
    let it = item(s, "改按钮");

    // Dispatch claim is unique per (ws, origin, effect_kind) and cascades on item delete.
    let c1 = s
        .write(|w| thread::claim_dispatch(w, WS, "msg_a", "create_item", true, 1))
        .unwrap();
    let DispatchClaim::Claimed(d) = c1 else {
        panic!("expected claimed");
    };
    s.write(|w| thread::settle_dispatch(w, &d.id, "applied", Some(&it), Some("msg_r"), 2))
        .unwrap();
    let c2 = s
        .write(|w| thread::claim_dispatch(w, WS, "msg_a", "create_item", false, 3))
        .unwrap();
    assert!(matches!(c2, DispatchClaim::Existing(e) if e.target_item_id.as_deref() == Some(&it)));
    assert!(
        s.write(|w| thread::set_dispatch_follow(w, &it, false))
            .unwrap()
    );
    s.write(|w| thread::set_dispatch_last_post(w, &it, 9))
        .unwrap();
    let fetched = s
        .read(|r| thread::dispatch_by_item(r, &it))
        .unwrap()
        .unwrap();
    assert_eq!((fetched.follow, fetched.last_post), (false, Some(9)));
    s.items().remove(&it).unwrap();
    assert!(
        s.read(|r| thread::dispatch_by_item(r, &it))
            .unwrap()
            .is_none()
    );

    // Delivery ref blocks a second send once settled as 'unknown' or 'sent'.
    let dc1 = s
        .write(|w| {
            thread::claim_delivery_ref(w, WS, "chan:sp1", "msg_o", "said", "conv:msg_o:said", 1)
        })
        .unwrap();
    assert!(matches!(dc1, DeliveryRefClaim::Claimed(_)));
    s.write(|w| {
        thread::settle_delivery_ref(w, WS, "chan:sp1", "msg_o", "said", "unknown", None, 2)
    })
    .unwrap();
    let dc2 = s
        .write(|w| {
            thread::claim_delivery_ref(
                w,
                WS,
                "chan:sp1",
                "msg_o",
                "failure_note",
                "conv:msg_o:failure_note",
                3,
            )
        })
        .unwrap();
    assert!(matches!(dc2, DeliveryRefClaim::Blocked(_)));

    // Notices: later release supersedes open release failure; close_notice marks host line closed.
    let n1 = s
        .write(|w| thread::post_notice(w, WS, "release", "fail", "fail 1", Some("ST-1"), true, 10))
        .unwrap();
    let _n2 = s
        .write(|w| thread::post_notice(w, WS, "release", "success", "ok", Some("ST-1"), false, 20))
        .unwrap();
    let list = s.read(|r| thread::list_notices(r, WS, 10)).unwrap();
    assert_eq!(list.len(), 2);
    assert!(
        list.iter()
            .find(|n| n.id == n1.id)
            .unwrap()
            .closed_at
            .is_some()
    );
    // Host line view reflects closed=true via thread_notices.
    let host_mid = n1.host_message_id.unwrap();
    let view = s
        .read(|r| r.message_views().of(WS, &[host_mid.clone()]))
        .unwrap();
    let meta: serde_json::Value = serde_json::to_value(&view[0].meta).unwrap();
    assert_eq!(meta["notice"]["closed"], json!(true));
}

#[test]
fn receipt_dispatch_contract_enforces_real_refs() {
    let t = with_steve();
    let s = &t.store;
    let it = item(s, "已有事项");

    let submit_and_claim = |key: &str, text: &str, lease: &str| {
        let SubmitOutcome::Accepted { message_id, .. } = s
            .write(|w| {
                thread::submit_message(
                    w,
                    &SubmitInput {
                        ws: WS,
                        space: thread::MAIN_SPACE,
                        actor_key: "u1",
                        submit_key: key,
                        payload_digest: key,
                        text,
                        item_id: None,
                        meta: &json!({}),
                        user_id: Some("u1"),
                        project_id: None,
                        conv_json: None,
                        to_item: None,
                        to_reply_to: None,
                        file_ids: &[],
                        files_json: None,
                        mentions_json: None,
                        enqueue_task: true,
                        validate_refs: false,
                    },
                )
            })
            .unwrap()
        else {
            panic!("expected accepted");
        };
        let task = s
            .write(|w| thread::claim_head(w, WS, thread::MAIN_SPACE, lease, 10))
            .unwrap()
            .expect("claimed");
        (message_id, task)
    };

    // 1. Handed-off create_item receipt: NoDispatch and nonexistent refs are rejected; real ref commits.
    let (m_create, t_create) = submit_and_claim("k_create", "建个事项", "l_create");
    let commit_with = |task: &ThreadTask,
                       lease: &str,
                       outcome: &str,
                       route_kind: &str,
                       item_id: Option<&str>,
                       dispatch: ReceiptDispatch<'_>| {
        s.write(|w| {
            thread::commit_reply(
                w,
                &CommitReplyInput {
                    task_id: &task.id,
                    lease_token: lease,
                    expected_generation: task.generation,
                    allow_cancelled_stop: false,
                    outcome,
                    reply_text: Some("ok"),
                    existing_reply_id: None,
                    reply_item_id: item_id,
                    reply_meta: &json!({}),
                    error_code: None,
                    route_kind,
                    model_kind: route_kind,
                    route_guard: None,
                    route_model: None,
                    route_confidence: Some(1.0),
                    route_signals: &[],
                    route_fallback: false,
                    route_ms: 1,
                    route_outcome: None,
                    item_id,
                    dispatch,
                    delivery_ref_id: None,
                    model_call_id: None,
                    user_id: Some("u1"),
                    project_id: None,
                    answer_model: None,
                    harness: None,
                    harness_session: None,
                    tools: 0,
                    turns: 0,
                    first_text_ms: None,
                    error_detail: None,
                    record_attention_commit: false,
                },
            )
        })
    };

    assert!(matches!(
        commit_with(
            &t_create,
            "l_create",
            "handed_off",
            "item",
            Some(&it),
            ReceiptDispatch::NoDispatch,
        ),
        Err(StoreError::Rejected(_))
    ));
    assert!(matches!(
        commit_with(
            &t_create,
            "l_create",
            "handed_off",
            "item",
            Some(&it),
            ReceiptDispatch::Dispatched("disp_missing"),
        ),
        Err(StoreError::Rejected(_))
    ));
    let DispatchClaim::Claimed(d_create) = s
        .write(|w| thread::claim_dispatch(w, WS, &m_create, "create_item", false, 11))
        .unwrap()
    else {
        panic!("expected claimed create_item");
    };
    s.write(|w| thread::settle_dispatch(w, &d_create.id, "applied", Some(&it), None, 12))
        .unwrap();
    let CommitOutcome::Committed { receipt, .. } = commit_with(
        &t_create,
        "l_create",
        "handed_off",
        "item",
        Some(&it),
        ReceiptDispatch::Dispatched(&d_create.id),
    )
    .unwrap() else {
        panic!("expected committed create receipt");
    };
    assert_eq!(
        receipt.dispatch_ref_id.as_deref(),
        Some(d_create.id.as_str())
    );

    // 2. Handed-off append_item receipt links its durable append_item dispatch ref.
    let (m_append, t_append) = submit_and_claim("k_append", "补充一下", "l_append");
    let DispatchClaim::Claimed(d_append) = s
        .write(|w| thread::claim_dispatch(w, WS, &m_append, "append_item", false, 13))
        .unwrap()
    else {
        panic!("expected claimed append_item");
    };
    s.write(|w| thread::settle_dispatch(w, &d_append.id, "applied", Some(&it), None, 14))
        .unwrap();
    let CommitOutcome::Committed { receipt, .. } = commit_with(
        &t_append,
        "l_append",
        "handed_off",
        "supplement",
        Some(&it),
        ReceiptDispatch::Dispatched(&d_append.id),
    )
    .unwrap() else {
        panic!("expected committed append receipt");
    };
    assert_eq!(
        receipt.dispatch_ref_id.as_deref(),
        Some(d_append.id.as_str())
    );
    assert_eq!(
        s.read(|r| thread::dispatch_by_id(r, &d_append.id))
            .unwrap()
            .unwrap()
            .effect_kind,
        "append_item"
    );

    // 3. Plain reply receipt uses explicit NoDispatch and leaves dispatch_ref_id NULL.
    let (_m_plain, t_plain) = submit_and_claim("k_plain", "你好", "l_plain");
    let CommitOutcome::Committed { receipt, .. } = commit_with(
        &t_plain,
        "l_plain",
        "answered",
        "answer",
        None,
        ReceiptDispatch::NoDispatch,
    )
    .unwrap() else {
        panic!("expected committed plain receipt");
    };
    assert_eq!(receipt.dispatch_ref_id, None);
}
