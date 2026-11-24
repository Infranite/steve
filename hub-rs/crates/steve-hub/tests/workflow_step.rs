//! WF-2 acceptance & adversarial integration tests against the real `steve-hub --workflow-step`
//! subprocess boundary.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

use serde_json::json;
use steve_hub::workflow_step::StepRunner;
use steve_workflow_api::{
    MAX_CALL_IDENTITY_BYTES, MAX_RESULT_BYTES, MAX_STEP_INPUT_BYTES, RecordedCall, StepFrame,
    StepInput, StepLimitsOverride, WorkflowFaultCode, WorkflowState,
};

fn runner() -> StepRunner {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_steve-hub"));
    // Cargo uplifts the ~500 MiB debug `steve-hub` binary onto a fresh APFS inode on each
    // `cargo test` run; on macOS the first `execve` of a fresh inode pays a one-time ~5s kernel
    // AMFI/syspolicyd verification before `main()` runs (in production `current_exe()` is already
    // running and cached). Warm the inode once per test process before measuring 5s step budgets.
    static WARM_EXEC: Once = Once::new();
    WARM_EXEC.call_once(|| {
        let out = std::process::Command::new(&bin)
            .arg("--build-info")
            .output()
            .expect("warm up steve-hub binary");
        assert!(out.status.success(), "steve-hub --build-info failed");
    });
    StepRunner::new(bin)
}

fn state() -> WorkflowState {
    WorkflowState {
        parent_item_id: "item-wf2".into(),
        cycle: 1,
        goal_version: 1,
        run_revision: 1,
        script_version: 1,
    }
}

fn step(script: &str) -> StepInput {
    StepInput {
        script_source: script.into(),
        state: state(),
        recorded_calls: BTreeMap::new(),
        limits: None,
    }
}

fn open_fd_count() -> Option<usize> {
    // macOS exposes open FDs under /dev/fd, Linux under /proc/self/fd.
    for path in ["/dev/fd", "/proc/self/fd"] {
        if let Ok(rd) = std::fs::read_dir(path) {
            return Some(rd.count());
        }
    }
    None
}

#[tokio::test]
async fn infinite_loop_is_reclaimed_by_interrupt_and_parent_sigkill() {
    let r = runner();

    // 1. In-child QuickJS interrupt stops infinite loop even inside try/catch.
    let mut inp = step(
        "export default async function () { try { while (true) {} } catch (e) { return 'escaped'; } }",
    );
    inp.limits = Some(StepLimitsOverride {
        wall_time_ms: Some(150),
        ..StepLimitsOverride::default()
    });
    let t = Instant::now();
    let f = r.execute(&inp).await;
    assert!(
        matches!(f, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::Timeout),
        "expected Timeout, got {f:?}"
    );
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "took {:?}",
        t.elapsed()
    );

    // 2. Force parent SIGKILL reclamation by setting parent wall timeout < child internal budget.
    let mut inp_kill = step("export default async function () { for (;;) {} }");
    inp_kill.limits = Some(StepLimitsOverride {
        wall_time_ms: Some(5_000),
        ..StepLimitsOverride::default()
    });
    let t2 = Instant::now();
    let f_kill = r
        .execute_with_parent_limits(&inp_kill, Duration::from_millis(200), 1024 * 1024)
        .await;
    assert!(
        matches!(f_kill, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::Timeout),
        "expected parent SIGKILL Timeout, got {f_kill:?}"
    );
    assert!(
        t2.elapsed() < Duration::from_secs(2),
        "took {:?}",
        t2.elapsed()
    );

    // Parent Hub stays healthy and executes subsequent steps cleanly.
    let ok = r
        .execute(&step(
            "export default async function () { return { alive: true }; }",
        ))
        .await;
    assert!(
        matches!(ok, StepFrame::Completed { ref result, .. } if *result == json!({ "alive": true }))
    );
}

#[tokio::test]
async fn memory_bomb_oom_is_reclaimed_and_parent_survives() {
    let r = runner();

    // Single huge allocation bomb.
    let f1 = r
        .execute(&step(
            "export default async function () { return 'x'.repeat(64 * 1024 * 1024); }",
        ))
        .await;
    assert!(
        matches!(f1, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::OutOfMemory),
        "expected OutOfMemory, got {f1:?}"
    );

    // Progressive array growth bomb.
    let f2 = r
        .execute(&step(
            "export default async function () { const a = []; for (;;) a.push('x'.repeat(4096) + a.length); }",
        ))
        .await;
    assert!(
        matches!(
            f2,
            StepFrame::Failed { ref fault, .. }
                if matches!(fault.code, WorkflowFaultCode::OutOfMemory | WorkflowFaultCode::Timeout)
        ),
        "expected OutOfMemory/Timeout, got {f2:?}"
    );

    // Parent survives without disruption.
    let ok = r
        .execute(&step("export default async function () { return 42; }"))
        .await;
    assert!(matches!(ok, StepFrame::Completed { ref result, .. } if *result == json!(42)));
}

#[tokio::test]
async fn prototype_pollution_and_sandbox_escapes_are_ineffective() {
    let r = runner();
    let script = r#"
        export default async function () {
          const out = {
            evalBlocked: typeof eval === "undefined",
            functionBlocked: typeof Function === "undefined",
            dateBlocked: typeof Date === "undefined",
            mathRandomBlocked: typeof Math.random === "undefined",
            fetchBlocked: typeof fetch === "undefined",
            timersBlocked: typeof setTimeout === "undefined" && typeof setInterval === "undefined",
            ctorBlocked: false,
            errorWorks: false,
          };
          try { (() => {}).constructor("return 1")(); } catch { out.ctorBlocked = true; }
          try { Object.prototype.polluted = "yes"; } catch {}
          try { ({}).__proto__.polluted = "yes"; } catch {}
          try { Array.prototype.polluted = "yes"; } catch {}
          try { Promise.prototype.then = () => {}; } catch {}
          try { state.parentItemId = "tampered"; } catch {}
          try { globalThis.backdoor = 1; } catch {}
          JSON.parse('{"__proto__":{"polluted":true}}');

          const e = new Error("still works");
          out.errorWorks = e.message === "still works";
          out.objectPolluted = ({}).polluted !== undefined;
          out.arrayPolluted = [].polluted !== undefined;
          out.stateTampered = state.parentItemId !== "item-wf2";
          out.globalMutated = "backdoor" in globalThis;
          return out;
        }
    "#;
    let frame = r.execute(&step(script)).await;
    let StepFrame::Completed { result, .. } = frame else {
        panic!("expected Completed, got {frame:?}");
    };
    assert_eq!(result["evalBlocked"], true);
    assert_eq!(result["functionBlocked"], true);
    assert_eq!(result["dateBlocked"], true);
    assert_eq!(result["mathRandomBlocked"], true);
    assert_eq!(result["fetchBlocked"], true);
    assert_eq!(result["timersBlocked"], true);
    assert_eq!(result["ctorBlocked"], true);
    assert_eq!(result["errorWorks"], true);
    assert_eq!(result["objectPolluted"], false);
    assert_eq!(result["arrayPolluted"], false);
    assert_eq!(result["stateTampered"], false);
    assert_eq!(result["globalMutated"], false);
}

#[tokio::test]
async fn unsafe_integers_and_nan_are_rejected_across_subprocess_boundary() {
    let r = runner();
    for (body, expected) in [
        ("return { v: NaN };", WorkflowFaultCode::NonFiniteNumber),
        (
            "return { v: Infinity };",
            WorkflowFaultCode::NonFiniteNumber,
        ),
        (
            "return { v: -Infinity };",
            WorkflowFaultCode::NonFiniteNumber,
        ),
        (
            "return { v: Number.MAX_SAFE_INTEGER + 1 };",
            WorkflowFaultCode::UnsafeInteger,
        ),
        (
            "return { v: Number.MIN_SAFE_INTEGER - 1 };",
            WorkflowFaultCode::UnsafeInteger,
        ),
        ("return { v: 1e20 };", WorkflowFaultCode::UnsafeInteger),
        (
            "await agent('x', { schema: { type: 'number', minimum: NaN } });",
            WorkflowFaultCode::NonFiniteNumber,
        ),
        (
            "await agent('x', { schema: { type: 'integer', const: 9007199254740992 } });",
            WorkflowFaultCode::UnsafeInteger,
        ),
    ] {
        let src = format!("export default async function () {{ {body} }}");
        let f = r.execute(&step(&src)).await;
        assert!(
            matches!(f, StepFrame::Failed { ref fault, .. } if fault.code == expected),
            "body `{body}` expected {expected:?}, got {f:?}"
        );
    }
}

#[tokio::test]
async fn oversized_frames_keys_results_and_inputs_are_rejected() {
    let r = runner();

    // 1. Return value > 32 KiB rejected as ResultTooLarge.
    let big_result = format!(
        "export default async function () {{ return {{ s: 'a'.repeat({}) }}; }}",
        MAX_RESULT_BYTES + 100
    );
    let f_res = r.execute(&step(&big_result)).await;
    assert!(
        matches!(f_res, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::ResultTooLarge),
        "expected ResultTooLarge, got {f_res:?}"
    );

    // 2. CallIdentity key > 8 KiB rejected as CallIdentityTooLarge.
    let big_key = format!(
        "export default async function () {{ return await agent('p', {{ title: 'k'.repeat({}) }}); }}",
        MAX_CALL_IDENTITY_BYTES + 1
    );
    let f_key = r.execute(&step(&big_key)).await;
    assert!(
        matches!(f_key, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::CallIdentityTooLarge),
        "expected CallIdentityTooLarge, got {f_key:?}"
    );

    // 3. Stdout frame exceeding parent streaming cap forces immediate kill & FrameTooLarge.
    let many_prompts = r#"
        export default async function () {
          const thunks = [];
          for (let i = 0; i < 40; i++) {
            thunks.push(() => agent("p".repeat(500) + i));
          }
          return await all(thunks);
        }
    "#;
    let f_stream = r
        .execute_with_parent_limits(&step(many_prompts), Duration::from_secs(5), 4096)
        .await;
    assert!(
        matches!(f_stream, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::FrameTooLarge),
        "expected FrameTooLarge from parent streaming guard, got {f_stream:?}"
    );

    // 4. Child-side frame cap also reports FrameTooLarge.
    let mut child_capped = step(many_prompts);
    child_capped.limits = Some(StepLimitsOverride {
        max_frame_bytes: Some(4096),
        ..StepLimitsOverride::default()
    });
    let f_child_cap = r.execute(&child_capped).await;
    assert!(
        matches!(f_child_cap, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::FrameTooLarge),
        "expected FrameTooLarge from child cap, got {f_child_cap:?}"
    );

    // 5. Oversized stdin StepInput rejected before spawning.
    let huge_input = StepInput {
        script_source: "x".repeat(MAX_STEP_INPUT_BYTES + 1),
        state: state(),
        recorded_calls: BTreeMap::new(),
        limits: None,
    };
    let f_in = r.execute(&huge_input).await;
    assert!(
        matches!(f_in, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::InputTooLarge),
        "expected InputTooLarge, got {f_in:?}"
    );
}

#[tokio::test]
async fn redos_patterns_are_intercepted_in_schema_and_bounded_in_js() {
    let r = runner();

    // 1. Static schema ReDoS interception (instant rejection, cannot be caught by JS try/catch).
    for pattern in [
        "(a+)+$",
        "(a*)*b",
        "([a-z]+)+",
        "(a|aa)+",
        ".*.*a",
        "a{101}",
    ] {
        let src = format!(
            r#"export default async function () {{
              try {{
                await agent("x", {{ schema: {{ type: "string", pattern: "{pattern}" }} }});
              }} catch (e) {{
                return "swallowed";
              }}
            }}"#
        );
        let t = Instant::now();
        let f = r.execute(&step(&src)).await;
        assert!(
            matches!(f, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::UnsafeRegex),
            "pattern `{pattern}` expected UnsafeRegex, got {f:?}"
        );
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    // 2. Direct JS backtracking ReDoS is bounded and reclaimed cleanly by timeout.
    let mut js_redos =
        step(r#"export default async function () { return /(a+)+$/.test("a".repeat(30) + "!"); }"#);
    js_redos.limits = Some(StepLimitsOverride {
        wall_time_ms: Some(150),
        ..StepLimitsOverride::default()
    });
    let t = Instant::now();
    let f_js = r.execute(&js_redos).await;
    assert!(
        matches!(f_js, StepFrame::Failed { ref fault, .. } if fault.code == WorkflowFaultCode::Timeout),
        "expected Timeout on JS ReDoS, got {f_js:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn repeated_faults_and_replays_leak_zero_file_descriptors() {
    let r = runner();

    // Warm up one call so any lazy runtime FDs are already open.
    let _ = r
        .execute(&step("export default async function () { return 1; }"))
        .await;

    let before = open_fd_count();
    for i in 0..8 {
        match i % 4 {
            0 => {
                let mut inp = step("export default async function () { while (true) {} }");
                inp.limits = Some(StepLimitsOverride {
                    wall_time_ms: Some(50),
                    ..StepLimitsOverride::default()
                });
                let _ = r.execute(&inp).await;
            }
            1 => {
                let _ = r
                    .execute(&step(
                        "export default async function () { return 'x'.repeat(64 * 1024 * 1024); }",
                    ))
                    .await;
            }
            2 => {
                let _ = r
                    .execute_with_parent_limits(
                        &step("export default async function () { for (;;) {} }"),
                        Duration::from_millis(80),
                        1024 * 1024,
                    )
                    .await;
            }
            _ => {
                let f = r
                    .execute(&step(
                        "export default async function () { return { ok: true }; }",
                    ))
                    .await;
                assert!(matches!(f, StepFrame::Completed { .. }));
            }
        }
    }
    let after = open_fd_count();
    if let (Some(b), Some(a)) = (before, after) {
        assert!(
            a <= b + 2,
            "FD count grew across subprocess burst: before={b}, after={a}"
        );
    }
}

#[tokio::test]
async fn twenty_call_replay_completes_deterministically_and_records_latency() {
    let r = runner();
    let script = r#"
        export default async function () {
          const plan = await phase("plan", () =>
            agent("draft architecture plan", {
              title: "Plan",
              schema: {
                type: "object",
                required: ["tasks"],
                properties: { tasks: { type: "array", items: { type: "string" } } }
              }
            })
          );

          const batch1 = await phase("implement", () =>
            all(plan.tasks.slice(0, 8).map((t, i) => () =>
              agent("implement task " + i + ": " + t, {
                bot: "coder",
                tags: ["rust"],
                title: "Impl " + i
              })
            ))
          );

          const batch2 = await phase("verify", () =>
            all(batch1.slice(0, 8).map((res, i) => () =>
              agent("verify result " + i + ": " + res.hash, {
                tags: ["test"],
                title: "Verify " + i
              })
            ))
          );

          const fast = await phase("bench", () =>
            race([
              () => agent("bench candidate A", { title: "Bench A" }),
              () => agent("bench candidate B", { title: "Bench B" }),
            ])
          );

          const review = await phase("review", () =>
            agent("independent final review of " + fast.winner, {
              review: true,
              title: "Final Review",
              schema: {
                type: "object",
                required: ["approved"],
                properties: { approved: { type: "boolean" } }
              }
            })
          );

          return {
            count: batch1.length + batch2.length + 4,
            fast: fast.winner,
            approved: review.approved
          };
        }
    "#;

    // Drive the workflow step-by-step to collect all 20 deterministic CallIdentities.
    let mut recorded = BTreeMap::new();
    let mut rev = 0_i64;
    loop {
        let inp = StepInput {
            script_source: script.into(),
            state: state(),
            recorded_calls: recorded.clone(),
            limits: None,
        };
        match r.execute(&inp).await {
            StepFrame::PendingCalls {
                emitted_calls,
                pending_identities,
                ..
            } => {
                assert!(!pending_identities.is_empty());
                for call in emitted_calls {
                    let hash = call.identity.identity_hash;
                    if !pending_identities.contains(&hash) || recorded.contains_key(&hash) {
                        continue;
                    }
                    rev += 1;
                    let result = if call.identity.phase.as_deref() == Some("plan") {
                        json!({
                            "tasks": ["t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7"]
                        })
                    } else if call.identity.phase.as_deref() == Some("implement") {
                        json!({ "hash": format!("h{rev}") })
                    } else if call.identity.phase.as_deref() == Some("verify") {
                        json!({ "passed": true })
                    } else if call.identity.phase.as_deref() == Some("bench") {
                        if call.prompt.ends_with('A') {
                            json!({ "winner": "A" })
                        } else {
                            json!({ "winner": "B" })
                        }
                    } else {
                        json!({ "approved": true })
                    };
                    recorded.insert(
                        hash,
                        RecordedCall::Succeeded {
                            result,
                            settled_at_revision: Some(rev),
                        },
                    );
                }
            }
            StepFrame::Completed {
                result,
                emitted_calls,
                ..
            } => {
                // 1 plan + 8 implement + 8 verify + 2 bench (race) + 1 review = 20 calls!
                assert_eq!(emitted_calls.len(), 20);
                assert_eq!(
                    result,
                    json!({ "count": 20, "fast": "A", "approved": true })
                );
                break;
            }
            StepFrame::Failed { fault, .. } => panic!("unexpected workflow fault: {fault:?}"),
        }
    }

    // Measure 20-call full replay latency (5 subprocess runs + 10 in-process runs).
    let replay_input = StepInput {
        script_source: script.into(),
        state: state(),
        recorded_calls: recorded,
        limits: None,
    };

    let mut subproc_ms = Vec::with_capacity(5);
    for _ in 0..5 {
        let t = Instant::now();
        let frame = r.execute(&replay_input).await;
        let dt = t.elapsed();
        assert!(matches!(
            frame,
            StepFrame::Completed { ref emitted_calls, .. } if emitted_calls.len() == 20
        ));
        subproc_ms.push(dt.as_secs_f64() * 1000.0);
    }
    subproc_ms.sort_by(|a, b| a.total_cmp(b));
    let subproc_median = subproc_ms[subproc_ms.len() / 2];

    eprintln!("wf2_bench_20_call_subprocess_ms: median={subproc_median:.2} samples={subproc_ms:?}");
}
