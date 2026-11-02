//! Bot use of secrets (accounts-vault.md §4.3–§4.5): `steve_secret_use` asks, the user approves (or a
//! grant covers it), Steve runs the command with the values injected and gives back the redacted
//! output.
//!
//! - Every use is a decision `{kind:"approve", subject_type:"secret_use"}` unless an open grant of this
//!   bot on this item covers the session's scope (its project, or `{"scratch":true}`). `high` items and
//!   bank cards are never covered: their options are only `once` / `deny`; others also offer
//!   `grant_1h|8h|24h`.
//! - Covered: the command runs now and the tool returns `Exit code <n>.\n<output>`.
//! - Asked: the tool returns `Approval requested. ` + END (the integrator marks pending=approval like
//!   `steve_request_approval`). The answer reaches [`UseDecisions`]; it records the choice (and the
//!   grant) in the dispatch transaction and queues a job. The job runs after that transaction commits
//!   (it waits for the store's write lock and re-reads the row), runs the command and sends the result
//!   to the item's session through [`SessionSink`] (steve-core's `send`).
//! - The request itself (command, directory, injections) is held in memory between asking and
//!   answering: no table has columns for it and the schema is frozen. On restart open requests are
//!   expired ([`SecretUses::start`]) and their sessions told so.
//! - After a run, the values are registered with the session redactor for an hour
//!   (`steve_vault::redact::register`).
//!
//! Nothing here holds a value except [`SecretUses::execute`], between decrypting and the end of the
//! run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_store::{ChangeEvent, Decision, NewDecision, Tx, base, now, uid};
use tokio::sync::mpsc;

use super::grants::{self, add_grant};
use super::inject::{
    self, DEFAULT_TIMEOUT_S, How, Injection, MAX_TIMEOUT_S, RunOutput, RunSpec, resolve_cwd,
};
use super::items::repo::{self as irepo, ItemRow};
use super::items::templates::BANK_CARD;
use super::use_repo::{self as urepo, UseRecord};
use super::{Vault, VaultError, VaultResult};
use crate::decisions::{DecisionCx, DecisionHandler, choice_result};
use crate::{ApiError, ApiResult};

/// The decisions' `subject_type`.
pub const SUBJECT: &str = "secret_use";
/// The tool result when the item doesn't exist or isn't visible (§6), verbatim.
pub const NO_SUCH: &str = "No such credential. Call steve_secret_list.";
/// Same as steve-mcp's `END`; agents rely on this wording.
pub const END: &str = "End your turn now. The user's answer will arrive as your next message.";
/// Characters of output in the message to the session (§4.3).
pub const MESSAGE_OUTPUT: usize = 8000;
pub const OPTIONS: [&str; 5] = ["once", "grant_1h", "grant_8h", "grant_24h", "deny"];
pub const OPTIONS_HIGH: [&str; 2] = ["once", "deny"];

const DAY_MS: i64 = 86_400_000;
/// Unanswered requests expire after a day (§9 `vault.cleanup`).
const PENDING_MS: i64 = DAY_MS;
/// Use rows are kept for a year (§4.5).
const RETENTION_MS: i64 = 365 * DAY_MS;
/// Temp directories older than this are removed by the hourly job.
const SCRATCH_AGE: Duration = Duration::from_secs(15 * 60);
const PURPOSE_MAX: usize = 500;
const COMMAND_MAX: usize = 8000;

/// steve-core's “send to the item's active session” (清单 §6.5 `send`): prompt the session if it is not
/// ended, otherwise start a new session on the item with this text. The integrator implements it.
#[async_trait]
pub trait SessionSink: Send + Sync {
    async fn send(&self, ws: &str, item_id: Option<&str>, session_id: Option<&str>, text: &str);
}

/// The message after a denial (§4.3), verbatim.
pub fn denied_message(title: &str) -> String {
    format!(
        "用户没有批准使用「{title}」。不要再尝试获取这个机密，换一种不需要它的做法，或者用 steve_ask 问用户。"
    )
}

/// The message after an approved run (§4.3), verbatim; the output is cut to [`MESSAGE_OUTPUT`].
pub fn approved_message(title: &str, out: &RunOutput) -> String {
    let all = out.combined();
    let cut: String = all.chars().take(MESSAGE_OUTPUT).collect();
    format!(
        "用户批准了使用「{title}」。命令已执行，退出码 {}。输出（机密已打码）：\n{cut}",
        out.code()
    )
}

/// What a bot asks for (the `steve_secret_use` arguments). Holds field labels, never values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UseRequest {
    /// Item id or exact title.
    pub item: String,
    pub purpose: String,
    pub command: String,
    /// Relative to the session's working directory.
    pub cwd: Option<String>,
    /// `VAR → field label`.
    pub env: Vec<(String, String)>,
    /// `VAR → field label`; VAR receives a temp file path.
    pub files: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub timeout_s: Option<f64>,
}

/// The session a request comes from (resolved by steve-mcp from the bearer token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseCx {
    pub ws: String,
    /// The user the agent works for: personal vault items are theirs only.
    pub user: String,
    pub bot_id: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
}

impl From<&crate::contacts::mcp::McpCx> for UseCx {
    fn from(c: &crate::contacts::mcp::McpCx) -> Self {
        Self {
            ws: c.ws.clone(),
            user: c.user.clone(),
            bot_id: c.bot_id.clone(),
            item_id: c.item_id.clone(),
            session_id: c.session_id.clone(),
        }
    }
}

/// What became of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseOutcome {
    /// A grant covered it; this is the (redacted) result.
    Ran { use_id: String, out: RunOutput },
    /// A decision is open.
    Asked { use_id: String, decision_id: String },
}

/// A request between asking and the answer.
#[derive(Debug, Clone)]
struct Pending {
    ws: String,
    item_id: String,
    title: String,
    bot_id: String,
    work_item: Option<String>,
    session_id: Option<String>,
    scope: Value,
    spec: RunSpec,
}

impl Pending {
    fn labels(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for i in &self.spec.injections {
            if !out.contains(&i.label) {
                out.push(i.label.clone());
            }
        }
        out
    }
}

enum Job {
    Run(String),
    Denied(String),
}

/// The secret-use service. Cheap to clone.
#[derive(Clone)]
pub struct SecretUses(Arc<Inner>);

struct Inner {
    vault: Vault,
    home: PathBuf,
    sink: Arc<dyn SessionSink>,
    pending: Mutex<HashMap<String, Pending>>,
    jobs: mpsc::UnboundedSender<Job>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Job>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `ITEM_VAR ← 令牌`, `KEY ← 私钥（文件）`, `标准输入 ← 密码`.
fn injection_text(inj: &[Injection]) -> String {
    inj.iter()
        .map(|i| match i.how {
            How::Env => format!("{} ← {}", i.var, i.label),
            How::File => format!("{} ← {}（文件）", i.var, i.label),
            How::Stdin => format!("标准输入 ← {}", i.label),
        })
        .collect::<Vec<_>>()
        .join("、")
}

fn mode_of(inj: &[Injection]) -> &'static str {
    [How::Env, How::File, How::Stdin]
        .into_iter()
        .find(|h| inj.iter().any(|i| i.how == *h))
        .map_or("env", How::mode)
}

/// The item a bot names: an id, or an exact title, among the items `user` sees (never the system
/// vault). Several items with that title → `400` listing their ids.
fn find_item(c: &rusqlite::Connection, ws: &str, user: &str, name: &str) -> VaultResult<ItemRow> {
    let name = name.trim();
    if let Ok((i, _)) = super::items::own_item(c, ws, user, name) {
        return Ok(i);
    }
    let all = irepo::list_items(c, ws, user, &irepo::ItemFilter::default())?;
    let hits: Vec<ItemRow> = all.into_iter().filter(|i| i.title == name).collect();
    match hits.len() {
        0 => Err(ApiError::not_found("条目").into()),
        1 => Ok(hits
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::not_found("条目"))?),
        _ => Err(ApiError::rule(format!(
            "Several credentials are titled \"{name}\": {}. Use the id.",
            hits.iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

impl SecretUses {
    /// `home` is `STEVE_HOME` (temp files go to `scratch/secrets/`).
    pub fn new(vault: Vault, home: impl Into<PathBuf>, sink: Arc<dyn SessionSink>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self(Arc::new(Inner {
            vault,
            home: home.into(),
            sink,
            pending: Mutex::default(),
            jobs: tx,
            rx: Mutex::new(Some(rx)),
        }))
    }

    pub fn vault(&self) -> &Vault {
        &self.0.vault
    }

    /// The handler to register: `registry.decision(use::SUBJECT, uses.decision_handler())`.
    pub fn decision_handler(&self) -> Box<dyn DecisionHandler> {
        Box::new(UseDecisions(self.clone()))
    }

    /// At startup, inside the runtime: removes leftover temp directories, fails uses that were
    /// running when the Hub stopped, expires the requests still waiting (their commands were only in
    /// memory) and tells their sessions; then starts the job worker and the hourly cleanup.
    pub fn start(&self) {
        inject::cleanup(&self.0.home, None);
        match self.expire_waiting(i64::MAX, true) {
            Ok(notes) => self.notify(notes),
            Err(e) => tracing::error!(error = %e, "vault: startup cleanup of secret uses failed"),
        }
        let Some(mut rx) = lock(&self.0.rx).take() else {
            return;
        };
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let me = me.clone();
                tokio::spawn(async move { me.job(job).await });
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(3600));
            t.tick().await;
            loop {
                t.tick().await;
                if let Err(e) = me.hourly().await {
                    tracing::error!(error = %e, "vault: hourly cleanup failed");
                }
            }
        });
    }

    /// The hourly part of `vault.cleanup` (§9): temp directories older than 15 minutes, requests
    /// unanswered for a day, use rows older than a year.
    pub async fn hourly(&self) -> VaultResult<()> {
        let me = self.clone();
        let notes = self
            .0
            .vault
            .blocking(move |v| {
                inject::cleanup(&me.0.home, Some(SCRATCH_AGE));
                let notes = me.expire_waiting(now() - PENDING_MS, false)?;
                v.store().transaction(|tx| {
                    Ok::<_, VaultError>(urepo::delete_uses_before(tx, now() - RETENTION_MS)?)
                })?;
                Ok(notes)
            })
            .await?;
        self.notify(notes);
        Ok(())
    }

    fn notify(&self, notes: Vec<(String, Option<String>, Option<String>, String)>) {
        if notes.is_empty() {
            return;
        }
        let sink = self.0.sink.clone();
        tokio::spawn(async move {
            for (ws, item, session, text) in notes {
                sink.send(&ws, item.as_deref(), session.as_deref(), &text)
                    .await;
            }
        });
    }

    /// Expires `pending` uses created before `before` (and their open decisions); at startup also
    /// fails `running` ones. Returns the messages for their sessions.
    #[allow(clippy::type_complexity)]
    fn expire_waiting(
        &self,
        before: i64,
        startup: bool,
    ) -> VaultResult<Vec<(String, Option<String>, Option<String>, String)>> {
        let store = self.0.vault.store();
        store.transaction(|tx| {
            let at = now();
            let mut notes = Vec::new();
            if startup {
                for u in urepo::uses_in(tx, "running", i64::MAX)? {
                    urepo::transition(tx, &u.id, "running", "failed", None, None, Some(at))?;
                    base::audit(tx, &u.workspace_id, "steve", "vault.use_failed", &u.item_id, &json!({"use": u.id, "reason": "restart"}))?;
                    publish(tx, &u);
                }
            }
            for u in urepo::uses_in(tx, "pending", before)? {
                urepo::transition(tx, &u.id, "pending", "expired", None, None, Some(at))?;
                if let Some(d) = &u.decision_id {
                    base::decisions::settle(tx, d, &json!({"expired": true}), "expired", &Value::Null)?;
                }
                lock(&self.0.pending).remove(&u.id);
                base::audit(tx, &u.workspace_id, "steve", "vault.use_expired", &u.item_id, &json!({"use": u.id}))?;
                publish(tx, &u);
                let title = irepo::item(tx, &u.item_id)?.map(|i| i.title).unwrap_or_default();
                let text = if startup {
                    format!("请求使用「{title}」的审批在 Steve 重启时失效了。需要的话请重新调用 steve_secret_use。")
                } else {
                    format!("用户一天内没有处理使用「{title}」的请求，请求已失效。需要的话请重新调用 steve_secret_use，或者换一种做法。")
                };
                notes.push((u.workspace_id.clone(), u.work_item_id.clone(), u.session_id.clone(), text));
            }
            Ok(notes)
        })
    }

    /// `steve_secret_use` (§6): runs now under a grant, or opens a decision.
    pub async fn request(&self, cx: &UseCx, r: UseRequest) -> VaultResult<UseOutcome> {
        let me = self.clone();
        let cx2 = cx.clone();
        let prepared = self.0.vault.blocking(move |_| me.prepare(&cx2, &r)).await?;
        match prepared {
            Prepared::Asked {
                use_id,
                decision_id,
            } => Ok(UseOutcome::Asked {
                use_id,
                decision_id,
            }),
            Prepared::Run { use_id, pending } => {
                let out = self.execute(&use_id, &pending).await;
                Ok(UseOutcome::Ran { use_id, out })
            }
        }
    }

    fn prepare(&self, cx: &UseCx, r: &UseRequest) -> VaultResult<Prepared> {
        let mut chk = crate::Check::new();
        chk.len("purpose", r.purpose.trim(), 1, PURPOSE_MAX).len(
            "command",
            r.command.trim(),
            1,
            COMMAND_MAX,
        );
        chk.done()?;
        let session = cx
            .session_id
            .as_deref()
            .ok_or_else(|| ApiError::rule("steve_secret_use only works inside a Steve session."))?;
        let store = self.0.vault.store();
        let (item, fields, workdir, work_item, project, bot_name) = store.reader(|c| {
            let (cwd, s_item, _) =
                urepo::session(c, &cx.ws, session)?.ok_or_else(|| ApiError::not_found("会话"))?;
            let item = find_item(c, &cx.ws, &cx.user, &r.item)?;
            let fields = irepo::fields(c, &item.id)?;
            let work_item = cx.item_id.clone().or(s_item);
            let project = match &work_item {
                Some(w) => urepo::item_project(c, &cx.ws, w)?.flatten(),
                None => None,
            };
            let bot = urepo::bot_name(c, &cx.bot_id)?.unwrap_or_else(|| cx.bot_id.clone());
            Ok::<_, VaultError>((item, fields, cwd, work_item, project, bot))
        })?;

        // injections: every label must be a field of the item, every VAR valid and used once
        let mut injections = Vec::new();
        let mut vars: Vec<&str> = Vec::new();
        let labels: Vec<&str> = fields.iter().map(|f| f.label.as_str()).collect();
        let check_label = |l: &str| -> VaultResult<()> {
            if labels.contains(&l) {
                Ok(())
            } else {
                Err(ApiError::rule(format!(
                    "No field \"{l}\" in this credential. Its fields: {}.",
                    labels.join(", ")
                ))
                .into())
            }
        };
        for (how, list) in [(How::Env, &r.env), (How::File, &r.files)] {
            for (var, label) in list {
                if !inject::valid_var(var) {
                    return Err(
                        ApiError::rule(format!("\"{var}\" is not a valid variable name.")).into(),
                    );
                }
                if vars.contains(&var.as_str()) {
                    return Err(ApiError::rule(format!("Variable {var} is given twice.")).into());
                }
                vars.push(var);
                check_label(label)?;
                injections.push(Injection {
                    var: var.clone(),
                    label: label.clone(),
                    how,
                });
            }
        }
        if let Some(l) = &r.stdin {
            check_label(l)?;
            injections.push(Injection {
                var: String::new(),
                label: l.clone(),
                how: How::Stdin,
            });
        }
        if injections.is_empty() {
            return Err(ApiError::rule(
                "Nothing to inject: name at least one field in env, files or stdin.",
            )
            .into());
        }
        let cwd = resolve_cwd(Path::new(&workdir), r.cwd.as_deref())?;
        let secs = r
            .timeout_s
            .filter(|t| t.is_finite() && *t > 0.0)
            .map_or(DEFAULT_TIMEOUT_S, |t| {
                (t.ceil() as u64).clamp(1, MAX_TIMEOUT_S)
            });
        let scope = match &project {
            Some(p) => json!({ "project_id": p }),
            None => json!({ "scratch": true }),
        };
        let use_id = uid("su");
        let pending = Pending {
            ws: cx.ws.clone(),
            item_id: item.id.clone(),
            title: item.title.clone(),
            bot_id: cx.bot_id.clone(),
            work_item: work_item.clone(),
            session_id: Some(session.to_string()),
            scope: scope.clone(),
            spec: RunSpec {
                use_id: use_id.clone(),
                cwd: cwd.clone(),
                command: r.command.clone(),
                injections,
                timeout: Duration::from_secs(secs),
            },
        };
        // a token pasted into the command itself is not stored
        let shown = steve_vault::guard::redact(&r.command).unwrap_or_else(|| r.command.clone());
        let at = now();
        let mut row = UseRecord {
            id: use_id.clone(),
            workspace_id: cx.ws.clone(),
            item_id: item.id.clone(),
            actor_type: "bot".into(),
            actor_id: cx.bot_id.clone(),
            session_id: Some(session.to_string()),
            work_item_id: work_item.clone(),
            purpose: r.purpose.trim().into(),
            mode: mode_of(&pending.spec.injections).into(),
            command: Some(shown.clone()),
            fields: pending.labels(),
            decision_id: None,
            grant_id: None,
            device_id: "hub".into(),
            status: "pending".into(),
            exit_code: None,
            created: at,
            finished: None,
        };

        let grant = store.reader(|c| grants::covering(c, &cx.ws, &item, &cx.bot_id, &scope))?;
        if let Some(g) = grant {
            row.status = "running".into();
            row.grant_id = Some(g.clone());
            store.transaction(|tx| {
                urepo::insert_use(tx, &row)?;
                base::audit(
                    tx,
                    &cx.ws,
                    &cx.bot_id,
                    "vault.use_start",
                    &item.id,
                    &json!({"use": use_id, "grant": g, "mode": row.mode, "fields": row.fields}),
                )?;
                publish(tx, &row);
                Ok::<_, VaultError>(())
            })?;
            return Ok(Prepared::Run {
                use_id,
                pending: Box::new(pending),
            });
        }

        let high = !grants::grantable(&item);
        let mut question = format!("{bot_name} 想用「{}」：{}", item.title, r.purpose.trim());
        if item.template == BANK_CARD {
            question.push_str("（这次使用可能涉及付款）");
        }
        let draft = format!(
            "命令：{shown}\n目录：{}\n注入：{}",
            cwd.display(),
            injection_text(&pending.spec.injections)
        );
        let options: Vec<String> = if high {
            OPTIONS_HIGH.iter().map(|s| s.to_string()).collect()
        } else {
            OPTIONS.iter().map(|s| s.to_string()).collect()
        };
        lock(&self.0.pending).insert(use_id.clone(), pending);
        let opened = store.transaction(|tx| {
            urepo::insert_use(tx, &row)?;
            let d = base::decisions::open(
                tx,
                &NewDecision {
                    workspace_id: cx.ws.clone(),
                    kind: "approve".into(),
                    subject_type: SUBJECT.into(),
                    subject_id: use_id.clone(),
                    item_id: work_item.clone(),
                    session_id: Some(session.to_string()),
                    question: question.clone(),
                    draft: Some(draft.clone()),
                    options: Some(options.clone()),
                    requested_by: cx.bot_id.clone(),
                },
            )?;
            urepo::set_decision(tx, &use_id, &d.id)?;
            base::audit(
                tx,
                &cx.ws,
                &cx.bot_id,
                "vault.use_request",
                &item.id,
                &json!({"use": use_id, "decision": d.id, "mode": row.mode, "fields": row.fields}),
            )?;
            publish(tx, &row);
            Ok::<_, VaultError>(d.id)
        });
        match opened {
            Ok(decision_id) => Ok(Prepared::Asked {
                use_id,
                decision_id,
            }),
            Err(e) => {
                lock(&self.0.pending).remove(&use_id);
                Err(e)
            }
        }
    }

    /// Decrypts, runs, registers the values with the session redactor, records the result. The
    /// values exist only inside this call.
    async fn execute(&self, use_id: &str, p: &Pending) -> RunOutput {
        let (ws, item, labels) = (p.ws.clone(), p.item_id.clone(), p.labels());
        let values = self
            .0
            .vault
            .blocking(move |v| v.injection_values(&ws, &item, &labels))
            .await;
        let out = match values {
            Ok(values) => {
                let out = inject::run(&self.0.home, &p.spec, &values).await;
                if let Some(s) = &p.session_id {
                    for (label, v) in &values {
                        steve_vault::redact::register(s, label, v.expose());
                    }
                }
                out
            }
            Err(e) => RunOutput {
                exit_code: None,
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e.to_string()),
            },
        };
        let (id, o) = (use_id.to_string(), out.clone());
        let recorded = self
            .0
            .vault
            .blocking(move |v| {
                v.store().transaction(|tx| {
                    let to = if o.ok() { "ok" } else { "failed" };
                    let Some(u) = urepo::get_use(tx, &id)? else {
                        return Ok(());
                    };
                    urepo::transition(tx, &id, "running", to, None, Some(i64::from(o.code())), Some(now()))?;
                    base::audit(
                        tx,
                        &u.workspace_id,
                        &u.actor_id,
                        "vault.use",
                        &u.item_id,
                        &json!({"use": id, "mode": u.mode, "fields": u.fields, "exit_code": o.code(),
                                "timed_out": o.timed_out, "grant": u.grant_id, "decision": u.decision_id}),
                    )?;
                    publish(tx, &u);
                    Ok::<_, VaultError>(())
                })
            })
            .await;
        if let Err(e) = recorded {
            tracing::error!(error = %e, "vault: could not record a secret use");
        }
        tracing::info!(use_id, code = out.code(), "vault: secret use ran");
        out
    }

    /// Waits until the use left `pending` (the answering transaction committed) and is in `want`.
    async fn claim(&self, use_id: &str, want: &str) -> Option<UseRecord> {
        for _ in 0..50 {
            let id = use_id.to_string();
            let u = self
                .0
                .vault
                .blocking(move |v| {
                    v.store()
                        .transaction(|tx| Ok::<_, VaultError>(urepo::get_use(tx, &id)?))
                })
                .await
                .ok()
                .flatten()?;
            if u.status == want {
                return Some(u);
            }
            if u.status != "pending" {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    async fn job(&self, job: Job) {
        match job {
            Job::Run(id) => {
                if self.claim(&id, "running").await.is_none() {
                    return;
                }
                let Some(p) = lock(&self.0.pending).remove(&id) else {
                    return;
                };
                let out = self.execute(&id, &p).await;
                let text = approved_message(&p.title, &out);
                self.0
                    .sink
                    .send(
                        &p.ws,
                        p.work_item.as_deref(),
                        p.session_id.as_deref(),
                        &text,
                    )
                    .await;
            }
            Job::Denied(id) => {
                if self.claim(&id, "denied").await.is_none() {
                    return;
                }
                let Some(p) = lock(&self.0.pending).remove(&id) else {
                    return;
                };
                self.0
                    .sink
                    .send(
                        &p.ws,
                        p.work_item.as_deref(),
                        p.session_id.as_deref(),
                        &denied_message(&p.title),
                    )
                    .await;
            }
        }
    }

    /// Requests waiting for an answer (tests, diagnostics).
    pub fn waiting(&self) -> usize {
        lock(&self.0.pending).len()
    }
}

enum Prepared {
    Run {
        use_id: String,
        pending: Box<Pending>,
    },
    Asked {
        use_id: String,
        decision_id: String,
    },
}

fn publish(tx: &Tx, u: &UseRecord) {
    tx.publish(ChangeEvent::Vault {
        id: u.item_id.clone(),
        ws: u.workspace_id.clone(),
    });
}

/// The `secret_use` decision handler (§4.3).
pub struct UseDecisions(SecretUses);

impl DecisionHandler for UseDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let inner = &self.0.0;
        let u = urepo::get_use(tx, &d.subject_id)?
            .filter(|u| u.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("使用记录"))?;
        if u.status != "pending" {
            return Err(ApiError::conflict("这次使用已经处理过了"));
        }
        let Some(p) = lock(&inner.pending).get(&u.id).cloned() else {
            return Err(ApiError::conflict("这个请求已经失效，请让 Bot 重新发起"));
        };
        let at = now();
        let mut result = choice_result(cx);
        result["use"] = json!(u.id);
        let job = match cx.choice {
            "deny" => {
                urepo::transition(tx, &u.id, "pending", "denied", None, None, Some(at))?;
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "vault.use_denied",
                    &u.item_id,
                    &json!({"use": u.id, "decision": d.id}),
                )?;
                Job::Denied(u.id.clone())
            }
            "once" => {
                urepo::transition(tx, &u.id, "pending", "running", None, None, None)?;
                Job::Run(u.id.clone())
            }
            c => {
                let hours = match c {
                    "grant_1h" => 1,
                    "grant_8h" => 8,
                    "grant_24h" => 24,
                    _ => return Err(ApiError::invalid("choice")),
                };
                let item = irepo::item(tx, &u.item_id)?
                    .filter(|i| i.status == "active")
                    .ok_or_else(|| ApiError::not_found("条目"))?;
                let g = add_grant(
                    tx,
                    cx.ws,
                    &item,
                    &p.bot_id,
                    &p.scope,
                    hours,
                    cx.actor,
                    Some(&d.id),
                )
                .map_err(|e| match e {
                    VaultError::Api(a) => a,
                    e => ApiError::Rule(e.to_string()),
                })?;
                urepo::transition(tx, &u.id, "pending", "running", Some(&g), None, None)?;
                result["grant"] = json!(g);
                Job::Run(u.id.clone())
            }
        };
        publish(tx, &u);
        // runs after the commit: the job waits for the write lock and re-reads the row
        let _ = inner.jobs.send(job);
        Ok(result)
    }

    fn before_undo(&self, _: &Tx, _: &Decision) -> ApiResult<()> {
        Err(ApiError::rule("机密使用的决定不能撤销"))
    }

    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if urepo::transition(
            tx,
            &d.subject_id,
            "pending",
            "expired",
            None,
            None,
            Some(now()),
        )? {
            lock(&self.0.0.pending).remove(&d.subject_id);
            if let Some(u) = urepo::get_use(tx, &d.subject_id)? {
                publish(tx, &u);
            }
        }
        Ok(())
    }
}
