//! Read-only item diagnostics shared by REST, HTTP MCP and Node tools.
//! Audience checks happen before reading evidence; a title-only audience never receives logs.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_core::Core;
use steve_store::diagnostics::{Details, Record, Severity};
use steve_store::{Session, Store, ThreadRun};
use tracing::Instrument;

use super::research_mcp::ItemScope;

pub const TOOL: &str = "steve_debug";

tokio::task_local! {
    static OBSERVING_TOOL: ();
    static EXECUTION: Execution;
}

#[derive(Clone)]
struct Execution {
    attempt: String,
    operation: String,
    call: String,
}

/// Correlation only: permissions always come from the Node's persisted execution checks.
pub async fn in_execution<F: std::future::Future>(
    execution: &steve_node_api::ExecutionRef,
    operation: &str,
    call: &str,
    future: F,
) -> F::Output {
    EXECUTION
        .scope(
            Execution {
                attempt: execution.attempt().as_str().to_string(),
                operation: operation.into(),
                call: call.into(),
            },
            future,
        )
        .await
}

/// A shared invocation boundary, independent of transport. Never copies arguments or results.
pub struct ToolObservation {
    store: Store,
    session: Session,
    name: String,
    nested: bool,
    execution: Option<Execution>,
}

impl ToolObservation {
    pub fn new(store: &Store, session: &Session, name: &str) -> Self {
        Self {
            store: store.clone(),
            session: session.clone(),
            name: name.into(),
            nested: name == TOOL || OBSERVING_TOOL.try_with(|_| true).unwrap_or(false),
            execution: EXECUTION.try_with(Clone::clone).ok(),
        }
    }

    pub async fn run<T, E>(
        self,
        future: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        if self.nested {
            return future.await;
        }
        let span = tracing::info_span!("tool.call",
            tool = %self.name, workspace_id = %self.session.workspace_id,
            item_id = self.session.item_id.as_deref(), session_id = %self.session.id,
            attempt_id = self.execution.as_ref().map(|e| e.attempt.as_str()),
            operation_id = self.execution.as_ref().map(|e| e.operation.as_str()),
            call_id = self.execution.as_ref().map(|e| e.call.as_str()));
        let mut observation = Observation {
            store: self.store,
            started: std::time::Instant::now(),
            record: Some(Record {
                workspace_id: self.session.workspace_id,
                item_id: self.session.item_id,
                session_id: Some(self.session.id),
                attempt_id: self.execution.as_ref().map(|e| e.attempt.clone()),
                operation_id: self.execution.as_ref().map(|e| e.operation.clone()),
                cause_id: self.execution.map(|e| e.call),
                kind: "tool.call".into(),
                details: Details {
                    rpc_method: Some(self.name),
                    ..Default::default()
                },
                ..Default::default()
            }),
        };
        let result = OBSERVING_TOOL.scope((), future.instrument(span)).await;
        if result.is_err() {
            observation.finish("tool_failed", Severity::Error, false, "failed");
        } else {
            observation.finish("tool_completed", Severity::Info, true, "completed");
        }
        result
    }
}

/// Dropping an observer never cancels or retries the wrapped operation. It only says that this
/// boundary did not observe a result; the operation's real outcome still belongs to its ledger.
struct Observation {
    store: Store,
    started: std::time::Instant,
    record: Option<Record>,
}

impl Observation {
    fn finish(&mut self, reason: &str, severity: Severity, detail: bool, state: &str) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        record.reason_code = reason.into();
        record.severity = severity;
        record.detail = detail;
        record.details.duration_ms =
            Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        record.details.to_state = Some(state.into());
        let _ = self.store.diagnostics().record(record);
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        self.finish(
            "observation_interrupted",
            Severity::Warning,
            false,
            "unobserved",
        );
    }
}

pub fn tool() -> Value {
    json!({
        "name": TOOL,
        "description": "Read an item's diagnostic snapshot or a bounded page of recorded events. Explain reports current blockers, evidence coverage and capture status; events follows returned cursors and stable event IDs. This tool never enables capture, retries work, answers decisions or changes task state. Access follows the current session and conversation audience; title-only access does not grant event access. Missing, expired or truncated evidence is not proof that a failure never happened.",
        "annotations": {"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false},
        "inputSchema": {
            "type":"object", "additionalProperties":false,
            "required":["action","item_id"],
            "properties": {
                "action":{"type":"string","enum":["explain","events"]},
                "item_id":{"type":"string","minLength":1,"maxLength":256},
                "cursor":{"type":"string","maxLength":2048},
                "limit":{"type":"integer","minimum":1,"maximum":200},
                "kinds":{"type":"array","maxItems":20,"items":{"type":"string","minLength":1,"maxLength":128}},
                "severity":{"type":"string","enum":["info","warning","error"]},
                "since":{"type":"integer","minimum":0,"description":"Inclusive Unix time in milliseconds."},
                "until":{"type":"integer","minimum":0,"description":"Inclusive Unix time in milliseconds."},
                "session_id":{"type":"string","maxLength":256},
                "attempt_id":{"type":"string","maxLength":256},
                "event_id":{"type":"string","maxLength":256}
            }
        }
    })
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Explain,
    Events,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    pub action: Action,
    pub item_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub severity: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub session_id: Option<String>,
    pub attempt_id: Option<String>,
    pub event_id: Option<String>,
}

impl Args {
    pub fn parse(value: Value) -> Result<Self, String> {
        let mut args: Self = serde_json::from_value(value)
            .map_err(|_| "Invalid diagnostic query arguments.".to_string())?;
        if args.item_id.trim().is_empty()
            || args.item_id.len() > 256
            || args.limit.is_some_and(|n| n == 0 || n > 200)
            || args.cursor.as_ref().is_some_and(|s| s.len() > 2048)
            || args.since.is_some_and(|n| n < 0)
            || args.until.is_some_and(|n| n < 0)
            || matches!((args.since, args.until), (Some(a), Some(b)) if a > b)
            || args.kinds.len() > 20
            || args
                .severity
                .as_deref()
                .is_some_and(|s| !matches!(s, "info" | "warning" | "error"))
            || args
                .kinds
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 128)
            || [&args.session_id, &args.attempt_id, &args.event_id]
                .into_iter()
                .flatten()
                .any(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err("Invalid diagnostic query arguments.".into());
        }
        args.item_id = args.item_id.trim().to_string();
        args.kinds.sort();
        args.kinds.dedup();
        if args.action == Action::Explain
            && (args.cursor.is_some()
                || args.limit.is_some()
                || !args.kinds.is_empty()
                || args.severity.is_some()
                || args.since.is_some()
                || args.until.is_some()
                || args.session_id.is_some()
                || args.attempt_id.is_some()
                || args.event_id.is_some())
        {
            return Err("Explain accepts only action and item_id.".into());
        }
        Ok(args)
    }

    fn filter_key(&self, ws: &str) -> String {
        steve_store::sha(
            &json!({
                "workspace":ws,"item":self.item_id,"kinds":self.kinds,"severity":self.severity,
                "since":self.since,"until":self.until,"session":self.session_id,
                "attempt":self.attempt_id,"event":self.event_id
            })
            .to_string(),
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    after: i64,
    through: i64,
    filter: String,
}

fn unavailable(error: impl std::fmt::Display) -> String {
    tracing::warn!("diagnostic query: {error}");
    "Diagnostic evidence is temporarily unavailable.".into()
}

/// Caller must already have full item access. A cursor is a position, never authority.
pub fn events(store: &Store, ws: &str, args: &Args) -> Result<Value, String> {
    let filter = args.filter_key(ws);
    let (after, through) = match &args.cursor {
        Some(raw) => {
            let cursor: Cursor = URL_SAFE_NO_PAD
                .decode(raw)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or("Invalid diagnostic cursor.")?;
            if cursor.filter != filter || cursor.after < 0 || cursor.through < cursor.after {
                return Err("Diagnostic cursor does not match this query.".into());
            }
            (Some(cursor.after), Some(cursor.through))
        }
        None => (None, None),
    };
    let mut page = store
        .diagnostics()
        .events(
            ws,
            &args.item_id,
            steve_store::diagnostics::Filter {
                after,
                through,
                limit: args.limit.unwrap_or(100),
                kinds: args.kinds.clone(),
                severity: args.severity.clone(),
                since: args.since,
                until: args.until,
                session_id: args.session_id.clone(),
                attempt_id: args.attempt_id.clone(),
                event_id: args.event_id.clone(),
                ..Default::default()
            },
        )
        .map_err(unavailable)?;
    let through = page["snapshot_cursor"]
        .as_i64()
        .ok_or("Diagnostic snapshot has no cursor.")?;
    page["item_id"] = json!(args.item_id);
    page["access"] = json!("full");
    page["next_cursor"] = match page["next_cursor"].as_i64() {
        Some(after) if page["has_more"] == true => json!(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    after,
                    through,
                    filter
                })
                .map_err(unavailable)?
            )
        ),
        _ => Value::Null,
    };
    Ok(page)
}

pub fn explain(
    store: &Store,
    ws: &str,
    item: &str,
    state: Value,
    can_capture: bool,
) -> Result<Value, String> {
    let mut view = store.diagnostics().explain(ws, item).map_err(unavailable)?;
    view["state"] = state;
    view["can_capture"] = json!(can_capture);
    view["access"] = json!("full");
    Ok(view)
}

/// The same safe projection used by the item tools: no current blockers, logs or metadata.
fn limited(
    store: &Store,
    ws: &str,
    scope: &ItemScope,
    args: &Args,
) -> Result<Option<Value>, String> {
    let item = store
        .items()
        .get(&args.item_id)
        .map_err(unavailable)?
        .filter(|i| i.workspace_id == ws && scope.sees(&i.id))
        .ok_or("Item is not available to this caller.")?;
    if matches!(scope, ItemScope::All) {
        return Ok(None);
    }
    if args.action == Action::Events {
        return Err("This conversation may read the item's summary, not diagnostic events.".into());
    }
    Ok(Some(json!({
        "observed_at":steve_store::now(),"access":"summary","can_capture":false,
        "item":{"id":item.id,"title":item.title,"status":item.status,
            "goal":store.items().goal(&item.id).map_err(unavailable)?},
        "coverage":{"restricted":true,"reason":"conversation_summary_only"}
    })))
}

fn query(
    store: &Store,
    core: &Core,
    ws: &str,
    scope: &ItemScope,
    args: Args,
) -> Result<String, String> {
    if let Some(view) = limited(store, ws, scope, &args)? {
        return Ok(view.to_string());
    }
    match args.action {
        Action::Explain => explain(
            store,
            ws,
            &args.item_id,
            core.diagnostic_snapshot(&args.item_id)
                .map_err(unavailable)?,
            false,
        ),
        Action::Events => events(store, ws, &args),
    }
    .map(|v| v.to_string())
}

pub fn for_session(
    store: &Store,
    core: &Core,
    session: &Session,
    args: Value,
) -> Result<String, String> {
    let args = Args::parse(args)?;
    let (session, scope) = super::item_mcp::scope(store, session)?;
    query(store, core, &session.workspace_id, &scope, args)
}

pub fn for_research(
    store: &Store,
    core: &Core,
    run: &ThreadRun,
    args: Value,
) -> Result<String, String> {
    let args = Args::parse(args)?;
    let (current, scope) = research_scope(store, run)?;
    query(store, core, &current.workspace_id, &scope, args)
}

fn research_scope(store: &Store, run: &ThreadRun) -> Result<(ThreadRun, ItemScope), String> {
    let current = store
        .thread()
        .run(&run.id)
        .map_err(unavailable)?
        .filter(|r| {
            r.status == "running"
                && r.workspace_id == run.workspace_id
                && r.message_id == run.message_id
        })
        .ok_or("Research run is no longer active.")?;
    let origin = store
        .messages()
        .get(&current.message_id)
        .map_err(unavailable)?
        .filter(|m| m.workspace_id == current.workspace_id)
        .ok_or("Research origin is not available.")?;
    let scope = ItemScope::of(store, &origin.id)?;
    Ok((current, scope))
}

/// Node idempotent results are still disclosures. Recheck their audience on every replay.
pub(super) fn authorize_cached(
    store: &Store,
    caller: &super::node_tools::Caller,
    result: &steve_node_api::ToolResult,
) -> Result<(), String> {
    use steve_node_api::{Part, ToolResult};
    let ToolResult::Completed { parts, .. } = result else {
        return Ok(());
    };
    let Some(Part::Text { text }) = parts.first() else {
        return Err("Invalid diagnostic receipt.".into());
    };
    let value: Value = serde_json::from_str(text).map_err(|_| "Invalid diagnostic receipt.")?;
    let item = value["item_id"]
        .as_str()
        .or_else(|| value["item"]["id"].as_str())
        .ok_or("Diagnostic receipt has no target.")?;
    let (ws, scope) = match caller {
        super::node_tools::Caller::Session(session)
        | super::node_tools::Caller::DependencyManagement(session) => {
            let (s, scope) = super::item_mcp::scope(store, session)?;
            (s.workspace_id, scope)
        }
        super::node_tools::Caller::Research(run) => {
            let (run, scope) = research_scope(store, run)?;
            (run.workspace_id, scope)
        }
    };
    if !scope.sees(item)
        || (value["access"] != "summary" && !matches!(scope, ItemScope::All))
        || store
            .items()
            .get(item)
            .map_err(unavailable)?
            .is_none_or(|i| i.workspace_id != ws)
    {
        return Err("Diagnostic receipt is no longer available to this audience.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[tokio::test]
    async fn observation_preserves_results_partial_effects_and_cancellation_without_retry() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(
            home.path().join("diagnostics.sqlite3"),
            steve_store::NoPublisher,
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        store.with_conn(|c| {
            c.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('i1','default',1,'Test','run','执行',1,1);
                INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES('s1','default','i1','fixture','','active',1,1);")?;
            Ok(())
        }).unwrap();
        let session = store.sessions().get("s1").unwrap().unwrap();
        store
            .diagnostics()
            .capture("default", "i1", "owner", Some(1))
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let result = ToolObservation::new(&store, &session, "fixture_success")
            .run(async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>(42)
            })
            .await;
        assert_eq!(result, Ok(42));
        let result = ToolObservation::new(&store, &session, "fixture_error")
            .run(async {
                calls.fetch_add(1, Ordering::SeqCst);
                store
                    .with_conn(|c| {
                        c.execute("UPDATE items SET now='effect committed' WHERE id='i1'", [])?;
                        Ok(())
                    })
                    .unwrap();
                Err::<(), _>("sensitive error text")
            })
            .await;
        assert_eq!(result, Err("sensitive error text"));
        assert_eq!(
            store.items().get("i1").unwrap().unwrap().now,
            "effect committed"
        );

        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let interrupted = ToolObservation::new(&store, &session, "fixture_cancelled");
        let (invocations, disposed, ready) = (calls.clone(), dropped.clone(), started.clone());
        let task = tokio::spawn(interrupted.run(async move {
            let _lifetime = Dropped(disposed);
            invocations.fetch_add(1, Ordering::SeqCst);
            ready.notify_one();
            std::future::pending::<Result<(), &str>>().await
        }));
        started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);

        let mut drained = false;
        for _ in 0..100 {
            if store.diagnostics().explain("default", "i1").unwrap()["coverage"]["instance_pending"]
                == 0
            {
                drained = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(drained);
        let page = store
            .diagnostics()
            .events("default", "i1", steve_store::diagnostics::Filter::default())
            .unwrap();
        let records = page["events"].as_array().unwrap();
        assert_eq!(records.len(), 3, "each invocation produces one observation");
        assert_eq!(records[0]["reason_code"], "tool_completed");
        assert_eq!(records[1]["reason_code"], "tool_failed");
        assert_eq!(records[2]["reason_code"], "observation_interrupted");
        assert_eq!(records[2]["details"]["to_state"], "unobserved");
        assert!(!page.to_string().contains("sensitive error text"));
        // Expiry can leave no retained event. The Hub must still preserve the Store's gap,
        // not replace its snapshot boundary with an empty retained-event maximum.
        store
            .with_conn(|c| {
                c.execute("UPDATE diagnostic_events SET at=1 WHERE item_id='i1'", [])?;
                Ok(())
            })
            .unwrap();
        let args = Args::parse(json!({"action":"events","item_id":"i1"})).unwrap();
        let expired = events(&store, "default", &args).unwrap();
        assert!(expired["events"].as_array().unwrap().is_empty());
        assert_eq!(expired["truncated"], true);
        assert!(!expired["gaps"].as_array().unwrap().is_empty());
    }
}
