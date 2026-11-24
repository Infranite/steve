//! Sandboxed QuickJS (`rquickjs`) single-step replay interpreter for Steve workflow scripts.
//!
//! Invoked exclusively inside the isolated `steve-hub --workflow-step` subprocess.

mod convert;
mod engine;
mod sandbox;

pub use engine::run_step;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use serde_json::json;
    use steve_workflow_api::{
        MAX_CALL_IDENTITY_BYTES, MAX_LOG_ENTRIES, MAX_LOG_ENTRY_BYTES, MAX_SAFE_INTEGER,
        RecordedCall, StepFrame, StepInput, StepLimitsOverride, WorkflowFaultCode, WorkflowState,
    };

    use super::*;

    fn base_state() -> WorkflowState {
        WorkflowState {
            parent_item_id: "item-1".into(),
            cycle: 1,
            goal_version: 2,
            run_revision: 3,
            script_version: 1,
        }
    }

    fn input(script: &str) -> StepInput {
        StepInput {
            script_source: script.into(),
            state: base_state(),
            recorded_calls: BTreeMap::new(),
            limits: None,
        }
    }

    #[test]
    fn sequential_replay_and_state_reading() {
        let script = r#"
            export default async function () {
              log("starting cycle " + state.cycle);
              const plan = await phase("plan", () => agent("make plan", { title: "Plan" }));
              const impl = await phase("build", () => agent("implement " + plan.summary, { bot: "coder" }));
              return { item: state.parentItemId, plan, impl };
            }
        "#;

        // Step 1: emits plan call.
        let f1 = run_step(&input(script));
        let StepFrame::PendingCalls {
            emitted_calls: c1,
            pending_identities: p1,
            logs: l1,
            ..
        } = f1
        else {
            panic!("expected PendingCalls, got {f1:?}");
        };
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].identity.phase.as_deref(), Some("plan"));
        assert_eq!(p1, vec![c1[0].identity.identity_hash.clone()]);
        assert_eq!(l1.len(), 1);
        assert_eq!(l1[0].message, "starting cycle 1");

        // Step 2: plan succeeded -> emits build call.
        let mut inp2 = input(script);
        inp2.recorded_calls.insert(
            c1[0].identity.identity_hash.clone(),
            RecordedCall::Succeeded {
                result: json!({ "summary": "v1" }),
                settled_at_revision: Some(1),
            },
        );
        let f2 = run_step(&inp2);
        let StepFrame::PendingCalls {
            emitted_calls: c2,
            pending_identities: p2,
            ..
        } = f2
        else {
            panic!("expected PendingCalls, got {f2:?}");
        };
        assert_eq!(c2.len(), 2);
        assert_eq!(c2[0].identity, c1[0].identity);
        assert_eq!(c2[1].identity.phase.as_deref(), Some("build"));
        assert_eq!(c2[1].prompt, "implement v1");
        assert_eq!(p2, vec![c2[1].identity.identity_hash.clone()]);

        // Step 3: both succeeded -> Completed.
        inp2.recorded_calls.insert(
            c2[1].identity.identity_hash.clone(),
            RecordedCall::Succeeded {
                result: json!({ "ok": true }),
                settled_at_revision: Some(2),
            },
        );
        let f3 = run_step(&inp2);
        let StepFrame::Completed {
            result,
            emitted_calls: c3,
            ..
        } = f3
        else {
            panic!("expected Completed, got {f3:?}");
        };
        assert_eq!(c3.len(), 2);
        assert_eq!(
            result,
            json!({
                "item": "item-1",
                "plan": { "summary": "v1" },
                "impl": { "ok": true }
            })
        );
    }

    #[test]
    fn all_preserves_index_order_and_occurrence_and_phase_isolation() {
        let script = r#"
            export default async function () {
              return await all([
                () => agent("same prompt"),
                () => phase("side", () => agent("same prompt")),
                () => agent("same prompt"),
              ]);
            }
        "#;
        let f1 = run_step(&input(script));
        let StepFrame::PendingCalls {
            emitted_calls,
            pending_identities,
            ..
        } = f1
        else {
            panic!("expected PendingCalls, got {f1:?}");
        };
        assert_eq!(emitted_calls.len(), 3);
        assert_eq!(emitted_calls[0].identity.phase, None);
        assert_eq!(emitted_calls[0].identity.occurrence, 0);
        assert_eq!(emitted_calls[1].identity.phase.as_deref(), Some("side"));
        assert_eq!(emitted_calls[1].identity.occurrence, 0);
        // Phase "side" must NOT leak to thunk 2!
        assert_eq!(emitted_calls[2].identity.phase, None);
        assert_eq!(emitted_calls[2].identity.occurrence, 1);
        assert_eq!(pending_identities.len(), 3);
    }

    #[test]
    fn race_selects_earliest_settled_revision_and_cancels_losers() {
        let script = r#"
            export default async function () {
              const winner = await race([
                () => agent("branch A"),
                () => agent("branch B"),
                () => agent("branch C"),
              ]);
              return { winner };
            }
        "#;
        let f1 = run_step(&input(script));
        let StepFrame::PendingCalls { emitted_calls, .. } = f1 else {
            panic!("expected PendingCalls");
        };
        assert_eq!(emitted_calls.len(), 3);
        let h_a = emitted_calls[0].identity.identity_hash.clone();
        let h_b = emitted_calls[1].identity.identity_hash.clone();
        let h_c = emitted_calls[2].identity.identity_hash.clone();

        // Branch A fails, B and C still pending -> race stays pending waiting for B/C.
        let mut inp_partial = input(script);
        inp_partial.recorded_calls.insert(
            h_a.clone(),
            RecordedCall::Failed {
                error_code: "agent_failed".into(),
                message: "nope".into(),
                settled_at_revision: Some(1),
            },
        );
        let f_partial = run_step(&inp_partial);
        assert!(matches!(
            f_partial,
            StepFrame::PendingCalls { ref pending_identities, .. }
            if pending_identities.len() == 2
        ));

        // Branch B succeeds at rev 2, C still pending -> B wins, C is cancelled!
        let mut inp_win = inp_partial.clone();
        inp_win.recorded_calls.insert(
            h_b.clone(),
            RecordedCall::Succeeded {
                result: json!("from B"),
                settled_at_revision: Some(2),
            },
        );
        let f_win = run_step(&inp_win);
        let StepFrame::Completed {
            result,
            cancel_identities,
            ..
        } = f_win
        else {
            panic!("expected Completed, got {f_win:?}");
        };
        assert_eq!(result, json!({ "winner": "from B" }));
        assert_eq!(cancel_identities, vec![h_c.clone()]);

        // Even if A later shows up as succeeded at rev 5 while B settled at rev 2, B still wins!
        let mut inp_both = input(script);
        inp_both.recorded_calls.insert(
            h_a,
            RecordedCall::Succeeded {
                result: json!("late A"),
                settled_at_revision: Some(5),
            },
        );
        inp_both.recorded_calls.insert(
            h_b,
            RecordedCall::Succeeded {
                result: json!("early B"),
                settled_at_revision: Some(2),
            },
        );
        inp_both.recorded_calls.insert(
            h_c,
            RecordedCall::Cancelled {
                settled_at_revision: Some(3),
            },
        );
        let f_det = run_step(&inp_both);
        assert!(matches!(
            f_det,
            StepFrame::Completed { ref result, .. } if *result == json!({ "winner": "early B" })
        ));
    }

    #[test]
    fn try_catch_handles_recorded_call_failure_but_cannot_swallow_fatal_faults() {
        let script = r#"
            export default async function () {
              try {
                return await agent("primary");
              } catch (e) {
                return await agent("fallback:" + e.code);
              }
            }
        "#;
        let f1 = run_step(&input(script));
        let StepFrame::PendingCalls { emitted_calls, .. } = f1 else {
            panic!("expected PendingCalls");
        };
        let mut inp2 = input(script);
        inp2.recorded_calls.insert(
            emitted_calls[0].identity.identity_hash.clone(),
            RecordedCall::Failed {
                error_code: "timeout".into(),
                message: "child timed out".into(),
                settled_at_revision: Some(1),
            },
        );
        let f2 = run_step(&inp2);
        let StepFrame::PendingCalls {
            emitted_calls: c2, ..
        } = f2
        else {
            panic!("expected PendingCalls, got {f2:?}");
        };
        assert_eq!(c2[1].prompt, "fallback:timeout");

        // Trying to catch a ReDoS schema fault must still fail the step!
        let bypass = r#"
            export default async function () {
              try {
                await agent("x", { schema: { type: "string", pattern: "(a+)+$" } });
              } catch (e) {
                return "bypassed";
              }
            }
        "#;
        let fb = run_step(&input(bypass));
        assert!(matches!(
            fb,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::UnsafeRegex
        ));
    }

    #[test]
    fn sandbox_disables_non_determinism_and_blocks_prototype_pollution() {
        let check_disabled = r#"
            export default async function () {
              const results = {};
              results.typeofDate = typeof Date;
              results.typeofEval = typeof eval;
              results.typeofFunction = typeof Function;
              results.typeofFetch = typeof fetch;
              results.typeofSetTimeout = typeof setTimeout;
              results.typeofMathRandom = typeof Math.random;
              results.ctorBlocked = false;
              try { (() => {}).constructor("return 1")(); } catch (e) { results.ctorBlocked = true; }
              results.asyncCtorBlocked = false;
              try { (async () => {}).constructor("return 1")(); } catch (e) { results.asyncCtorBlocked = true; }
              results.protoPolluted = false;
              try { Object.prototype.polluted = "yes"; } catch (e) {}
              try { ({}).__proto__.polluted = "yes"; } catch (e) {}
              try { Array.prototype.hacked = 1; } catch (e) {}
              try { Promise.prototype.hacked = 1; } catch (e) {}
              try { state.cycle = 999; } catch (e) {}
              try { globalThis.evil = 1; } catch (e) {}
              const parsed = JSON.parse('{"__proto__":{"polluted":true}}');
              results.protoPolluted = ({}).polluted === "yes" || ({}).polluted === true;
              results.arrayPolluted = [].hacked === 1;
              results.stateMutated = state.cycle !== 1;
              results.globalMutated = "evil" in globalThis;
              results.parsedHasOwn = Object.prototype.hasOwnProperty.call(parsed, "__proto__");
              return results;
            }
        "#;
        let frame = run_step(&input(check_disabled));
        let StepFrame::Completed { result, .. } = frame else {
            panic!("expected Completed, got {frame:?}");
        };
        assert_eq!(result["typeofDate"], "undefined");
        assert_eq!(result["typeofEval"], "undefined");
        assert_eq!(result["typeofFunction"], "undefined");
        assert_eq!(result["typeofFetch"], "undefined");
        assert_eq!(result["typeofSetTimeout"], "undefined");
        assert_eq!(result["typeofMathRandom"], "undefined");
        assert_eq!(result["ctorBlocked"], true);
        assert_eq!(result["asyncCtorBlocked"], true);
        assert_eq!(result["protoPolluted"], false);
        assert_eq!(result["arrayPolluted"], false);
        assert_eq!(result["stateMutated"], false);
        assert_eq!(result["globalMutated"], false);
        assert_eq!(result["parsedHasOwn"], true);
    }

    #[test]
    fn rejects_nan_infinity_and_unsafe_integers() {
        for (expr, expected) in [
            ("return { x: NaN };", WorkflowFaultCode::NonFiniteNumber),
            (
                "return { x: Infinity };",
                WorkflowFaultCode::NonFiniteNumber,
            ),
            (
                "return { x: -Infinity };",
                WorkflowFaultCode::NonFiniteNumber,
            ),
            (
                "return { x: Number.MAX_SAFE_INTEGER + 1 };",
                WorkflowFaultCode::UnsafeInteger,
            ),
            (
                "return { x: Number.MIN_SAFE_INTEGER - 1 };",
                WorkflowFaultCode::UnsafeInteger,
            ),
            ("return { x: 1n };", WorkflowFaultCode::UnsafeInteger),
            (
                "await agent('x', { schema: { type: 'number', minimum: NaN } });",
                WorkflowFaultCode::NonFiniteNumber,
            ),
            (
                "await agent('x', { schema: { type: 'integer', const: 9007199254740992 } });",
                WorkflowFaultCode::UnsafeInteger,
            ),
        ] {
            let script = format!("export default async function () {{ {expr} }}");
            let frame = run_step(&input(&script));
            assert!(
                matches!(frame, StepFrame::Failed { ref fault, .. } if fault.code == expected),
                "expr `{expr}` expected {expected:?}, got {frame:?}"
            );
        }

        // Exact MAX_SAFE_INTEGER and MIN_SAFE_INTEGER are accepted.
        let ok_script = r#"
            export default async function () {
              return { max: Number.MAX_SAFE_INTEGER, min: Number.MIN_SAFE_INTEGER, negZero: -0 };
            }
        "#;
        let StepFrame::Completed { result, .. } = run_step(&input(ok_script)) else {
            panic!("expected Completed");
        };
        assert_eq!(result["max"], MAX_SAFE_INTEGER);
        assert_eq!(result["min"], -MAX_SAFE_INTEGER);
        assert_eq!(result["negZero"], 0);
    }

    #[test]
    fn enforces_timeout_oom_stack_call_count_key_size_and_logs() {
        // Infinite loop stopped within wall_time_ms (even inside try/catch).
        let mut loop_inp = input(
            "export default async function () { try { while (true) {} } catch (e) { return 'escaped'; } }",
        );
        loop_inp.limits = Some(StepLimitsOverride {
            wall_time_ms: Some(150),
            ..StepLimitsOverride::default()
        });
        let t = Instant::now();
        let f_loop = run_step(&loop_inp);
        assert!(matches!(
            f_loop,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::Timeout
        ));
        assert!(t.elapsed().as_millis() < 1000);

        // Memory bomb hits OOM.
        let f_oom = run_step(&input(
            "export default async function () { return 'x'.repeat(64 * 1024 * 1024); }",
        ));
        assert!(matches!(
            f_oom,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::OutOfMemory
        ));

        // Stack overflow hits StackOverflow.
        let f_stack = run_step(&input(
            "export default async function () { function f(n) { return f(n + 1) + 1; } return f(0); }",
        ));
        assert!(matches!(
            f_stack,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::StackOverflow
        ));

        // > 256 calls rejected.
        let f_calls = run_step(&input(
            "export default async function () { for (let i = 0; i < 257; i++) agent('p' + i); return 1; }",
        ));
        assert!(matches!(
            f_calls,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::TooManyCalls
        ));

        // > 8 KiB CallIdentity key rejected.
        let big_key_script = format!(
            "export default async function () {{ return await agent('p', {{ title: 'x'.repeat({}) }}); }}",
            MAX_CALL_IDENTITY_BYTES + 1
        );
        let f_key = run_step(&input(&big_key_script));
        assert!(matches!(
            f_key,
            StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::CallIdentityTooLarge
        ));

        // Logs capped at 64 entries and 512 bytes each without failing the run.
        let f_logs = run_step(&input(
            "export default async function () { for (let i = 0; i < 80; i++) log('a'.repeat(700)); return 'ok'; }",
        ));
        let StepFrame::Completed { logs, .. } = f_logs else {
            panic!("expected Completed");
        };
        assert_eq!(logs.len(), MAX_LOG_ENTRIES);
        assert!(logs.iter().all(|l| l.message.len() == MAX_LOG_ENTRY_BYTES));
    }

    #[test]
    fn twenty_call_in_process_replay_latency() {
        let script = r#"
            export default async function () {
              const plan = await phase("plan", () => agent("draft plan", { title: "Plan" }));
              const impls = await phase("impl", () =>
                all(plan.tasks.map((t, i) => () => agent("impl " + i + ":" + t, { tags: ["rust"] })))
              );
              const checks = await phase("verify", () =>
                all(impls.map((r, i) => () => agent("verify " + i + ":" + r.ok, { tags: ["test"] })))
              );
              const rev = await phase("review", () =>
                agent("review " + checks.length, { review: true })
              );
              return { total: impls.length + checks.length + 2, approved: rev.approved };
            }
        "#;

        let mut recorded = BTreeMap::new();
        let mut rev = 0_i64;
        loop {
            let inp = StepInput {
                script_source: script.into(),
                state: base_state(),
                recorded_calls: recorded.clone(),
                limits: None,
            };
            match run_step(&inp) {
                StepFrame::PendingCalls {
                    emitted_calls,
                    pending_identities,
                    ..
                } => {
                    for call in emitted_calls {
                        let h = call.identity.identity_hash;
                        if !pending_identities.contains(&h) || recorded.contains_key(&h) {
                            continue;
                        }
                        rev += 1;
                        let val = match call.identity.phase.as_deref() {
                            Some("plan") => json!({
                                "tasks": ["t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8"]
                            }),
                            Some("impl") => json!({ "ok": true }),
                            Some("verify") => json!({ "passed": true }),
                            _ => json!({ "approved": true }),
                        };
                        recorded.insert(
                            h,
                            RecordedCall::Succeeded {
                                result: val,
                                settled_at_revision: Some(rev),
                            },
                        );
                    }
                }
                StepFrame::Completed {
                    emitted_calls,
                    result,
                    ..
                } => {
                    assert_eq!(emitted_calls.len(), 20);
                    assert_eq!(result, json!({ "total": 20, "approved": true }));
                    break;
                }
                StepFrame::Failed { fault, .. } => panic!("unexpected fault: {fault:?}"),
            }
        }

        let replay = StepInput {
            script_source: script.into(),
            state: base_state(),
            recorded_calls: recorded,
            limits: None,
        };
        let mut samples_ms = Vec::with_capacity(10);
        for _ in 0..10 {
            let t = Instant::now();
            let f = run_step(&replay);
            let dt = t.elapsed().as_secs_f64() * 1000.0;
            assert!(matches!(
                f,
                StepFrame::Completed { ref emitted_calls, .. } if emitted_calls.len() == 20
            ));
            samples_ms.push(dt);
        }
        samples_ms.sort_by(|a, b| a.total_cmp(b));
        let median = samples_ms[samples_ms.len() / 2];
        eprintln!("wf2_bench_20_call_in_process_ms: median={median:.2} samples={samples_ms:?}");
    }
}
