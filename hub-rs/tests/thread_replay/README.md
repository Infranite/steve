# W3.4 Thread Synthetic Replay Suite

Synthetic replay fixtures for the W3.4 conversation answer flow (`thread` command, claim/lease,
cancellation fence, recovery, outbound intent deduplication, attention watermarks, and S2 read
projections).

## Layout

| File | Driver | Scenarios | Scope |
|---|---|---:|---|
| `hub-rs/tests/thread_replay/report.rs` | Shared reporter | — | Deterministic `W34 <id> PASS\|FAIL` contract; baseline pin check vs `STEVE_THREAD_REPLAY_STRICT=1` |
| `hub-rs/crates/steve-core/tests/fixtures/thread_replay.rs` | `w34_thread_replay_core` | 104 | Submit/idempotency (10), queued edit/withdraw (12), double claim & fencing (12), ACL & guest isolation (12), cancel & late results (12), stream interruption & timeouts (10), restart before/after commit & attention (14), parallel spaces & ordering (10), deterministic routing & preflight (12) |
| `hub-rs/crates/steve-hub/tests/thread_replay.rs` | `w34_thread_replay_hub` | 18 | HTTP `Dedupe` & restart tombstones (6), ACL/cross-workspace attachments & `/api/me/attention` (5), S2 `Projection::reconcile()` & `/api/snapshot` / `/api/views/page` consistency (7) |
| `hub-rs/crates/steve-channels/tests/thread_replay.rs` | `w34_thread_replay_channels` | 14 | `conv_deliver`, `conv_operation`, `Outbox`, `FakeChannel` sent-unknown (`lose_answer`), multiple replies per origin, failure notes, `host_reply=ineligible`, `Mode::Own` verification, `Outcome::Skip`/`AskOwner`, disconnected space, `converse_web` fallback |
| **Total** | **3 drivers** | **136** | |

## Running

Baseline mode (default — passes when results match the pinned baseline for current `main`, fails on
unexpected regressions or unrecorded changes):

```sh
cargo test --manifest-path hub-rs/Cargo.toml -p steve-core --lib w34_thread_replay_core -- --nocapture
cargo test --manifest-path hub-rs/Cargo.toml -p steve-hub --test thread_replay -- --nocapture
cargo test --manifest-path hub-rs/Cargo.toml -p steve-channels --test thread_replay -- --nocapture
```

Strict W3.4 acceptance mode (requires **0** invariant violations across all 136 scenarios):

```sh
STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-core --lib w34_thread_replay_core -- --nocapture
STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-hub --test thread_replay -- --nocapture
STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-channels --test thread_replay -- --nocapture
```

All tests use isolated temp SQLite directories, `/usr/bin/false` adapters, `STEVE_FORBID_REAL_HOSTS=1`,
and in-process Axum `tower::ServiceExt::oneshot` dispatch (no external network or production access).

## Baseline on `main@3e90e613`

| Driver | Total | PASS | FAIL (baseline violations) |
|---|---:|---:|---:|
| `steve-core` (`w34_thread_replay_core`) | 104 | 68 | 36 |
| `steve-hub` (`w34_thread_replay_hub`) | 18 | 14 | 4 |
| `steve-channels` (`w34_thread_replay_channels`) | 14 | 10 | 4 |
| **Total** | **136** | **92** | **44** |

### Baseline Violation Inventory (44 cases)

#### 1. Submit idempotency, atomicity, empty input & scope boundaries (12 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.submit.01_duplicate_core_say_same_text` | `le1_final_reply_per_intent`, `submit_idempotent_key_deduped` | `steve-core/src/thread/mod.rs::say_with` has no durable `client_msg_id` key |
| `core.submit.02_duplicate_core_say_item_dispatch` | `le1_dispatch_per_intent` | Duplicate `say` dispatches 2 items without origin idempotency key |
| `core.submit.03_empty_text_rejected_no_router` | `empty_message_rejected`, `explicit_route_zero_router_calls` | `say_with` stores empty message and invokes router |
| `core.submit.04_whitespace_only_newlines_no_router` | `empty_message_rejected`, `explicit_route_zero_router_calls` | `say_with` stores whitespace-only message and invokes router |
| `core.submit.05_say_atomic_message_and_task` | `submit_atomic_message_and_task` | `say_with` writes `messages` and `core_queue::enqueue` in 2 separate transactions |
| `core.submit.06_cross_ws_item_target_rejected` | `cross_scope_ref_rejected` | `say_to` does not fail-closed on cross-workspace `item` target before storing |
| `core.submit.07_cross_ws_reply_to_rejected` | `cross_scope_ref_rejected` | `say_to` does not fail-closed on cross-workspace `reply_to` before storing |
| `core.submit.08_cross_space_reply_to_rejected` | `cross_scope_ref_rejected` | `say_to` does not fail-closed on cross-space `reply_to` before storing |
| `core.submit.09_duplicate_say_in_guest_turn` | `le1_final_reply_per_intent`, `submit_idempotent_key_deduped` | `say_in` has no durable `(ws, space, provider_origin)` submit key |
| `hub.submit.02_same_client_id_different_payload_conflict` | `submit_payload_conflict_rejected` | `steve-hub/src/api/dedupe.rs::Dedupe::once` ignores payload digest (returns 200 instead of 409) |
| `hub.submit.03_same_client_id_across_hub_restart_deduped` | `le1_final_reply_per_intent`, `submit_idempotent_across_restart` | `Dedupe` is in-memory only; lost across Hub restart |
| `hub.submit.04_withdrawn_client_id_retry_after_restart_no_resurrect` | `withdrawn_tombstone_no_resurrect` | No durable submit tombstone; retrying withdrawn `client_id` after restart resurrects it |

#### 2. Queued withdrawal tombstone (1 case)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.queued.12_withdraw_keeps_tombstone_no_hard_delete` | `withdrawn_tombstone_no_resurrect` | `thread_queued_withdraw` hard-deletes the message row via `core_queue::remove_message` |

#### 3. Double claim, stale worker fencing & single dispatch per origin (5 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.claim.03_double_thread_ready_two_cores_interrupted_run` | `double_claim_single_execution`, `le1_final_reply_per_intent` | `runs_to_resume`/`resume_run` lack DB claim CAS across instances |
| `core.claim.04_stale_worker_commits_after_recover_restart` | `le1_final_reply_per_intent`, `stale_worker_fenced` | `inline_reply` has no `(lease_token, generation)` fence on commit |
| `core.claim.06_two_offers_same_origin_single_dispatch` | `le1_dispatch_per_intent` | `offer` checks per-reply JSON `meta.offer` only; no DB unique dispatch key per origin |
| `core.claim.09_concurrent_promote_two_cores` | `le1_dispatch_per_intent` | `promote` uses in-memory `OnceCell` mutex; two instances both create items |
| `core.claim.10_promote_then_reroute_item_same_origin` | `le1_dispatch_per_intent` | `promote` and `reroute(Item)` share no durable origin-level `effect_key` |

#### 4. ACL, space fallback & caller identity preservation (3 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.acl.08_missing_origin_no_main_space_fallback` | `missing_origin_no_main_fallback` | `steve-core/src/thread/mod.rs::conv_of` falls back to `MAIN ("steve")` when message ID is missing |
| `core.acl.10_suggest_here_preserves_caller_user_id` | `caller_identity_preserved` | `suggest::answer_suggestion_here` hardcodes `user: None` |
| `core.acl.11_reroute_inline_preserves_caller_user_id` | `caller_identity_preserved` | `reroute::research_again` hardcodes `user: None` |

#### 5. Cancellation fence & late results (4 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.cancel.02_stop_twice_during_routing_single_receipt` | `le1_final_reply_per_intent`, `stop_idempotent_single_receipt` | `stop_turn` does not check `routing.is_stopped()`, allowing duplicate stop receipts |
| `core.cancel.07_stop_during_research_drops_late_offer` | `stopped_no_late_offer` | `research_now` attaches `outcome.offer` even when run stopped |
| `core.cancel.09_stop_during_guest_answer_cancels_promptly` | `guest_stop_cancels_in_flight` | `guest_answer` awaits `host.answer` without `select!` on `stop` |
| `core.cancel.11_reroute_fences_running_inline_reply` | `le1_final_reply_per_intent`, `reroute_fences_stale_reply` | `reroute(Item)` does not fence an in-flight inline worker from committing a 2nd reply |

#### 6. Stream interruption (1 case)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.stream.02_research_failed_with_partial_text_marks_error` | `stream_failed_explicit_error` | `research_now` writes partial text on `RunEnd::Failed` without `meta.error` or failure marker |

#### 7. Restart recovery, attention deduplication & boot postback (5 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.restart.03_crash_during_running_main_run_no_auto_replay` | `le1_final_reply_per_intent`, `unconfirmed_start_zero_auto_replay` | `thread_recover` + `thread_ready` auto-replay unknown `running` main runs and post extra `resume_pending` message |
| `core.restart.04_crash_twice_during_running_main_run` | `le1_final_reply_per_intent`, `unconfirmed_start_zero_auto_replay` | Double crash auto-replays running run and posts multiple status messages |
| `core.restart.05_crash_during_queued_main_run_single_reply` | `le1_final_reply_per_intent` | Even for queued runs, `thread_recover` posts extra `"重启后会继续回答"` message before final answer (2 replies) |
| `core.restart.13_duplicate_resumed_reply_adds_zero_extra_unread` | `duplicate_reply_zero_new_unread` | Unfenced stale worker commits a 2nd reply and inserts a 2nd unread row in `thread_reply_commits` |
| `core.restart.14_boot_postback_no_unsolicited_message` | `no_boot_postback_mutation` | `thread_postback` / `POSTBACK = ["ST-20"]` mutates conversation at boot |

#### 8. Parallel spaces, serial head ordering & concurrency budget (4 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.spaces.04_same_space_direct_does_not_overtake_head` | `same_space_serial_order` | `direct_lane` overtakes unsettled head in the same space |
| `core.spaces.05_same_space_ahead_does_not_overtake_head` | `same_space_serial_order` | `ahead` (`now_while_answering`) overtakes unsettled head in the same space |
| `core.spaces.07_guest_completion_preserves_main_resume_session` | `per_space_resume_isolation` | `resume_for` queries workspace-wide `run_last_done(ws)`; guest completion breaks main resume |
| `core.spaces.10_parallel_spaces_respect_global_concurrency_budget` | `parallel_spaces_global_budget` | Unbounded per-space inline lanes with no global concurrency cap |

#### 9. Deterministic routing, single generation & tool preflight (4 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `core.routing.07_explicit_at_bot_zero_router_calls` | `explicit_route_zero_router_calls` | `needs_router` forces `@Bot` messages through the router model |
| `core.routing.08_explicit_item_phrase_zero_router_calls` | `explicit_route_zero_router_calls` | `needs_router` forces explicit `"建个事项"` phrases through the router model |
| `core.routing.09_obvious_greeting_single_generation` | `obvious_answer_single_generation` | Obvious greeting `"谢谢"` invokes 2 LLM calls (router + answer) |
| `core.routing.10_hands_on_needs_tools_preflight_zero_probe` | `tool_preflight_zero_probe_calls` | `handoff::hand_off` probes a tool-less model first until `GatewayError::NeedsTools` |

#### 10. Channels outbound idempotency & sent-unknown (4 cases)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `chan.out.03_two_replies_same_origin_single_outbound_intent` | `le1_outbound_intent_per_origin` | `conv_operation` keys on temporary reply `m.id` (`conv:{m.id}:{origin}:{purpose}`), so 2 replies for 1 origin send twice |
| `chan.out.05_sent_unknown_then_second_reply_same_origin_no_second_send` | `le1_outbound_intent_per_origin`, `unknown_outbound_no_second_send` | After `lose_answer` (`unknown`), a 2nd reply for the same origin produces a new `operation_id` and sends again |
| `chan.out.12_bot_failure_note_two_error_replies_same_origin` | `le1_outbound_intent_per_origin` | Bot failure notes key on reply `m.id`, sending 2 external failure notes for 1 origin |
| `chan.out.13_trigger_failed_note_two_error_replies_same_origin` | `le1_outbound_intent_per_origin` | Trigger failure notes key on reply `m.id`, sending 2 external failure notes for 1 origin |

#### 11. S2 projection invalidation (1 case)

| Case ID | Violated invariants | Current root cause |
|---|---|---|
| `hub.proj.05_notice_dismiss_invalidates_message_projection` | `s2_projection_zero_diff` | `steve-hub/src/notify.rs::dismiss` writes `noticeDismissed` via `setting_put_raw` without emitting a `MessageView` invalidation event |

## Additional Acceptance Scope (W3.4 Cutover)

These synthetic replay fixtures establish the deterministic domain/HTTP/outbound contract gate.
When W3.4 switches production code, cutover acceptance additionally covers:
- Real `Store::write` commit-boundary failpoints
- Process-level `SIGKILL` Hub restart tests
- Mid-turn scoped grant approval and permission revocation
- Clean installation E2E (Hub + Node + Web)
- Enabled PostgreSQL and MySQL storage contract suites
