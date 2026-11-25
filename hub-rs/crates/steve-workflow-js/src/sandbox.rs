//! Hardened QuickJS runtime & context construction.
//!
//! Isolation invariants enforced before any workflow module is parsed or evaluated:
//! - Only pure deterministic intrinsics are added (`Eval` for host module parsing only, `RegExp`,
//!   `Json`, `MapSet`, `Promise`). `Date`, `Performance`, `Proxy`, `WeakRef`, `TypedArrays`,
//!   `DOMException`, timers, `console`, `fetch`, `fs`, `os`, `std`, and module loaders are absent.
//! - `globalThis.eval`, `globalThis.Function`, and `Math.random` are deleted.
//! - `.constructor` on `Function`, `AsyncFunction`, `GeneratorFunction`, and
//!   `AsyncGeneratorFunction` prototypes is replaced with a frozen throwing stub so
//!   `(() => {}).constructor("...")()` cannot escape.
//! - The six host primitives (`agent`, `all`, `race`, `phase`, `state`, `log`) are installed as
//!   non-writable, non-configurable properties on `globalThis`.
//! - `state`, all built-in constructors/prototypes/iterators, and `globalThis` itself are
//!   deeply frozen so prototype pollution fails in strict mode and has zero effect.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rquickjs::context::intrinsic;
use rquickjs::{Context, Ctx, Function, Runtime, Value as JsValue};
use steve_workflow_api::{EffectiveLimits, WorkflowFault, WorkflowFaultCode, WorkflowState};

use crate::convert::json_to_js;

/// Bootstrap script executed by the host BEFORE user module evaluation.
///
/// Receives `(hostAgent, hostAll, hostRace, hostPhase, hostLog, stateObj)` and returns nothing.
/// Strips dynamic code execution & non-determinism, defines the 6 immutable host primitives,
/// deep-freezes every intrinsic prototype/constructor/iterator and `globalThis`.
const LOCKDOWN_BOOTSTRAP: &str = r#"(function (hostAgent, hostAll, hostRace, hostPhase, hostLog, stateObj) {
  "use strict";

  const defineLocked = (target, name, value) => {
    Object.defineProperty(target, name, {
      value,
      writable: false,
      enumerable: true,
      configurable: false,
    });
  };

  // 1. Neutralize dynamic code evaluation & hidden Function constructors.
  const disabledCtor = Object.freeze(function disabledFunctionConstructor() {
    throw new TypeError("Function constructor is disabled in workflow sandbox");
  });
  const fnProtos = [
    Function.prototype,
    Object.getPrototypeOf(async function () {}),
    Object.getPrototypeOf(function* () {}),
    Object.getPrototypeOf(async function* () {}),
  ];
  for (const proto of fnProtos) {
    if (proto) {
      Object.defineProperty(proto, "constructor", {
        value: disabledCtor,
        writable: false,
        enumerable: false,
        configurable: false,
      });
    }
  }
  delete globalThis.eval;
  delete globalThis.Function;

  // 2. Strip non-deterministic Math.random.
  if (typeof Math === "object" && Math !== null) {
    delete Math.random;
  }

  // 3. Collect hidden built-in prototypes before freezing.
  const arrayIterProto = Object.getPrototypeOf([][Symbol.iterator]());
  const iterProto = Object.getPrototypeOf(arrayIterProto);
  const extraRoots = [
    ...fnProtos,
    iterProto,
    arrayIterProto,
    Object.getPrototypeOf(""[Symbol.iterator]()),
    Object.getPrototypeOf((new Map())[Symbol.iterator]()),
    Object.getPrototypeOf((new Set())[Symbol.iterator]()),
    Object.getPrototypeOf((function* () {})()),
    Object.getPrototypeOf((async function* () {})()),
    Object.getPrototypeOf(Promise.resolve()),
    stateObj,
    hostAgent,
    hostAll,
    hostRace,
    hostPhase,
    hostLog,
  ];

  // 4. Install the exact 6 host primitives as immutable globals.
  defineLocked(globalThis, "agent", hostAgent);
  defineLocked(globalThis, "all", hostAll);
  defineLocked(globalThis, "race", hostRace);
  defineLocked(globalThis, "phase", hostPhase);
  defineLocked(globalThis, "state", stateObj);
  defineLocked(globalThis, "log", hostLog);

  // 5. Recursively freeze all reachable intrinsics, state, and globalThis.
  const seen = new Set();
  const deepFreeze = (obj) => {
    if (obj === null || (typeof obj !== "object" && typeof obj !== "function")) {
      return;
    }
    if (seen.has(obj)) {
      return;
    }
    seen.add(obj);

    const keys = [
      ...Object.getOwnPropertyNames(obj),
      ...Object.getOwnPropertySymbols(obj),
    ];
    for (const key of keys) {
      const desc = Object.getOwnPropertyDescriptor(obj, key);
      if (!desc) continue;
      if ("value" in desc) {
        deepFreeze(desc.value);
      }
      if (desc.get) deepFreeze(desc.get);
      if (desc.set) deepFreeze(desc.set);
    }

    const proto = Object.getPrototypeOf(obj);
    if (proto) {
      deepFreeze(proto);
    }
    Object.freeze(obj);
  };

  for (const root of extraRoots) {
    deepFreeze(root);
  }
  deepFreeze(globalThis);
})"#;

pub struct SandboxRuntime {
    pub context: Context,
    _runtime: Runtime,
    pub timed_out: Arc<AtomicBool>,
}

impl SandboxRuntime {
    pub fn new(limits: EffectiveLimits) -> Result<Self, WorkflowFault> {
        let runtime = Runtime::new().map_err(|e| {
            WorkflowFault::new(
                WorkflowFaultCode::RuntimeError,
                format!("failed to initialize QuickJS runtime: {e}"),
            )
        })?;
        runtime.set_memory_limit(limits.js_heap_bytes);
        runtime.set_max_stack_size(limits.js_stack_bytes);

        let started_at = Instant::now();
        let wall_time = limits.wall_time;
        let timed_out = Arc::new(AtomicBool::new(false));
        let timed_out_flag = Arc::clone(&timed_out);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            if started_at.elapsed() >= wall_time {
                timed_out_flag.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        })));

        // Minimal deterministic intrinsic set: Eval is required internally by QuickJS to parse
        // the workflow module; globalThis.eval and Function are deleted during lockdown.
        // Date, Performance, Proxy, WeakRef, TypedArrays, DOMException are intentionally omitted.
        let context = Context::custom::<(
            intrinsic::Eval,
            intrinsic::RegExpCompiler,
            intrinsic::RegExp,
            intrinsic::Json,
            intrinsic::MapSet,
            intrinsic::Promise,
        )>(&runtime)
        .map_err(|e| {
            WorkflowFault::new(
                WorkflowFaultCode::RuntimeError,
                format!("failed to initialize QuickJS context: {e}"),
            )
        })?;

        Ok(Self {
            _runtime: runtime,
            context,
            timed_out,
        })
    }

    pub fn is_timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Relaxed)
    }
}

/// Runs [`LOCKDOWN_BOOTSTRAP`] with the 6 host primitives before any user code is evaluated.
pub fn install_and_lock<'js>(
    ctx: &Ctx<'js>,
    host_agent: Function<'js>,
    host_all: Function<'js>,
    host_race: Function<'js>,
    host_phase: Function<'js>,
    host_log: Function<'js>,
    state: &WorkflowState,
) -> Result<(), WorkflowFault> {
    let state_json = serde_json::to_value(state).expect("WorkflowState to JSON");
    let state_js: JsValue<'js> =
        json_to_js(ctx, &state_json).map_err(|e| e.into_fault("workflow state"))?;

    let bootstrap: Function<'js> = ctx.eval(LOCKDOWN_BOOTSTRAP).map_err(|e| {
        WorkflowFault::new(
            WorkflowFaultCode::RuntimeError,
            format!("sandbox lockdown compile failed: {e}"),
        )
    })?;
    bootstrap
        .call::<_, ()>((
            host_agent, host_all, host_race, host_phase, host_log, state_js,
        ))
        .map_err(|e| {
            WorkflowFault::new(
                WorkflowFaultCode::RuntimeError,
                format!("sandbox lockdown execution failed: {e}"),
            )
        })
}
