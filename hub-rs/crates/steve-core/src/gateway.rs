//! Steve's short model calls use one model on the purpose's harness, with one absolute deadline. The router, chat answers and config drafts
//! are built here from data the caller collected under the core lock, so no lock is held while a model runs.

use crate::error::{CoreError, Result, msg};
use crate::repo::{Bot, Project, Tag};
use crate::util::{js_head, js_len};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
pub use steve_modules::gateway::{GatewayError, TimeoutPhase};
pub use steve_node_api::UsageReport;
pub use steve_node_api::{Image, OneShotError};
pub use steve_store::ModelUsage;

/// "fast" is for routing and other short calls, "smart" for answers and drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Smart,
}

/// A model's reply and which model of the tier gave it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Printed {
    pub text: String,
    /// The model of the tier that was asked (what the call is booked under).
    pub model: String,
    /// The model the harness itself named for the answering run, in its own reports; `None` when
    /// it named none (a harness default, a harness that reports no model). Not the model asked for:
    /// a caller that records who answered takes this one.
    pub reported_model: Option<String>,
    /// The dollars the answering run reported, when it reports any (module budgets add them up).
    pub cost_usd: Option<f64>,
    /// Tokens per accounting model in the answering attempt.
    pub usage: Vec<ModelUsage>,
    /// Attempts made by this call.
    pub attempts: u32,
    /// Wall time of the whole call.
    pub duration_ms: u64,
}

/// One call as the statistics see it: the reply or the error, and what it took either way.
#[derive(Debug)]
pub struct Attempt {
    pub result: Result<Printed>,
    /// Models tried.
    pub attempts: u32,
    pub duration_ms: u64,
    /// Tokens per model, failed tries included.
    pub usage: Vec<ModelUsage>,
    /// The model that answered, or the last one tried.
    pub model: String,
    /// `<model>: <error>` of each try that failed (503, timeout…).
    pub failures: Vec<String>,
    /// The harness adapter the call ran on (its id); empty: none was chosen.
    pub harness: String,
}

impl Attempt {
    pub fn before_dispatch(error: CoreError, duration_ms: u64) -> Self {
        Self {
            failures: vec![error.to_string()],
            result: Err(error),
            attempts: 0,
            duration_ms,
            usage: vec![],
            model: String::new(),
            harness: String::new(),
        }
    }
}

/// Told what the model has written so far in its current message, each time it grows.
pub type OnText<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// One planned model call; implemented by [`SingleModelGateway`], faked in tests.
#[async_trait]
pub trait ModelGateway: Send + Sync {
    /// `claudePrintWith`: `tools` are what the call may use besides answering (a chat answer that reads
    /// its attachments: [`ThinkTools::Read`]); most calls use none.
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
    ) -> Result<Printed>;
    /// [`ModelGateway::call`] that reports the text while it is generated (a chat answer shown as it is
    /// written). Without streaming, `on_text` hears the whole text once.
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        let p = self.call(prompt, tier, timeout, tools).await?;
        on_text(&p.text);
        Ok(p)
    }
    /// `claudePrint`: the text only.
    async fn print(&self, prompt: &str, tier: Tier, timeout: Duration) -> Result<String> {
        Ok(self
            .call(prompt, tier, timeout, &ThinkTools::None)
            .await?
            .text)
    }
    /// [`ModelGateway::call`] (or [`ModelGateway::call_streaming`] with `on_text`) that also tells, when
    /// the call fails, how many models were tried and what they used; for [`crate::stats::RecordingGateway`].
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let t0 = Instant::now();
        let result = match on_text {
            Some(f) => self.call_streaming(prompt, tier, timeout, tools, f).await,
            None => self.call(prompt, tier, timeout, tools).await,
        };
        let ms = elapsed_ms(t0);
        match result {
            Ok(p) => Attempt {
                attempts: p.attempts.max(1),
                duration_ms: if p.duration_ms > 0 { p.duration_ms } else { ms },
                usage: p.usage.clone(),
                model: p.model.clone(),
                failures: vec![],
                harness: String::new(),
                result: Ok(p),
            },
            Err(e) => Attempt {
                attempts: 1,
                duration_ms: ms,
                usage: vec![],
                model: String::new(),
                failures: vec![js_head(&e.to_string(), 200).to_string()],
                harness: String::new(),
                result: Err(e),
            },
        }
    }
}

fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One try of one model ([`OneModel::once`]): its reply (text and `total_cost_usd`) or error, and the
/// tokens it reported either way (a failed try spent them too).
pub struct Tried {
    pub reply: Result<(String, Option<f64>)>,
    pub usage: Vec<ModelUsage>,
    /// The model the harness named in its own reports of this try ([`reported_model`]), before a
    /// report without one is booked under the model asked for.
    pub reported_model: Option<String>,
}

/// The model a harness named itself in its usage reports of one run: that of its main
/// conversation, else of its whole run (the last one named); `None` when no report names one.
pub fn reported_model(reports: &[UsageReport]) -> Option<String> {
    use steve_node_api::UsageScope;
    let named = |scope: UsageScope| {
        reports
            .iter()
            .rev()
            .filter(|r| r.scope == scope)
            .find_map(|r| r.model.clone().filter(|m| !m.is_empty()))
    };
    named(UsageScope::Main).or_else(|| named(UsageScope::All))
}

impl Tried {
    /// A try that failed before the model ran (nothing spent).
    pub fn failed(e: CoreError) -> Tried {
        Tried {
            reply: Err(e),
            usage: vec![],
            reported_model: None,
        }
    }

    /// A one-shot run in the gateway's terms: its error in Steve's words, its usage as rows. A run
    /// elsewhere (a Node job) is put back into these terms too. Runners that know the selected
    /// model resolve missing report names with `merged_usage` before calling this boundary.
    pub fn of(
        reply: std::result::Result<(String, Option<f64>), OneShotError>,
        usage: Vec<UsageReport>,
    ) -> Tried {
        // Normalize reports before turning them into accounting rows.
        let reports = merged_usage(usage, "");
        let costs: Vec<_> = reports
            .iter()
            .filter_map(|r| {
                r.cost
                    .as_ref()
                    .filter(|m| m.currency == "USD")
                    .and_then(cost_f64)
            })
            .collect();
        let cost = (!costs.is_empty())
            .then(|| costs.iter().sum::<f64>())
            .filter(|n| n.is_finite());
        let mut usage = vec![];
        merge_usage(
            &mut usage,
            reports.iter().cloned().map(model_usage).collect(),
        );
        for u in &mut usage {
            if u.tokens == steve_store::Tokens::default()
                && reports
                    .iter()
                    .filter(|r| r.model.as_deref().unwrap_or_default() == u.model)
                    .all(|r| !steve_node_api::usage::accounting_tokens(r, &reports))
            {
                u.detail["accounting_tokens"] = json!(false);
            }
        }
        Tried {
            reply: reply
                .map(|(text, reported)| {
                    (
                        text,
                        cost.or(reported.filter(|n| n.is_finite() && *n >= 0.0)),
                    )
                })
                .map_err(call_error),
            usage,
            // a runner that knows sets it: what reaches here may name the model asked for already
            reported_model: None,
        }
    }
}

/// What a failed one-shot call says: the adapter's words, cut to 300 characters.
fn call_error(e: OneShotError) -> CoreError {
    match e {
        OneShotError::OutputLimit => msg("输出超出这次运行的限制"),
        OneShotError::Timeout(phase) => GatewayError::Timeout(phase).into(),
        OneShotError::Failed(err) => GatewayError::Startup(js_head(&err, 300).to_string()).into(),
        OneShotError::ToolRefused => GatewayError::NeedsTools.into(),
        OneShotError::Harness { text, .. } => msg(js_head(&text, 300).to_string()),
        e @ OneShotError::Unsupported(_) => msg(e.to_string()),
        OneShotError::Io(e) => CoreError::Io(e),
    }
}

/// Dollars as reported (`Money.decimal`), read back.
pub fn cost_f64(m: &steve_node_api::Money) -> Option<f64> {
    steve_node_api::usage::amount(m)
}

// The durable ledger and Node jobs share this accounting primitive.
pub use steve_node_api::merged_usage;

/// A usage report as the statistics' row (its dollars back to the same `f64`).
pub use steve_store::gateway_ledger::model_usage;

/// The purpose, required capabilities, and trusted attachment grants of a single plan.
/// This is built from caller intent, never inferred from the prompt or expanded by the model.
pub struct CallSpec<'a> {
    pub purpose: &'static str,
    pub required_tools: &'a ThinkTools,
    pub auth_scope: &'a ThinkTools,
    pub deadline: tokio::time::Instant,
}

impl CallSpec<'_> {
    pub fn authorize(&self) -> Result<()> {
        match (self.required_tools, self.auth_scope) {
            (ThinkTools::None, _) => Ok(()),
            (ThinkTools::Read(required), ThinkTools::Read(granted))
                if !required.is_empty() && required.iter().all(|file| granted.contains(file)) =>
            {
                Ok(())
            }
            _ => Err(GatewayError::ToolDenied.into()),
        }
    }
}

/// One planned inference at the selected adapter. Images and attachments cannot widen its grant.
pub struct OneCall<'a> {
    pub prompt: &'a str,
    pub spec: CallSpec<'a>,
    pub harness: &'a str,
    pub images: &'a [Image],
}

#[async_trait]
pub trait OneModel: Send + Sync {
    /// Discover capabilities without starting an inference. Fail closed for tool-bearing calls
    /// unless the backend implements discovery for the selected target.
    async fn preflight(&self, call: &OneCall<'_>) -> Result<()> {
        if call.spec.required_tools.any() {
            Err(GatewayError::MissingCapability.into())
        } else {
            Ok(())
        }
    }

    /// Runs `model` (empty: the harness's default) once; streaming when `on_text` is given.
    async fn once(
        &self,
        call: &OneCall<'_>,
        model: &str,
        effort: Option<steve_types::ReasoningEffort>,
        on_text: Option<OnText<'_>>,
    ) -> Tried;
}

/// Select the first configured model for the purpose, or the adapter default. A failure never
/// causes a second prompt or selects a different harness.
pub struct SingleModelGateway<M> {
    pub one: M,
    pub fast: Vec<String>,
    pub smart: Vec<String>,
    /// The model-settings resolver; tests can supply fixed tier lists instead.
    pub models: Option<crate::models::Models>,
}

/// The router's time limit: a route that takes longer becomes [`Route::fallback`] (successful routes
/// take under 20 s).
pub const ROUTE_TIMEOUT: Duration = Duration::from_secs(30);

/// The images of [`with_images`] blocks (`{"type":"image","source":{"type":"base64",…}}`); a block
/// without base64 data is left out.
fn images_of(blocks: &[Value]) -> Vec<Image> {
    use base64::Engine;
    blocks
        .iter()
        .filter_map(|b| {
            let src = &b["source"];
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(src["data"].as_str()?)
                .ok()?;
            Some(Image {
                mime: src["media_type"].as_str().unwrap_or("").to_string(),
                bytes,
            })
        })
        .collect()
}

pub use steve_modules::gateway::{STATIC_END, split_static, with_static};

tokio::task_local! {
    /// Image blocks for the user message of the calls made inside [`with_images`].
    static IMAGES: std::sync::Arc<Vec<Value>>;
}

/// Runs `f` with `images` (`{"type":"image","source":{…}}` blocks) attached to the user message of every
/// model call it makes, like [`crate::stats::tagged`] carries the call's purpose: a chat answer about a
/// screenshot sees the screenshot without a tool call.
pub async fn with_images<F: std::future::Future>(images: Vec<Value>, f: F) -> F::Output {
    if images.is_empty() {
        return f.await;
    }
    IMAGES.scope(std::sync::Arc::new(images), f).await
}

tokio::task_local! {
    /// The harness and models the calls made inside [`with_choice`] run on.
    static CHOICE: crate::models::Choice;
    /// A model to avoid selecting during [`with_excluded_model`] (e.g. the drafting model during
    /// a `disclosure` audit).
    static EXCLUDE_MODEL: String;
}

/// Runs `f` with its model calls on `choice` instead of what 设置 · 模型 says for their purpose: a
/// caller that resolved the purpose itself and narrowed it (the completion judge keeps off the
/// worker's model, `crate::judge`). The purpose still books the calls.
pub async fn with_choice<F: std::future::Future>(choice: crate::models::Choice, f: F) -> F::Output {
    CHOICE.scope(choice, f).await
}

/// Runs `f` excluding `model` from selection when another candidate is available.
pub async fn with_excluded_model<F: std::future::Future>(model: Option<&str>, f: F) -> F::Output {
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => EXCLUDE_MODEL.scope(m.to_string(), f).await,
        None => f.await,
    }
}

/// The choice of [`with_choice`], if the current call runs inside one.
pub fn current_choice() -> Option<crate::models::Choice> {
    CHOICE.try_with(Clone::clone).ok()
}

/// The model excluded by [`with_excluded_model`], if any.
pub fn current_excluded_model() -> Option<String> {
    EXCLUDE_MODEL.try_with(Clone::clone).ok()
}

/// The images of the current call, if any (see [`with_images`]).
pub fn current_images() -> Option<std::sync::Arc<Vec<Value>>> {
    IMAGES
        .try_with(|i| i.clone())
        .ok()
        .filter(|i| !i.is_empty())
}

/// A file a think call may read: the attachment's id and where its copy sits relative to the call's
/// working directory (`.steve/attachments/<id>-<name>`, as the prompt names it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readable {
    pub file_id: String,
    pub rel_path: String,
}

/// What a think call may use besides answering. The Node places the files and configures the harness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ThinkTools {
    /// No tools at all (the harness's own system prompt and tools left out where it allows).
    #[default]
    None,
    /// The Read tool only, for these files (copied into the job directory).
    Read(Vec<Readable>),
}

impl ThinkTools {
    /// Whether the call has any tool.
    pub fn any(&self) -> bool {
        matches!(self, ThinkTools::Read(_))
    }
}

/// The tools of a chat answer: none without readable attachments, else Read for those files.
pub fn answer_tools(files: &ChatFiles) -> ThinkTools {
    if files.readable.is_empty() {
        ThinkTools::None
    } else {
        ThinkTools::Read(files.readable.clone())
    }
}

use steve_store::gateway_ledger::merge_usage;

impl<M: OneModel> SingleModelGateway<M> {
    /// The models (and effort) of this call: its purpose and workspace come from the caller's
    /// [`crate::stats::CallTag`].
    pub fn choice(&self, tier: Tier) -> crate::models::Choice {
        let excluded = current_excluded_model();
        /* a caller that chose for itself ([`with_choice`]: the judge, kept off the worker's model) */
        if let Some(mut c) = current_choice() {
            if let Some(ex) = excluded.as_deref().filter(|s| !s.is_empty()) {
                let filtered: Vec<String> = c
                    .models
                    .iter()
                    .filter(|m| m.as_str() != ex)
                    .cloned()
                    .collect();
                if !filtered.is_empty() {
                    c.models = filtered;
                }
            }
            return c;
        }
        match &self.models {
            Some(m) => {
                let tag = crate::stats::current_tag();
                m.for_call_excluding(&tag.workspace_id, tag.purpose, tier, excluded.as_deref())
            }
            None => {
                let mut models = match tier {
                    Tier::Fast => self.fast.clone(),
                    Tier::Smart => self.smart.clone(),
                };
                if let Some(ex) = excluded.as_deref().filter(|s| !s.is_empty()) {
                    let filtered: Vec<String> = models
                        .iter()
                        .filter(|m| m.as_str() != ex)
                        .cloned()
                        .collect();
                    if !filtered.is_empty() {
                        models = filtered;
                    }
                }
                crate::models::Choice::default_adapter(models)
            }
        }
    }

    async fn run_once(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let t0 = tokio::time::Instant::now();
        let choice = self.choice(tier);
        let model = choice.models.first().map_or("", String::as_str);
        let images = current_images().map(|b| images_of(&b)).unwrap_or_default();
        let call = OneCall {
            prompt,
            spec: CallSpec {
                purpose: crate::stats::current_tag().purpose,
                required_tools: tools,
                auth_scope: tools, // the legacy call form supplies only its explicit attachment grants
                deadline: t0 + timeout,
            },
            harness: &choice.harness,
            images: &images,
        };
        let queue_timeout = || GatewayError::Timeout(TimeoutPhase::Queue).into();
        let preflight = async {
            call.spec.authorize()?;
            self.one.preflight(&call).await
        };
        let preflight = if tokio::time::Instant::now() >= call.spec.deadline {
            Err(queue_timeout())
        } else {
            tokio::time::timeout_at(call.spec.deadline, preflight)
                .await
                .unwrap_or_else(|_| Err(queue_timeout()))
        };

        let mut attempts = 0;
        let tried = async {
            preflight?;
            if tokio::time::Instant::now() >= call.spec.deadline {
                return Err(queue_timeout());
            }
            let account = crate::stats::current_call()
                .map(|c| c.attempt(&choice.harness, model))
                .transpose()
                .map_err(|e| msg(format!("usage ownership: {e}")))?;
            attempts = 1;
            let run = self.one.once(&call, model, choice.effort, on_text);
            let tried = match &account {
                Some(a) => a.scope(run).await,
                None => run.await,
            };
            if let Some(account) = account {
                let error = tried.reply.as_ref().err().map(ToString::to_string);
                if let Err(e) = account.call.store.stats().gateway_attempt_end(
                    &account.id,
                    tried.reply.is_ok(),
                    error.as_deref(),
                    steve_store::now(),
                ) {
                    tracing::warn!("stats: attempt end: {e}");
                }
            }
            Ok(tried)
        }
        .await
        .unwrap_or_else(Tried::failed);

        let duration_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = tried.reply.map(|(text, cost_usd)| Printed {
            text,
            cost_usd,
            model: model.to_string(),
            reported_model: tried.reported_model,
            usage: tried.usage.clone(),
            attempts,
            duration_ms,
        });
        let failures = result
            .as_ref()
            .err()
            .map(|e| {
                let detail = js_head(&e.to_string(), 200).to_string();
                tracing::warn!(harness = %choice.harness, model, "gateway: {detail}");
                format!("{model}: {detail}")
            })
            .into_iter()
            .collect();
        Attempt {
            result,
            attempts,
            duration_ms,
            usage: tried.usage,
            model: model.to_string(),
            failures,
            harness: choice.harness.clone(),
        }
    }
}

#[async_trait]
impl<M: OneModel> ModelGateway for SingleModelGateway<M> {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
    ) -> Result<Printed> {
        self.run_once(prompt, tier, timeout, tools, None)
            .await
            .result
    }
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        self.run_once(prompt, tier, timeout, tools, Some(on_text))
            .await
            .result
    }
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        self.run_once(prompt, tier, timeout, tools, on_text).await
    }
}

/// Steve's gateway as the modules' [`steve_modules::Gateway`] (B1), so core and modules share one model
/// gateway: same selection policy, same scratch directory.
pub struct SharedGateway(pub std::sync::Arc<dyn ModelGateway>);

#[async_trait]
impl steve_modules::Gateway for SharedGateway {
    async fn run_excluding(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        exclude_model: Option<&str>,
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, GatewayError> {
        with_excluded_model(exclude_model, self.run(tier, prompt, timeout)).await
    }

    async fn run_with_images(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        images: &[Value],
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, GatewayError> {
        with_images(images.to_vec(), self.run(tier, prompt, timeout)).await
    }

    async fn run(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, GatewayError> {
        let tier = match tier {
            steve_modules::Tier::Fast => Tier::Fast,
            steve_modules::Tier::Smart => Tier::Smart,
        };
        self.0
            .call(prompt, tier, timeout, &ThinkTools::None)
            .await
            .map(|p| steve_modules::GatewayOut {
                text: p.text,
                cost_usd: p.cost_usd,
                model: p.model,
                usage: p.usage,
                attempts: p.attempts,
                duration_ms: p.duration_ms,
            })
            .map_err(gateway_error)
    }
}

/// Keep typed gateway failures across the module and conversation boundaries.
pub fn gateway_error(error: CoreError) -> GatewayError {
    match error {
        CoreError::Gateway(error) => error,
        error => GatewayError::Other(error.to_string()),
    }
}

/// JS `String(v)` for a JSON value.
pub fn jstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a
            .iter()
            .map(|x| if x.is_null() { String::new() } else { jstr(x) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub use steve_runtime::json::truthy;

/// The greedy first `{…}` of a model reply, parsed; `None` without braces, an error for bad JSON.
pub fn first_json(t: &str) -> Result<Option<Value>> {
    Ok(steve_runtime::json::first_json(t)?)
}

/// The string value of `"key": "…` in a reply still being written, as far as it has got (escapes decoded,
/// an escape cut off at the end left out); `None` until the value has begun.
pub fn partial_str(t: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let b = t.as_bytes();
    let mut from = 0;
    let start = loop {
        let at = from + t[from..].find(&k)?;
        let rest = t[at + k.len()..].trim_start();
        if let Some(v) = rest.strip_prefix(':').map(str::trim_start)
            && let Some(v) = v.strip_prefix('"')
        {
            break t.len() - v.len();
        }
        from = at + k.len();
    };
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'"' => break,
            b'\\' if b.get(i + 1) == Some(&b'u') => {
                if i + 6 > b.len() {
                    break;
                }
                i += 6
            }
            b'\\' if i + 1 >= b.len() => break,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    let body = &t[start..i];
    let decode = |s: &str| serde_json::from_str::<String>(&format!("\"{s}\"")).ok();
    /* a high surrogate whose pair is still to come */
    decode(body).or_else(|| decode(&body[..body.rfind("\\u")?]))
}

/// The tag categories a new tag may have; `类型` holds only the six item categories (ST-34).
pub const TAG_CATS: [&str; 4] = ["项目", "领域", "客户", "主题"];

/// The routing results.
pub const KINDS: [&str; 9] = [
    "answer",
    "research",
    "item",
    "suggest",
    "ask",
    "supplement",
    "status",
    "config",
    "reroute",
];

/// Why the router chose what it chose; the guards in `thread::decide` read them.
pub const SIGNALS: [&str; 7] = [
    "explicit",
    "change",
    "deliverable",
    "long",
    "question",
    "lookup",
    "vague",
];

#[derive(Debug, Clone, PartialEq)]
pub struct NewTag {
    pub name: String,
    pub cat: String,
    pub description: String,
}

/// Work an answer or research run leads to; the user decides whether it becomes an item.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Offer {
    pub title: String,
    pub goal: String,
}

/// The router's decision for a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// one of [`KINDS`]
    pub kind: String,
    /// 0–1, 0.5 when the model gave none
    pub confidence: f64,
    /// a subset of [`SIGNALS`]
    pub signals: Vec<String>,
    /// work to offer after an answer or research
    pub offer: Option<Offer>,
    /// the item a supplement, status or reroute is about (checked to be of this workspace)
    pub item: Option<String>,
    /// choices of an `ask`
    pub options: Vec<String>,
    pub title: String,
    /// The task for the bot, self-contained (a follow-up like “按建议” restated from Steve's reply).
    pub goal: String,
    pub project: Option<String>,
    pub tag_ids: Vec<String>,
    pub new_tags: Vec<NewTag>,
    /// one sentence for the user, without the tags (the code adds them once)
    pub reply: String,
    /// the router's one-sentence reason (≤200)
    pub why: String,
    /// which model routed
    pub model: Option<String>,
    /// the router failed and this is the default
    pub fallback: bool,
    /// Asked only while Steve is still answering earlier messages ([`RouteCtx::pending`]): the message
    /// needs that answer first (“就按你说的改”, “再详细点”).
    pub after: bool,
}

impl Default for Route {
    fn default() -> Route {
        Route {
            kind: "answer".into(),
            confidence: 0.5,
            signals: vec![],
            offer: None,
            item: None,
            options: vec![],
            title: String::new(),
            goal: String::new(),
            project: None,
            tag_ids: vec![],
            new_tags: vec![],
            reply: String::new(),
            why: String::new(),
            model: None,
            fallback: false,
            after: false,
        }
    }
}

impl Route {
    /// Not a fallback: a kind the code knew without the router (`route_log.guard = "q"`).
    pub fn plain(self) -> Route {
        Route {
            fallback: false,
            ..self
        }
    }

    /// What the router falls back to when the model fails: never an item. Research when
    /// there is a project to read (`project`), else a direct answer.
    pub fn fallback(text: &str, project: Option<String>) -> Route {
        Route {
            kind: if project.is_some() {
                "research".into()
            } else {
                "answer".into()
            },
            title: js_head(text, 24).into(),
            goal: text.into(),
            project,
            why: "没能判断，先在这里回答".into(),
            fallback: true,
            ..Route::default()
        }
    }
}

/// A thread message as context for the router and chat answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub author: String,
    pub text: String,
}

/// An open item of the conversation the router may send a message to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FocusItem {
    pub id: String,
    pub title: String,
    pub status: String,
    /// the start of the user message it came from
    pub origin: String,
}

/// What the router may pick from (collected under the core lock).
#[derive(Debug, Clone, Default)]
pub struct RouteCtx {
    /// enabled bots of the workspace
    pub bots: Vec<Bot>,
    pub projects: Vec<Project>,
    pub tags: Vec<Tag>,
    /// the project whose path is `STEVE_REPO`, if any
    pub steve_project: Option<String>,
    /// the conversation's open items, most recent first (≤3)
    pub focus: Vec<FocusItem>,
    /// ids of the workspace's items: an `item` the router names must be one of them
    pub items: Vec<String>,
    /// Finished items remain valid context for status/research, but cannot receive supplements.
    pub closed_items: Vec<String>,
    /// an inline research run is reading right now
    pub researching: bool,
    /// The main conversation's messages Steve is still answering (running first, then waiting): the
    /// router says whether this message needs those answers first ([`Route::after`]).
    pub pending: Vec<String>,
    pub debug: bool,
    /// The workspace's own conversation: its research may act on the Hub's machine with the
    /// owner's access ([`crate::thread::ResearchRequest::full`]), so work on the machine stays in it.
    pub operator: bool,
}

fn js_json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// The fixed part of the router prompt: principles, kinds and examples.
pub const ROUTE_RULES: &str = include_str!("thread/route_prompt.txt");

/// The router prompt: [`ROUTE_RULES`] plus what exists in the workspace and the recent conversation.
///
/// Laid out for the prompt cache: the rules, the workspace (bots, projects, tags) and the reply format
/// come first and are the same for every message of the workspace; after [`STATIC_END`] come the
/// conversation's open items, the recent conversation and the message.
pub fn route_prompt(c: &RouteCtx, text: &str, history: &[Said], promote: bool) -> String {
    route_prompt_for(c, &[text.to_string()], history, promote)
}

/// Maximum recent conversation messages included in the route classification prompt.
const ROUTE_HISTORY_MAX: usize = 6;
/// Character cap per recent conversation message in the route classification prompt.
const ROUTE_HISTORY_CHARS: usize = 800;
/// Maximum recent conversation messages included in the direct chat answer prompt.
const ANSWER_HISTORY_MAX: usize = 10;

/// The router prompt for several messages the user sent in a row, decided in one call ([`route_many`]);
/// one message gives exactly [`route_prompt`].
pub fn route_prompt_for(c: &RouteCtx, texts: &[String], history: &[Said], promote: bool) -> String {
    let skip = history.len().saturating_sub(ROUTE_HISTORY_MAX);
    let recent: Vec<String> = history[skip..]
        .iter()
        .map(|m| {
            let t = if js_len(&m.text) > ROUTE_HISTORY_CHARS {
                format!("{}…", js_head(&m.text, ROUTE_HISTORY_CHARS))
            } else {
                m.text.clone()
            };
            format!(
                "  {}: {}",
                if m.author == "me" { "User" } else { "Steve" },
                js_json(&t)
            )
        })
        .collect();
    let mut head = route_instructions(c);
    let mut lines: Vec<String> = vec![];
    let context = texts
        .iter()
        .map(String::as_str)
        .chain(history.iter().map(|s| s.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let closed: Vec<&str> = c
        .closed_items
        .iter()
        .filter(|id| crate::thread::decide::names_item(&context, id))
        .map(String::as_str)
        .collect();
    if !closed.is_empty() {
        lines.push(format!(
            "Closed items (context only; never supplement these): {}",
            closed.join(", ")
        ));
    }
    if !c.focus.is_empty() {
        lines.push(
            "Open items of this conversation (id: title · status · from the user's message):"
                .into(),
        );
        lines.extend(c.focus.iter().map(|f| {
            format!(
                "  {}: {} · {} · {}",
                f.id,
                f.title,
                f.status,
                js_json(js_head(&f.origin, 80))
            )
        }));
    }
    if c.researching {
        lines.push("Steve is reading for an earlier question right now. What the user says meanwhile does not become an item unless they explicitly ask for one.".into());
    }
    if !c.pending.is_empty() {
        lines.push("Steve is still answering these earlier messages of the user (their answers are not in the conversation yet):".into());
        lines.extend(
            c.pending
                .iter()
                .map(|q| format!("  {}", js_json(js_head(q, 200)))),
        );
        lines.push("Add \"after\": true or false to the JSON. true when the message only makes sense once those answers are out: it refers to them (“就按你说的改”, “再详细点”, “第二个方案”, “那退款那块呢”), follows them up or widens them, or you are not sure. false only when it stands on its own: an instruction for one of the open items, new work with a clear object, a status question, a setting.".into());
    }
    if !recent.is_empty() {
        lines.push("Recent conversation, oldest first:".into());
        lines.extend(recent);
    }
    match texts {
        [text] => lines.push(format!("User message: {}", js_json(text))),
        _ => {
            lines.push("The user sent these messages in a row, oldest first. Decide each one on its own; a later one may build on an earlier one:".into());
            lines.extend(
                texts
                    .iter()
                    .enumerate()
                    .map(|(n, t)| format!("  {}: {}", n + 1, js_json(t))),
            );
        }
    }
    lines.push(if promote {
        "The user pressed “做成事项” on Steve's last answer: kind is \"item\"; goal is the work this conversation is about, drawn from the question and Steve's answer.".into()
    } else {
        "If the message only confirms or picks up something Steve proposed in its last reply (e.g. “按建议”, “就这么办”, “好，做吧”, “改吧”, “可以”, “第二个”), it is about that proposal: when the proposal is work, kind is \"item\" with signal \"explicit\" and goal restates the proposed work completely from Steve's reply; when it is a change of Steve's own settings or bots (new bots, their roles and rules), kind is \"config\".".into()
    });
    lines.push(if texts.len() == 1 {
        "Reply with ONLY the JSON object described in the instructions.".into()
    } else {
        format!(
            "Reply with ONLY a JSON array of {} objects, one per message in the same order, each the JSON object described in the instructions.",
            texts.len()
        )
    });
    head.retain(|l| !l.is_empty());
    lines.retain(|l| !l.is_empty());
    with_static(&head.join("\n"), &lines.join("\n"))
}

/// Build the stable routing instructions and workspace catalog before conversation-specific context.
/// Bot rosters are deliberately omitted: `Route` does not select a Bot, and keeping bots out of
/// the static head saves per-message tokens and preserves prompt-cache stability across bot edits.
fn route_instructions(c: &RouteCtx) -> Vec<String> {
    let mut head: Vec<String> = vec![
        ROUTE_RULES.trim_end().to_string(),
        "For item proposals, use task tags that describe the work and do not assign or name a Bot in the reply.".into(),
    ];
    head.push("Projects (id: name — description · default bot). For an item: the project whose directory the work happens in; for research: the project to read; null if none fits:".into());
    head.extend(c.projects.iter().map(|p| {
        format!(
            "  {}: {} — {}{}",
            p.id,
            p.name,
            p.description,
            p.default_bot
                .as_deref()
                .map(|b| format!(" · {b}"))
                .unwrap_or_default()
        )
    }));
    if c.operator {
        head.push("In this conversation Steve also acts on the machine the Hub runs on, with the owner's own access (files anywhere, commands, the network). Looking at, organising, installing or checking things on this machine (\"看下本机的目录\", \"把下载目录里的截图归到一个文件夹\", \"装一下 ripgrep\", \"磁盘还剩多少\", \"上网搜一下…\") is research: Steve does it right in the conversation, never an item and never suggest. Changing a project's code or documents is still an item.".into());
    }
    if let Some(s) = &c.steve_project {
        head.push(format!("Anything about Steve itself — its chat page, web UI, cards, Hub, bots, knowledge system, releases, design — is in project {s}."));
    }
    let (kinds, domains): (Vec<&Tag>, Vec<&Tag>) = c
        .tags
        .iter()
        .partition(|t| t.cat == steve_store::kinds::KIND_CAT);
    if !kinds.is_empty() {
        head.push("Item category, required for an item: exactly ONE of these tag ids, first in tags, chosen by the item's main acceptance goal (a fix is bug, new or better behaviour is feat, a plan or explanation is doc, deploying or configuring is ops; chore only for upkeep, never as a default):".into());
        head.extend(
            kinds
                .iter()
                .map(|t| format!("  {}: {} · {}", t.id, t.name, t.description)),
        );
    }
    head.push(format!(
        "Domain tags (id: name · category · description), for items only, added after the category. Choose existing tags that fit; only if none of the non-project tags fits, propose ONE new tag with category in {} and a concrete description:",
        TAG_CATS.join("/")
    ));
    head.extend(
        domains
            .iter()
            .map(|t| format!("  {}: {} · {} · {}", t.id, t.name, t.cat, t.description)),
    );
    head.push("goal: the task, self-contained, in the user language, written as short Markdown a person can scan: first one sentence saying what will be done and what counts as done; then only the sections that apply, each a few short bullets: **背景** (why, at most two lines), **要做**, **验收** (how to check it is done), **不做/约束**. Keep it under about 12 lines; never paste a wall of text, logs or file lists into it (the conversation keeps those). For a short ordinary message whose meaning is plain, the goal may be that one sentence. title: names the work concretely, never just “按建议” or “好的”.".into());
    head.push("Reply with ONLY one JSON object: {\"kind\":\"answer\"|\"research\"|\"item\"|\"suggest\"|\"ask\"|\"supplement\"|\"status\"|\"config\"|\"reroute\",\"confidence\":0.0-1.0,\"signals\":[\"explicit\"|\"change\"|\"deliverable\"|\"long\"|\"question\"|\"lookup\"|\"vague\"],\"offer\":{\"title\":\"\",\"goal\":\"\"} or null,\"item\":\"<item id or null>\",\"options\":[\"\"],\"title\":\"<= 24 chars, user language\",\"goal\":\"\",\"project\":\"<project id or null>\",\"tags\":[\"<tag id>\"],\"newTags\":[{\"name\":\"\",\"cat\":\"\",\"description\":\"\"}],\"reply\":\"one short sentence in the user language, without tags\",\"why\":\"one short sentence in the user language: why this kind\"}".into());
    head
}

static TAG_NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*标签[：:][^。]*(。|$)").expect("valid regex"));

/// The router's reply without the tag note the code appends itself (“标签：前端。” once, not twice).
pub fn strip_tag_note(reply: &str) -> String {
    TAG_NOTE.replace_all(reply, "").trim().to_string()
}

/// An `{title, goal}` object as an [`Offer`]; `None` without a title.
pub fn offer_of(o: &Value) -> Option<Offer> {
    if !o.is_object() {
        return None;
    }
    let title = o.get("title").map(jstr).unwrap_or_default();
    let title = title.trim();
    (!title.is_empty()).then(|| Offer {
        title: js_head(title, 40).to_string(),
        goal: match o.get("goal").map(jstr) {
            Some(g) if !g.trim().is_empty() => g.trim().to_string(),
            _ => title.to_string(),
        },
    })
}

/// The answer without its closing `OFFER: {…}` line, and the offer it names.
pub fn split_offer(text: &str) -> (String, Option<Offer>) {
    let t = text.trim_end();
    let Some(at) = t.rfind("OFFER:") else {
        return (t.to_string(), None);
    };
    let line_start = t[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if !t[line_start..at].trim().is_empty() {
        return (t.to_string(), None);
    }
    let offer = serde_json::from_str::<Value>(t[at + "OFFER:".len()..].trim())
        .ok()
        .as_ref()
        .and_then(offer_of);
    match offer {
        Some(o) => (t[..line_start].trim_end().to_string(), Some(o)),
        None => (t.to_string(), None),
    }
}

/// Validate a router reply against what exists; `None` when it is unusable (the caller falls back).
pub fn parse_route(c: &RouteCtx, text: &str, raw: &str, promote: bool) -> Option<Route> {
    parse_route_value(c, text, &first_json(raw).ok()??, promote)
}

/// The routes of several messages from one reply ([`route_prompt_for`]): a JSON array with one usable
/// object per message, in order; `None` when any is missing or unusable (each is then routed alone).
pub fn parse_routes(c: &RouteCtx, texts: &[String], raw: &str) -> Option<Vec<Route>> {
    let (from, to) = (raw.find('[')?, raw.rfind(']')?);
    let a: Vec<Value> = serde_json::from_str(raw.get(from..=to)?).ok()?;
    if a.len() != texts.len() {
        return None;
    }
    texts
        .iter()
        .zip(&a)
        .map(|(t, r)| parse_route_value(c, t, r, false))
        .collect()
}

fn parse_route_value(c: &RouteCtx, text: &str, r: &Value, promote: bool) -> Option<Route> {
    let kind = match r.get("kind").and_then(Value::as_str)? {
        k if KINDS.contains(&k) => k,
        _ => return None,
    };
    let kind = if promote { "item" } else { kind };
    let str_of = |k: &str| match r.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => Some(jstr(v)).filter(|s| !s.trim().is_empty()),
    };
    /* the item a supplement, status or reroute is about must be one of this workspace's */
    let item = str_of("item").map(|s| s.trim().trim_start_matches('#').to_uppercase());
    let item = match item {
        Some(i) if c.items.contains(&i) => Some(i),
        Some(_) if matches!(kind, "supplement" | "status") => return None,
        None if matches!(kind, "supplement" | "status") => return None,
        _ => None,
    };
    let project = r
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| c.projects.iter().any(|x| x.id == *p))
        .map(String::from);
    let s_or = |k: &str| match r.get(k) {
        Some(v) if truthy(v) => jstr(v),
        _ => text.to_string(),
    };
    let tag_ids = route_tag_ids(c, r.get("tags"))?;
    let new_tags = route_new_tags(r.get("newTags"))?;
    let confidence = r
        .get("confidence")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(0.5);
    let signals = match r.get("signals") {
        Some(Value::Array(a)) => {
            let mut v: Vec<String> = vec![];
            for s in a.iter().filter_map(Value::as_str) {
                if SIGNALS.contains(&s) && !v.iter().any(|x| x == s) {
                    v.push(s.into());
                }
            }
            v
        }
        _ => vec![],
    };
    let offer = r.get("offer").and_then(offer_of);
    let options = match r.get("options") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(3)
            .map(String::from)
            .collect(),
        _ => vec![],
    };
    Some(Route {
        kind: kind.into(),
        confidence,
        signals,
        offer,
        item,
        options,
        title: js_head(&s_or("title"), 40).into(),
        goal: s_or("goal"),
        project,
        tag_ids,
        new_tags,
        reply: match r.get("reply") {
            None | Some(Value::Null) => String::new(),
            Some(v) => strip_tag_note(&jstr(v)),
        },
        why: match r.get("why") {
            None | Some(Value::Null) => String::new(),
            Some(v) => js_head(&jstr(v), 200).to_string(),
        },
        model: None,
        fallback: false,
        /* asked and not said: it waits, the safe side */
        after: r
            .get("after")
            .and_then(Value::as_bool)
            .unwrap_or(!c.pending.is_empty()),
    })
}

/// Validate and normalize the router's tags list using its original ordering rules.
fn route_tag_ids(c: &RouteCtx, value: Option<&Value>) -> Option<Vec<String>> {
    Some(match value {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => {
            /* known tags once each, and one category at most: the first named */
            let mut v: Vec<String> = vec![];
            let mut kind = false;
            for x in a.iter().filter_map(Value::as_str) {
                let Some(t) = c.tags.iter().find(|t| t.id == x) else {
                    continue;
                };
                let is_kind = t.cat == steve_store::kinds::KIND_CAT;
                if v.iter().any(|y| y == x) || (is_kind && kind) {
                    continue;
                }
                kind |= is_kind;
                if is_kind {
                    v.insert(0, x.into());
                } else {
                    v.push(x.into());
                }
            }
            v
        }
        Some(_) => return None,
    })
}

/// Validate and normalize the router's newTags list using its original ordering rules.
fn route_new_tags(value: Option<&Value>) -> Option<Vec<NewTag>> {
    Some(match value {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .filter(|t| {
                t.get("name").is_some_and(truthy)
                    && t.get("cat")
                        .and_then(Value::as_str)
                        .is_some_and(|x| TAG_CATS.contains(&x))
                    && t.get("description").is_some_and(truthy)
            })
            .take(1)
            .map(|t| NewTag {
                name: jstr(&t["name"]),
                cat: jstr(&t["cat"]),
                description: jstr(&t["description"]),
            })
            .collect(),
        Some(_) => return None,
    })
}

/// Route a burst in one call. Failure uses the same safe deterministic route as a single
/// request; do not replay the failed batch as individual model calls.
pub async fn route_many(
    gw: &dyn ModelGateway,
    c: &RouteCtx,
    texts: &[String],
    history: &[Said],
) -> Vec<Route> {
    let prompt = route_prompt_for(c, texts, history, false);
    let routed = match gw
        .call(&prompt, Tier::Fast, ROUTE_TIMEOUT, &ThinkTools::None)
        .await
    {
        Ok(Printed {
            text: raw, model, ..
        }) => received_routes(c, texts, &raw, model),
        Err(e) => {
            tracing::error!("route batch failed {e}");
            None
        }
    };
    routed.unwrap_or_else(|| {
        texts
            .iter()
            .map(|text| route_without_model(c, text, false))
            .collect()
    })
}

/// Interpret one successful batch reply and attach its answering model after validation.
fn received_routes(c: &RouteCtx, texts: &[String], raw: &str, model: String) -> Option<Vec<Route>> {
    if c.debug {
        tracing::info!("route batch raw: {raw}");
    }
    let rs = parse_routes(c, texts, raw);
    if rs.is_none() {
        tracing::warn!("route batch unusable: {}", js_head(raw, 200));
    }
    rs.map(|rs| {
        rs.into_iter()
            .map(|r| Route {
                model: Some(model.clone()),
                ..r
            })
            .collect()
    })
}

/// Route a message; any failure gives [`Route::fallback`], which never creates an item. Where Steve
/// operates the machine ([`RouteCtx::operator`]) the fallback is research even with no project: the
/// message may ask for something to be done there, and an answer has no tools to do it with.
pub async fn route(
    gw: &dyn ModelGateway,
    c: &RouteCtx,
    text: &str,
    history: &[Said],
    promote: bool,
) -> Route {
    let prompt = route_prompt(c, text, history, promote);
    match gw
        .call(&prompt, Tier::Fast, ROUTE_TIMEOUT, &ThinkTools::None)
        .await
    {
        Ok(Printed {
            text: raw, model, ..
        }) => {
            if let Some(r) = received_route(c, text, &raw, model, promote) {
                return r;
            }
        }
        Err(e) => tracing::error!("route failed {e}"),
    }
    route_without_model(c, text, promote)
}

fn route_without_model(c: &RouteCtx, text: &str, promote: bool) -> Route {
    let project = c.steve_project.clone().or_else(|| {
        c.projects
            .iter()
            .find(|p| !p.archived)
            .map(|p| p.id.clone())
    });
    let mut r = Route::fallback(text, project);
    if c.operator && r.kind == "answer" {
        r.kind = "research".into();
    }
    if promote {
        /* “做成事项” was pressed: the user asked for an item, the router only names it */
        r.kind = "item".into();
        r.project = None;
    }
    r
}

/// Interpret one successful routing reply; unusable output remains a caller-owned fallback.
fn received_route(
    c: &RouteCtx,
    text: &str,
    raw: &str,
    model: String,
    promote: bool,
) -> Option<Route> {
    if c.debug {
        tracing::info!("route raw: {raw}");
    }
    if let Some(r) = parse_route(c, text, raw, promote) {
        return Some(Route {
            model: Some(model),
            ..r
        });
    }
    tracing::warn!("route unusable: {}", js_head(raw, 200));
    None
}

/// Attachments of a chat message, as copied next to the print-mode run (its cwd is the scratch dir).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatFiles {
    /// how many the user attached
    pub count: usize,
    /// the path note, or `\n（附件没能读取：…）` when copying failed
    pub note: String,
    /// copied files that are not inline images: the model reads them with the Read tool only
    pub readable: Vec<Readable>,
    /// images that go into the message itself (`{"type":"image","source":{…}}`)
    pub images: Vec<Value>,
}

/// How Steve may draw an interface in an answer of the main conversation (ST-46): the chat
/// answer and in-place research of the web conversation get it; an IM conversation's answers never do.
pub const A2UI_HINT: &str = include_str!("a2ui_hint.txt");

/// The chat-answer prompt (gateway.ts, with the “做成事项” hint replaced by an offer). `extra` is module context (B1 `PromptContext::chat`),
/// inserted after the main bot's part and before the history; `""` leaves the prompt as in TS.
pub fn answer_prompt(
    main: &Bot,
    text: &str,
    history: &[Said],
    files: &ChatFiles,
    extra: &str,
) -> String {
    let skip = history.len().saturating_sub(ANSWER_HISTORY_MAX);
    let ctx = history[skip..]
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                if m.author == "me" { "User" } else { "Steve" },
                m.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let read = match (
        files.count > 0,
        !files.readable.is_empty(),
        files.images.is_empty(),
    ) {
        (false, ..) => "",
        (true, true, true) => "Read the attached files with the Read tool before answering.\n\n",
        (true, true, false) => {
            "The attached images are in this message; read the other attached files with the Read tool before answering.\n\n"
        }
        (true, false, _) => {
            "The attached images are in this message: look at them before answering.\n\n"
        }
    };
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("{extra}\n\n")
    };
    // the bot and the reply format are the cached head; module context and the conversation change
    with_static(
        &format!(
            "You are {}, the user's assistant. {} {}\nAnswer in the user's language, briefly. You answer in the conversation: you don't change anything, change settings or start work from this reply, so never claim you did. Steve can change its own settings when the user says the change itself in the conversation (e.g. “把免打扰改成 22:00 – 09:00”, “停用写作 Bot”, “加一个验收 Bot”): for supported settings, it drafts a card the user applies and can undo. Native harness approval (YOLO) is changed only by the workspace owner in More; it cannot be set from chat. Asked whether or how settings can be changed, say so and give an example sentence; never claim it is impossible. When the user asks for a change or reports a problem, give a concrete plan right away; never ask whether to plan here or make it an item. If the answer leads to a change worth making, put it in \"offer\" (the user can make it a tracked item with one click); otherwise leave offer null.\n{}\nReply with ONLY one JSON object: {{\"thought\":\"2-4 short sentences in the user's language: what the user is asking and how you approach the answer\",\"answer\":\"the answer itself (markdown allowed)\",\"offer\":{{\"title\":\"<= 24 chars\",\"goal\":\"the work, self-contained\"}} or null}}",
            main.name,
            main.persona,
            main.rules,
            A2UI_HINT.trim_end()
        ),
        &format!(
            "{read}{extra}Recent conversation:\n{ctx}\n\nUser: {text}{}",
            files.note
        ),
    )
}

/// A direct answer, with a short account of how Steve got there (shown folded above the answer).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answer {
    pub text: String,
    pub thought: String,
    /// work the answer leads to
    pub offer: Option<Offer>,
    pub model: Option<String>,
    pub failed: bool,
    /// It failed because the question needs tools the answer does not have ([`steve_modules::gateway::GatewayError::NeedsTools`]).
    pub needs_tools: bool,
}

/// Told the reasoning and the answer so far while a chat answer is written.
pub type OnDraft<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// The reasoning and the answer so far in a chat answer still being written (see [`answer_prompt`]). A reply
/// that is not the JSON object asked for is the answer itself, as when it is done.
pub fn draft_of(raw: &str) -> (String, String) {
    let thought = partial_str(raw, "thought").unwrap_or_default();
    let answer = match partial_str(raw, "answer") {
        Some(a) => a,
        None if raw.trim_start().starts_with(['{', '`']) => String::new(),
        None => raw.to_string(),
    };
    (thought.trim().to_string(), answer.trim_start().to_string())
}

/// Answer a chat message; a failure becomes the reply text. `on_draft` hears the answer as it is written.
pub async fn answer(
    gw: &dyn ModelGateway,
    main: &Bot,
    text: &str,
    history: &[Said],
    files: &ChatFiles,
    context: &str,
    on_draft: OnDraft<'_>,
) -> Answer {
    let tools = answer_tools(files);
    let timeout = Duration::from_secs(if files.count > 0 { 150 } else { 90 });
    let on_text = |raw: &str| {
        let (t, a) = draft_of(raw);
        on_draft(&t, &a)
    };
    match with_images(
        files.images.clone(),
        gw.call_streaming(
            &answer_prompt(main, text, history, files, context),
            Tier::Smart,
            timeout,
            &tools,
            &on_text,
        ),
    )
    .await
    {
        Ok(Printed {
            text: raw, model, ..
        }) => {
            let r = first_json(&raw).ok().flatten();
            match r
                .as_ref()
                .and_then(|r| r.get("answer"))
                .and_then(Value::as_str)
                .filter(|a| !a.trim().is_empty())
            {
                Some(a) => {
                    let thought = match r.as_ref().and_then(|r| r.get("thought")) {
                        None | Some(Value::Null) => String::new(),
                        Some(v) => js_head(jstr(v).trim(), 1200).to_string(),
                    };
                    let (text, line) = split_offer(a.trim());
                    let offer = r
                        .as_ref()
                        .and_then(|r| r.get("offer"))
                        .and_then(offer_of)
                        .or(line);
                    Answer {
                        text,
                        thought,
                        offer,
                        model: Some(model),
                        failed: false,
                        needs_tools: false,
                    }
                }
                None => {
                    let (text, offer) = split_offer(raw.trim());
                    Answer {
                        text,
                        thought: String::new(),
                        offer,
                        model: Some(model),
                        failed: false,
                        needs_tools: false,
                    }
                }
            }
        }
        Err(e) => Answer {
            text: format!("（回答失败：{}）", js_head(&e.to_string(), 80)),
            thought: String::new(),
            offer: None,
            model: None,
            failed: true,
            needs_tools: gateway_error(e).can_handoff(),
        },
    }
}

pub use steve_modules::changes::FieldSpec;

/// Map a request onto fields; returns `(key, value)` for known keys only.
pub async fn draft_fields(
    gw: &dyn ModelGateway,
    target: &str,
    fields: &[FieldSpec],
    request: &str,
) -> Result<Vec<(String, Value)>> {
    let mut lines = vec![
        format!(
            "You configure \"{target}\" in Steve, a personal assistant app. Map the user's request onto these fields. Change only what the request asks for."
        ),
        "Fields (key · label · type · allowed values · current value):".to_string(),
    ];
    for f in fields {
        let vals = f
            .values
            .as_ref()
            .map(|v| format!(" · {}", v.join("|")))
            .unwrap_or_default();
        lines.push(format!(
            "  {} · {} · {}{} · {}",
            f.key, f.label, f.ty, vals, f.current
        ));
    }
    lines.push("For free-text fields, write the full new value in the user's language (append to the current text if the request adds a rule).".into());
    lines.push("Reply with ONLY JSON: {\"changes\":[{\"key\":\"\",\"value\":...}]}".into());
    let prompt = with_static(&lines.join("\n"), &format!("Request: {}", js_json(request)));
    let raw = gw
        .print(&prompt, Tier::Smart, Duration::from_secs(90))
        .await?;
    let r = first_json(&raw)?;
    let Some(Value::Array(cs)) = r.as_ref().and_then(|r| r.get("changes")) else {
        return Ok(vec![]);
    };
    Ok(cs
        .iter()
        .filter_map(|c| {
            let k = c.get("key")?.as_str()?;
            fields.iter().any(|f| f.key == k).then(|| {
                (
                    k.to_string(),
                    c.get("value").cloned().unwrap_or(Value::Null),
                )
            })
        })
        .collect())
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use steve_node_api::{Epoch, Measurement, Money, Tokens, UsageScope};

    fn report(
        id: &str,
        measurement: Measurement,
        output: Option<u64>,
        cost: Option<&str>,
    ) -> UsageReport {
        UsageReport {
            report_id: id.into(),
            model: Some("model".into()),
            meter_epoch: Epoch::new("meter").unwrap(),
            scope: UsageScope::Main,
            measurement,
            tokens: Tokens {
                output,
                ..Tokens::default()
            },
            cost: cost.map(|decimal| Money {
                decimal: decimal.into(),
                currency: "USD".into(),
            }),
            context_used: None,
            context_limit: None,
        }
    }

    #[test]
    fn tried_accepts_raw_reports_and_already_normalized_reports_equally() {
        let mut restart = report("restart", Measurement::Delta, Some(3), Some("0.03"));
        restart.meter_epoch = Epoch::new("next").unwrap();
        let reports = vec![
            report("delta", Measurement::Delta, Some(4), Some("0.1")),
            report("live", Measurement::Cumulative, Some(9), Some("0.2")),
            report("end", Measurement::Final, Some(12), Some("0.25")),
            report("end", Measurement::Final, Some(12), Some("0.25")),
            report("stale", Measurement::Cumulative, Some(2), Some("0.01")),
            restart,
        ];
        let raw = Tried::of(Ok(("ok".into(), None)), reports.clone());
        let normalized = Tried::of(Ok(("ok".into(), None)), merged_usage(reports, "model"));
        assert_eq!(raw.usage, normalized.usage);
        assert_eq!(
            raw.usage.len(),
            1,
            "epochs of one model are one accounting row"
        );
        assert_eq!(raw.usage[0].tokens.output, Some(15));
        assert_eq!(raw.usage[0].cost_reported, Some(0.28));
        assert_eq!(raw.reply.unwrap().1, Some(0.28));
    }

    #[test]
    fn cumulative_and_final_replace_deltas_field_by_field_and_ignore_replays() {
        let mut delta = report("d1", Measurement::Delta, Some(4), Some("0.1"));
        delta.tokens.input = Some(8);
        delta.tokens.cache_read = Some(2);
        let mut live = report("live", Measurement::Cumulative, Some(9), Some("0.2"));
        live.tokens.input = Some(10);
        let mut end = report("end", Measurement::Final, Some(0), None);
        end.tokens.reasoning = Some(0);
        let rows = merged_usage(
            [
                delta.clone(),
                delta,
                live.clone(),
                live,
                end.clone(),
                end,
                report("late", Measurement::Delta, Some(10), Some("1")),
            ],
            "model",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].tokens,
            Tokens {
                input: Some(10),
                output: Some(0),
                cache_read: Some(2),
                cache_write: None,
                reasoning: Some(0),
            }
        );
        assert_eq!(rows[0].cost.as_ref().unwrap().decimal, "0.2");
    }

    #[test]
    fn context_only_and_cost_only_reports_do_not_reset_token_counts_or_finalize_them() {
        let mut context = report("context", Measurement::Final, None, None);
        context.context_used = Some(100);
        let rows = merged_usage(
            [
                report("d1", Measurement::Delta, Some(4), None),
                context,
                report("cost", Measurement::Cumulative, None, Some("0.2")),
                report("d2", Measurement::Delta, Some(6), None),
                report("end", Measurement::Final, Some(12), None),
            ],
            "model",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens.output, Some(12));
        assert_eq!(rows[0].tokens.input, None);
        assert_eq!(rows[0].context_used, Some(100));
        assert_eq!(rows[0].cost.as_ref().unwrap().decimal, "0.2");
    }

    #[test]
    fn all_is_selected_per_model_and_epoch_with_main_as_a_field_fallback() {
        let mut main = report("main", Measurement::Final, Some(20), Some("0.2"));
        main.tokens.input = Some(10);
        let mut all = report("all", Measurement::Final, Some(25), None);
        all.scope = UsageScope::All;
        let mut other_model = report("other-model", Measurement::Final, Some(7), Some("0.07"));
        other_model.model = Some("other".into());
        let mut other_epoch = report("other-epoch", Measurement::Final, Some(3), Some("0.03"));
        other_epoch.meter_epoch = Epoch::new("next").unwrap();
        let rows = merged_usage([main, all, other_model, other_epoch], "model");
        assert_eq!(
            rows.len(),
            3,
            "main/all of one model and epoch share one bill"
        );
        let all = rows.iter().find(|r| r.scope == UsageScope::All).unwrap();
        assert_eq!((all.tokens.input, all.tokens.output), (Some(10), Some(25)));
        assert_eq!(all.cost.as_ref().unwrap().decimal, "0.2");
        assert!(
            rows.iter()
                .any(|r| r.model.as_deref() == Some("other") && r.tokens.output == Some(7))
        );
        assert!(
            rows.iter()
                .any(|r| r.meter_epoch.as_str() == "next" && r.tokens.output == Some(3))
        );
        assert_eq!(
            merged_usage(rows.clone(), "model"),
            rows,
            "normalization is idempotent"
        );
    }

    #[test]
    fn switching_scopes_across_epochs_does_not_create_an_extra_model_call() {
        let mut all = report("all", Measurement::Final, Some(25), None);
        all.scope = UsageScope::All;
        let mut restart = report("restart", Measurement::Final, Some(3), Some("0.03"));
        restart.meter_epoch = Epoch::new("next").unwrap();
        let tried = Tried::of(
            Ok(("ok".into(), None)),
            vec![
                report("main", Measurement::Final, Some(20), Some("0.2")),
                all,
                restart,
            ],
        );
        assert_eq!(tried.usage.len(), 1);
        assert_eq!(tried.usage[0].tokens.output, Some(28));
        assert_eq!(tried.usage[0].cost_reported, Some(0.23));
        assert_eq!(tried.usage[0].detail, json!({"scope": "all"}));
    }

    #[test]
    fn choosing_all_preserves_the_first_model_for_call_metadata() {
        let mut other = report("other", Measurement::Final, Some(7), None);
        other.model = Some("other".into());
        let mut all = report("all", Measurement::Final, Some(25), None);
        all.scope = UsageScope::All;
        let tried = Tried::of(
            Ok(("ok".into(), None)),
            vec![
                report("main", Measurement::Final, Some(20), None),
                other,
                all,
            ],
        );
        assert_eq!(
            tried
                .usage
                .iter()
                .map(|u| (u.model.as_str(), u.tokens.output))
                .collect::<Vec<_>>(),
            [("model", Some(25)), ("other", Some(7)),]
        );
    }

    #[test]
    fn empty_or_cost_only_all_does_not_erase_main_tokens_and_background_is_not_this_attempt() {
        for (output, cost, want_output, want_cost) in [
            (None, None, Some(20), "0.2"),
            (None, Some("0.3"), Some(20), "0.3"),
            (Some(0), Some("0"), Some(0), "0"),
        ] {
            let mut all = report("all", Measurement::Final, output, cost);
            all.scope = UsageScope::All;
            let mut background = report("background", Measurement::Final, Some(99), Some("9"));
            background.scope = UsageScope::Background;
            let rows = merged_usage(
                [
                    report("main", Measurement::Final, Some(20), Some("0.2")),
                    all,
                    background,
                ],
                "model",
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].tokens.output, want_output);
            assert_eq!(rows[0].cost.as_ref().unwrap().decimal, want_cost);
        }
    }

    #[test]
    fn missing_models_are_filled_before_selecting_scopes() {
        let mut main = report("main", Measurement::Final, Some(20), None);
        main.model = None;
        let mut all = report("all", Measurement::Final, Some(25), None);
        all.scope = UsageScope::All;
        let rows = merged_usage([main, all], "model");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens.output, Some(25));
    }

    #[test]
    fn invalid_delta_cost_does_not_become_a_reported_zero() {
        let rows = merged_usage(
            [
                report("d1", Measurement::Delta, Some(0), Some("invalid")),
                report("d2", Measurement::Delta, None, Some("0")),
            ],
            "model",
        );
        assert_eq!(model_usage(rows[0].clone()).cost_reported, None);
        for decimal in ["NaN", "inf", "-inf"] {
            assert_eq!(
                cost_f64(&Money {
                    decimal: decimal.into(),
                    currency: "USD".into()
                }),
                None
            );
        }
    }

    #[test]
    fn delta_snapshots_are_idempotent_even_with_context_and_cost_only_updates() {
        let reports = vec![
            report("d1", Measurement::Delta, Some(4), None),
            report("context", Measurement::Cumulative, None, None),
            report("d2", Measurement::Delta, Some(6), None),
            report("cost", Measurement::Cumulative, None, Some("0.1")),
        ];
        let rows = merged_usage(reports.clone(), "model");
        assert_eq!(rows[0].tokens.output, Some(10));
        assert_eq!(rows[0].tokens.input, None);
        assert_eq!(rows[0].measurement, Measurement::Cumulative);
        assert_eq!(merged_usage(rows.clone(), "model"), rows);
        assert_eq!(
            Tried::of(Ok(("ok".into(), None)), reports).usage,
            Tried::of(Ok(("ok".into(), None)), rows).usage,
        );
        let unknown_cost = vec![report("bad", Measurement::Delta, None, Some("invalid"))];
        let rows = merged_usage(unknown_cost.clone(), "model");
        assert!(rows.is_empty());
        assert_eq!(
            Tried::of(Ok(("ok".into(), None)), unknown_cost).usage,
            Tried::of(Ok(("ok".into(), None)), rows).usage,
        );
    }

    #[test]
    fn invalid_or_mixed_currency_deltas_stay_unknown_until_a_valid_snapshot() {
        let mut foreign = report("foreign", Measurement::Delta, Some(2), Some("0.2"));
        foreign.cost.as_mut().unwrap().currency = "EUR".into();
        let reports = vec![
            report("d1", Measurement::Delta, Some(4), Some("0.1")),
            foreign,
            report("d2", Measurement::Delta, Some(3), Some("0")),
        ];
        let rows = merged_usage(reports.clone(), "model");
        assert_eq!(rows[0].tokens.output, Some(9));
        assert_eq!(rows[0].cost, None);
        let rows = merged_usage(
            reports
                .into_iter()
                .chain([report("end", Measurement::Final, Some(10), Some("0.4"))]),
            "model",
        );
        assert_eq!(rows[0].cost.as_ref().unwrap().decimal, "0.4");
    }

    #[test]
    fn failed_retries_of_the_same_model_are_added_after_each_attempt_is_settled() {
        let mut total = vec![];
        for _ in 0..2 {
            let tried = Tried::of(
                Err(OneShotError::Failed("failed".into())),
                vec![
                    report("live", Measurement::Cumulative, Some(4), Some("0.04")),
                    report("end", Measurement::Final, Some(6), Some("0.06")),
                ],
            );
            merge_usage(&mut total, tried.usage);
        }
        assert_eq!(total.len(), 1);
        assert_eq!(total[0].tokens.output, Some(12));
        assert_eq!(total[0].tokens.input, None);
        assert_eq!(total[0].cost_reported, Some(0.12));
    }

    #[test]
    fn cancellation_and_timeout_keep_consumed_usage_without_a_successful_reply() {
        for error in [
            OneShotError::Timeout(TimeoutPhase::Execution),
            OneShotError::Harness {
                text: "cancelled".into(),
                code: None,
                transient: false,
            },
        ] {
            let tried = Tried::of(
                Err(error),
                vec![
                    report("live", Measurement::Cumulative, Some(10), Some("0.1")),
                    report("end", Measurement::Final, Some(12), Some("0.12")),
                ],
            );
            assert!(tried.reply.is_err());
            assert_eq!(tried.usage.len(), 1);
            assert_eq!(tried.usage[0].tokens.output, Some(12));
            assert_eq!(tried.usage[0].cost_reported, Some(0.12));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(id: &str) -> Bot {
        Bot {
            id: id.into(),
            workspace_id: "default".into(),
            owner: None,
            name: id.into(),
            role: "r".into(),
            specialties: vec![],
            main: id == "steve",
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: steve_types::ReasoningEffort::Default,
            rules: String::new(),
            accept_mode: "inherit".into(),
            perm_mode: "ask".into(),
            enabled: true,
            revision: 1,
            updated: 0,
            persona: String::new(),
            proactive: 1,
            avatar: Default::default(),
            default_project: None,
            prompt: None,
            device: None,
        }
    }
    fn project(id: &str, default_bot: Option<&str>) -> Project {
        Project {
            id: id.into(),
            workspace_id: "default".into(),
            name: id.into(),
            description: String::new(),
            device_id: "hub".into(),
            path: "/x".into(),
            kind: "dir".into(),
            worktree: false,
            default_bot: default_bot.map(String::from),
            release: None,
            archived: false,
            created: 0,
            updated: 0,
        }
    }
    fn ctx() -> RouteCtx {
        RouteCtx {
            bots: vec![bot("steve"), bot("dev"), bot("writer")],
            projects: vec![project("prj_a", Some("dev")), project("prj_b", None)],
            tags: vec![
                Tag {
                    id: "tag_1".into(),
                    workspace_id: "default".into(),
                    name: "t".into(),
                    cat: "领域".into(),
                    description: "d".into(),
                    created_by: "你".into(),
                    created: 0,
                },
                Tag {
                    id: "tag_bug".into(),
                    workspace_id: "default".into(),
                    name: "bug".into(),
                    cat: "类型".into(),
                    description: "修复".into(),
                    created_by: "Steve".into(),
                    created: 0,
                },
                Tag {
                    id: "tag_feat".into(),
                    workspace_id: "default".into(),
                    name: "feat".into(),
                    cat: "类型".into(),
                    description: "新增".into(),
                    created_by: "Steve".into(),
                    created: 0,
                },
            ],
            steve_project: Some("prj_a".into()),
            ..Default::default()
        }
    }

    #[test]
    fn first_json_is_greedy() {
        assert_eq!(
            first_json("x {\"a\":{\"b\":1}} y").unwrap(),
            Some(json!({"a":{"b":1}}))
        );
        assert_eq!(first_json("none").unwrap(), None);
        assert!(first_json("{bad}").is_err());
    }

    #[test]
    fn route_validates_against_what_exists() {
        let c = ctx();
        let r = parse_route(&c, "做个东西", r#"{"kind":"item","title":"","bot":"ghost","project":"prj_x","tags":["tag_1","tag_9"],"newTags":[{"name":"x","cat":"类型","description":"d"},{"name":"n","cat":"领域","description":"d"},{"name":"m","cat":"领域","description":"d"}],"reply":"好"}"#, false).unwrap();
        assert_eq!(r.project, None);
        assert_eq!(r.title, "做个东西");
        assert_eq!(r.goal, "做个东西");
        assert_eq!(r.tag_ids, vec!["tag_1"]);
        // a new tag never goes under 类型: the first one that may is taken
        assert_eq!(
            r.new_tags.iter().map(|t| &*t.name).collect::<Vec<_>>(),
            ["n"]
        );
        // one category at most, the first named, put first
        let r = parse_route(
            &c,
            "修",
            r#"{"kind":"item","tags":["tag_1","tag_bug","tag_feat","tag_1"]}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.tag_ids, vec!["tag_bug", "tag_1"]);
        assert!(parse_route(&c, "x", r#"{"kind":"nope"}"#, false).is_none());
        assert!(parse_route(&c, "x", "no json", false).is_none());
    }

    #[test]
    fn route_prompt_marks_only_referenced_finished_items_as_context() {
        let mut c = ctx();
        c.items = vec![
            "ST-2".into(),
            "ST-20".into(),
            "ST-206".into(),
            "ST-99".into(),
        ];
        c.closed_items = c.items.clone();
        let history = [Said {
            author: "steve".into(),
            text: "st20 的工作已经结束".into(),
        }];
        let texts = [
            "根据 #st-206 的结果修复登录页".into(),
            "st-206 的进度怎么样了？".into(),
        ];
        let p = route_prompt_for(&c, &texts, &history, false);
        let (head, tail) = split_static(&p);
        assert!(!head.unwrap().contains("Closed items (context only"));
        assert!(
            tail.contains("Closed items (context only; never supplement these): ST-20, ST-206\n")
        );
        assert!(!tail.contains("ST-99"));
        // Finished ids remain valid for status; suppressing them at parsing would lose context.
        assert_eq!(
            parse_route(
                &c,
                "ST-206 进度怎么样了？",
                r#"{"kind":"status","item":"ST-206"}"#,
                false
            )
            .unwrap()
            .kind,
            "status"
        );
    }

    #[test]
    fn route_prompt_has_context() {
        let c = ctx();
        let h = vec![
            Said {
                author: "me".into(),
                text: "问".into(),
            },
            Said {
                author: "steve".into(),
                text: "答".into(),
            },
        ];
        let p = route_prompt(&c, "按建议", &h, false);
        assert!(p.contains("Recent conversation, oldest first:\n  User: \"问\"\n  Steve: \"答\""));
        assert!(p.contains("is in project prj_a."));
        assert!(p.contains("User message: \"按建议\""));
        // the workspace part is the cached head, the conversation comes after it
        let (head, tail) = split_static(&p);
        let head = head.unwrap();
        assert!(head.starts_with(ROUTE_RULES.trim_end()));
        assert!(
            head.contains("is in project prj_a.")
                && head.contains("Reply with ONLY one JSON object")
        );
        assert!(!head.contains("User message:") && tail.contains("User message: \"按建议\""));
        assert!(!head.contains("\n\n") && !tail.contains("\n\n"));
        // the same workspace gives the same head whatever the message
        assert_eq!(
            split_static(&route_prompt(&c, "别的", &[], true)).0,
            Some(head)
        );
    }

    fn readable(id: &str, name: &str) -> Readable {
        Readable {
            file_id: id.into(),
            rel_path: format!(".steve/attachments/{}-{name}", &id[4..]),
        }
    }

    #[test]
    fn an_answer_reading_attachments_has_read_and_nothing_else() {
        let none = ChatFiles::default();
        assert_eq!(
            answer_tools(&none),
            ThinkTools::None,
            "no attachments: no tools"
        );
        // attached, but only inline images: nothing to read, still no tools
        let images = ChatFiles {
            count: 1,
            images: vec![json!({"type": "image"})],
            ..ChatFiles::default()
        };
        assert_eq!(answer_tools(&images), ThinkTools::None);
        let files = ChatFiles {
            readable: vec![readable("fil_a1", "log.txt"), readable("fil_b2", "big.png")],
            count: 3,
            images: vec![json!({"type": "image"})],
            ..ChatFiles::default()
        };
        let t = answer_tools(&files);
        assert_eq!(
            t,
            ThinkTools::Read(vec![
                Readable {
                    file_id: "fil_a1".into(),
                    rel_path: ".steve/attachments/a1-log.txt".into()
                },
                Readable {
                    file_id: "fil_b2".into(),
                    rel_path: ".steve/attachments/b2-big.png".into()
                },
            ])
        );
        assert!(t.any() && !ThinkTools::None.any());
    }

    #[test]
    fn a_reply_being_written_is_read_as_far_as_it_goes() {
        let full = "```json\n{\"thought\":\"用户问\\\"你好\\\"\",\"answer\":\"你好\\n**呀** \\ud83d\\ude00\"}\n```";
        let mut seen = vec![];
        for n in (0..=full.len()).filter(|n| full.is_char_boundary(*n)) {
            seen.push(draft_of(&full[..n]));
        }
        assert_eq!(seen[0], (String::new(), String::new()));
        assert!(seen.contains(&("用户".into(), String::new())));
        assert!(
            seen.contains(&("用户问\"".into(), String::new())),
            "an escape is decoded once whole"
        );
        assert!(
            seen.contains(&("用户问\"你好\"".into(), "你好\n**呀** ".into())),
            "a cut-off \\u escape or surrogate pair is left out"
        );
        assert_eq!(
            seen.last().unwrap(),
            &("用户问\"你好\"".into(), "你好\n**呀** 😀".into())
        );
        /* it only ever grows */
        for w in seen.windows(2) {
            assert!(w[1].1.starts_with(&w[0].1), "{:?} → {:?}", w[0], w[1]);
        }
        assert_eq!(
            partial_str(
                r#"{"thought":"说 \"answer\": \"x\"","answer": "y"#,
                "answer"
            )
            .as_deref(),
            Some("y")
        );
        assert_eq!(draft_of("直接回答"), (String::new(), "直接回答".into()));
    }

    /// What a failed call says: the adapter's words, cut.
    #[test]
    fn a_failed_run_says_why_in_steves_words() {
        let said = |e| call_error(e).to_string();
        assert_eq!(
            said(OneShotError::Timeout(TimeoutPhase::FirstByte)),
            "网关在 first_byte 阶段超时"
        );
        assert_eq!(
            said(OneShotError::Failed("nope".into())),
            "模型请求无法启动：nope"
        );
        let long = "x".repeat(400);
        assert_eq!(
            said(OneShotError::Failed(long.clone())).len(),
            "模型请求无法启动：".len() + 300
        );
        assert_eq!(
            said(OneShotError::Harness {
                text: long,
                code: None,
                transient: false
            })
            .len(),
            300
        );
        assert_eq!(
            said(OneShotError::Unsupported("images")),
            "unsupported: images"
        );
    }

    #[test]
    fn fallback_route_never_makes_an_item() {
        let r = Route::fallback(
            "一二三四五六七八九十一二三四五六七八九十一二三四五六七八",
            Some("prj_a".into()),
        );
        assert_eq!(r.title.chars().count(), 24);
        assert_eq!((r.kind.as_str(), r.fallback), ("research", true));
        assert_eq!(r.why, "没能判断，先在这里回答");
        assert_eq!(Route::fallback("x", None).kind, "answer");
    }

    #[tokio::test]
    async fn a_failing_or_unusable_router_answers_in_place() {
        struct Gw(Option<&'static str>);
        #[async_trait]
        impl ModelGateway for Gw {
            async fn call(&self, _: &str, _: Tier, _: Duration, _: &ThinkTools) -> Result<Printed> {
                match self.0 {
                    Some(t) => Ok(Printed {
                        text: t.into(),
                        model: "m".into(),
                        cost_usd: None,
                        ..Default::default()
                    }),
                    None => Err(msg("Command timed out after 60000 ms")),
                }
            }
        }
        let c = ctx();
        for gw in [
            Gw(None),
            Gw(Some("不是 JSON")),
            Gw(Some(r#"{"kind":"build"}"#)),
        ] {
            let r = route(&gw, &c, "知识系统怎么运作", &[], false).await;
            assert_eq!((r.kind.as_str(), r.fallback), ("research", true));
            assert_eq!(r.project.as_deref(), Some("prj_a"));
        }
        let none = RouteCtx {
            steve_project: None,
            projects: vec![],
            ..ctx()
        };
        assert_eq!(
            route(&Gw(None), &none, "hey", &[], false).await.kind,
            "answer"
        );
        /* the owner's own conversation can act on the machine: a run with tools, not a bare answer */
        let operator = RouteCtx {
            operator: true,
            ..none.clone()
        };
        let r = route(&Gw(None), &operator, "把下载目录整理一下", &[], false).await;
        assert_eq!(
            (r.kind.as_str(), r.fallback, r.project),
            ("research", true, None)
        );
        /* “做成事项” was pressed: still an item */
        assert_eq!(route(&Gw(None), &c, "x", &[], true).await.kind, "item");
    }

    #[test]
    fn route_reads_the_new_fields() {
        let mut c = ctx();
        c.items = vec!["ST-3".into()];
        let r = parse_route(&c, "x", r#"{"kind":"research","confidence":0.82,"signals":["question","lookup","nope","question"],"offer":{"title":"修评论渲染","goal":""},"project":"prj_a","bot":"steve"}"#, false).unwrap();
        assert_eq!(r.kind, "research");
        assert_eq!(r.confidence, 0.82);
        assert_eq!(r.signals, vec!["question", "lookup"]);
        assert_eq!(
            r.offer,
            Some(Offer {
                title: "修评论渲染".into(),
                goal: "修评论渲染".into()
            })
        );
        let r = parse_route(&c, "x", r#"{"kind":"answer"}"#, false).unwrap();
        assert_eq!((r.kind.as_str(), r.confidence), ("answer", 0.5));
        let r = parse_route(
            &c,
            "x",
            r##"{"kind":"supplement","item":"#st-3","confidence":2}"##,
            false,
        )
        .unwrap();
        assert_eq!((r.item.as_deref(), r.confidence), (Some("ST-3"), 1.0));
        assert!(
            parse_route(&c, "x", r#"{"kind":"supplement","item":"ST-9"}"#, false).is_none(),
            "another workspace's item"
        );
        assert!(parse_route(&c, "x", r#"{"kind":"status"}"#, false).is_none());
        let r = parse_route(&c, "x", r#"{"kind":"reroute","item":"ST-9"}"#, false).unwrap();
        assert_eq!(r.item, None);
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"ask","options":["对话页"," ","事项页","设置页","更多"]}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.options, vec!["对话页", "事项页", "设置页"]);
        for k in KINDS {
            assert!(
                parse_route(
                    &c,
                    "x",
                    &format!(r#"{{"kind":"{k}","item":"ST-3"}}"#),
                    false
                )
                .is_some()
            );
        }
    }

    #[test]
    fn the_tag_note_is_left_to_the_code() {
        assert_eq!(
            strip_tag_note("好，交给开发 Bot 了。 标签：前端。"),
            "好，交给开发 Bot 了。"
        );
        assert_eq!(
            strip_tag_note("好。标签：前端、发布交付（新建）。 标签：前端。"),
            "好。"
        );
        assert_eq!(strip_tag_note("标签：前端"), "");
        let r = parse_route(
            &ctx(),
            "x",
            r#"{"kind":"item","reply":"交给它了。 标签：前端。"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.reply, "交给它了。");
    }

    #[test]
    fn an_offer_line_is_taken_off_the_answer() {
        let (t, o) = split_offer(
            "结论在前。\n\n细节。\nOFFER: {\"title\":\"修评论\",\"goal\":\"让评论支持 markdown\"}\n",
        );
        assert_eq!(t, "结论在前。\n\n细节。");
        assert_eq!(o.unwrap().goal, "让评论支持 markdown");
        let (t, o) = split_offer("说到 OFFER: 这个词");
        assert_eq!((t.as_str(), o), ("说到 OFFER: 这个词", None));
        let (t, o) = split_offer("答\nOFFER: 不是 JSON");
        assert_eq!((t.as_str(), o), ("答\nOFFER: 不是 JSON", None));
    }

    #[test]
    fn the_router_hears_about_the_machine_where_steve_operates_it() {
        const MACHINE: &str = "acts on the machine the Hub runs on";
        let c = ctx();
        assert!(!c.operator);
        assert!(!route_prompt(&c, "看下本机的目录", &[], false).contains(MACHINE));
        let c = RouteCtx {
            operator: true,
            ..ctx()
        };
        let p = route_prompt(&c, "看下本机的目录", &[], false);
        assert!(p.contains(MACHINE), "{p}");
        assert!(
            p.contains("is research") && p.contains("is still an item"),
            "{p}"
        );
    }

    #[test]
    fn a_burst_is_one_prompt_and_one_array() {
        let c = ctx();
        let one = route_prompt(&c, "修登录页", &[], false);
        assert_eq!(one, route_prompt_for(&c, &["修登录页".into()], &[], false));
        let texts: Vec<String> = vec!["修登录页".into(), "加一个叫财务的标签".into()];
        let p = route_prompt_for(&c, &texts, &[], false);
        assert!(p.contains("  1: \"修登录页\"\n  2: \"加一个叫财务的标签\""));
        assert!(p.contains("a JSON array of 2 objects"));
        assert!(!p.contains("User message:"));
        let raw = r#"[{"kind":"item","confidence":0.9,"signals":["change"]},{"kind":"config","confidence":0.9}]"#;
        let rs = parse_routes(&c, &texts, &format!("好的：\n{raw}")).unwrap();
        assert_eq!(
            rs.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            ["item", "config"]
        );
        /* one missing or unusable: use deterministic routes for the batch */
        assert!(parse_routes(&c, &texts, r#"[{"kind":"item"}]"#).is_none());
        assert!(parse_routes(&c, &texts, r#"[{"kind":"item"},{"kind":"nope"}]"#).is_none());
        assert!(parse_routes(&c, &texts, r#"{"kind":"item"}"#).is_none());
    }

    #[test]
    fn route_prompt_lists_focus_items_and_research() {
        let mut c = ctx();
        c.focus = vec![FocusItem {
            id: "ST-21".into(),
            title: "改按钮".into(),
            status: "run".into(),
            origin: "帮我把按钮改了".into(),
        }];
        c.researching = true;
        let p = route_prompt(&c, "x", &[], false);
        assert!(p.starts_with("You are the router of Steve"));
        assert!(p.contains("Reading code, docs or the knowledge base to answer a question is research, not an item."));
        assert!(p.contains("  ST-21: 改按钮 · run · \"帮我把按钮改了\""));
        assert!(p.contains("Steve is reading for an earlier question right now."));
        assert!(p.contains("  prj_a: prj_a —  · dev"));
        assert!(!p.contains("Bots (id: role):"));
    }
    #[tokio::test]
    async fn shared_gateway_passes_images_and_clears_the_scope() {
        struct Capture;
        #[async_trait]
        impl ModelGateway for Capture {
            async fn call(&self, _: &str, _: Tier, _: Duration, _: &ThinkTools) -> Result<Printed> {
                let images = current_images().expect("image scope reaches actual model gateway");
                assert_eq!(images[0]["source"]["data"], "aW1hZ2U=");
                Ok(Printed::default())
            }
        }
        use steve_modules::Gateway;
        let g = SharedGateway(std::sync::Arc::new(Capture));
        let image = json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}});
        g.run_with_images(
            steve_modules::Tier::Smart,
            "read",
            &[image],
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(current_images().is_none());
    }

    #[tokio::test]
    async fn shared_gateway_passes_exclude_model_and_clears_scope() {
        struct Capture;
        #[async_trait]
        impl ModelGateway for Capture {
            async fn call(&self, _: &str, _: Tier, _: Duration, _: &ThinkTools) -> Result<Printed> {
                assert_eq!(
                    current_excluded_model().as_deref(),
                    Some("claude-sonnet-5-5")
                );
                Ok(Printed {
                    model: "claude-haiku-5-5".into(),
                    ..Printed::default()
                })
            }
        }
        use steve_modules::Gateway;
        let g = SharedGateway(std::sync::Arc::new(Capture));
        let out = g
            .run_excluding(
                steve_modules::Tier::Fast,
                "check",
                Some("claude-sonnet-5-5"),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(out.model, "claude-haiku-5-5");
        assert!(current_excluded_model().is_none());

        // SingleModelGateway also skips the excluded model on fixed tier lists.
        struct NoopOne;
        #[async_trait]
        impl OneModel for NoopOne {
            async fn once(
                &self,
                _: &OneCall<'_>,
                _: &str,
                _: Option<steve_types::ReasoningEffort>,
                _: Option<OnText<'_>>,
            ) -> Tried {
                unreachable!()
            }
        }
        let single = SingleModelGateway {
            one: NoopOne,
            fast: vec!["claude-sonnet-5-5".into(), "claude-haiku-5-5".into()],
            smart: vec![],
            models: None,
        };
        let picked = with_excluded_model(Some("claude-sonnet-5-5"), async {
            single.choice(Tier::Fast)
        })
        .await;
        assert_eq!(picked.models, vec!["claude-haiku-5-5"]);
    }
}
