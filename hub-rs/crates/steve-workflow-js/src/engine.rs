//! Single-step deterministic replay engine for sandboxed workflow JS scripts.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use rquickjs::function::Rest;
use rquickjs::promise::PromiseState;
use rquickjs::{
    CatchResultExt, CaughtError, Ctx, Error as JsError, Function, Module, Object, Promise, Type,
    Value as JsValue,
};
use serde_json::Value;
use steve_workflow_api::{
    CallIdentity, CallOptions, CallSpec, EffectiveLimits, IdentityError, MAX_LOG_ENTRIES,
    MAX_LOG_ENTRY_BYTES, MAX_PHASE_NAME_BYTES, MAX_PROMPT_BYTES, MAX_SCRIPT_SOURCE_BYTES,
    RecordedCall, SchemaError, StepFrame, StepInput, StepLogEntry, WorkflowFault,
    WorkflowFaultCode, to_canonical_json, validate_json_value,
};

use crate::convert::{js_to_json, json_to_js};
use crate::sandbox::{SandboxRuntime, install_and_lock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmittedCallState {
    Pending,
    Succeeded { settled_at_revision: Option<i64> },
    Failed,
    Cancelled,
}

struct ActiveRace<'js> {
    race_id: u32,
    branches: Vec<Promise<'js>>,
    resolve: Function<'js>,
    reject: Function<'js>,
    settled: bool,
}

struct EngineState<'js> {
    limits: EffectiveLimits,
    recorded_calls: BTreeMap<String, RecordedCall>,
    occurrence_counts: HashMap<(Option<String>, String, String), u32>,
    emitted_calls: Vec<CallSpec>,
    call_states: Vec<EmittedCallState>,
    phase_stack: Vec<String>,
    race_stack: Vec<(u32, usize)>,
    race_branches: HashMap<u32, Vec<Vec<usize>>>,
    active_races: Vec<ActiveRace<'js>>,
    next_race_id: u32,
    cancelled_identities: Vec<String>,
    cancelled_set: HashSet<String>,
    logs: Vec<StepLogEntry>,
    fatal_fault: Option<WorkflowFault>,
}

impl<'js> EngineState<'js> {
    fn new(limits: EffectiveLimits, recorded_calls: BTreeMap<String, RecordedCall>) -> Self {
        Self {
            limits,
            recorded_calls,
            occurrence_counts: HashMap::new(),
            emitted_calls: Vec::new(),
            call_states: Vec::new(),
            phase_stack: Vec::new(),
            race_stack: Vec::new(),
            race_branches: HashMap::new(),
            active_races: Vec::new(),
            next_race_id: 0,
            cancelled_identities: Vec::new(),
            cancelled_set: HashSet::new(),
            logs: Vec::new(),
            fatal_fault: None,
        }
    }

    fn set_fatal(&mut self, fault: WorkflowFault) {
        if self.fatal_fault.is_none() {
            self.fatal_fault = Some(fault);
        }
    }

    fn current_phase(&self) -> Option<String> {
        if self.phase_stack.is_empty() {
            None
        } else {
            Some(self.phase_stack.join("/"))
        }
    }

    fn mark_cancelled(&mut self, identity_hash: &str) {
        if self.cancelled_set.insert(identity_hash.to_owned()) {
            self.cancelled_identities.push(identity_hash.to_owned());
        }
    }

    fn pending_identities(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (spec, st) in self.emitted_calls.iter().zip(self.call_states.iter()) {
            if matches!(st, EmittedCallState::Pending)
                && !self.cancelled_set.contains(&spec.identity.identity_hash)
                && seen.insert(spec.identity.identity_hash.clone())
            {
                out.push(spec.identity.identity_hash.clone());
            }
        }
        out
    }
}

/// Evaluates one deterministic workflow replay step inside an isolated QuickJS runtime.
pub fn run_step(input: &StepInput) -> StepFrame {
    let limits = EffectiveLimits::resolve(input.limits.as_ref());

    if let Some(fault) = preflight_input(input, limits) {
        return StepFrame::Failed {
            fault,
            emitted_calls: Vec::new(),
            logs: Vec::new(),
        };
    }

    let sandbox = match SandboxRuntime::new(limits) {
        Ok(s) => s,
        Err(fault) => {
            return StepFrame::Failed {
                fault,
                emitted_calls: Vec::new(),
                logs: Vec::new(),
            };
        }
    };

    let frame = sandbox.context.with(|ctx| {
        let state = Rc::new(RefCell::new(EngineState::new(
            limits,
            input.recorded_calls.clone(),
        )));

        let out = match setup_host_primitives(&ctx, &state, &input.state) {
            Ok(()) => eval_workflow_module(&ctx, &sandbox, &state, &input.script_source),
            Err(fault) => StepFrame::Failed {
                fault,
                emitted_calls: Vec::new(),
                logs: Vec::new(),
            },
        };

        // Release all JS handles held inside EngineState before Context teardown so no
        // Rust<->QuickJS reference cycle keeps GC objects alive past JS_FreeRuntime.
        state.borrow_mut().active_races.clear();
        out
    });

    enforce_frame_budget(frame, limits)
}

fn preflight_input(input: &StepInput, limits: EffectiveLimits) -> Option<WorkflowFault> {
    if input.script_source.len() > MAX_SCRIPT_SOURCE_BYTES {
        return Some(WorkflowFault::new(
            WorkflowFaultCode::ScriptTooLarge,
            format!(
                "script source is {} bytes, exceeding limit of {MAX_SCRIPT_SOURCE_BYTES}",
                input.script_source.len()
            ),
        ));
    }
    if let Err(e) = input.state.validate() {
        return Some(WorkflowFault::new(
            WorkflowFaultCode::UnsafeInteger,
            format!("invalid workflow state: {e}"),
        ));
    }
    for (hash, rec) in &input.recorded_calls {
        if let RecordedCall::Succeeded { result, .. } = rec {
            if let Err(e) = validate_json_value(result) {
                return Some(canonical_err_to_fault(
                    &e,
                    &format!("recorded result for {hash}"),
                ));
            }
            let bytes = serde_json::to_vec(result).expect("recorded JSON serialization");
            if bytes.len() > limits.max_result_bytes {
                return Some(WorkflowFault::new(
                    WorkflowFaultCode::ResultTooLarge,
                    format!(
                        "recorded result for {hash} is {} bytes, exceeding limit of {}",
                        bytes.len(),
                        limits.max_result_bytes
                    ),
                ));
            }
        }
    }
    None
}

fn enforce_frame_budget(frame: StepFrame, limits: EffectiveLimits) -> StepFrame {
    let Ok(bytes) = serde_json::to_vec(&frame) else {
        return StepFrame::Failed {
            fault: WorkflowFault::new(
                WorkflowFaultCode::ProtocolError,
                "failed to serialize StepFrame",
            ),
            emitted_calls: Vec::new(),
            logs: Vec::new(),
        };
    };
    if bytes.len() > limits.max_frame_bytes {
        return StepFrame::Failed {
            fault: WorkflowFault::new(
                WorkflowFaultCode::FrameTooLarge,
                format!(
                    "serialized StepFrame is {} bytes, exceeding limit of {} bytes",
                    bytes.len(),
                    limits.max_frame_bytes
                ),
            ),
            emitted_calls: Vec::new(),
            logs: Vec::new(),
        };
    }
    frame
}

fn setup_host_primitives<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    wf_state: &steve_workflow_api::WorkflowState,
) -> Result<(), WorkflowFault> {
    let host_agent = build_agent_fn(ctx, state)?;
    let host_all = build_all_fn(ctx, state)?;
    let host_race = build_race_fn(ctx, state)?;
    let host_phase = build_phase_fn(ctx, state)?;
    let host_log = build_log_fn(ctx, state)?;
    install_and_lock(
        ctx, host_agent, host_all, host_race, host_phase, host_log, wf_state,
    )
}

fn build_agent_fn<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<Function<'js>, WorkflowFault> {
    let st = Rc::clone(state);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<JsValue<'js>>| -> rquickjs::Result<JsValue<'js>> {
            handle_agent(&ctx, &st, &args.0)
        },
    )
    .map_err(|e| WorkflowFault::new(WorkflowFaultCode::RuntimeError, e.to_string()))
}

fn handle_agent<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    args: &[JsValue<'js>],
) -> rquickjs::Result<JsValue<'js>> {
    if args.is_empty() || args.len() > 2 {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "agent(prompt, options?) expects 1 or 2 arguments",
            ),
        );
    }

    let Some(prompt_js) = args[0].as_string() else {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "agent(prompt, options?): prompt must be a string",
            ),
        );
    };
    let prompt = prompt_js.to_string()?;
    if prompt.len() > MAX_PROMPT_BYTES {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::PromptTooLarge,
                format!(
                    "agent prompt is {} bytes, exceeding limit of {MAX_PROMPT_BYTES}",
                    prompt.len()
                ),
            ),
        );
    }

    let options: CallOptions = if args.len() == 2 && !args[1].is_undefined() && !args[1].is_null() {
        let raw_val = match js_to_json(ctx, &args[1], true) {
            Ok(v) => v,
            Err(e) => return fail_fatal(ctx, state, e.into_fault("agent options")),
        };
        if !raw_val.is_object() {
            return fail_fatal(
                ctx,
                state,
                WorkflowFault::new(
                    WorkflowFaultCode::InvalidOptions,
                    "agent(prompt, options?): options must be a plain object",
                ),
            );
        }
        match serde_json::from_value::<CallOptions>(raw_val) {
            Ok(o) => o,
            Err(e) => {
                return fail_fatal(
                    ctx,
                    state,
                    WorkflowFault::new(
                        WorkflowFaultCode::InvalidOptions,
                        format!("invalid agent options: {e}"),
                    ),
                );
            }
        }
    } else {
        CallOptions::default()
    };

    let (identity, recorded) = {
        let mut st = state.borrow_mut();
        if st.emitted_calls.len() >= st.limits.max_calls {
            let max_calls = st.limits.max_calls;
            drop(st);
            return fail_fatal(
                ctx,
                state,
                WorkflowFault::new(
                    WorkflowFaultCode::TooManyCalls,
                    format!("step exceeded maximum of {max_calls} agent() calls"),
                ),
            );
        }

        if let Err(schema_err) = options.validate() {
            drop(st);
            return fail_fatal(ctx, state, schema_err_to_fault(&schema_err));
        }

        let phase = st.current_phase();
        let prompt_sha = steve_workflow_api::sha256_hex(prompt.as_bytes());
        let opts_val = serde_json::to_value(&options).expect("CallOptions to JSON");
        let opts_canonical = match to_canonical_json(&opts_val) {
            Ok(c) => c,
            Err(e) => {
                drop(st);
                return fail_fatal(ctx, state, canonical_err_to_fault(&e, "agent options"));
            }
        };

        let key = (phase.clone(), prompt_sha, opts_canonical);
        let occ = *st.occurrence_counts.get(&key).unwrap_or(&0);
        st.occurrence_counts.insert(key, occ + 1);

        let identity = match CallIdentity::compute_with_limit(
            phase.as_deref(),
            &prompt,
            &options,
            occ,
            st.limits.max_call_identity_bytes,
        ) {
            Ok(id) => id,
            Err(e) => {
                drop(st);
                return fail_fatal(ctx, state, identity_err_to_fault(&e));
            }
        };

        let seq_no = st.emitted_calls.len() as u32;
        let recorded = st.recorded_calls.get(&identity.identity_hash).cloned();
        let call_state = match &recorded {
            Some(RecordedCall::Succeeded {
                settled_at_revision,
                ..
            }) => EmittedCallState::Succeeded {
                settled_at_revision: *settled_at_revision,
            },
            Some(RecordedCall::Failed { .. }) => EmittedCallState::Failed,
            Some(RecordedCall::Cancelled { .. }) => EmittedCallState::Cancelled,
            None => EmittedCallState::Pending,
        };

        st.emitted_calls.push(CallSpec {
            seq_no,
            identity: identity.clone(),
            prompt,
            options,
        });
        st.call_states.push(call_state);

        let active_races: Vec<(u32, usize)> = st.race_stack.clone();
        for (race_id, branch_idx) in active_races {
            if let Some(branches) = st.race_branches.get_mut(&race_id) {
                if let Some(calls) = branches.get_mut(branch_idx) {
                    calls.push(seq_no as usize);
                }
            }
        }

        (identity, recorded)
    };

    let (promise, resolve, reject) = Promise::new(ctx)?;
    match recorded {
        Some(RecordedCall::Succeeded { result, .. }) => {
            let js_res = match json_to_js(ctx, &result) {
                Ok(v) => v,
                Err(e) => {
                    return fail_fatal(
                        ctx,
                        state,
                        e.into_fault(&format!("recorded result for {}", identity.identity_hash)),
                    );
                }
            };
            resolve.call::<_, ()>((js_res,))?;
        }
        Some(RecordedCall::Failed {
            error_code,
            message,
            ..
        }) => {
            let err_obj = make_coded_error(ctx, &error_code, &message)?;
            reject.call::<_, ()>((err_obj,))?;
        }
        Some(RecordedCall::Cancelled { .. }) => {
            let err_obj = make_coded_error(ctx, "call_cancelled", "workflow call was cancelled")?;
            reject.call::<_, ()>((err_obj,))?;
        }
        None => {
            // Leave promise pending so the workflow suspends and returns StepFrame::PendingCalls.
        }
    }
    Ok(promise.into_value())
}

fn build_all_fn<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<Function<'js>, WorkflowFault> {
    let st = Rc::clone(state);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<JsValue<'js>>| -> rquickjs::Result<JsValue<'js>> {
            handle_all(&ctx, &st, &args.0)
        },
    )
    .map_err(|e| WorkflowFault::new(WorkflowFaultCode::RuntimeError, e.to_string()))
}

fn handle_all<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    args: &[JsValue<'js>],
) -> rquickjs::Result<JsValue<'js>> {
    let thunks = extract_thunks(ctx, state, args, "all")?;
    let promise_ctor: Object<'js> = ctx.globals().get("Promise")?;
    let promise_all: Function<'js> = promise_ctor.get("all")?;

    let saved_phase = state.borrow().phase_stack.clone();
    let arr = rquickjs::Array::new(ctx.clone())?;
    for (idx, thunk) in thunks.into_iter().enumerate() {
        state.borrow_mut().phase_stack = saved_phase.clone();
        let p = invoke_thunk_as_promise(ctx, &thunk)?;
        if state.borrow().fatal_fault.is_some() {
            state.borrow_mut().phase_stack = saved_phase;
            return Err(ctx.throw(make_js_error(ctx, "workflow fatal fault")?));
        }
        arr.set(idx, p.into_value())?;
    }
    state.borrow_mut().phase_stack = saved_phase;

    let combined: JsValue<'js> = promise_all.call((rquickjs::function::This(promise_ctor), arr))?;
    Ok(combined)
}

fn build_race_fn<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<Function<'js>, WorkflowFault> {
    let st = Rc::clone(state);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<JsValue<'js>>| -> rquickjs::Result<JsValue<'js>> {
            handle_race(&ctx, &st, &args.0)
        },
    )
    .map_err(|e| WorkflowFault::new(WorkflowFaultCode::RuntimeError, e.to_string()))
}

fn handle_race<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    args: &[JsValue<'js>],
) -> rquickjs::Result<JsValue<'js>> {
    let thunks = extract_thunks(ctx, state, args, "race")?;
    if thunks.is_empty() {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "race(thunks) requires at least one thunk",
            ),
        );
    }

    let (race_id, saved_phase) = {
        let mut st = state.borrow_mut();
        let id = st.next_race_id;
        st.next_race_id += 1;
        st.race_branches.insert(id, vec![Vec::new(); thunks.len()]);
        (id, st.phase_stack.clone())
    };

    let mut branches = Vec::with_capacity(thunks.len());
    let noop = Function::new(ctx.clone(), || {})?;
    for (idx, thunk) in thunks.into_iter().enumerate() {
        {
            let mut st = state.borrow_mut();
            st.phase_stack = saved_phase.clone();
            st.race_stack.push((race_id, idx));
        }
        let p = invoke_thunk_as_promise(ctx, &thunk)?;
        state.borrow_mut().race_stack.pop();
        if state.borrow().fatal_fault.is_some() {
            state.borrow_mut().phase_stack = saved_phase;
            return Err(ctx.throw(make_js_error(ctx, "workflow fatal fault")?));
        }
        // Attach a passive observer so losing rejected branches never trigger unhandled rejection.
        let _: JsValue<'js> = p.then()?.call((
            rquickjs::function::This(p.clone()),
            noop.clone(),
            noop.clone(),
        ))?;
        branches.push(p);
    }
    state.borrow_mut().phase_stack = saved_phase;

    let (result_promise, resolve, reject) = Promise::new(ctx)?;
    state.borrow_mut().active_races.push(ActiveRace {
        race_id,
        branches,
        resolve,
        reject,
        settled: false,
    });
    Ok(result_promise.into_value())
}

fn build_phase_fn<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<Function<'js>, WorkflowFault> {
    let st = Rc::clone(state);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<JsValue<'js>>| -> rquickjs::Result<JsValue<'js>> {
            handle_phase(&ctx, &st, &args.0)
        },
    )
    .map_err(|e| WorkflowFault::new(WorkflowFaultCode::RuntimeError, e.to_string()))
}

fn handle_phase<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    args: &[JsValue<'js>],
) -> rquickjs::Result<JsValue<'js>> {
    if args.len() != 2 {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "phase(name, fn) expects exactly 2 arguments",
            ),
        );
    }
    let Some(name_js) = args[0].as_string() else {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "phase(name, fn): name must be a string",
            ),
        );
    };
    let name = name_js.to_string()?;
    if name.is_empty() || name.len() > MAX_PHASE_NAME_BYTES || name.contains('\0') {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                format!("phase name must be 1..={MAX_PHASE_NAME_BYTES} non-NUL UTF-8 bytes"),
            ),
        );
    }
    let Some(func) = args[1].as_function().cloned() else {
        return fail_fatal(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                "phase(name, fn): fn must be a function",
            ),
        );
    };

    let expected_depth = {
        let mut st = state.borrow_mut();
        let depth = st.phase_stack.len();
        st.phase_stack.push(name.clone());
        depth
    };

    let inner = invoke_thunk_as_promise(ctx, &func)?;
    if state.borrow().fatal_fault.is_some() {
        pop_phase_if_matching(state, expected_depth, &name);
        return Err(ctx.throw(make_js_error(ctx, "workflow fatal fault")?));
    }

    let (outer, resolve, reject) = Promise::new(ctx)?;
    let st_ok = Rc::clone(state);
    let name_ok = name.clone();
    let on_ok = Function::new(
        ctx.clone(),
        move |val: JsValue<'js>| -> rquickjs::Result<()> {
            pop_phase_if_matching(&st_ok, expected_depth, &name_ok);
            resolve.call::<_, ()>((val,))
        },
    )?;
    let st_err = Rc::clone(state);
    let on_err = Function::new(
        ctx.clone(),
        move |err: JsValue<'js>| -> rquickjs::Result<()> {
            pop_phase_if_matching(&st_err, expected_depth, &name);
            reject.call::<_, ()>((err,))
        },
    )?;
    let _: JsValue<'js> =
        inner
            .then()?
            .call((rquickjs::function::This(inner.clone()), on_ok, on_err))?;
    Ok(outer.into_value())
}

fn pop_phase_if_matching(state: &Rc<RefCell<EngineState<'_>>>, expected_depth: usize, name: &str) {
    let mut st = state.borrow_mut();
    if st.phase_stack.len() == expected_depth + 1
        && st.phase_stack.last().is_some_and(|top| top == name)
    {
        st.phase_stack.pop();
    }
}

fn build_log_fn<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<Function<'js>, WorkflowFault> {
    let st = Rc::clone(state);
    Function::new(
        ctx.clone(),
        move |_ctx: Ctx<'js>, args: Rest<JsValue<'js>>| -> rquickjs::Result<()> {
            // Logs never affect workflow state or throw; cap count and truncate cleanly.
            let mut st = st.borrow_mut();
            if st.logs.len() >= MAX_LOG_ENTRIES {
                return Ok(());
            }
            let raw = match args.0.first() {
                None => String::new(),
                Some(v) => format_log_arg(v),
            };
            let message = truncate_utf8(&raw, MAX_LOG_ENTRY_BYTES);
            let seq = st.logs.len() as u32;
            st.logs.push(StepLogEntry { seq, message });
            Ok(())
        },
    )
    .map_err(|e| WorkflowFault::new(WorkflowFaultCode::RuntimeError, e.to_string()))
}

fn format_log_arg(v: &JsValue<'_>) -> String {
    match v.type_of() {
        Type::String => v
            .as_string()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_default(),
        Type::Null => "null".into(),
        Type::Undefined => "undefined".into(),
        Type::Bool => v.as_bool().unwrap_or(false).to_string(),
        Type::Int => v.as_int().unwrap_or(0).to_string(),
        Type::Float => v.as_float().unwrap_or(f64::NAN).to_string(),
        other => format!("[{other:?}]"),
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn eval_workflow_module<'js>(
    ctx: &Ctx<'js>,
    sandbox: &SandboxRuntime,
    state: &Rc<RefCell<EngineState<'js>>>,
    source: &str,
) -> StepFrame {
    let declared = match Module::declare(ctx.clone(), "workflow.js", source).catch(ctx) {
        Ok(m) => m,
        Err(err) => return build_error_frame(sandbox, state, err),
    };
    let (module, eval_promise) = match declared.eval().catch(ctx) {
        Ok(pair) => pair,
        Err(err) => return build_error_frame(sandbox, state, err),
    };

    if let Err(err) = eval_promise.finish::<JsValue>().catch(ctx) {
        return build_error_frame(sandbox, state, err);
    }
    if let Some(fault) = state.borrow().fatal_fault.clone() {
        return failed_with_state(state, fault);
    }

    let default_export: JsValue<'js> = match module
        .namespace()
        .and_then(|ns| ns.get("default"))
        .catch(ctx)
    {
        Ok(v) => v,
        Err(err) => return build_error_frame(sandbox, state, err),
    };
    let Some(entry_fn) = default_export.as_function().cloned() else {
        return failed_with_state(
            state,
            WorkflowFault::new(
                WorkflowFaultCode::MissingEntrypoint,
                "workflow script must export a default function: `export default async function () { ... }`",
            ),
        );
    };

    let ret_val: JsValue<'js> = match entry_fn.call(()).catch(ctx) {
        Ok(v) => v,
        Err(err) => return build_error_frame(sandbox, state, err),
    };
    if let Some(fault) = state.borrow().fatal_fault.clone() {
        return failed_with_state(state, fault);
    }

    let main_promise = match ret_val.as_promise().cloned() {
        Some(p) => p,
        None => {
            return finalize_completed(ctx, state, ret_val);
        }
    };

    if let Err(err) = drive_until_quiescent(ctx, sandbox, state) {
        return build_error_frame(sandbox, state, err);
    }
    if sandbox.is_timed_out() {
        return failed_with_state(
            state,
            WorkflowFault::new(
                WorkflowFaultCode::Timeout,
                "workflow step exceeded wall time limit",
            ),
        );
    }
    if let Some(fault) = state.borrow().fatal_fault.clone() {
        return failed_with_state(state, fault);
    }

    match main_promise.state() {
        PromiseState::Resolved => match main_promise.result::<JsValue>() {
            Some(Ok(val)) => finalize_completed(ctx, state, val),
            Some(Err(e)) => {
                let caught = Err::<(), _>(e).catch(ctx).unwrap_err();
                build_error_frame(sandbox, state, caught)
            }
            None => unreachable!("resolved promise must yield a result"),
        },
        PromiseState::Rejected => {
            let caught = main_promise
                .result::<JsValue>()
                .expect("rejected promise result")
                .catch(ctx)
                .unwrap_err();
            build_error_frame(sandbox, state, caught)
        }
        PromiseState::Pending => {
            let st = state.borrow();
            let pending_identities = st.pending_identities();
            if pending_identities.is_empty() {
                return StepFrame::Failed {
                    fault: WorkflowFault::new(
                        WorkflowFaultCode::UnresolvedPromise,
                        "workflow returned a pending Promise with no pending agent() calls",
                    ),
                    emitted_calls: st.emitted_calls.clone(),
                    logs: st.logs.clone(),
                };
            }
            StepFrame::PendingCalls {
                emitted_calls: st.emitted_calls.clone(),
                pending_identities,
                cancel_identities: st.cancelled_identities.clone(),
                logs: st.logs.clone(),
            }
        }
    }
}

fn drive_until_quiescent<'js>(
    ctx: &Ctx<'js>,
    sandbox: &SandboxRuntime,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> Result<(), CaughtError<'js>> {
    loop {
        while ctx.execute_pending_job() {
            if sandbox.is_timed_out() || state.borrow().fatal_fault.is_some() {
                return Ok(());
            }
        }
        if sandbox.is_timed_out() || state.borrow().fatal_fault.is_some() {
            return Ok(());
        }
        let settled_any = settle_ready_races(ctx, state).catch(ctx)?;
        if !settled_any {
            return Ok(());
        }
    }
}

/// Inspects all active `race()` invocations after microtask quiescence and deterministically
/// settles any race that has at least one fulfilled branch or where all branches rejected.
fn settle_ready_races<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
) -> rquickjs::Result<bool> {
    enum RaceAction<'js> {
        Resolve {
            resolve: Function<'js>,
            value: JsValue<'js>,
            cancel_hashes: Vec<String>,
        },
        Reject {
            reject: Function<'js>,
            message: String,
        },
    }

    let mut actions: Vec<RaceAction<'js>> = Vec::new();
    {
        let mut st = state.borrow_mut();
        for i in 0..st.active_races.len() {
            if st.active_races[i].settled {
                continue;
            }
            let race_id = st.active_races[i].race_id;
            let branch_count = st.active_races[i].branches.len();

            // Find all fulfilled branches and rank by deterministic (branch_settled_rev, branch_idx).
            let mut winner: Option<(i64, usize)> = None;
            let mut all_rejected = true;
            for branch_idx in 0..branch_count {
                match st.active_races[i].branches[branch_idx].state() {
                    PromiseState::Resolved => {
                        all_rejected = false;
                        let rev = branch_settled_revision(&st, race_id, branch_idx);
                        let key = (rev, branch_idx);
                        if winner.is_none_or(|cur| key < cur) {
                            winner = Some(key);
                        }
                    }
                    PromiseState::Pending => {
                        all_rejected = false;
                    }
                    PromiseState::Rejected => {}
                }
            }

            if let Some((_, win_idx)) = winner {
                let win_val = match st.active_races[i].branches[win_idx].result::<JsValue>() {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => return Err(e),
                    None => unreachable!("resolved branch has a result"),
                };
                let mut cancel_hashes = Vec::new();
                if let Some(branch_calls) = st.race_branches.get(&race_id) {
                    for (idx, call_indices) in branch_calls.iter().enumerate() {
                        if idx == win_idx {
                            continue;
                        }
                        for &call_idx in call_indices {
                            if matches!(
                                st.call_states.get(call_idx),
                                Some(EmittedCallState::Pending)
                            ) {
                                cancel_hashes.push(
                                    st.emitted_calls[call_idx].identity.identity_hash.clone(),
                                );
                            }
                        }
                    }
                }
                let resolve = st.active_races[i].resolve.clone();
                st.active_races[i].settled = true;
                actions.push(RaceAction::Resolve {
                    resolve,
                    value: win_val,
                    cancel_hashes,
                });
            } else if all_rejected {
                let reject = st.active_races[i].reject.clone();
                st.active_races[i].settled = true;
                actions.push(RaceAction::Reject {
                    reject,
                    message: "all race branches failed".into(),
                });
            }
        }
    }

    if actions.is_empty() {
        return Ok(false);
    }

    for action in actions {
        match action {
            RaceAction::Resolve {
                resolve,
                value,
                cancel_hashes,
            } => {
                {
                    let mut st = state.borrow_mut();
                    for hash in cancel_hashes {
                        st.mark_cancelled(&hash);
                    }
                }
                resolve.call::<_, ()>((value,))?;
            }
            RaceAction::Reject { reject, message } => {
                let err = make_coded_error(ctx, "race_all_failed", &message)?;
                reject.call::<_, ()>((err,))?;
            }
        }
    }
    Ok(true)
}

fn branch_settled_revision(st: &EngineState<'_>, race_id: u32, branch_idx: usize) -> i64 {
    let Some(call_indices) = st
        .race_branches
        .get(&race_id)
        .and_then(|v| v.get(branch_idx))
    else {
        return i64::MIN;
    };
    if call_indices.is_empty() {
        return i64::MIN;
    }
    call_indices
        .iter()
        .map(|&idx| match st.call_states.get(idx) {
            Some(EmittedCallState::Succeeded {
                settled_at_revision: Some(r),
            }) => *r,
            Some(EmittedCallState::Succeeded {
                settled_at_revision: None,
            }) => i64::MAX,
            _ => i64::MAX,
        })
        .max()
        .unwrap_or(i64::MIN)
}

fn finalize_completed<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    val: JsValue<'js>,
) -> StepFrame {
    if let Some(fault) = state.borrow().fatal_fault.clone() {
        return failed_with_state(state, fault);
    }
    let result_json = if val.is_undefined() {
        Value::Null
    } else {
        match js_to_json(ctx, &val, true) {
            Ok(v) => v,
            Err(e) => return failed_with_state(state, e.into_fault("workflow return value")),
        }
    };
    let canonical = match to_canonical_json(&result_json) {
        Ok(s) => s,
        Err(e) => {
            return failed_with_state(state, canonical_err_to_fault(&e, "workflow return value"));
        }
    };
    let max_result = state.borrow().limits.max_result_bytes;
    if canonical.len() > max_result {
        return failed_with_state(
            state,
            WorkflowFault::new(
                WorkflowFaultCode::ResultTooLarge,
                format!(
                    "workflow return value is {} bytes, exceeding limit of {max_result} bytes",
                    canonical.len()
                ),
            ),
        );
    }
    let st = state.borrow();
    StepFrame::Completed {
        result: result_json,
        emitted_calls: st.emitted_calls.clone(),
        cancel_identities: st.cancelled_identities.clone(),
        logs: st.logs.clone(),
    }
}

fn extract_thunks<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    args: &[JsValue<'js>],
    name: &str,
) -> rquickjs::Result<Vec<Function<'js>>> {
    if args.len() != 1 {
        fail_fatal::<()>(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                format!("{name}(thunks) expects exactly 1 array argument"),
            ),
        )?;
        unreachable!();
    }
    let Some(arr) = args[0].as_array() else {
        fail_fatal::<()>(
            ctx,
            state,
            WorkflowFault::new(
                WorkflowFaultCode::InvalidArguments,
                format!("{name}(thunks): argument must be an array of functions"),
            ),
        )?;
        unreachable!();
    };
    let mut thunks = Vec::with_capacity(arr.len());
    for i in 0..arr.len() {
        let item: JsValue<'js> = arr.get(i)?;
        let Some(func) = item.as_function().cloned() else {
            fail_fatal::<()>(
                ctx,
                state,
                WorkflowFault::new(
                    WorkflowFaultCode::InvalidArguments,
                    format!("{name}(thunks): element at index {i} must be a function"),
                ),
            )?;
            unreachable!();
        };
        thunks.push(func);
    }
    Ok(thunks)
}

fn invoke_thunk_as_promise<'js>(
    ctx: &Ctx<'js>,
    thunk: &Function<'js>,
) -> rquickjs::Result<Promise<'js>> {
    match thunk.call::<_, JsValue<'js>>(()).catch(ctx) {
        Ok(v) => {
            if let Some(p) = v.as_promise().cloned() {
                Ok(p)
            } else {
                let (p, resolve, _) = Promise::new(ctx)?;
                resolve.call::<_, ()>((v,))?;
                Ok(p)
            }
        }
        Err(caught) => {
            let (p, _, reject) = Promise::new(ctx)?;
            let err_val = caught_to_js_value(ctx, caught)?;
            reject.call::<_, ()>((err_val,))?;
            Ok(p)
        }
    }
}

fn caught_to_js_value<'js>(
    ctx: &Ctx<'js>,
    caught: CaughtError<'js>,
) -> rquickjs::Result<JsValue<'js>> {
    match caught {
        CaughtError::Exception(exc) => Ok(exc.into_value()),
        CaughtError::Value(v) => Ok(v),
        CaughtError::Error(e) => make_js_error(ctx, &e.to_string()),
    }
}

fn make_js_error<'js>(ctx: &Ctx<'js>, msg: &str) -> rquickjs::Result<JsValue<'js>> {
    // Construct via JS `Error(msg)` so `message` is defined as an own property even though
    // `Error.prototype` is frozen (avoiding `[[Set]]` against read-only `Error.prototype.message`).
    let ctor: Function<'js> = ctx.globals().get("Error")?;
    ctor.call((msg,))
}

fn make_coded_error<'js>(ctx: &Ctx<'js>, code: &str, msg: &str) -> rquickjs::Result<JsValue<'js>> {
    let err_val = make_js_error(ctx, msg)?;
    if let Some(obj) = err_val.as_object() {
        obj.set("code", code)?;
    }
    Ok(err_val)
}

fn fail_fatal<'js, T>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<EngineState<'js>>>,
    fault: WorkflowFault,
) -> rquickjs::Result<T> {
    let detail = fault.detail.clone();
    state.borrow_mut().set_fatal(fault);
    let err = make_js_error(ctx, &detail)?;
    Err(ctx.throw(err))
}

fn failed_with_state(state: &Rc<RefCell<EngineState<'_>>>, fault: WorkflowFault) -> StepFrame {
    let st = state.borrow();
    StepFrame::Failed {
        fault,
        emitted_calls: st.emitted_calls.clone(),
        logs: st.logs.clone(),
    }
}

fn build_error_frame(
    sandbox: &SandboxRuntime,
    state: &Rc<RefCell<EngineState<'_>>>,
    caught: CaughtError<'_>,
) -> StepFrame {
    if sandbox.is_timed_out() {
        return failed_with_state(
            state,
            WorkflowFault::new(
                WorkflowFaultCode::Timeout,
                "workflow step exceeded wall time limit",
            ),
        );
    }
    if let Some(fault) = state.borrow().fatal_fault.clone() {
        return failed_with_state(state, fault);
    }
    let fault = classify_caught_error(caught);
    failed_with_state(state, fault)
}

fn classify_caught_error(caught: CaughtError<'_>) -> WorkflowFault {
    match caught {
        CaughtError::Exception(exc) => {
            let name = exc
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "Error".into());
            let message = exc.message().unwrap_or_default();
            let full = if message.is_empty() {
                name.clone()
            } else {
                format!("{name}: {message}")
            };
            let lower = full.to_ascii_lowercase();
            if lower.contains("interrupted") {
                WorkflowFault::new(WorkflowFaultCode::Timeout, full)
            } else if lower.contains("out of memory") {
                WorkflowFault::new(WorkflowFaultCode::OutOfMemory, full)
            } else if lower.contains("stack overflow")
                || lower.contains("maximum call stack size exceeded")
            {
                WorkflowFault::new(WorkflowFaultCode::StackOverflow, full)
            } else if name == "SyntaxError" {
                WorkflowFault::new(WorkflowFaultCode::SyntaxError, full)
            } else {
                WorkflowFault::new(WorkflowFaultCode::RuntimeError, full)
            }
        }
        CaughtError::Value(v) => {
            // QuickJS may surface a null exception value when OOM prevents allocating Error.
            if v.is_null() {
                WorkflowFault::new(WorkflowFaultCode::OutOfMemory, "QuickJS out of memory")
            } else {
                let msg = v
                    .as_string()
                    .and_then(|s| s.to_string().ok())
                    .unwrap_or_else(|| format!("{:?}", v.type_of()));
                WorkflowFault::new(WorkflowFaultCode::RuntimeError, msg)
            }
        }
        CaughtError::Error(JsError::WouldBlock) => WorkflowFault::new(
            WorkflowFaultCode::UnresolvedPromise,
            "module evaluation promise never settled",
        ),
        CaughtError::Error(e) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("out of memory") {
                WorkflowFault::new(WorkflowFaultCode::OutOfMemory, msg)
            } else if lower.contains("stack overflow") {
                WorkflowFault::new(WorkflowFaultCode::StackOverflow, msg)
            } else {
                WorkflowFault::new(WorkflowFaultCode::RuntimeError, msg)
            }
        }
    }
}

fn schema_err_to_fault(err: &SchemaError) -> WorkflowFault {
    let code = if err.is_regex_safety() {
        WorkflowFaultCode::UnsafeRegex
    } else if matches!(
        err,
        SchemaError::InvalidNumber {
            source: steve_workflow_api::CanonicalError::NonFiniteNumber,
            ..
        }
    ) {
        WorkflowFaultCode::NonFiniteNumber
    } else if matches!(
        err,
        SchemaError::InvalidNumber {
            source: steve_workflow_api::CanonicalError::UnsafeInteger { .. },
            ..
        }
    ) {
        WorkflowFaultCode::UnsafeInteger
    } else {
        WorkflowFaultCode::InvalidSchema
    };
    WorkflowFault::new(code, err.to_string())
}

fn identity_err_to_fault(err: &IdentityError) -> WorkflowFault {
    match err {
        IdentityError::KeyTooLarge { .. } => {
            WorkflowFault::new(WorkflowFaultCode::CallIdentityTooLarge, err.to_string())
        }
        IdentityError::Canonical(c) => canonical_err_to_fault(c, "CallIdentity"),
        IdentityError::Schema(s) => schema_err_to_fault(s),
        IdentityError::IntegrityMismatch => {
            WorkflowFault::new(WorkflowFaultCode::ProtocolError, err.to_string())
        }
    }
}

fn canonical_err_to_fault(err: &steve_workflow_api::CanonicalError, ctx: &str) -> WorkflowFault {
    match err {
        steve_workflow_api::CanonicalError::NonFiniteNumber => {
            WorkflowFault::new(WorkflowFaultCode::NonFiniteNumber, format!("{ctx}: {err}"))
        }
        steve_workflow_api::CanonicalError::UnsafeInteger { .. } => {
            WorkflowFault::new(WorkflowFaultCode::UnsafeInteger, format!("{ctx}: {err}"))
        }
        steve_workflow_api::CanonicalError::MaxDepthExceeded { .. } => {
            WorkflowFault::new(WorkflowFaultCode::UnsupportedValue, format!("{ctx}: {err}"))
        }
    }
}
