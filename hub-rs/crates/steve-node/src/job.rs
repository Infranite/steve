//! Think jobs: one Steve model call, a one-shot ACP call of the chosen harness adapter
//! ([`steve_acp::oneshot`]) with exactly one model, in a scratch directory of its own. The Hub chose
//! the model and keeps the fallback; a failing model ends the attempt as failed and nothing here
//! tries another.
//!
//! The harness leads its own process group, recorded in the journal before it runs on ([`procs`]),
//! so a cancel, a deadline or the Hub's abandon after a restart stops the whole group and proves it
//! gone. The draft streams as `message_started`/`text_delta` (a new message replaces the draft), the
//! usage the harness reported comes once at the end, and the answer is the completion's text.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_acp::oneshot::{Image, OneShotError};
use steve_node_api::*;
use tokio::sync::Notify;

use crate::journal::Tx;
use crate::procs::{self, Stopped};
use crate::{Inner, emit_all, now};

/// The key a job attempt's process group is recorded under (the journal keeps groups by session).
pub(crate) fn group_key(attempt: &AttemptId) -> SessionId {
    SessionId::new(derived_id("job", attempt.as_str())).expect("derived id")
}

/// The attempt's own scratch directory under the Node's home.
fn scratch(inner: &Inner, attempt: &AttemptId) -> PathBuf {
    use sha2::Digest as _;
    let h = hex::encode(sha2::Sha256::digest(attempt.as_str().as_bytes()));
    inner.cfg.home.join("jobs").join(&h[..32])
}

/// Stops the recorded group of a job attempt (if any) and forgets it once it is gone.
pub(crate) async fn stop_group(inner: &Inner, key: &SessionId) -> Stopped {
    let p = match inner.j.read(|t| t.proc_of(key)) {
        Ok(Some(p)) => p,
        Ok(None) => return Stopped::Gone,
        Err(_) => return Stopped::Uncertain,
    };
    let s = procs::stop(p.pgid, p.started).await;
    if s.gone() && inner.j.tx(|t| t.drop_proc(key, p.pgid)).is_err() {
        return Stopped::Uncertain;
    }
    s
}

/// Why the Node stopped an attempt before the harness ended it.
enum Stop {
    /// It ends with this outcome and error (`None`: a cancel).
    End(Outcome, Option<ApiError>, &'static str),
    /// The Node shuts down: the attempt stays open and is `unknown` at the next start.
    Shutdown,
}

/// What the model has written so far, as events went out.
#[derive(Default)]
struct Draft {
    /// The number of the adapter's current message.
    current: Option<u32>,
    /// Messages started (for their ids).
    n: u32,
    text: String,
}

/// Runs the accepted think job `spec` under operation `op` to its end (or to the Node's shutdown).
pub(crate) async fn run(inner: Arc<Inner>, op: OperationId, spec: JobSpec, cancel: Arc<Notify>) {
    let exec = ExecutionRef::Job {
        job: spec.job.clone(),
        attempt: spec.run.attempt.clone(),
    };
    let key = group_key(&spec.run.attempt);
    let dir = scratch(&inner, &spec.run.attempt);
    let group: Mutex<Option<(i64, Option<u64>)>> = Mutex::new(None);
    let output = AtomicU64::new(0);
    let broken = AtomicBool::new(false);
    let draft = Mutex::new(Draft::default());
    let lim = spec.run.limits.clone();

    let spawned = |pid: u32| {
        let pid = pid as i64;
        let started = procs::start_time(pid);
        let ok = inner
            .j
            .tx(|t| t.put_proc(&key, pid, 0, inner.incarnation, "job", started))
            .is_ok();
        if ok {
            *group.lock().unwrap() = Some((pid, started));
        }
        ok
    };
    let on_message = |m: u32, text: &str| {
        let mut d = draft.lock().unwrap();
        let mut evs = vec![];
        if d.current != Some(m) || !text.starts_with(d.text.as_str()) {
            d.current = Some(m);
            d.n += 1;
            d.text.clear();
            evs.push(EventBody::MessageStarted {
                message: message_id(&exec, d.n),
                block: "b0".into(),
                role: Role::Assistant,
            });
        }
        let add = &text[d.text.len()..];
        if !add.is_empty() {
            evs.push(EventBody::TextDelta {
                message: message_id(&exec, d.n),
                block: "b0".into(),
                utf8_offset: d.text.len() as u64,
                text: add.to_string(),
            });
            output.fetch_add(add.len() as u64, Ordering::SeqCst);
            d.text = text.to_string();
        }
        let evs = evs
            .into_iter()
            .map(|b| (Subject::Execution(exec.clone()), b))
            .collect();
        if inner.j.tx(|t| emit_all(&inner, t, evs)).is_err() {
            broken.store(true, Ordering::SeqCst);
        }
        inner.wake.notify_one();
    };
    let work = async {
        let (images, message) = match prepare(&inner, &exec, &spec, &dir).await {
            Ok(x) => x,
            Err(e) => return Err(e),
        };
        Ok(acp_think(
            &inner,
            &spec,
            &dir,
            &message,
            &images,
            &spawned,
            &on_message,
        )
        .await)
    };
    let stop = async {
        loop {
            if let Some(s) = why_stop(&inner, &op, &lim, &output, &broken) {
                return s;
            }
            tokio::select! {
                _ = cancel.notified() => return Stop::End(Outcome::Cancelled, None, "cancelled"),
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
    };
    // boxed, so the run (and its process handle) can go before its directory does
    let mut work = Box::pin(work);
    let ended = tokio::select! {
        biased;
        s = stop => Err(s),
        r = &mut work => Ok(r),
    };
    // Whatever ended it, the group goes first. A run still going is signalled (TERM, then KILL)
    // and goes on being polled, so its leader is reaped as it dies; what is left of the group after
    // that is stopped too, and only a group proven gone frees the attempt.
    let recorded = *group.lock().unwrap();
    let stopped = match recorded {
        None => Stopped::Gone,
        Some((pgid, started)) => {
            let mut reaped = ended.is_ok();
            if !reaped {
                for sig in [libc::SIGTERM, libc::SIGKILL] {
                    procs::signal(pgid, started, sig);
                    if tokio::time::timeout(Duration::from_secs(3), &mut work)
                        .await
                        .is_ok()
                    {
                        reaped = true;
                        break;
                    }
                }
            }
            if reaped {
                procs::stop_reaped(pgid).await
            } else {
                procs::state(pgid, started)
            }
        }
    };
    drop(work);
    let _ = std::fs::remove_dir_all(&dir);
    let gone = stopped.gone();
    let completion = match ended {
        Err(Stop::Shutdown) => {
            if gone && let Some((pgid, _)) = recorded {
                let _ = inner.j.tx(|t| t.drop_proc(&key, pgid));
            }
            return;
        }
        _ if !gone => {
            return finish(&inner, |t| {
                unknown(&inner, t, &op, "the harness did not stop")
            })
            .await;
        }
        Err(Stop::End(outcome, error, stop_reason)) => Completion {
            outcome,
            stop_reason: stop_reason.into(),
            error,
            ..ended_with(Outcome::Cancelled, "")
        },
        Ok(Err(e)) => Completion {
            error: Some(e),
            ..ended_with(Outcome::Failed, "error")
        },
        Ok(Ok(ran)) => return end(&inner, &op, &exec, &key, recorded, ran).await,
    };
    end_with(&inner, &op, &exec, &key, recorded, completion, vec![]).await;
}

/// What the adapter ended the call with: its answer or error, how it stopped (ACP's
/// `stopReason`), and the usage it reported either way.
struct Ran {
    reply: Result<String, OneShotError>,
    stop_reason: String,
    usage: Vec<UsageReport>,
}

/// The think job as a one-shot ACP call. The prompt's stable head goes as the session's system
/// prompt where the adapter takes one; without tools the harness's own tools are off where it
/// declares how.
async fn acp_think(
    inner: &Inner,
    spec: &JobSpec,
    dir: &Path,
    message: &str,
    images: &[Image],
    spawned: &(dyn Fn(u32) -> bool + Send + Sync),
    on_message: &(dyn Fn(u32, &str) + Send + Sync),
) -> Ran {
    use crate::acp_job::OneShotError;
    use serde_json::json;
    use steve_acp::normalize::ToolFact;

    let failed = |error| Ran {
        reply: Err(error),
        stop_reason: "error".into(),
        usage: vec![],
    };
    let JobWork::Think(think) = &spec.work else {
        return failed(OneShotError::Failed("not a think job".into()));
    };
    let readable: Vec<PathBuf> = spec
        .run
        .prompt
        .iter()
        .filter_map(|part| match part {
            Part::File { name, .. } => Some(dir.join(name)),
            _ => None,
        })
        .collect();
    let allow = |tool: &ToolFact| {
        think_tool(
            tool,
            think.tools,
            dir,
            &readable,
            inner
                .cfg
                .acp
                .adapter_properties(&spec.run.choice.harness)
                .self_read_only,
        )
    };
    let mut prompt = vec![json!({"type": "text", "text": message})];
    prompt.extend(images.iter().map(Image::block));
    let properties = inner.cfg.acp.adapter_properties(&spec.run.choice.harness);
    let state_dirs = properties
        .state
        .as_ref()
        .map(|st| st.ambient_dirs(&inner.cfg.acp.user_home))
        .unwrap_or_default();
    let writable: Vec<PathBuf> = std::iter::once(dir.to_path_buf())
        .chain(state_dirs)
        .flat_map(|p| {
            let real = steve_research::guard::resolve(&p);
            if real == p { vec![p] } else { vec![p, real] }
        })
        .collect();
    if let Err(e) = std::fs::create_dir_all(dir.join("tmp")) {
        return failed(OneShotError::Io(e));
    }
    // Keep the existing Auto sandbox policy: use the adapter's own read-only sandbox where
    // declared, otherwise apply the host's read-only wrapper when the platform provides it.
    let sandbox = if !properties.self_read_only
        && properties.nested_sandbox
        && steve_research::sandbox::available()
    {
        if cfg!(target_os = "macos") {
            let Some(profile) = steve_research::sandbox::profile(&writable) else {
                return failed(OneShotError::Failed(
                    "invalid read-only sandbox paths".into(),
                ));
            };
            // and the control plane out of reach, after the profile's own allows ([`crate::confine`])
            let profile = format!(
                "{profile}{}",
                inner.confine.seatbelt_rules().unwrap_or_default()
            );
            let path = dir.join("readonly.sb");
            if let Err(e) = std::fs::write(&path, profile) {
                return failed(OneShotError::Io(e));
            }
            Some(crate::acp_job::ReadonlySandbox::Seatbelt(path))
        } else {
            Some(crate::acp_job::ReadonlySandbox::Bwrap {
                dir: dir.to_path_buf(),
                writable: writable.clone(),
            })
        }
    } else {
        None
    };
    let state = Mutex::new((None::<String>, 0u32, String::new()));
    let usage = Mutex::new(vec![]);
    let event = |ev: EventBody| {
        let mut state = state.lock().unwrap();
        match ev {
            EventBody::MessageStarted {
                message,
                role: Role::Assistant,
                ..
            } => {
                state.0 = Some(message);
                state.1 += 1;
                state.2.clear();
            }
            EventBody::TextDelta { message, text, .. }
                if state.0.as_deref() == Some(message.as_str()) =>
            {
                state.2.push_str(&text);
                on_message(state.1, &state.2);
            }
            EventBody::Usage { report } => usage.lock().unwrap().push(report),
            _ => {}
        }
    };
    let wrap = crate::acp_job::wrap(
        sandbox.as_ref(),
        properties.nested_sandbox.then_some(&inner.confine),
        Some(dir),
        &inner.cfg.acp.user_home,
        &properties.outer_sandbox_args,
    );
    let req = crate::acp_job::Request {
        cfg: &inner.cfg.acp,
        harness: &spec.run.choice.harness,
        cwd: dir,
        model: spec.run.choice.model.as_deref(),
        effort: spec.run.choice.effort.as_deref(),
        resume: None,
        head: think.instructions.as_deref(),
        tools_off: think.tools == ThinkToolMode::None,
        mode: PermissionMode::Plan,
        prompt,
        mcp_servers: json!([]),
        wrap: Some(&wrap),
        timeout: Duration::from_millis((spec.run.limits.deadline.0 - now()).max(0) as u64),
        prefix: spec.run.attempt.as_str(),
        on_session: None,
        max_output_bytes: spec.run.limits.max_output_bytes,
        on_activity: None,
        allow_tool: &allow,
    };
    match crate::acp_job::run(req, spawned, &event).await {
        Ok(out) => Ran {
            reply: Ok(out.text),
            stop_reason: out.stop_reason,
            usage: out.usage,
        },
        Err(e) => Ran {
            reply: Err(e),
            stop_reason: "error".into(),
            usage: usage.lock().unwrap().clone(),
        },
    }
}

fn think_tool(
    tool: &steve_acp::normalize::ToolFact,
    mode: ThinkToolMode,
    dir: &Path,
    files: &[PathBuf],
    self_read_only: bool,
) -> bool {
    // Adapters report internal work such as context compaction as a tool. Only an
    // action-free harness Think fact is inert; its display name grants no permission.
    if tool.origin == ToolOrigin::Harness && tool.category == ToolCategory::Think {
        return tool.command.is_none()
            && tool.url.is_none()
            && tool.paths.is_empty()
            && tool.cwd.is_none();
    }
    mode == ThinkToolMode::ReadAttached && attached_tool(tool, dir, files, self_read_only)
}

fn attached_path(path: &str, dir: &Path, files: &[PathBuf]) -> bool {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    files
        .iter()
        .any(|p| p.canonicalize().ok().as_ref() == Some(&path))
}

fn attached_tool(
    tool: &steve_acp::normalize::ToolFact,
    dir: &Path,
    files: &[PathBuf],
    self_read_only: bool,
) -> bool {
    if tool.origin != ToolOrigin::Harness || tool.url.is_some() {
        return false;
    }
    if !tool
        .paths
        .iter()
        .all(|path| attached_path(path, dir, files))
        || tool.cwd.as_deref().is_some_and(|cwd| {
            let cwd = if Path::new(cwd).is_absolute() {
                PathBuf::from(cwd)
            } else {
                dir.join(cwd)
            };
            cwd.canonicalize().ok() != dir.canonicalize().ok()
        })
    {
        return false;
    }
    if matches!(tool.category, ToolCategory::Read | ToolCategory::Search) {
        return !tool.paths.is_empty() && tool.command.is_none() && tool.url.is_none();
    }
    if tool.category != ToolCategory::Exec || !self_read_only {
        return false;
    }
    // Preserve the existing read-only CLI contract for attachment parsers (sed, Python, PDF
    // tools, etc.). The adapter's declared OS sandbox enforces no writes; research's shared
    // guard additionally rejects reported out-of-scope/secret paths. A program-name allowlist
    // would break document reading when moving the same call from the CLI to ACP.
    let scope = steve_research::Scope::new(dir, &[], Path::new("/"));
    crate::research::acp::guard(tool, &scope, false, true).is_ok()
}

/// The id of the attempt's `n`th message.
fn message_id(exec: &ExecutionRef, n: u32) -> String {
    format!("{}.m{n}", exec.attempt())
}

/// The prompt as the harness gets it: its files placed in the scratch directory at the names the
/// prompt uses, its images as bytes, its text as the message.
async fn prepare(
    inner: &Arc<Inner>,
    exec: &ExecutionRef,
    spec: &JobSpec,
    dir: &Path,
) -> ApiResult<(Vec<Image>, String)> {
    let io = |e: std::io::Error| ApiError::new(ErrorCode::Internal, e.to_string());
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut text = vec![];
    let mut images = vec![];
    for p in &spec.run.prompt {
        match p {
            Part::Text { text: t } => text.push(t.as_str()),
            Part::Image { artifact } => {
                let path = crate::fetch(inner, exec, artifact).await?;
                images.push(Image {
                    mime: artifact.mime.clone(),
                    bytes: std::fs::read(path).map_err(io)?,
                });
            }
            Part::File { artifact, name } => {
                // the name was checked when the job was taken: `.steve/attachments/<file>`
                let path = crate::fetch(inner, exec, artifact).await?;
                let to = dir.join(name);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                std::fs::copy(path, &to).map_err(io)?;
            }
            Part::Diff { .. } => {}
        }
    }
    Ok((images, text.join("\n\n")))
}

/// The Node stops the attempt: a cancel, its deadline, the Hub gone past the lease, its output
/// limit, a full or failing journal, or the Node shutting down.
fn why_stop(
    inner: &Inner,
    op: &OperationId,
    lim: &Limits,
    output: &AtomicU64,
    broken: &AtomicBool,
) -> Option<Stop> {
    let now = now();
    let hub_down = inner.hub_down_since().is_some_and(|since| since < now);
    let exhausted = |why: NodeStop| Some(Stop::End(Outcome::Cancelled, Some(why.error()), "error"));
    if inner.closed.load(Ordering::SeqCst) {
        Some(Stop::Shutdown)
    } else if inner
        .j
        .read(|t| t.op(op))
        .ok()
        .flatten()
        .is_some_and(|o| o.cancel)
    {
        Some(Stop::End(Outcome::Cancelled, None, "cancelled"))
    } else if now >= lim.deadline.0 {
        Some(Stop::End(
            Outcome::TimedOut,
            Some(NodeStop::Deadline.error()),
            "timeout",
        ))
    } else if hub_down && now >= lim.offline_until.0 {
        Some(Stop::End(
            Outcome::TimedOut,
            Some(NodeStop::HubUnreachable.error()),
            "timeout",
        ))
    } else if output.load(Ordering::SeqCst) > lim.max_output_bytes {
        exhausted(NodeStop::OutputLimit)
    } else if broken.load(Ordering::SeqCst)
        || inner.j.read(|t| inner.over_limit(t)).unwrap_or(false)
    {
        exhausted(NodeStop::JournalBudget)
    } else if inner.faulted() {
        Some(Stop::End(
            Outcome::Failed,
            Some(NodeStop::JournalFails.error()),
            "error",
        ))
    } else {
        None
    }
}

fn ended_with(outcome: Outcome, stop_reason: &str) -> Completion {
    Completion {
        outcome,
        stop_reason: stop_reason.into(),
        error: None,
        result: vec![],
        continuation: None,
        transient: false,
        acted: false,
        job: None,
    }
}

/// The adapter's own end. A turn it ended (`end_turn`) succeeds with its answer; any other
/// `stopReason` it gave (`max_tokens`, `refusal`, `cancelled`) fails with that reason. A call that
/// failed is `timeout` or `error`, its `error.safe_detail` what the adapter said and `transient`
/// whether another try may succeed, so the Hub can put it in its own words.
async fn end(
    inner: &Inner,
    op: &OperationId,
    exec: &ExecutionRef,
    key: &SessionId,
    group: Option<(i64, Option<u64>)>,
    ran: Ran,
) {
    let completion = match ran.reply {
        Ok(text) if ran.stop_reason == "end_turn" => Completion {
            result: vec![Part::Text { text }],
            ..ended_with(Outcome::Succeeded, "end_turn")
        },
        Ok(_) => Completion {
            error: Some(ApiError::new(
                ErrorCode::Internal,
                format!("the harness stopped: {}", ran.stop_reason),
            )),
            ..ended_with(Outcome::Failed, &ran.stop_reason)
        },
        Err(e) => {
            let transient = e.transient();
            let (outcome, code, kind, detail) = match e {
                OneShotError::OutputLimit => (
                    Outcome::Cancelled,
                    ErrorCode::Exhausted,
                    "error",
                    NodeStop::OutputLimit.detail(),
                ),
                OneShotError::Timeout(_) => (
                    Outcome::TimedOut,
                    ErrorCode::Expired,
                    "timeout",
                    NodeStop::Deadline.detail(),
                ),
                e @ OneShotError::Unsupported(_) => (
                    Outcome::Failed,
                    ErrorCode::Unsupported,
                    "error",
                    e.to_string(),
                ),
                e => (Outcome::Failed, ErrorCode::Internal, "error", e.to_string()),
            };
            Completion {
                outcome,
                error: Some(ApiError::new(code, detail)),
                transient,
                ..ended_with(outcome, kind)
            }
        }
    };
    let usage = ran
        .usage
        .into_iter()
        .map(|report| EventBody::Usage { report })
        .collect();
    end_with(inner, op, exec, key, group, completion, usage).await;
}

/// The attempt's last events, its final state and `attempt_ended`, and its group forgotten, in one
/// transaction.
async fn end_with(
    inner: &Inner,
    op: &OperationId,
    exec: &ExecutionRef,
    key: &SessionId,
    group: Option<(i64, Option<u64>)>,
    completion: Completion,
    before: Vec<EventBody>,
) {
    finish(inner, |t| {
        if let Some((pgid, _)) = group {
            t.drop_proc(key, pgid)?;
        }
        let state = Observed::Finished {
            outcome: completion.outcome,
            result: Some(OperationResult::Execution(completion.clone())),
            error: completion.error.clone(),
        };
        t.set_state(op, &state)?;
        let mut evs: Vec<_> = before
            .iter()
            .cloned()
            .map(|b| (Subject::Execution(exec.clone()), b))
            .collect();
        evs.push((
            Subject::Execution(exec.clone()),
            EventBody::AttemptEnded {
                completion: completion.clone(),
            },
        ));
        emit_all(inner, t, evs)
    })
    .await;
}

/// The attempt may still run (its group did not stop): it stays unknown, its group recorded, until
/// the Hub abandons it.
fn unknown(inner: &Inner, t: &Tx, op: &OperationId, why: &str) -> ApiResult<()> {
    let state = Observed::Unknown { reason: why.into() };
    t.set_state(op, &state)?;
    emit_all(
        inner,
        t,
        vec![(
            Subject::Operation {
                operation: op.clone(),
            },
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: op.clone(),
                    state,
                },
            },
        )],
    )
}

/// Writes the end, again until the journal takes it (the end of an attempt is always written).
async fn finish(inner: &Inner, f: impl Fn(&Tx) -> ApiResult<()>) {
    loop {
        let r = inner.j.tx(&f);
        inner.wake.notify_one();
        match r {
            Ok(()) => return,
            Err(e) => {
                tracing::error!("job end: {e}");
                inner.fault();
                if inner.closed.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    use serde_json::json;
    use steve_acp::normalize::{Normalizer, ToolFact};

    fn tool(command: &str) -> ToolFact {
        let mut norm = Normalizer::new("/", "test", Epoch::new("test").unwrap());
        norm.update(&json!({
            "sessionUpdate":"tool_call", "toolCallId":"t", "kind":"execute",
            "rawInput":{"command":command}
        }));
        norm.fact("t").unwrap().clone()
    }

    fn internal_thought() -> ToolFact {
        let mut norm = Normalizer::new("/", "test", Epoch::new("test").unwrap());
        norm.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "t", "kind": "think",
            "name": "arbitrary_internal_step", "status": "in_progress"
        }));
        norm.fact("t").unwrap().clone()
    }

    #[test]
    fn internal_thinking_is_allowed_without_attachment_permissions() {
        let fact = internal_thought();
        for mode in [ThinkToolMode::None, ThinkToolMode::ReadAttached] {
            for self_read_only in [false, true] {
                assert!(think_tool(&fact, mode, Path::new("/"), &[], self_read_only));
            }
        }
    }

    #[test]
    fn internal_thinking_cannot_carry_actions_or_an_external_origin() {
        let plain = internal_thought();
        let mut command = plain.clone();
        command.command = tool("cat input.txt").command;
        let mut path = plain.clone();
        path.paths.push("input.txt".into());
        let mut cwd = plain.clone();
        cwd.cwd = Some(".".into());
        let mut url = plain.clone();
        url.url = Some("https://example.invalid".into());
        let mut external = plain.clone();
        external.origin = ToolOrigin::External;
        let mut steve = plain;
        steve.origin = ToolOrigin::Steve;
        for fact in [command, path, cwd, url, external, steve] {
            for mode in [ThinkToolMode::None, ThinkToolMode::ReadAttached] {
                assert!(!think_tool(&fact, mode, Path::new("/"), &[], true));
            }
        }
        assert!(!think_tool(
            &tool("cat input.txt"),
            ThinkToolMode::None,
            Path::new("/"),
            &[],
            true
        ));
    }

    #[test]
    fn sandboxed_attachment_parsers_keep_readonly_scope() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("attached file.txt");
        std::fs::write(&file, "data").unwrap();
        let files = [file];
        assert!(attached_tool(
            &tool("cat 'attached file.txt'"),
            temp.path(),
            &files,
            true
        ));
        assert!(attached_tool(
            &tool("/bin/zsh -lc \"head -n 20 'attached file.txt'\""),
            temp.path(),
            &files,
            true
        ));
        assert!(!attached_tool(
            &tool("cat 'attached file.txt'"),
            temp.path(),
            &files,
            false
        ));
        for command in [
            "sed -n '1,80p' 'attached file.txt'",
            "python3 -c \"from pathlib import Path; print(Path('attached file.txt').read_text())\"",
        ] {
            assert!(
                attached_tool(&tool(command), temp.path(), &files, true),
                "{command}"
            );
        }
        for command in [
            "cat /etc/hosts",
            "cat ~/.ssh/id_rsa",
            "cat ../../outside.txt",
        ] {
            assert!(
                !attached_tool(&tool(command), temp.path(), &files, true),
                "{command}"
            );
        }
    }

    #[test]
    fn an_external_tool_cannot_claim_attachment_read_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("input.txt");
        std::fs::write(&file, "data").unwrap();
        let mut fact = tool("cat input.txt");
        fact.category = ToolCategory::Read;
        fact.command = None;
        fact.paths = vec!["input.txt".into()];
        fact.origin = ToolOrigin::External;
        assert!(!attached_tool(&fact, temp.path(), &[file], true));
    }
}
