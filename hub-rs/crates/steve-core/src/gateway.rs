//! Model gateway (gateway.ts): Steve's own short model calls through the harness CLI in print mode, with a
//! timeout per call and a fallback down each tier's model list. The router, chat answers and config drafts
//! are built here from data the caller collected under the core lock, so no lock is held while a model runs.

use crate::error::{CoreError, Result, msg};
use crate::repo::{Bot, Project, Tag};
use crate::util::{js_head, js_len};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
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
    pub model: String,
    /// The harness's `total_cost_usd` of the answering run, when it reports one (module budgets add it up).
    pub cost_usd: Option<f64>,
    /// Tokens per model over every model tried, failed tries included.
    pub usage: Vec<ModelUsage>,
    /// Models of the tier tried, the one that answered included.
    pub attempts: u32,
    /// Wall time of the whole call, fallbacks included.
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
    /// The harness the call ran on as the statistics name it (`claude-cli`, `codex-cli`); empty: `claude-cli`.
    pub harness: &'static str,
}

/// Told what the model has written so far in its current message, each time it grows.
pub type OnText<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// One model call with fallback; implemented by [`Fallback`] ([`ClaudeCli`] in process), faked in tests.
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
                harness: "",
                result: Ok(p),
            },
            Err(e) => Attempt {
                attempts: 1,
                duration_ms: ms,
                usage: vec![],
                model: String::new(),
                failures: vec![js_head(&e.to_string(), 200).to_string()],
                harness: "",
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
    /// The try ran out of its time: the next model is not tried (it would most likely wait as long).
    pub timed_out: bool,
}

impl Tried {
    /// A try that failed before the model ran (nothing spent).
    pub fn failed(e: CoreError) -> Tried {
        Tried {
            reply: Err(e),
            usage: vec![],
            timed_out: false,
        }
    }

    /// A [`steve_print`] run in the gateway's terms: its error in Steve's words, its usage as rows
    /// (`codex`: the run was Codex's). A run elsewhere (a Node job) is put back into these terms too.
    pub fn of(ran: steve_print::Ran, codex: bool) -> Tried {
        let timed_out = matches!(ran.reply, Err(steve_print::PrintError::Timeout(_)));
        Tried {
            timed_out,
            reply: ran
                .reply
                .map(|r| (r.text, r.cost.as_ref().and_then(steve_print::cost_f64)))
                .map_err(|e| print_error(e, codex)),
            usage: ran.usage.into_iter().map(model_usage).collect(),
        }
    }
}

/// What a failed print run says (`codex`: stderr is cut to 300 characters).
fn print_error(e: steve_print::PrintError, codex: bool) -> CoreError {
    use steve_print::PrintError as P;
    match e {
        P::OutputLimit => msg("输出超出这次运行的限制"),
        P::Timeout(t) => msg(format!("Command timed out after {} ms", t.as_millis())),
        P::Failed(err) if codex => msg(format!("Command failed: {}", js_head(&err, 300))),
        P::Failed(err) => msg(format!("Command failed: {err}")),
        P::Harness { text, .. } => msg(js_head(&text, 300).to_string()),
        P::CodexFailed => msg("Codex 失败"),
        P::NoResult => msg("harness 没有返回结果"),
        P::NoOutput => msg("harness 没有输出"),
        P::Io(e) => CoreError::Io(e),
    }
}

/// A usage report of a print run as the statistics' row (its dollars back to the same `f64`).
pub fn model_usage(r: steve_print::UsageReport) -> ModelUsage {
    use steve_print::UsageScope;
    let n = |x: Option<u64>| x.and_then(|x| i64::try_from(x).ok());
    let scope = match r.scope {
        UsageScope::All => "all",
        UsageScope::Main => "main",
        UsageScope::Background => "background",
    };
    ModelUsage {
        model: r.model.unwrap_or_default(),
        tokens: steve_store::Tokens {
            input: n(r.tokens.input),
            output: n(r.tokens.output),
            cache_read: n(r.tokens.cache_read),
            cache_write: n(r.tokens.cache_write),
            reasoning: n(r.tokens.reasoning),
        },
        cost_reported: r.cost.as_ref().and_then(steve_print::cost_f64),
        detail: json!({"scope": scope}),
    }
}

/// One think call as [`Fallback`] hands it to each model in turn: everything but the model and effort,
/// which the fallback chooses.
pub struct OneCall<'a> {
    pub prompt: &'a str,
    /// Per try, not for the whole fallback.
    pub timeout: Duration,
    pub tools: &'a ThinkTools,
    /// The purpose runs on Codex (设置 · 模型), else on Claude Code.
    pub codex: bool,
    /// The images of the call ([`with_images`]), the same for every model tried.
    pub images: &'a [steve_print::Image],
}

/// A single attempt of one model, with no fallback of its own: in process ([`LocalPrint`]), or elsewhere
/// (a Node). [`Fallback`] keeps the policy around it: which models, in which order, and what the
/// statistics see of the tries.
#[async_trait]
pub trait OneModel: Send + Sync {
    /// Runs `model` (empty: the harness's default) once; streaming when `on_text` is given.
    async fn once(
        &self,
        call: &OneCall<'_>,
        model: &str,
        effort: Option<&str>,
        on_text: Option<OnText<'_>>,
    ) -> Tried;
}

/// Steve's judgments through a harness CLI in `STEVE_HOME/scratch`, in this process.
///
/// - Claude Code: `claude -p <prompt> --output-format json --model <m>`.
/// - Codex: `codex exec --json --ephemeral -s read-only [-m <m>] -` with the prompt on stdin (see
///   [`steve_print::codex_args`]).
pub struct LocalPrint {
    pub executable: PathBuf,
    /// `codex` for purposes that run on Codex; `None`: such calls fail with a clear error.
    pub codex: Option<PathBuf>,
    pub cwd: PathBuf,
}

#[async_trait]
impl OneModel for LocalPrint {
    /// One run of `model` through [`steve_print`]: Codex when `codex`, else `claude -p` (json, or
    /// stream-json when `on_text` is given or there are images). `effort` comes from 设置 · 模型 and is
    /// kept apart from `tools`, so a call without tools keeps its cache-friendly layout ([`print_args`]).
    async fn once(
        &self,
        call: &OneCall<'_>,
        model: &str,
        effort: Option<&str>,
        on_text: Option<OnText<'_>>,
    ) -> Tried {
        let (harness, executable) = if call.codex {
            let Some(exe) = &self.codex else {
                return Tried::failed(msg(
                    "没有配置 codex（CODEX_EXECUTABLE），不能用 Codex 做判断",
                ));
            };
            (steve_print::Harness::Codex, exe)
        } else {
            (steve_print::Harness::Claude, &self.executable)
        };
        let (head, message) = split_static(call.prompt);
        let run = steve_print::PrintRun {
            harness,
            executable,
            cwd: &self.cwd,
            model,
            effort,
            head,
            message,
            tools: tool_mode(call.tools),
            images: call.images,
            timeout: call.timeout,
        };
        Tried::of(steve_print::run(&run, on_text).await, call.codex)
    }
}

/// The Hub's model gateway: each model of the call's list in turn over `one`, until one answers. The
/// purpose's harness and list come from 设置 · 模型 ([`crate::models::Models::for_call`], Steve's bot
/// unless overridden) when `models` is set, else the tier's on Claude Code.
pub struct Fallback<M> {
    /// Runs one model once.
    pub one: M,
    pub fast: Vec<String>,
    pub smart: Vec<String>,
    /// The resolver of 设置 · 模型; `None` (tests) uses `fast` / `smart` as they are.
    pub models: Option<crate::models::Models>,
}

/// Steve's gateway in process: the fallback over [`LocalPrint`].
pub type ClaudeCli = Fallback<LocalPrint>;

/// The mark on a failure that ran out of time ([`Attempt::failures`]), ahead of the harness's words.
pub const TIMED_OUT: &str = "[timeout]";

/// The router's time limit: a route that takes longer becomes [`Route::fallback`] (successful routes
/// take under 20 s).
pub const ROUTE_TIMEOUT: Duration = Duration::from_secs(30);

/// The statistics' harness name of `codex exec` calls (`usage_records.harness`).
pub const CODEX_HARNESS: &str = "codex-cli";

/// The images of [`with_images`] blocks (`{"type":"image","source":{"type":"base64",…}}`); a block
/// without base64 data is left out.
fn images_of(blocks: &[Value]) -> Vec<steve_print::Image> {
    use base64::Engine;
    blocks
        .iter()
        .filter_map(|b| {
            let src = &b["source"];
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(src["data"].as_str()?)
                .ok()?;
            Some(steve_print::Image {
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

/// What a think call may use besides answering. Typed rather than CLI arguments, so a harness other
/// than the in-process one can place the files and choose its own flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ThinkTools {
    /// No tools at all (the cache-friendly layout of [`print_args`]).
    #[default]
    None,
    /// The Read tool only, for these files (already copied next to the in-process run).
    Read(Vec<Readable>),
}

impl ThinkTools {
    /// Whether the call has any tool.
    pub fn any(&self) -> bool {
        matches!(self, ThinkTools::Read(_))
    }
}

/// The `claude -p` arguments of `tools`: none, or [`ATTACHMENT_TOOLS`].
pub fn cli_tools(tools: &ThinkTools) -> &'static [&'static str] {
    steve_print::tool_args(tool_mode(tools))
}

/// The tools of a chat answer: none without readable attachments, else Read for those files.
pub fn answer_tools(files: &ChatFiles) -> ThinkTools {
    if files.readable.is_empty() {
        ThinkTools::None
    } else {
        ThinkTools::Read(files.readable.clone())
    }
}

/// The print run's tool mode of `tools` (the files themselves are already next to the in-process run).
fn tool_mode(tools: &ThinkTools) -> steve_print::ToolMode {
    match tools {
        ThinkTools::None => steve_print::ToolMode::None,
        ThinkTools::Read(_) => steve_print::ToolMode::ReadOnly,
    }
}

pub use steve_print::{ATTACHMENT_TOOLS, GATEWAY_SYSTEM};

/// The message for `-p` and the arguments that lay a `claude -p` call out for the prompt cache
/// ([`steve_print::claude_layout`]): without tools the prompt's stable head (or [`GATEWAY_SYSTEM`])
/// replaces Claude Code's system prompt and tools; with tools it is appended to them.
pub fn print_args<'a>(prompt: &'a str, tools: &ThinkTools) -> (&'a str, Vec<&'a str>) {
    let (head, message) = split_static(prompt);
    (message, steve_print::claude_layout(head, tool_mode(tools)))
}

/// The tokens a harness `result` object reports ([`steve_print::claude_usage`]): one row per key of
/// `modelUsage` (the models that actually answered, sub-calls included), else one row from `usage`
/// booked to `model`, the model asked for. Missing figures stay `None`. Dollars: each model's
/// `costUSD`, else `total_cost_usd` on the first row.
pub fn usage_of(d: &Value, model: &str) -> Vec<ModelUsage> {
    steve_print::claude_usage(d, model)
        .into_iter()
        .map(model_usage)
        .collect()
}

/// Adds `more` to `into`, summing rows of the same model and scope (a model tried twice).
fn merge_usage(into: &mut Vec<ModelUsage>, more: Vec<ModelUsage>) {
    fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        }
    }
    for m in more {
        match into
            .iter_mut()
            .find(|r| r.model == m.model && r.detail == m.detail)
        {
            Some(r) => {
                let (a, b) = (&mut r.tokens, m.tokens);
                a.input = add(a.input, b.input);
                a.output = add(a.output, b.output);
                a.cache_read = add(a.cache_read, b.cache_read);
                a.cache_write = add(a.cache_write, b.cache_write);
                a.reasoning = add(a.reasoning, b.reasoning);
                r.cost_reported = add(r.cost_reported, m.cost_reported);
            }
            None => into.push(m),
        }
    }
}

impl<M: OneModel> Fallback<M> {
    /// The models (and effort) of this call: its purpose and workspace come from the caller's
    /// [`crate::stats::CallTag`].
    pub fn choice(&self, tier: Tier) -> crate::models::Choice {
        match &self.models {
            Some(m) => {
                let tag = crate::stats::current_tag();
                m.for_call(&tag.workspace_id, tag.purpose, tier)
            }
            None => crate::models::Choice::claude(match tier {
                Tier::Fast => self.fast.clone(),
                Tier::Smart => self.smart.clone(),
            }),
        }
    }

    /// Each model of the tier in turn until one answers or one runs out of time; streaming when `on_text`
    /// is given. Counts the models tried and keeps the tokens of failed tries too (they were spent).
    async fn fall_back(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let t0 = Instant::now();
        let choice = self.choice(tier);
        let models = &choice.models;
        let codex = choice.harness == crate::models::CODEX;
        let mut a = Attempt {
            result: Err(msg("没有可用的模型")),
            attempts: 0,
            duration_ms: 0,
            usage: vec![],
            model: String::new(),
            failures: vec![],
            harness: if codex { CODEX_HARNESS } else { "" },
        };
        // Codex with no model named runs its configured default (`-m` left out)
        let default_only = [String::new()];
        let models: &[String] = if codex && models.is_empty() {
            &default_only
        } else {
            models
        };
        // the images of the call ([`with_images`]), for every model tried
        let images = current_images().map(|b| images_of(&b)).unwrap_or_default();
        let call = OneCall {
            prompt,
            timeout,
            tools,
            codex,
            images: &images,
        };
        for m in models {
            let r = self
                .one
                .once(&call, m, choice.effort.as_deref(), on_text)
                .await;
            a.attempts += 1;
            a.model = m.clone();
            merge_usage(&mut a.usage, r.usage);
            match r.reply {
                Ok((text, cost_usd)) => {
                    a.duration_ms = elapsed_ms(t0);
                    a.result = Ok(Printed {
                        text,
                        model: m.clone(),
                        cost_usd,
                        usage: a.usage.clone(),
                        attempts: a.attempts,
                        duration_ms: a.duration_ms,
                    });
                    return a;
                }
                Err(e) => {
                    let e1 = js_head(&e.to_string(), 200).to_string();
                    tracing::error!("model {m} failed: {e1}");
                    a.failures.push(if r.timed_out {
                        format!("{m}: {TIMED_OUT} {e1}")
                    } else {
                        format!("{m}: {e1}")
                    });
                    a.result = Err(e);
                    // a timeout is not the model's own fault (the provider or the harness is slow): the
                    // next one would wait as long, so the chain ends here; fast errors fall back
                    if r.timed_out {
                        break;
                    }
                }
            }
        }
        a.duration_ms = elapsed_ms(t0);
        a
    }
}

#[async_trait]
impl<M: OneModel> ModelGateway for Fallback<M> {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        tools: &ThinkTools,
    ) -> Result<Printed> {
        self.fall_back(prompt, tier, timeout, tools, None)
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
        self.fall_back(prompt, tier, timeout, tools, Some(on_text))
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
        self.fall_back(prompt, tier, timeout, tools, on_text).await
    }
}

/// Steve's gateway as the modules' [`steve_modules::Gateway`] (B1), so core and modules share one model
/// gateway: same tier lists, same fallback, same scratch directory.
pub struct SharedGateway(pub std::sync::Arc<dyn ModelGateway>);

#[async_trait]
impl steve_modules::Gateway for SharedGateway {
    async fn run_with_images(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        images: &[Value],
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, String> {
        with_images(images.to_vec(), self.run(tier, prompt, timeout)).await
    }

    async fn run(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, String> {
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
            .map_err(|e| e.to_string())
    }
}

pub use steve_print::{jstr, truthy};

static BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[\s\S]*\}").expect("valid regex"));

/// The greedy first `{…}` of a model reply, parsed; `None` without braces, an error for bad JSON.
pub fn first_json(t: &str) -> Result<Option<Value>> {
    match BRACES.find(t) {
        None => Ok(None),
        Some(m) => Ok(Some(serde_json::from_str(m.as_str())?)),
    }
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

/// The routing results. `chat` from older routers reads as `answer`.
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
    pub bot: String,
    /// The bot is the project's default bot in place of the router's “steve”: the router's `reply`
    /// may name the wrong bot then, so the code writes the hand-over sentence itself.
    pub bot_from_project: bool,
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
            bot: "steve".into(),
            bot_from_project: false,
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
    /// an inline research run is reading right now
    pub researching: bool,
    /// The main conversation's messages Steve is still answering (running first, then waiting): the
    /// router says whether this message needs those answers first ([`Route::after`]).
    pub pending: Vec<String>,
    pub debug: bool,
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
    let skip = history.len().saturating_sub(6);
    let recent: Vec<String> = history[skip..]
        .iter()
        .map(|m| {
            let t = if js_len(&m.text) > 800 {
                format!("{}…", js_head(&m.text, 800))
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
    let mut head: Vec<String> = vec![
        ROUTE_RULES.trim_end().to_string(),
        "Bots (id: role):".into(),
    ];
    head.extend(c.bots.iter().map(|b| format!("  {}: {}", b.id, b.role)));
    head.push("For an item, pick \"steve\" unless the task clearly needs a specialised bot; work in a project with a default bot goes to that bot. For other kinds the bot does not matter.".into());
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
    head.push("goal: the task, self-contained, in the user language (for an ordinary message, the message itself). title: names the work concretely, never just “按建议” or “好的”.".into());
    head.push("Reply with ONLY one JSON object: {\"kind\":\"answer\"|\"research\"|\"item\"|\"suggest\"|\"ask\"|\"supplement\"|\"status\"|\"config\"|\"reroute\",\"confidence\":0.0-1.0,\"signals\":[\"explicit\"|\"change\"|\"deliverable\"|\"long\"|\"question\"|\"lookup\"|\"vague\"],\"offer\":{\"title\":\"\",\"goal\":\"\"} or null,\"item\":\"<item id or null>\",\"options\":[\"\"],\"title\":\"<= 24 chars, user language\",\"goal\":\"\",\"bot\":\"<bot id>\",\"project\":\"<project id or null>\",\"tags\":[\"<tag id>\"],\"newTags\":[{\"name\":\"\",\"cat\":\"\",\"description\":\"\"}],\"reply\":\"one short sentence in the user language, without tags\",\"why\":\"one short sentence in the user language: why this kind\"}".into());
    let mut lines: Vec<String> = vec![];
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
    lines.push(format!("User message: {}", js_json(text)));
    lines.push(if promote {
        "The user pressed “做成事项” on Steve's last answer: kind is \"item\"; goal is the work this conversation is about, drawn from the question and Steve's answer.".into()
    } else {
        "If the message only confirms or picks up something Steve proposed in its last reply (e.g. “按建议”, “就这么办”, “好，做吧”, “改吧”, “可以”, “第二个”), it is about that proposal: when the proposal is work, kind is \"item\" with signal \"explicit\" and goal restates the proposed work completely from Steve's reply; when it is a change of Steve's own settings or bots (new bots, their roles and rules), kind is \"config\".".into()
    });
    lines.push("Reply with ONLY the JSON object described in the instructions.".into());
    head.retain(|l| !l.is_empty());
    lines.retain(|l| !l.is_empty());
    with_static(&head.join("\n"), &lines.join("\n"))
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
    let r = first_json(raw).ok()??;
    let kind = match r.get("kind").and_then(Value::as_str)? {
        "chat" => "answer",
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
    let has_bot = |id: &str| c.bots.iter().any(|b| b.id == id);
    let project = r
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| c.projects.iter().any(|x| x.id == *p))
        .map(String::from);
    /* work in a project goes to the project's own bot unless a specialised one was picked; only items have a bot */
    let own = project
        .as_deref()
        .and_then(|p| c.projects.iter().find(|x| x.id == p))
        .and_then(|p| p.default_bot.clone())
        .filter(|b| has_bot(b));
    let picked = r.get("bot").and_then(Value::as_str).filter(|b| has_bot(b));
    let (bot, bot_from_project) = match (kind, picked, &own) {
        ("item", Some("steve"), Some(o)) => (o.clone(), o != "steve"),
        ("item", None, Some(o)) => (o.clone(), true),
        (_, Some(b), _) => (b.to_string(), false),
        _ => ("steve".to_string(), false),
    };
    let s_or = |k: &str| match r.get(k) {
        Some(v) if truthy(v) => jstr(v),
        _ => text.to_string(),
    };
    let tag_ids = match r.get("tags") {
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
    };
    let new_tags = match r.get("newTags") {
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
    };
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
        bot,
        bot_from_project,
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

/// Route a message; any failure gives [`Route::fallback`], which never creates an item.
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
            if c.debug {
                tracing::info!("route raw: {raw}");
            }
            if let Some(r) = parse_route(c, text, &raw, promote) {
                return Route {
                    model: Some(model),
                    ..r
                };
            }
            tracing::warn!("route unusable: {}", js_head(&raw, 200));
        }
        Err(e) => tracing::error!("route failed {e}"),
    }
    let project = c.steve_project.clone().or_else(|| {
        c.projects
            .iter()
            .find(|p| !p.archived)
            .map(|p| p.id.clone())
    });
    let mut r = Route::fallback(text, project);
    if promote {
        /* “做成事项” was pressed: the user asked for an item, the router only names it */
        r.kind = "item".into();
        r.project = None;
    }
    r
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
    let skip = history.len().saturating_sub(10);
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
            "You are {}, the user's assistant. {} {}\nAnswer in the user's language, briefly. You answer in the conversation: you don't change anything, change settings or start work from this reply, so never claim you did. Steve can change its own settings when the user says the change itself in the conversation (e.g. “把免打扰改成 22:00 – 09:00”, “停用写作 Bot”, “加一个验收 Bot”, “开 YOLO”): it drafts the change as a card the user applies with one click and can undo. Asked whether or how settings can be changed, say so and give an example sentence; never claim it is impossible. When the user asks for a change or reports a problem, give a concrete plan right away; never ask whether to plan here or make it an item. If the answer leads to a change worth making, put it in \"offer\" (the user can make it a tracked item with one click); otherwise leave offer null.\n{}\nReply with ONLY one JSON object: {{\"thought\":\"2-4 short sentences in the user's language: what the user is asking and how you approach the answer\",\"answer\":\"the answer itself (markdown allowed)\",\"offer\":{{\"title\":\"<= 24 chars\",\"goal\":\"the work, self-contained\"}} or null}}",
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
        },
    }
}

/// A field the config draft may change.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    /// text | enum | number
    pub ty: &'static str,
    pub values: Option<Vec<String>>,
    pub current: Value,
}

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
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(id: &str) -> Bot {
        Bot {
            id: id.into(),
            workspace_id: "default".into(),
            name: id.into(),
            role: "r".into(),
            main: id == "steve",
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: None,
            rules: String::new(),
            accept_mode: "inherit".into(),
            perm_mode: "default".into(),
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
        assert_eq!(r.bot, "steve");
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
    fn project_work_goes_to_the_project_bot() {
        let c = ctx();
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"item","bot":"steve","project":"prj_a"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.bot, "dev");
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"item","bot":"writer","project":"prj_a"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.bot, "writer");
        let r = parse_route(&c, "x", r#"{"kind":"item","project":"prj_b"}"#, false).unwrap();
        assert_eq!(r.bot, "steve");
        let r = parse_route(&c, "x", r#"{"kind":"chat","project":"prj_a"}"#, true).unwrap();
        assert_eq!(r.kind, "item");
        assert_eq!((r.bot.as_str(), r.bot_from_project), ("dev", true));
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
        assert_eq!(
            cli_tools(&t),
            [
                "--tools",
                "Read",
                "--allowedTools",
                "Read",
                "--setting-sources",
                "user"
            ],
            "Read is the only tool there is, user settings only"
        );
        assert!(cli_tools(&ThinkTools::None).is_empty());
    }

    /// The `claude -p` layout and tool arguments in the order [`LocalPrint::once`] passes them.
    fn layout<'a>(prompt: &'a str, tools: &ThinkTools) -> (&'a str, Vec<&'a str>) {
        let (m, mut a) = print_args(prompt, tools);
        a.extend(cli_tools(tools));
        (m, a)
    }

    #[test]
    fn print_args_put_the_stable_head_in_the_system_prompt() {
        let p = with_static("rules", "message");
        assert_eq!(
            layout(&p, &ThinkTools::None),
            (
                "message",
                vec![
                    "--system-prompt",
                    "rules",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--disable-slash-commands"
                ]
            )
        );
        // no head: a short fixed system prompt, still no tools
        assert_eq!(
            layout("just this", &ThinkTools::None),
            (
                "just this",
                vec![
                    "--system-prompt",
                    GATEWAY_SYSTEM,
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--disable-slash-commands"
                ]
            )
        );
        // tools wanted: Claude Code's own prompt stays, the head is appended, Read is the only tool;
        // the files themselves are not arguments (they are already next to the run)
        let read = ThinkTools::Read(vec![readable("fil_a1", "log.txt")]);
        assert_eq!(
            layout(&p, &read),
            (
                "message",
                vec![
                    "--append-system-prompt",
                    "rules",
                    "--strict-mcp-config",
                    "--disable-slash-commands",
                    "--tools",
                    "Read",
                    "--allowedTools",
                    "Read",
                    "--setting-sources",
                    "user"
                ]
            )
        );
        assert_eq!(
            layout("m", &ThinkTools::Read(vec![])),
            (
                "m",
                vec![
                    "--strict-mcp-config",
                    "--disable-slash-commands",
                    "--tools",
                    "Read",
                    "--allowedTools",
                    "Read",
                    "--setting-sources",
                    "user"
                ]
            )
        );
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

    /// What a failed print run says, word for word as before the run moved to steve-print.
    #[test]
    fn a_failed_run_says_why_in_steves_words() {
        use steve_print::PrintError as P;
        let said = |e, codex| print_error(e, codex).to_string();
        assert_eq!(
            said(P::Timeout(Duration::from_millis(200)), false),
            "Command timed out after 200 ms"
        );
        assert_eq!(
            said(P::Failed("nope".into()), false),
            "Command failed: nope"
        );
        let long = "x".repeat(400);
        assert_eq!(said(P::Failed(long.clone()), false).len(), 16 + 400);
        assert_eq!(
            said(P::Failed(long.clone()), true).len(),
            16 + 300,
            "Codex's stderr is cut"
        );
        assert_eq!(
            said(
                P::Harness {
                    text: long,
                    transient: false
                },
                false
            )
            .len(),
            300
        );
        assert_eq!(said(P::CodexFailed, true), "Codex 失败");
        assert_eq!(said(P::NoResult, false), "harness 没有返回结果");
        assert_eq!(said(P::NoOutput, true), "harness 没有输出");
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
        /* research does not pick a bot: steve stays steve (the ST-20 swap) */
        assert_eq!((r.bot.as_str(), r.bot_from_project), ("steve", false));
        let r = parse_route(&c, "x", r#"{"kind":"chat"}"#, false).unwrap();
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
}
