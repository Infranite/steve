//! Which harness and models Steve's own judgments use (设置 · 模型).
//!
//! Steve is a bot: every judgment (routing, chat answers, research, titles, extraction, disclosure,
//! YOLO answers, drafts…) runs on the main Steve bot's harness and model, with its reasoning effort, as
//! set on its Bot page. A purpose may override that ("高级"): another harness, an ordered fallback list
//! of models, an effort. Overrides are stored per workspace in the settings table (`models.<purpose>`);
//! both are read on every call, so a change takes effect without a restart.
//!
//! Every model call goes through [`Models::for_call`]: [`crate::Fallback`] asks it with the call's
//! [`crate::stats::CallTag`] (purpose and workspace). The agent-session default per harness
//! (`models.agent.<harness>`) is unchanged.
//!
//! Upgrade: values stored before Steve became a bot (`{"models": [...], "effort": ...}`, no harness) ran
//! on Claude Code, so they read as Claude Code overrides. [`Models::migrate`] adds once, per workspace,
//! the recommended overrides of the cheap high-volume purposes (route, title, disclosure, flow): the
//! fast list the environment names, else the quick model Steve's harness says it has, so that a
//! routing decision does not wait for the strongest model. They are ordinary overrides afterwards.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use steve_store::Store;

use crate::config::CoreConfig;
use crate::gateway::Tier;

/// One decision Steve makes with a model of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Purpose {
    /// The stats purpose (`usage_records.purpose`) and the setting's key suffix.
    pub key: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    /// The tier its calls used before Steve became a bot; fast purposes get a recommended override.
    pub tier: Tier,
}

const fn p(key: &'static str, label: &'static str, desc: &'static str, tier: Tier) -> Purpose {
    Purpose {
        key,
        label,
        desc,
        tier,
    }
}

/// Every purpose, in the order the settings list them.
pub const PURPOSES: [Purpose; 11] = [
    p(
        "route",
        "分流",
        "判断一条消息该聊天、研究还是建事项，也判断渠道规则的意图",
        Tier::Fast,
    ),
    p("chat", "对话回答", "在对话里直接回答你", Tier::Smart),
    p(
        "research",
        "就地研究",
        "在对话里翻项目和资料后回答；你自己的对话里还能直接在本机动手，前一个模型失败才换下一个",
        Tier::Smart,
    ),
    p("title", "起标题", "给新项目起名字、写一句说明", Tier::Fast),
    p(
        "extract",
        "记忆提取",
        "从对话和资料里提取值得记住的知识",
        Tier::Smart,
    ),
    p(
        "disclosure",
        "披露检查",
        "对外回复前检查有没有说出不该说的",
        Tier::Fast,
    ),
    p(
        "yolo",
        "YOLO 代答",
        "YOLO 打开时替你回答 Agent 的提问",
        Tier::Smart,
    ),
    p(
        "draft",
        "草稿/配置",
        "按你的一句话起草 Bot、流程和设置的改动",
        Tier::Smart,
    ),
    p(
        "flow",
        "流程 Bot 节点",
        "流程里 Bot 节点的判断和汇报",
        Tier::Fast,
    ),
    p("mail", "邮件起草", "起草邮件回复", Tier::Smart),
    p("compose", "渠道回复", "在渠道里替你写回复", Tier::Smart),
];

pub use crate::harnesses::{Harness, HarnessModel, HarnessSource};

/// The display name of a harness id as declared or built in (`claude-code` → `Claude Code`);
/// unknown ids as they are. [`Models::harness_name`] knows the adapters the Hub declares too.
pub fn harness_name(h: &str) -> String {
    steve_acp::adapters::spec(h, None).name
}

/// The label of a reasoning effort, by its usual name (`high` → 高); an unknown one has none.
pub fn effort_label(e: &str) -> Option<&'static str> {
    Some(match e {
        "none" => "关闭",
        "minimal" => "最低",
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "很高",
        "max" => "最高",
        "ultra" => "极致",
        _ => return None,
    })
}

/// [`effort_label`] as the UI shows it, in the request's language (the Chinese label stays what models read).
pub fn effort_shown(e: &str) -> Option<String> {
    Some(match e {
        "none" => steve_i18n::t!("core.effort.none"),
        "minimal" => steve_i18n::t!("core.effort.minimal"),
        "low" => steve_i18n::t!("core.effort.low"),
        "medium" => steve_i18n::t!("core.effort.medium"),
        "high" => steve_i18n::t!("core.effort.high"),
        "xhigh" => steve_i18n::t!("core.effort.xhigh"),
        "max" => steve_i18n::t!("core.effort.max"),
        "ultra" => steve_i18n::t!("core.effort.ultra"),
        _ => return None,
    })
}

/// Efforts as the settings page lists them: `{key, label}` with the label in the request's language
/// (an effort with no usual name by its own).
fn efforts_shown(list: &[String]) -> Vec<Value> {
    list.iter()
        .map(|k| json!({"key": k, "label": effort_shown(k).unwrap_or_else(|| k.clone())}))
        .collect()
}

/// A purpose's label as the UI shows it, in the request's language ([`Purpose::label`] is what models read).
pub fn purpose_label(key: &str) -> String {
    match key {
        "route" => steve_i18n::t!("core.purpose.route.label"),
        "chat" => steve_i18n::t!("core.purpose.chat.label"),
        "research" => steve_i18n::t!("core.purpose.research.label"),
        "title" => steve_i18n::t!("core.purpose.title.label"),
        "extract" => steve_i18n::t!("core.purpose.extract.label"),
        "disclosure" => steve_i18n::t!("core.purpose.disclosure.label"),
        "yolo" => steve_i18n::t!("core.purpose.yolo.label"),
        "draft" => steve_i18n::t!("core.purpose.draft.label"),
        "flow" => steve_i18n::t!("core.purpose.flow.label"),
        "mail" => steve_i18n::t!("core.purpose.mail.label"),
        "compose" => steve_i18n::t!("core.purpose.compose.label"),
        _ => key.to_string(),
    }
}

/// A purpose's description as the UI shows it, in the request's language.
pub fn purpose_desc(key: &str) -> String {
    match key {
        "route" => steve_i18n::t!("core.purpose.route.desc"),
        "chat" => steve_i18n::t!("core.purpose.chat.desc"),
        "research" => steve_i18n::t!("core.purpose.research.desc"),
        "title" => steve_i18n::t!("core.purpose.title.desc"),
        "extract" => steve_i18n::t!("core.purpose.extract.desc"),
        "disclosure" => steve_i18n::t!("core.purpose.disclosure.desc"),
        "yolo" => steve_i18n::t!("core.purpose.yolo.desc"),
        "draft" => steve_i18n::t!("core.purpose.draft.desc"),
        "flow" => steve_i18n::t!("core.purpose.flow.desc"),
        "mail" => steve_i18n::t!("core.purpose.mail.desc"),
        "compose" => steve_i18n::t!("core.purpose.compose.desc"),
        _ => String::new(),
    }
}

/// At most this many models in one fallback list.
pub const MAX_MODELS: usize = 5;

/// A `day` no usage record at or after `since` is filed before, whatever zone its workspace counts days
/// in (no zone is a day off UTC): bounds a query by `ts` so the `(workspace_id, day)` index serves it.
fn day_floor(since: i64) -> String {
    steve_stats::day(since - 86_400_000, steve_stats::tz(Some("UTC")))
}

/// The setting purpose a stats purpose is chosen by: `intent` (channel rules) is a kind of routing;
/// `digest` and `other` have no setting and follow Steve.
pub fn purpose_of(tag: &str) -> Option<&'static Purpose> {
    let k = match tag {
        "intent" => "route",
        k => k,
    };
    PURPOSES.iter().find(|p| p.key == k)
}

/// The settings key of a purpose's override.
pub fn key(purpose: &str) -> String {
    format!("models.{purpose}")
}

/// The settings key of a harness's agent default.
pub fn agent_key(harness: &str) -> String {
    format!("models.agent.{harness}")
}

/// Set once per workspace by [`Models::migrate`].
pub const MIGRATED_KEY: &str = "models.v2";

/// Set once per workspace by [`Models::migrate`], when the cheap purposes got their fast model.
pub const FAST_KEY: &str = "models.v3";

fn default_adapter() -> String {
    steve_acp::adapters::DEFAULT_ADAPTER.0.into()
}

/// The harness to run on, the models to try in order (empty: the harness's own default) and the effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// Values stored before Steve became a bot have none: they ran on the default adapter.
    #[serde(default = "default_adapter")]
    pub harness: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for Choice {
    fn default() -> Self {
        Choice {
            harness: default_adapter(),
            models: vec![],
            effort: None,
        }
    }
}

impl Choice {
    /// A choice on the default adapter (tests, the gateway without a resolver).
    pub fn default_adapter(models: Vec<String>) -> Choice {
        Choice {
            models,
            ..Choice::default()
        }
    }

    /// `Claude Code · claude-haiku-5-5 → claude-sonnet-5-5`
    pub fn describe(&self) -> String {
        let models = if self.models.is_empty() {
            "默认模型".to_string()
        } else {
            self.models.join(" → ")
        };
        format!("{} · {models}", harness_name(&self.harness))
    }

    /// [`Choice::describe`] as the UI shows it, in the request's language.
    pub fn describe_shown(&self) -> String {
        let models = if self.models.is_empty() {
            steve_i18n::t!("core.models.default_model")
        } else {
            self.models.join(" → ")
        };
        format!("{} · {models}", harness_name(&self.harness))
    }
}

/// What the environment says (`STEVE_MODELS_*`, `STEVE_DEFAULT_MODEL_*`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Defaults {
    pub fast: Vec<String>,
    pub smart: Vec<String>,
    /// `STEVE_MODELS_RESEARCH` (no longer a default: research follows Steve; kept for the config view).
    pub research: Vec<String>,
    /// `STEVE_MODELS_CHECK`: the recommended disclosure override; empty: the fast list.
    pub check: Vec<String>,
    /// Agent sessions per harness (`STEVE_DEFAULT_MODEL_CLAUDE`).
    pub agent: BTreeMap<String, String>,
}

impl Defaults {
    pub fn of(c: &CoreConfig) -> Defaults {
        Defaults {
            fast: c.models_fast.clone(),
            smart: c.models_smart.clone(),
            research: c.models_research.clone(),
            check: c.models_check.clone(),
            agent: c
                .default_models
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// The models of `h` when nothing names one: Steve's smart list (`STEVE_MODELS_SMART`) where
    /// the adapter falls back down it, else none: the adapter's own default model.
    fn harness_list(&self, h: Option<&Harness>) -> Vec<String> {
        match h {
            Some(h) if h.model_fallback => self.smart.clone(),
            _ => vec![],
        }
    }
}

/// The main Steve bot as the runner of Steve's judgments.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Steve {
    pub bot_id: Option<String>,
    pub name: String,
    /// The bot's harness as set on its Bot page.
    pub harness: String,
    /// The bot's model as set (`None`: the harness default).
    pub model: Option<String>,
    /// The bot's reasoning effort as set.
    pub bot_effort: Option<String>,
    /// What its judgments run on: its harness (judgments fail while that adapter is missing).
    pub runs_on: String,
    /// The models its judgments try, in order (the bot's model first, then the harness's fallbacks).
    pub models: Vec<String>,
    /// The effort passed to the harness (the bot's, when the harness takes it).
    pub effort: Option<String>,
    /// Why its judgments cannot run now, when they cannot (its adapter is missing).
    pub note: Option<String>,
    /// Where the first model comes from when the bot names none.
    pub model_note: Option<String>,
}

/// The resolver. Cheap to clone.
#[derive(Clone)]
pub struct Models {
    store: Store,
    defaults: Arc<Defaults>,
    /// Shared by every clone: the Hub sets it once it knows its adapters.
    harnesses: Arc<std::sync::RwLock<HarnessSource>>,
}

/// A model id: what `--model` takes (`claude-opus-5-5`, `opus`, `us.anthropic.x-v1:0`, `x[1m]`, `kiro/x`).
static MODEL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:/@\[\]-]{0,99}$").expect("valid regex"));

/// Is `m` something `--model` could take?
pub fn valid_model(m: &str) -> bool {
    MODEL_ID.is_match(m)
}

impl Models {
    pub fn new(store: Store, defaults: Defaults) -> Models {
        Models {
            store,
            defaults: Arc::new(defaults),
            harnesses: Arc::new(std::sync::RwLock::new(crate::harnesses::builtin_source())),
        }
    }

    /// From now on (this resolver and its clones) over the Hub's adapters ([`crate::harnesses`]).
    pub fn set_harnesses(&self, harnesses: HarnessSource) {
        *self.harnesses.write().unwrap_or_else(|e| e.into_inner()) = harnesses;
    }

    /// The harness adapters Steve can run on now.
    pub fn harnesses(&self) -> Vec<Harness> {
        let source = self
            .harnesses
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        source()
    }

    /// Adapter `id`, if the Hub has it.
    pub fn harness(&self, id: &str) -> Option<Harness> {
        self.harnesses().into_iter().find(|h| h.id == id)
    }

    /// The display name of adapter `id`.
    pub fn harness_name(&self, id: &str) -> String {
        crate::harnesses::name_of(&self.harnesses(), id)
    }

    pub fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    /// The workspace a call with no workspace is for: the only one there is, else none.
    fn ws_of(&self, ws: &str) -> Option<String> {
        if !ws.is_empty() {
            return Some(ws.to_string());
        }
        match self.store.workspaces().ids() {
            Ok(ids) if ids.len() == 1 => ids.into_iter().next(),
            _ => None,
        }
    }

    /// The main bot and what its judgments run on.
    pub fn steve(&self) -> Steve {
        let bot = match self.store.bots().main() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("models: main bot not read: {e}");
                None
            }
        };
        let (bot_id, name, harness, model, bot_effort) = match bot {
            Some(b) => (
                Some(b.id),
                b.name,
                b.harness,
                b.model.filter(|m| valid_model(m)),
                b.reasoning_effort.filter(|e| !e.is_empty()),
            ),
            None => (None, "Steve".into(), default_adapter(), None, None),
        };
        // the bot's harness, whatever it is: a missing adapter fails its judgments (and says so)
        // rather than moving them to another one
        let h = self.harness(&harness);
        let note = h.is_none().then(|| {
            steve_i18n::t!(
                "core.models.note.harness_missing",
                harness = harness_name(&harness)
            )
        });
        let mut models: Vec<String> = model.iter().cloned().collect();
        // after the bot's model, Steve's lists where the adapter falls back down them (a 503 on
        // one model is common); otherwise the bot's model only, or the adapter's own default
        for m in self.defaults.harness_list(h.as_ref()) {
            if !models.contains(&m) && models.len() < MAX_MODELS {
                models.push(m);
            }
        }
        let effort = bot_effort
            .clone()
            .filter(|e| h.as_ref().is_some_and(|h| h.takes_effort(e)));
        let model_note = match &model {
            Some(_) => None,
            None if models.is_empty() => Some(steve_i18n::t!("core.models.note.adapter_default")),
            None => Some(steve_i18n::t!("core.models.note.bot_default")),
        };
        Steve {
            bot_id,
            name,
            runs_on: harness.clone(),
            harness,
            model,
            bot_effort,
            models,
            effort,
            note,
            model_note,
        }
    }

    /// The override of `purpose`, if one is stored (a broken value counts as none).
    pub fn custom(&self, ws: &str, purpose: &str) -> Option<Choice> {
        let ws = self.ws_of(ws)?;
        let v: Option<Value> = match self.store.settings().get(&ws, &key(purpose), None) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("models: setting {purpose} not read: {e}");
                None
            }
        };
        let c: Choice = serde_json::from_value(v?).ok()?;
        let h = self.harness(&c.harness)?;
        let c = Choice {
            models: c.models.into_iter().filter(|m| valid_model(m)).collect(),
            effort: c.effort.filter(|e| h.takes_effort(e)),
            harness: c.harness,
        };
        Some(c)
    }

    /// The recommended override of `purpose`, for the cheap high-volume purposes only (none for the
    /// others): the fast list on the default adapter where the environment names one, else the
    /// quick model Steve's own harness says it has ([`Harness::fast_model`]) - without either, a
    /// routing decision waits for the harness's strongest model.
    pub fn recommended(&self, purpose: &Purpose) -> Option<Choice> {
        if purpose.tier != Tier::Fast {
            return None;
        }
        let d = &self.defaults;
        let models = if purpose.key == "disclosure" && !d.check.is_empty() {
            d.check.clone()
        } else {
            d.fast.clone()
        };
        if !models.is_empty() {
            return Some(Choice::default_adapter(models));
        }
        if self.named() {
            return None;
        }
        let h = self.harness(&self.steve().runs_on)?;
        Some(Choice {
            models: vec![h.fast_model()?],
            harness: h.id,
            effort: None,
        })
    }

    /// The environment names the cheap purposes' models itself (`STEVE_MODELS_FAST`, `STEVE_MODELS_CHECK`).
    fn named(&self) -> bool {
        !self.defaults.fast.is_empty() || !self.defaults.check.is_empty()
    }

    /// `models::for_purpose`: what a call for `purpose` of workspace `ws` uses.
    pub fn for_purpose(&self, ws: &str, purpose: &str) -> Choice {
        self.for_call(ws, purpose, Tier::Smart)
    }

    /// What one call uses: the purpose's override, else Steve's harness, models and effort. An override
    /// with no models uses Steve's models when it is on Steve's harness, else that harness's defaults;
    /// with no effort, Steve's effort on Steve's harness. `_tier` is the call site's old tier (unused:
    /// Steve's model answers every call that is not overridden).
    pub fn for_call(&self, ws: &str, purpose: &str, _tier: Tier) -> Choice {
        let st = self.steve();
        self.resolve(ws, purpose, &st)
    }

    fn resolve(&self, ws: &str, purpose: &str, st: &Steve) -> Choice {
        match purpose_of(purpose).and_then(|p| self.custom(ws, p.key)) {
            Some(c) => {
                // the same adapter as Steve's (an id, not a name check)
                let same = c.harness.eq(&st.runs_on);
                Choice {
                    models: if !c.models.is_empty() {
                        c.models
                    } else if same {
                        st.models.clone()
                    } else {
                        self.defaults
                            .harness_list(self.harness(&c.harness).as_ref())
                    },
                    effort: c
                        .effort
                        .or_else(|| same.then(|| st.effort.clone()).flatten()),
                    harness: c.harness,
                }
            }
            None => Choice {
                harness: st.runs_on.clone(),
                models: st.models.clone(),
                effort: st.effort.clone(),
            },
        }
    }

    /// Writes the recommended overrides once per workspace (`models.v3`), for purposes with nothing
    /// stored: an override removed afterwards stays removed. True when it wrote something. Where the
    /// recommendation comes from the harness and it has not said what it offers yet, nothing is
    /// written or marked: asked again (the next start, the next probe).
    pub fn migrate(&self, ws: &str) -> steve_store::Result<bool> {
        let s = self.store.settings();
        let done = |k: &str| {
            Ok::<_, steve_store::StoreError>(s.get::<Option<Value>>(ws, k, None)?.is_some())
        };
        if done(FAST_KEY)? {
            return Ok(false);
        }
        let mut values = Map::new();
        // the environment's lists were written when `models.v2` was: not a second time
        if !(self.named() && done(MIGRATED_KEY)?) {
            let mut any = false;
            for p in &PURPOSES {
                let Some(r) = self.recommended(p) else {
                    continue;
                };
                any = true;
                let stored: Option<Value> = s.get(ws, &key(p.key), None)?;
                if !stored.is_some_and(|v| !v.is_null()) {
                    values.insert(key(p.key), serde_json::to_value(r)?);
                }
            }
            if !any && !self.named() {
                return Ok(false);
            }
        }
        let wrote = !values.is_empty();
        values.insert(MIGRATED_KEY.into(), json!(2));
        values.insert(FAST_KEY.into(), json!(1));
        self.write(ws, &values, "steve", "settings.models.migrate")?;
        Ok(wrote)
    }

    /// [`Models::migrate`] for every workspace (the Hub calls it when its harness list changes).
    pub fn migrate_all(&self) {
        let ids = match self.store.workspaces().ids() {
            Ok(ids) => ids,
            Err(e) => return tracing::warn!("models: workspaces not read: {e}"),
        };
        for ws in ids {
            match self.migrate(&ws) {
                Ok(true) => tracing::info!("models: recommended overrides written for {ws}"),
                Ok(false) => {}
                Err(e) => tracing::warn!("models: recommended overrides of {ws}: {e}"),
            }
        }
    }

    /// The model of a new agent session on `harness` for a bot that names none.
    pub fn agent_model(&self, ws: &str, harness: &str) -> Option<String> {
        let set = self.ws_of(ws).and_then(|ws| {
            self.store
                .settings()
                .get::<Option<String>>(&ws, &agent_key(harness), None)
                .ok()
                .flatten()
                .filter(|m| valid_model(m))
        });
        set.or_else(|| self.defaults.agent.get(harness).cloned())
    }

    /// What each purpose ran on recently (gateway and research calls since `since`): per purpose the
    /// last call and the calls per harness and model, busiest first. `intent` counts as `route`.
    pub fn recent(&self, ws: &str, since: i64) -> BTreeMap<String, Value> {
        let rows = self
            .store
            .reader::<_, steve_store::StoreError>(|c| {
                let mut st = c.prepare(
                    "SELECT purpose, harness, model, count(*), max(ts), sum(CASE WHEN ok = 0 THEN 1 ELSE 0 END) FROM usage_records
                     WHERE workspace_id = ? AND day >= ? AND source = 'gateway' AND ts >= ? AND model != ''
                     GROUP BY purpose, harness, model",
                )?;
                let v = st
                    .query_map(rusqlite::params![ws, day_floor(since), since], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, i64>(5)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .unwrap_or_default();
        type Used = (String, String, i64, i64, i64);
        let mut by: BTreeMap<String, Vec<Used>> = BTreeMap::new();
        for (p, h, m, n, at, failed) in rows {
            let p = purpose_of(&p).map_or(p, |x| x.key.to_string());
            let list = by.entry(p).or_default();
            match list.iter_mut().find(|x| x.0 == h && x.1 == m) {
                Some(x) => {
                    x.2 += n;
                    x.3 = x.3.max(at);
                    x.4 += failed;
                }
                None => list.push((h, m, n, at, failed)),
            }
        }
        by.into_iter()
            .map(|(p, mut list)| {
                list.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)));
                let last = list.iter().max_by_key(|x| x.3).cloned();
                let v = json!({
                    "last": last.map(|(h, m, _, at, _)| json!({"harness": h, "model": m, "at": at})),
                    "used": list.iter().map(|(h, m, n, at, failed)| json!({"harness": h, "model": m, "calls": n, "failed": failed, "at": at})).collect::<Vec<_>>(),
                });
                (p, v)
            })
            .collect()
    }

    /// The models `harness` ran with since `since` (agent turns and Steve's own calls of workspace `ws`):
    /// per model the calls, the last success and the last failure. A passive check of which models
    /// currently answer: no model is asked anything.
    pub fn used(&self, ws: &str, harness: &str, since: i64) -> BTreeMap<String, Value> {
        self.store
            .reader::<_, steve_store::StoreError>(|c| {
                let mut st = c.prepare(
                    "SELECT model, count(*), max(CASE WHEN ok = 1 THEN ts END), max(CASE WHEN ok = 0 THEN ts END) FROM usage_records
                     WHERE workspace_id = ? AND day >= ? AND harness = ? AND ts >= ? AND model != '' GROUP BY model",
                )?;
                let v = st
                    .query_map(rusqlite::params![ws, day_floor(since), harness, since], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            json!({"calls": r.get::<_, i64>(1)?, "ok_at": r.get::<_, Option<i64>>(2)?, "failed_at": r.get::<_, Option<i64>>(3)?}),
                        ))
                    })?
                    .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
                Ok(v)
            })
            .unwrap_or_default()
    }

    /// `GET /api/settings/models`: Steve's runtime, every purpose with its override, what it resolves
    /// to and what it ran on recently, the agent defaults, the harnesses and their efforts.
    pub fn view(&self, ws: &str) -> Value {
        let st = self.steve();
        let recent = self
            .ws_of(ws)
            .map(|w| self.recent(&w, steve_store::now() - 7 * 86_400_000))
            .unwrap_or_default();
        let purposes: Vec<Value> = PURPOSES
            .iter()
            .map(|p| {
                let custom = self.custom(ws, p.key);
                let effective = self.resolve(ws, p.key, &st);
                json!({
                    "key": p.key, "label": purpose_label(p.key), "desc": purpose_desc(p.key),
                    "value": custom,
                    "follows": custom.is_none(),
                    "effective": effective,
                    "recommended": self.recommended(p),
                    "recent": recent.get(p.key),
                })
            })
            .collect();
        let list = self.harnesses();
        let agents: Vec<Value> = list
            .iter()
            .map(|h| {
                let value = self.ws_of(ws).and_then(|w| {
                    self.store
                        .settings()
                        .get::<Option<String>>(&w, &agent_key(&h.id), None)
                        .ok()
                        .flatten()
                });
                json!({
                    "harness": h.id, "name": h.name, "models": h.model_ids(),
                    "default": self.defaults.agent.get(&h.id),
                    "value": value,
                })
            })
            .collect();
        let harnesses: Vec<Value> = list
            .iter()
            .map(|h| {
                json!({
                    "id": h.id, "name": h.name, "builtin": h.builtin, "ready": h.ready,
                    "modes": h.modes,
                    "efforts": efforts_shown(&h.efforts),
                    "default_model": h.default_model,
                    "default_models": self.defaults.harness_list(Some(h)),
                })
            })
            .collect();
        json!({
            "steve": st,
            "purposes": purposes,
            "agents": agents,
            "harnesses": harnesses,
            "max": MAX_MODELS,
        })
    }

    /// The current stored value of every key `patch` touches (for undo and the audit's `before`).
    pub fn stored(&self, ws: &str, patch: &Map<String, Value>) -> Map<String, Value> {
        patch
            .keys()
            .map(|k| {
                let v: Option<Value> = self.store.settings().get(ws, k, None).ok().flatten();
                (k.clone(), v.unwrap_or(Value::Null))
            })
            .collect()
    }

    /// Validates `patch` ([`validate`]), stores it (a settings revision) and audits it. Returns the
    /// settings keys written.
    pub fn apply(
        &self,
        ws: &str,
        patch: &Value,
        actor: &str,
    ) -> std::result::Result<Map<String, Value>, String> {
        let values = validate(patch, &self.harnesses())?;
        self.write(ws, &values, actor, "settings.models")
            .map_err(|e| e.to_string())?;
        Ok(values)
    }

    /// Stores settings keys already validated, with a revision and an audit row (`before` and `after`).
    pub fn write(
        &self,
        ws: &str,
        values: &Map<String, Value>,
        actor: &str,
        action: &str,
    ) -> steve_store::Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let before = self.stored(ws, values);
        self.store.settings().set(ws, values, actor)?;
        self.store.audit(
            actor,
            action,
            ws,
            &json!({"before": before, "after": values}),
            ws,
        )
    }
}

/// Checks a `PATCH /api/settings/models` body and turns it into settings keys:
/// `{"purposes": {"route": {"harness": "<adapter>", "models": [...], "effort": "high"} | null}, "agents": {"<adapter>": "id" | null}}`.
/// `null` (or an empty object) puts a purpose back on Steve. A missing harness means the default
/// adapter. A harness is one of `harnesses`, an effort one it takes.
pub fn validate(
    patch: &Value,
    harnesses: &[Harness],
) -> std::result::Result<Map<String, Value>, String> {
    let o = patch
        .as_object()
        .ok_or_else(|| steve_i18n::t!("core.models.invalid.not_object"))?;
    let mut out = Map::new();
    for k in o.keys() {
        if k != "purposes" && k != "agents" {
            return Err(steve_i18n::t!(
                "core.models.invalid.unknown_field",
                field = k
            ));
        }
    }
    if let Some(ps) = o.get("purposes") {
        let ps = ps
            .as_object()
            .ok_or_else(|| steve_i18n::t!("core.models.invalid.purposes_object"))?;
        for (k, v) in ps {
            let p = PURPOSES
                .iter()
                .find(|p| p.key == k)
                .ok_or_else(|| steve_i18n::t!("core.models.invalid.no_purpose", purpose = k))?;
            out.insert(key(p.key), choice_value(p, v, harnesses)?);
        }
    }
    if let Some(ags) = o.get("agents") {
        let ags = ags
            .as_object()
            .ok_or_else(|| steve_i18n::t!("core.models.invalid.agents_object"))?;
        for (h, v) in ags {
            if !harnesses.iter().any(|x| &x.id == h) {
                return Err(steve_i18n::t!(
                    "core.models.invalid.no_harness",
                    harness = h
                ));
            }
            let m = match v {
                Value::Null => Value::Null,
                Value::String(s) if s.trim().is_empty() => Value::Null,
                Value::String(s) if valid_model(s.trim()) => json!(s.trim()),
                _ => {
                    return Err(steve_i18n::t!(
                        "core.models.invalid.agent_model",
                        harness = h
                    ));
                }
            };
            out.insert(agent_key(h), m);
        }
    }
    if out.is_empty() {
        return Err(steve_i18n::t!("core.models.invalid.nothing"));
    }
    Ok(out)
}

fn choice_value(
    p: &Purpose,
    v: &Value,
    harnesses: &[Harness],
) -> std::result::Result<Value, String> {
    let o = match v {
        Value::Null => return Ok(Value::Null),
        Value::Object(o) => o,
        _ => {
            return Err(steve_i18n::t!(
                "core.models.invalid.choice",
                purpose = purpose_label(p.key)
            ));
        }
    };
    let mut models: Vec<String> = vec![];
    for (k, x) in o {
        match k.as_str() {
            "models" => {
                let a = x.as_array().ok_or_else(|| {
                    steve_i18n::t!(
                        "core.models.invalid.models_list",
                        purpose = purpose_label(p.key)
                    )
                })?;
                for m in a {
                    let m = m.as_str().map(str::trim).ok_or_else(|| {
                        steve_i18n::t!(
                            "core.models.invalid.model_text",
                            purpose = purpose_label(p.key)
                        )
                    })?;
                    if !valid_model(m) {
                        return Err(steve_i18n::t!(
                            "core.models.invalid.model_name",
                            purpose = purpose_label(p.key),
                            model = m
                        ));
                    }
                    if models.iter().any(|x| x == m) {
                        return Err(steve_i18n::t!(
                            "core.models.invalid.model_dup",
                            purpose = purpose_label(p.key),
                            model = m
                        ));
                    }
                    models.push(m.to_string());
                }
                if models.len() > MAX_MODELS {
                    return Err(steve_i18n::t!(
                        "core.models.invalid.too_many",
                        purpose = purpose_label(p.key),
                        max = MAX_MODELS
                    ));
                }
            }
            "effort" | "harness" => {}
            _ => {
                return Err(steve_i18n::t!(
                    "core.models.invalid.choice_field",
                    purpose = purpose_label(p.key),
                    field = k
                ));
            }
        }
    }
    let known = |h: &str| harnesses.iter().find(|x| x.id == h);
    let harness = match o.get("harness") {
        None | Some(Value::Null) => None,
        Some(Value::String(h)) if known(h).is_some() => Some(h.clone()),
        Some(_) => {
            return Err(steve_i18n::t!(
                "core.models.invalid.choice_harness",
                purpose = purpose_label(p.key)
            ));
        }
    };
    let h = harness.clone().unwrap_or_else(default_adapter);
    let effort = match o.get("effort") {
        None | Some(Value::Null) => None,
        Some(Value::String(e)) if e.is_empty() => None,
        Some(Value::String(e)) if known(&h).is_some_and(|x| x.takes_effort(e)) => Some(e.clone()),
        Some(_) => {
            return Err(steve_i18n::t!(
                "core.models.invalid.choice_effort",
                purpose = purpose_label(p.key)
            ));
        }
    };
    if harness.is_none() && models.is_empty() && effort.is_none() {
        return Ok(Value::Null);
    }
    Ok(serde_json::to_value(Choice {
        harness: h,
        models,
        effort,
    })
    .unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_store::{BotPatch, NewBot, NewUsage};

    const CLAUDE_CODE: &str = "claude-code";
    const CODEX: &str = "codex";

    fn store(dir: &std::path::Path) -> Store {
        let s = Store::open(dir.join("steve.sqlite3"), steve_store::FnPublisher(|_| {})).unwrap();
        s.ensure_workspace().unwrap();
        s
    }

    /// The main bot as `orchestrator::seed` makes it, on `harness` with `model`.
    fn steve_bot(s: &Store, harness: &str, model: Option<&str>) {
        s.bots()
            .seed(
                "steve",
                true,
                &NewBot {
                    name: "Steve".into(),
                    harness: harness.into(),
                    model: model.map(String::from),
                    ..NewBot::default()
                },
                "default",
            )
            .unwrap();
    }

    fn patch_bot(s: &Store, p: BotPatch) {
        s.bots().patch("steve", &p, "me").unwrap();
    }

    fn defaults() -> Defaults {
        Defaults {
            fast: vec!["haiku".into(), "sonnet".into()],
            smart: vec!["sonnet".into(), "opus".into()],
            research: vec!["r1".into(), "r2".into()],
            check: vec![],
            agent: BTreeMap::from([("claude-code".into(), "opus".into())]),
        }
    }

    fn l(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn ch(h: &str, m: &[&str], e: Option<&str>) -> Choice {
        Choice {
            harness: h.into(),
            models: l(m),
            effort: e.map(String::from),
        }
    }

    #[test]
    fn everything_follows_the_steve_bot() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        let m = Models::new(s.clone(), defaults());
        // no main bot at all: Claude Code on the smart list
        assert_eq!(
            m.for_purpose("default", "chat"),
            ch(CLAUDE_CODE, &["sonnet", "opus"], None)
        );

        // a Claude Code Steve with no model: the smart list, for every purpose and any tier
        steve_bot(&s, CLAUDE_CODE, None);
        for p in [
            "route",
            "chat",
            "research",
            "title",
            "disclosure",
            "digest",
            "other",
            "intent",
        ] {
            assert_eq!(
                m.for_call("default", p, Tier::Fast),
                ch(CLAUDE_CODE, &["sonnet", "opus"], None),
                "{p}"
            );
        }
        // its model comes first, the smart list is the fallback; its effort is passed on
        patch_bot(
            &s,
            BotPatch {
                model: Some(Some("claude-opus-5-5".into())),
                reasoning_effort: Some(Some("high".into())),
                ..BotPatch::default()
            },
        );
        assert_eq!(
            m.for_purpose("", "chat"),
            ch(
                CLAUDE_CODE,
                &["claude-opus-5-5", "sonnet", "opus"],
                Some("high")
            )
        );
        // Codex Steve: its model, no fallback list; an effort Codex does not take is dropped
        patch_bot(
            &s,
            BotPatch {
                harness: Some(CODEX.into()),
                model: Some(Some("gpt-6-luna".into())),
                reasoning_effort: Some(Some("ultra".into())),
                ..BotPatch::default()
            },
        );
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CODEX, &["gpt-6-luna"], Some("ultra"))
        );
        patch_bot(
            &s,
            BotPatch {
                model: Some(None),
                reasoning_effort: Some(Some("max".into())),
                ..BotPatch::default()
            },
        );
        // no model: the adapter's own default, shown as such (Codex takes max too)
        assert_eq!(
            m.for_purpose("default", "chat"),
            ch(CODEX, &[], Some("max"))
        );
        let st = m.steve();
        assert_eq!(
            (st.harness.as_str(), st.runs_on.as_str(), st.note),
            (CODEX, CODEX, None)
        );
        assert_eq!(st.model_note.as_deref(), Some("该 harness 自己的默认模型"));
        // a harness with no adapter here: still that harness (its judgments fail), said so; never
        // moved to another one
        patch_bot(
            &s,
            BotPatch {
                harness: Some("gemini".into()),
                model: Some(Some("gemini-x".into())),
                ..BotPatch::default()
            },
        );
        let st = m.steve();
        assert_eq!(st.runs_on, "gemini");
        assert!(st.note.as_deref().is_some_and(|n| n.contains("gemini")));
        assert_eq!(
            m.for_purpose("default", "chat"),
            ch("gemini", &["gemini-x"], None)
        );
    }

    #[test]
    fn an_override_wins_with_its_harness_models_and_effort() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CLAUDE_CODE, Some("claude-sonnet-5-5"));
        let m = Models::new(s.clone(), defaults());
        m.apply(
            "default",
            &json!({"purposes": {
                "route": {"harness": "claude-code", "models": ["opus", "haiku"], "effort": "low"},
                "chat": {"harness": "codex", "models": ["gpt-6-sol"], "effort": "ultra"},
                "draft": {"harness": "codex"},
                "title": {"harness": "claude-code", "effort": "max"},
            }}),
            "me",
        )
        .unwrap();
        assert_eq!(
            m.for_call("default", "route", Tier::Fast),
            ch(CLAUDE_CODE, &["opus", "haiku"], Some("low"))
        );
        // channel-rule intent is routing; a call with no workspace is for the only workspace
        assert_eq!(
            m.for_call("", "intent", Tier::Fast).models,
            l(&["opus", "haiku"])
        );
        assert_eq!(
            m.for_purpose("default", "chat"),
            ch(CODEX, &["gpt-6-sol"], Some("ultra"))
        );
        // a harness alone: that harness's default model (not Steve's, which is another harness's)
        assert_eq!(m.for_purpose("default", "draft"), ch(CODEX, &[], None));
        // on Steve's harness with no models: Steve's models, the override's effort
        assert_eq!(
            m.for_purpose("default", "title"),
            ch(
                CLAUDE_CODE,
                &["claude-sonnet-5-5", "sonnet", "opus"],
                Some("max")
            )
        );
        // null goes back to following Steve
        m.apply("default", &json!({"purposes": {"route": null}}), "me")
            .unwrap();
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CLAUDE_CODE, &["claude-sonnet-5-5", "sonnet", "opus"], None)
        );
        // a value stored before Steve was a bot (no harness) ran on Claude Code and still does
        s.settings()
            .set(
                "default",
                json!({"models.yolo": {"models": ["opus"], "effort": "high"}})
                    .as_object()
                    .unwrap(),
                "me",
            )
            .unwrap();
        assert_eq!(
            m.for_purpose("default", "yolo"),
            ch(CLAUDE_CODE, &["opus"], Some("high"))
        );
        // agent defaults are unchanged
        m.apply(
            "default",
            &json!({"agents": {"claude-code": "sonnet"}}),
            "me",
        )
        .unwrap();
        assert_eq!(
            m.agent_model("default", "claude-code").as_deref(),
            Some("sonnet")
        );
        m.apply("default", &json!({"agents": {"claude-code": ""}}), "me")
            .unwrap();
        assert_eq!(
            m.agent_model("default", "claude-code").as_deref(),
            Some("opus")
        );
        assert_eq!(m.agent_model("default", "codex"), None);
    }

    #[test]
    fn migration_keeps_the_cheap_purposes_on_the_fast_list_once() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CODEX, None);
        let mut df = defaults();
        df.check = l(&["check"]);
        let m = Models::new(s.clone(), df);
        // an override the user made before stays as it is
        s.settings()
            .set(
                "default",
                json!({"models.title": {"models": ["opus"]}})
                    .as_object()
                    .unwrap(),
                "me",
            )
            .unwrap();
        assert!(m.migrate("default").unwrap());
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CLAUDE_CODE, &["haiku", "sonnet"], None)
        );
        assert_eq!(
            m.for_purpose("default", "flow"),
            ch(CLAUDE_CODE, &["haiku", "sonnet"], None)
        );
        assert_eq!(
            m.for_purpose("default", "disclosure"),
            ch(CLAUDE_CODE, &["check"], None)
        );
        assert_eq!(
            m.for_purpose("default", "title"),
            ch(CLAUDE_CODE, &["opus"], None)
        );
        // the rest follows Steve (Codex here)
        for p in [
            "chat", "research", "extract", "yolo", "draft", "mail", "compose",
        ] {
            assert_eq!(m.for_purpose("default", p), ch(CODEX, &[], None), "{p}");
            assert!(m.custom("default", p).is_none());
        }
        // once: removing an override afterwards is respected
        m.apply("default", &json!({"purposes": {"route": null}}), "me")
            .unwrap();
        assert!(!m.migrate("default").unwrap());
        assert_eq!(m.for_purpose("default", "route").harness, CODEX);
        let v = m.view("default");
        let route = v["purposes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "route")
            .unwrap();
        assert_eq!(route["follows"], json!(true));
        assert_eq!(
            route["recommended"],
            json!({"harness": "claude-code", "models": ["haiku", "sonnet"]})
        );
        assert_eq!(v["steve"]["runs_on"], json!("codex"));
    }

    /// The built-in adapters, `id` having answered a probe with `models`.
    fn probed(m: &Models, id: &'static str, models: &'static [&'static str]) {
        m.set_harnesses(Arc::new(move || {
            let mut list = crate::harnesses::builtin();
            for h in list.iter_mut().filter(|h| h.id == id) {
                h.ready = Some(true);
                h.models = models
                    .iter()
                    .map(|m| HarnessModel {
                        id: m.to_string(),
                        name: None,
                        description: None,
                    })
                    .collect();
            }
            list
        }));
    }

    #[test]
    fn the_cheap_purposes_get_the_harness_s_quick_model_once_it_has_said_so() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CLAUDE_CODE, None);
        // the environment names no model at all
        let m = Models::new(s.clone(), Defaults::default());
        let route = purpose_of("route").unwrap();
        // before the adapter has answered: its declared list is not what this installation has, so
        // nothing is recommended, written or marked
        assert_eq!(m.recommended(route), None);
        assert!(!m.migrate("default").unwrap());
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CLAUDE_CODE, &[], None)
        );
        // it offers no model its declaration calls quick: still nothing
        probed(&m, CLAUDE_CODE, &["default", "opus"]);
        assert!(!m.migrate("default").unwrap());
        // it does: the newest model with the earliest declared word ([`Harness::fast_model`])
        probed(
            &m,
            CLAUDE_CODE,
            &["default", "opus", "claude-haiku-4-5", "claude-haiku-5-5"],
        );
        assert_eq!(
            m.recommended(route),
            Some(ch(CLAUDE_CODE, &["claude-haiku-5-5"], None))
        );
        assert_eq!(m.recommended(purpose_of("chat").unwrap()), None);
        // an override made before stays as it is
        m.apply(
            "default",
            &json!({"purposes": {"title": {"harness": CLAUDE_CODE, "models": ["opus"]}}}),
            "me",
        )
        .unwrap();
        assert!(m.migrate("default").unwrap());
        for p in ["route", "disclosure", "flow", "intent"] {
            assert_eq!(
                m.for_purpose("default", p),
                ch(CLAUDE_CODE, &["claude-haiku-5-5"], None),
                "{p}"
            );
        }
        assert_eq!(
            m.for_purpose("default", "title"),
            ch(CLAUDE_CODE, &["opus"], None)
        );
        // everything else still follows Steve
        for p in [
            "chat", "research", "extract", "yolo", "draft", "mail", "compose",
        ] {
            assert!(m.custom("default", p).is_none(), "{p}");
        }
        // once: put back on Steve afterwards, it stays there, on every later probe
        m.apply("default", &json!({"purposes": {"route": null}}), "me")
            .unwrap();
        m.migrate_all();
        assert!(!m.migrate("default").unwrap());
        assert!(m.custom("default", "route").is_none());

        // Steve on another harness: that harness's own quick model, on that harness
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CODEX, None);
        let m = Models::new(s.clone(), Defaults::default());
        probed(
            &m,
            CODEX,
            &["gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-luna"],
        );
        m.migrate_all();
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CODEX, &["gpt-6-luna"], None)
        );
        // an adapter that declares no such word: nothing, and asked again later
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, "mine", None);
        let m = Models::new(s.clone(), Defaults::default());
        m.set_harnesses(Arc::new(|| {
            let mut h = Harness::declared("mine", None);
            h.ready = Some(true);
            h.models = vec![HarnessModel {
                id: "x-haiku-mini".into(),
                name: None,
                description: None,
            }];
            vec![h]
        }));
        assert!(!m.migrate("default").unwrap());
        assert!(m.custom("default", "route").is_none());

        // a fast list in the environment that was written under `models.v2`: not written again
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CLAUDE_CODE, None);
        s.settings()
            .set(
                "default",
                json!({"models.v2": 2}).as_object().unwrap(),
                "steve",
            )
            .unwrap();
        let m = Models::new(s.clone(), defaults());
        probed(&m, CLAUDE_CODE, &["haiku"]);
        assert!(!m.migrate("default").unwrap());
        assert!(m.custom("default", "route").is_none());
        // an installation marked `models.v2` with no list in the environment gets the harness's
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        steve_bot(&s, CLAUDE_CODE, None);
        s.settings()
            .set(
                "default",
                json!({"models.v2": 2}).as_object().unwrap(),
                "steve",
            )
            .unwrap();
        let m = Models::new(s.clone(), Defaults::default());
        probed(&m, CLAUDE_CODE, &["haiku"]);
        assert!(m.migrate("default").unwrap());
        assert_eq!(
            m.for_purpose("default", "route"),
            ch(CLAUDE_CODE, &["haiku"], None)
        );
    }

    #[test]
    fn recent_usage_per_purpose() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        let m = Models::new(s.clone(), defaults());
        let add = |purpose: &str, harness: &str, model: &str, ok: bool, ts: i64| {
            s.stats()
                .usage_add(&NewUsage {
                    workspace_id: "default".into(),
                    ts,
                    day: steve_stats::day(ts, None),
                    source: "gateway".into(),
                    purpose: purpose.into(),
                    harness: harness.into(),
                    turn_id: None,
                    session_id: None,
                    item_id: None,
                    bot_id: None,
                    project_id: None,
                    usage: steve_store::ModelUsage {
                        model: model.into(),
                        ..Default::default()
                    },
                    duration_ms: None,
                    ok,
                    attempts: 1,
                })
                .unwrap();
        };
        let now = steve_store::now();
        add("route", "claude-code", "claude-haiku-5-5", true, now - 5000);
        add(
            "intent",
            "claude-code",
            "claude-haiku-5-5",
            true,
            now - 4000,
        );
        add("route", "codex", "gpt-6-luna", false, now - 1000);
        add("chat", "claude-code", "old", true, now - 30 * 86_400_000);
        let r = m.recent("default", now - 7 * 86_400_000);
        assert_eq!(
            r["route"]["last"],
            json!({"harness": "codex", "model": "gpt-6-luna", "at": now - 1000})
        );
        assert_eq!(r["route"]["used"][0]["calls"], json!(2));
        assert_eq!(r["route"]["used"][0]["harness"], json!("claude-code"));
        assert_eq!(r["route"]["used"][1]["failed"], json!(1));
        assert!(!r.contains_key("chat"), "older than the window");
    }

    /// The day bound of the usage queries never cuts a record of the window, in any zone.
    #[test]
    fn the_day_floor_is_not_after_any_zone_s_day() {
        let since = steve_store::now();
        for z in ["Pacific/Kiritimati", "Etc/GMT+12", "Asia/Shanghai", "UTC"] {
            assert!(
                day_floor(since) <= steve_stats::day(since, steve_stats::tz(Some(z))),
                "{z}"
            );
        }
        assert!(day_floor(since) <= steve_stats::day(since, None));
    }

    #[test]
    fn validation() {
        let bad = [
            json!([]),
            json!({}),
            json!({"x": 1}),
            json!({"purposes": {"nope": null}}),
            json!({"purposes": {"route": "haiku"}}),
            json!({"purposes": {"route": {"models": "haiku"}}}),
            json!({"purposes": {"route": {"models": ["a b"]}}}),
            json!({"purposes": {"route": {"models": ["-x"]}}}),
            json!({"purposes": {"route": {"models": ["a", "a"]}}}),
            json!({"purposes": {"route": {"models": ["a", "b", "c", "d", "e", "f"]}}}),
            json!({"purposes": {"route": {"models": ["a"], "effort": "huge"}}}),
            json!({"purposes": {"route": {"models": ["a"], "tier": "fast"}}}),
            json!({"purposes": {"route": {"harness": "gemini"}}}),
            // an effort only the other harness has
            json!({"purposes": {"route": {"harness": "claude-code", "effort": "ultra"}}}),
            json!({"agents": {"gemini": "x"}}),
            json!({"agents": {"codex": 3}}),
        ];
        let hs = crate::harnesses::builtin();
        for b in bad {
            assert!(validate(&b, &hs).is_err(), "{b}");
        }
        let ok = validate(
            &json!({"purposes": {
            "research": {"models": [" claude-opus-5-5[1m] ", "us.anthropic.claude-x-v1:0"], "effort": "xhigh"},
            "chat": {"models": []},
            "route": {"harness": "codex", "effort": "ultra"},
        }, "agents": {"codex": "gpt-6-sol"}}),
            &hs,
        )
        .unwrap();
        assert_eq!(
            Value::Object(ok),
            json!({
                "models.research": {"harness": "claude-code", "models": ["claude-opus-5-5[1m]", "us.anthropic.claude-x-v1:0"], "effort": "xhigh"},
                "models.chat": null,
                "models.route": {"harness": "codex", "models": [], "effort": "ultra"},
                "models.agent.codex": "gpt-6-sol",
            })
        );
    }

    #[test]
    fn changes_are_revisions_and_audited() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path());
        let m = Models::new(s.clone(), defaults());
        m.apply(
            "default",
            &json!({"purposes": {"yolo": {"models": ["opus"]}}}),
            "u1",
        )
        .unwrap();
        m.apply(
            "default",
            &json!({"purposes": {"yolo": {"harness": "codex", "models": ["g"]}}}),
            "u1",
        )
        .unwrap();
        let (audits, revs): (Vec<String>, i64) = s
            .with_conn(|c| {
                let mut st = c.prepare(
                    "SELECT detail FROM audit WHERE action = 'settings.models' AND actor = 'u1' ORDER BY id",
                )?;
                let a = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let n = c.query_row(
                    "SELECT count(*) FROM revisions WHERE object_type = 'settings' AND author = 'u1'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((a, n))
            })
            .unwrap();
        assert_eq!(revs, 2);
        assert_eq!(audits.len(), 2);
        let last: Value = serde_json::from_str(&audits[1]).unwrap();
        assert_eq!(last["before"]["models.yolo"]["models"], json!(["opus"]));
        assert_eq!(
            last["after"]["models.yolo"],
            json!({"harness": "codex", "models": ["g"]})
        );
        let v = m.view("default");
        let y = v["purposes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "yolo")
            .unwrap();
        assert_eq!(y["value"]["models"], json!(["g"]));
        assert_eq!(y["effective"]["harness"], json!("codex"));
        assert_eq!(v["purposes"].as_array().unwrap().len(), 11);
        assert_eq!(v["harnesses"][1]["efforts"][0]["key"], json!("low"));
    }
}
