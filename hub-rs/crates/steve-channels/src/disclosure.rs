//! The audience-aware disclosure check (C5, channels-personas.md §2.6): B1's
//! [`steve_modules::outbound::OutboundCheck`], used by every channel message and by email drafts and
//! GitHub comments.
//!
//! For one text and its audience:
//! 0. secrets: what the vault's guard calls a secret (`guard::scan`: tokens, keys, card numbers,
//!    “密码是 …”) or a value the redactor knows ([`SecretScan`]) → [`Verdict::Blocked`], never
//!    rewritten;
//! 1. effective levels: knowledge whose visibility the audience is entirely inside counts as
//!    `public` (quoting this group's own discussion back to it);
//! 2. the rule check: names of contacts and organizations (and their aliases) not in the audience,
//!    amounts and numbers with units, URLs, file names, other spaces' names and item numbers that are
//!    in the reply *and* only in `conclusion` / `reasoning` / `never` knowledge → a span each;
//! 3. the model check on a model other than the drafting one (`STEVE_MODELS_CHECK`, default the first
//!    of `STEVE_MODELS_FAST` that differs; without a pinned runner the gateway's fast tier): the whole
//!    reply, the audience, each piece of knowledge with its effective level →
//!    `{"verdict":"pass|leak","spans":[…]}`. It also catches combinations and “根据之前和 Acme 的讨论”.
//!
//! Any span → [`Verdict::Leak`]. A failing or unreadable check model is a leak too, so a broken check
//! never lets text through. Rewriting is the caller's: channels rewrite up to [`MAX_REWRITES`] times
//! ([`rewrite_prompt`]), email doesn't.
//!
//! Wiring (integrator): the check is registered before `Modules` exists and bound after:
//! ```ignore
//! let disclosure = steve_channels::disclosure::Disclosure::new();
//! registry.outbound_check(Box::new(disclosure.clone()));
//! // … modules = Modules::new(store, gateway, registry) …
//! disclosure.bind(modules.store.clone(), modules.gateway.clone());
//! ```

use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::Value;
use steve_modules::outbound::{
    Audience, BasisEntry, CheckCx, CheckResult, KnowledgeUse, OutboundCheck, Span, Verdict,
};
use steve_modules::{Gateway, Tier};
use steve_store::Store;
use steve_store::base::Level;

use crate::compose::PinnedModel;

/// The check prompt starts with this (tests script the gateway by it).
pub const CHECK_HEAD: &str = "【披露检查】";
/// The rewrite prompt starts with this.
pub const REWRITE_HEAD: &str = "【改写回复】";
/// Rewrites before a leaking reply becomes a draft (§2.6 step 3).
pub const MAX_REWRITES: usize = 2;
/// The check model list (comma separated).
pub const ENV_CHECK: &str = "STEVE_MODELS_CHECK";
/// The fast list, the default source of check models.
pub const ENV_FAST: &str = "STEVE_MODELS_FAST";
/// The check model's time limit.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// A name shorter than this (in characters) is too common to count as a disclosure.
const MIN_NAME: usize = 2;

/// Span reasons (verbatim in the inbox).
pub const R_SECRET: &str = "包含机密值";
pub const R_UNBOUND: &str = "披露检查还没有接上";
pub const R_MODEL_FAILED: &str = "检查模型没有给出结果";

/// The vault redactor's registered values, as the check needs them: the field labels of values found
/// in `text`. The integrator wires the vault's; default none (the guard still runs).
pub trait SecretScan: Send + Sync {
    fn find(&self, ws: &str, text: &str) -> Vec<String>;
}

/// No registered values.
pub struct NoSecrets;

impl SecretScan for NoSecrets {
    fn find(&self, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }
}

struct Inner {
    bound: OnceLock<(Store, Arc<dyn Gateway>)>,
    secrets: OnceLock<Arc<dyn SecretScan>>,
    pinned: OnceLock<Arc<dyn PinnedModel>>,
    /// Candidate check models, in order (from the environment unless set).
    models: Vec<String>,
}

/// C5's disclosure check. Cheap to clone; register one clone, bind another.
#[derive(Clone)]
pub struct Disclosure(Arc<Inner>);

impl Default for Disclosure {
    fn default() -> Self {
        Self::new()
    }
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Disclosure {
    /// Check models from `STEVE_MODELS_CHECK`, else `STEVE_MODELS_FAST`.
    pub fn new() -> Self {
        let mut models = env_list(ENV_CHECK);
        if models.is_empty() {
            models = env_list(ENV_FAST);
        }
        Self::with_models(models)
    }

    /// Explicit check models (tests).
    pub fn with_models(models: Vec<String>) -> Self {
        Self(Arc::new(Inner {
            bound: OnceLock::new(),
            secrets: OnceLock::new(),
            pinned: OnceLock::new(),
            models,
        }))
    }

    /// The store (names of contacts, organizations, spaces) and the gateway. Once; later calls are
    /// ignored. Unbound, every check is a leak ([`R_UNBOUND`]).
    pub fn bind(&self, store: Store, gateway: Arc<dyn Gateway>) {
        let _ = self.0.bound.set((store, gateway));
    }

    /// The vault's registered values.
    pub fn secrets(&self, s: Arc<dyn SecretScan>) {
        let _ = self.0.secrets.set(s);
    }

    /// Runs the check on a specific model (when the gateway can pin one).
    pub fn pinned(&self, p: Arc<dyn PinnedModel>) {
        let _ = self.0.pinned.set(p);
    }

    /// The model the check uses for a draft written by `draft_model`: the first candidate that
    /// differs. `None`: the gateway's fast tier decides.
    pub fn check_model(&self, draft_model: Option<&str>) -> Option<String> {
        self.0
            .models
            .iter()
            .find(|m| Some(m.as_str()) != draft_model)
            .cloned()
    }

    async fn run_model(&self, cx: &CheckCx, prompt: &str) -> Result<(String, String), String> {
        let (_, gw) = self.0.bound.get().ok_or(R_UNBOUND)?;
        let model = self.check_model(cx.draft_model.as_deref());
        let out = match (self.0.pinned.get(), &model) {
            (Some(p), Some(m)) => p.run(m, prompt, TIMEOUT).await,
            _ => {
                steve_modules::gateway::purpose("disclosure", gw.run(Tier::Fast, prompt, TIMEOUT))
                    .await
            }
        }?;
        let name = if out.model.is_empty() {
            model.unwrap_or_default()
        } else {
            out.model
        };
        Ok((out.text, name))
    }
}

// ---------------------------------------------------------------- levels

/// The level of `k` for this audience: `public` when the audience is entirely inside the source's
/// visibility; `never` stays `never`.
pub fn effective(k: &KnowledgeUse, a: &Audience) -> Level {
    if k.level == Level::Never {
        return Level::Never;
    }
    if a.scopes.iter().any(|s| s == &k.visibility) {
        return Level::Public;
    }
    k.level
}

/// The Chinese label of a level (as in the prompt).
pub fn level_label(l: Level) -> &'static str {
    match l {
        Level::Public => "可以引用",
        Level::Conclusion => "可转述结论",
        Level::Reasoning => "仅用于推理",
        Level::Never => "不可使用",
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The part of `text` that is `term` with whitespace ignored (“30 万” for “30万”), as written there.
fn find_loose(text: &str, term: &str) -> Option<String> {
    let want: Vec<char> = term.chars().filter(|c| !c.is_whitespace()).collect();
    if want.is_empty() {
        return None;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for start in 0..chars.len() {
        let (mut i, mut k) = (start, 0);
        while i < chars.len() && k < want.len() {
            let c = chars[i].1;
            if c == want[k] {
                k += 1;
            } else if !(c.is_whitespace() && k > 0) {
                break;
            }
            i += 1;
        }
        if k == want.len() {
            let end = chars.get(i).map_or(text.len(), |x| x.0);
            return Some(text[chars[start].0..end].to_string());
        }
    }
    None
}

/// Whether the reply shares content with a piece of knowledge: a run of 6 characters (whitespace
/// ignored), or one of its sensitive terms.
fn overlaps(reply: &str, k: &str, terms: &[String]) -> bool {
    let r = squash(reply);
    let k: Vec<char> = squash(k).chars().collect();
    if terms.iter().any(|t| r.contains(&squash(t))) {
        return true;
    }
    const RUN: usize = 6;
    k.windows(RUN)
        .any(|w| r.contains(&w.iter().collect::<String>()))
}

// ---------------------------------------------------------------- the rule check

static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"[¥￥$€£]\s*\d[\d,.]*\s*[万亿千kKwWmM]?",
        r"|\d[\d,.]*\s*(?:万元|亿元|万美元|美元|美金|万|亿|千|元|块|%|％|人|台|个|家|天|小时|周|个月|月|年|GB|TB|MB|USD|RMB|CNY|[kK]\b|[wW]\b)",
    ))
    .expect("amount regex")
});
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s，。、）)」』>\]]+").expect("url regex"));
static FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\w\-\p{Han}]+\.(?:pdf|docx?|xlsx?|pptx?|csv|txt|md|zip|rar|png|jpe?g|gif|key|numbers|pages|json|ya?ml|sql|log)\b")
        .expect("file regex")
});
static ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{1,6}-\d{1,7}\b").expect("item regex"));

/// The pattern terms of a text: amounts, URLs, file names, item numbers.
pub fn pattern_terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for re in [&*AMOUNT, &*URL, &*FILE, &*ITEM] {
        for m in re.find_iter(text) {
            let t = m.as_str().trim().to_string();
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// Names Steve knows in a workspace: contacts and organizations (with aliases) and spaces.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// `(party id, name or alias)`.
    pub parties: Vec<(String, String)>,
    /// `(space id, name)`.
    pub spaces: Vec<(String, String)>,
}

impl Names {
    pub fn load(c: &Connection, ws: &str) -> rusqlite::Result<Self> {
        let mut parties = Vec::new();
        let mut st = c.prepare_cached(
            "SELECT id, name, aliases FROM parties WHERE workspace_id = ? AND status = 'active' AND merged_into IS NULL",
        )?;
        let rows = st.query_map([ws], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, name, aliases) = row?;
            parties.push((id.clone(), name));
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&aliases) {
                for x in a {
                    if let Some(s) = x.as_str() {
                        parties.push((id.clone(), s.to_string()));
                    }
                }
            }
        }
        let mut spaces = Vec::new();
        let mut st =
            c.prepare_cached("SELECT id, name FROM spaces WHERE workspace_id = ? AND name <> ''")?;
        for row in st.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))? {
            spaces.push(row?);
        }
        parties.retain(|(_, n)| n.trim().chars().count() >= MIN_NAME);
        spaces.retain(|(_, n): &(String, String)| n.trim().chars().count() >= MIN_NAME);
        Ok(Self { parties, spaces })
    }

    /// The names in `text` the audience does not already know (not their own, not their space's).
    fn sensitive_in(&self, text: &str, a: &Audience) -> Vec<String> {
        let mut out = Vec::new();
        for (id, n) in &self.parties {
            if !a.parties.contains(id) && text.contains(n.as_str()) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        for (id, n) in &self.spaces {
            let own = a.scopes.iter().any(|s| s == &format!("space:{id}"));
            if !own && text.contains(n.as_str()) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    }
}

/// Every sensitive term of one piece of knowledge.
fn terms_of(k: &KnowledgeUse, names: &Names, a: &Audience) -> Vec<String> {
    let mut t = pattern_terms(&k.text);
    for n in names.sensitive_in(&k.text, a) {
        if !t.contains(&n) {
            t.push(n);
        }
    }
    t
}

/// Step 2 of the check: spans for terms in the reply that only non-public knowledge has. Returns the
/// spans and, per knowledge entry, whether one of them came from it.
pub fn rule_check(
    reply: &str,
    a: &Audience,
    knowledge: &[KnowledgeUse],
    names: &Names,
) -> (Vec<Span>, Vec<bool>) {
    let levels: Vec<Level> = knowledge.iter().map(|k| effective(k, a)).collect();
    let terms: Vec<Vec<String>> = knowledge.iter().map(|k| terms_of(k, names, a)).collect();
    let reply_sq = squash(reply);
    let public: BTreeSet<String> = knowledge
        .iter()
        .zip(&levels)
        .filter(|(_, l)| **l == Level::Public)
        .flat_map(|(k, _)| {
            let mut v = pattern_terms(&k.text);
            v.extend(names.sensitive_in(&k.text, a));
            v
        })
        .map(|t| squash(&t))
        .collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut flagged = vec![false; knowledge.len()];
    for (i, k) in knowledge.iter().enumerate() {
        if levels[i] == Level::Public {
            continue;
        }
        for t in &terms[i] {
            let sq = squash(t);
            if sq.is_empty() || !reply_sq.contains(&sq) || public.contains(&sq) {
                continue;
            }
            flagged[i] = true;
            if spans.iter().any(|s| squash(&s.text) == sq) {
                continue;
            }
            let shown = find_loose(reply, t).unwrap_or_else(|| t.clone());
            spans.push(Span {
                text: shown.clone(),
                reason: format!(
                    "「{shown}」只在{}（{}）里出现，这些听众不在它的可见范围",
                    if k.source_label.is_empty() {
                        "一条知识"
                    } else {
                        k.source_label.as_str()
                    },
                    level_label(levels[i])
                ),
            });
        }
    }
    (spans, flagged)
}

/// The basis entries (`reply_basis.entries`) for a checked reply.
pub fn entries(
    reply: &str,
    a: &Audience,
    knowledge: &[KnowledgeUse],
    names: &Names,
    flagged: &[bool],
) -> Vec<BasisEntry> {
    knowledge
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let eff = effective(k, a);
            let terms = terms_of(k, names, a);
            let blocked = flagged.get(i).copied().unwrap_or(false) || eff == Level::Never;
            BasisEntry {
                source_type: k.source_type.clone(),
                source_id: k.source_id.clone(),
                source_label: k.source_label.clone(),
                level: k.level,
                effective: eff,
                used: overlaps(reply, &k.text, &terms),
                out: if blocked {
                    "blocked"
                } else {
                    match eff {
                        Level::Public => "verbatim",
                        Level::Conclusion => "conclusion",
                        _ => "reasoning",
                    }
                }
                .into(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------- the model check

fn audience_line(a: &Audience) -> String {
    let d = if a.description.trim().is_empty() {
        "（没有说明）".to_string()
    } else {
        a.description.trim().to_string()
    };
    format!(
        "{d}（{}）",
        if a.external {
            "有外部的人"
        } else {
            "都是内部的人"
        }
    )
}

/// The check model's prompt.
pub fn check_prompt(a: &Audience, reply: &str, knowledge: &[KnowledgeUse]) -> String {
    let mut p = format!(
        "{CHECK_HEAD}\n你在检查一条马上要发出去的回复，会不会让听众知道他们不该知道的内容。\n\n听众：{}\n\n回复全文：\n<<<\n{}\n>>>\n\n起草时用到的知识（括号里是对这些听众的有效级别）：\n",
        audience_line(a),
        reply.trim()
    );
    if knowledge.is_empty() {
        p.push_str("（无）\n");
    }
    for (i, k) in knowledge.iter().enumerate() {
        let label = if k.source_label.is_empty() {
            k.source_type.as_str()
        } else {
            k.source_label.as_str()
        };
        p.push_str(&format!(
            "[{}] 来源：{label}（{}）：{}\n",
            i + 1,
            level_label(effective(k, a)),
            k.text.trim()
        ));
    }
    p.push_str(
        "\n级别：可以引用 = 能直接说；可转述结论 = 只能说结论，不能提来源、人名、数字和细节；仅用于推理 = 不能出现在回复里；不可使用 = 完全不能用。\n要看整段回复：几句话合起来能不能推出不该说的内容；也要看“根据之前和某某的讨论”这类来源指代，以及有没有暗示某件不能说的事存在。\n只回答 JSON：{\"verdict\": \"pass\" 或 \"leak\", \"spans\": [{\"text\": \"回复里的原文片段\", \"reason\": \"为什么\"}]}",
    );
    p
}

/// Reads the check model's answer; `None` when it is not one.
pub fn parse_check(answer: &str) -> Option<(bool, Vec<Span>)> {
    let v = steve_modules::changes::first_json(answer)?;
    let leak = match v["verdict"].as_str()? {
        "pass" => false,
        "leak" => true,
        _ => return None,
    };
    let spans = v["spans"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let text = s["text"].as_str()?.trim().to_string();
                    let reason = s["reason"].as_str().unwrap_or("").trim().to_string();
                    (!text.is_empty()).then_some(Span { text, reason })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some((leak, spans))
}

/// The rewrite prompt for the drafting model (§2.6 step 3).
pub fn rewrite_prompt(a: &Audience, reply: &str, spans: &[Span]) -> String {
    let mut p = format!(
        "{REWRITE_HEAD}\n下面这条回复可能让听众知道了他们不该知道的内容。改写它：把标出的片段改成结论，去掉来源、人名、数字和细节；不能说的内容，像不知道一样回答，不暗示它存在。其他意思和语气不变。\n\n听众：{}\n\n原回复：\n<<<\n{}\n>>>\n\n要改的片段：\n",
        audience_line(a),
        reply.trim()
    );
    for s in spans {
        p.push_str(&format!("- 「{}」：{}\n", s.text, s.reason));
    }
    p.push_str("\n只回答 JSON：{\"text\": \"改写后的回复\"}");
    p
}

/// Reads the rewrite; `None` when it gave no text.
pub fn parse_rewrite(answer: &str) -> Option<String> {
    if let Some(v) = steve_modules::changes::first_json(answer)
        && v.is_object()
    {
        let t = v["text"].as_str().unwrap_or("").trim().to_string();
        return (!t.is_empty()).then_some(t);
    }
    let t = answer.trim();
    (!t.is_empty() && !t.starts_with('{')).then(|| t.to_string())
}

/// What the guard and the redactor find; the spans never contain the value itself.
pub fn secret_spans(secrets: &dyn SecretScan, ws: &str, text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = steve_vault::guard::scan(text)
        .into_iter()
        .map(|s| Span {
            text: steve_vault::guard::REPLACEMENT.into(),
            reason: format!("{R_SECRET}（{}）", s.kind),
        })
        .collect();
    for label in secrets.find(ws, text) {
        out.push(Span {
            text: steve_vault::redact::mask(&label),
            reason: format!("{R_SECRET}（保险箱：{label}）"),
        });
    }
    out
}

fn result(
    verdict: Verdict,
    entries: Vec<BasisEntry>,
    spans: Vec<Span>,
    model: Option<String>,
) -> CheckResult {
    CheckResult {
        verdict,
        entries,
        spans,
        check_model: model,
    }
}

#[async_trait]
impl OutboundCheck for Disclosure {
    async fn check(
        &self,
        cx: &CheckCx,
        audience: &Audience,
        text: &str,
        knowledge: &[KnowledgeUse],
    ) -> CheckResult {
        let Some((store, _)) = self.0.bound.get() else {
            return result(
                Verdict::Leak,
                Vec::new(),
                vec![Span {
                    text: text.chars().take(40).collect(),
                    reason: R_UNBOUND.into(),
                }],
                None,
            );
        };
        // never-level knowledge does not reach the check model either
        let usable: Vec<KnowledgeUse> = knowledge
            .iter()
            .filter(|k| k.level != Level::Never)
            .cloned()
            .collect();
        let names =
            match store.reader(|c| Names::load(c, &cx.ws).map_err(steve_store::StoreError::from)) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("disclosure: names not loaded: {e}");
                    Names::default()
                }
            };

        // 0. secrets: blocked outright
        let secrets: Arc<dyn SecretScan> = self
            .0
            .secrets
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(NoSecrets));
        let found = secret_spans(secrets.as_ref(), &cx.ws, text);
        if !found.is_empty() {
            let (_, flagged) = rule_check(text, audience, knowledge, &names);
            return result(
                Verdict::Blocked,
                entries(text, audience, knowledge, &names, &flagged),
                found,
                None,
            );
        }

        // 1–2. the rule check (with every entry, never included: quoting one is a leak)
        let (mut spans, mut flagged) = rule_check(text, audience, knowledge, &names);
        for (i, k) in knowledge.iter().enumerate() {
            if k.level == Level::Never && overlaps(text, &k.text, &[]) {
                flagged[i] = true;
                spans.push(Span {
                    text: k.source_label.clone(),
                    reason: format!("用到了不可使用的知识（{}）", k.source_label),
                });
            }
        }

        // 3. the model check
        let prompt = check_prompt(audience, text, &usable);
        let (leak, model) = match self.run_model(cx, &prompt).await {
            Ok((answer, model)) => match parse_check(&answer) {
                Some((leak, s)) => {
                    for x in s {
                        if !spans.iter().any(|y| y.text == x.text) {
                            spans.push(x);
                        }
                    }
                    (leak, Some(model))
                }
                None => {
                    spans.push(Span {
                        text: String::new(),
                        reason: R_MODEL_FAILED.into(),
                    });
                    (true, Some(model))
                }
            },
            Err(e) => {
                tracing::warn!("disclosure: check model failed: {e}");
                spans.push(Span {
                    text: String::new(),
                    reason: R_MODEL_FAILED.into(),
                });
                (true, None)
            }
        };
        let verdict = if leak || !spans.is_empty() {
            Verdict::Leak
        } else {
            Verdict::Pass
        };
        result(
            verdict,
            entries(text, audience, knowledge, &names, &flagged),
            spans,
            model,
        )
    }
}

// ---------------------------------------------------------------- audiences of spaces

/// The audience of a space: the owners of its member accounts (minus the sending account), whether
/// it has outside members, and the scopes it is entirely inside (`space:<id>`; `workspace` when
/// every member is a user of the workspace; `private` when the only reader is `owner`).
pub fn space_audience(
    c: &Connection,
    space: &crate::spaces::SpaceRow,
    sender_account: &str,
    owner: &str,
) -> rusqlite::Result<Audience> {
    let mut st = c.prepare_cached(
        "SELECT sm.account_id, a.party_id, p.user_id, coalesce(nullif(a.display_name, ''), p.name, a.handle_display)
           FROM space_members sm JOIN accounts a ON a.id = sm.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE sm.space_id = ? AND sm.account_id <> ? ORDER BY sm.joined",
    )?;
    let rows = st
        .query_map(params![space.id, sender_account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut a = Audience {
        external: space.has_external,
        scopes: vec![format!("space:{}", space.id)],
        ..Audience::default()
    };
    let mut names = Vec::new();
    for (acc, party, _, name) in &rows {
        a.accounts.push(acc.clone());
        if let Some(p) = party
            && !a.parties.contains(p)
        {
            a.parties.push(p.clone());
        }
        names.push(name.clone());
    }
    if !rows.is_empty() && !a.external && rows.iter().all(|r| r.2.is_some()) {
        a.scopes.push("workspace".into());
        if rows.iter().all(|r| r.2.as_deref() == Some(owner)) {
            a.scopes.push("private".into());
        }
    }
    let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
    let space_name = if space.name.trim().is_empty() {
        "私聊"
    } else {
        space.name.as_str()
    };
    a.description = format!(
        "「{space_name}」的 {} 位成员{}{}",
        rows.len().max(space.member_count.max(0) as usize),
        if shown.is_empty() {
            String::new()
        } else {
            format!("：{}", shown.join("、"))
        },
        if a.external {
            "，有外部成员"
        } else {
            "，都是本公司的人"
        }
    );
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(text: &str, level: Level, vis: &str) -> KnowledgeUse {
        KnowledgeUse {
            source_type: "mem".into(),
            source_id: "mm_1".into(),
            source_label: "Acme 群的讨论".into(),
            level,
            visibility: vis.into(),
            text: text.into(),
        }
    }

    #[test]
    fn loose_matches_keep_the_replys_spacing() {
        assert_eq!(
            find_loose("预算有 30 万。", "30万").as_deref(),
            Some("30 万")
        );
        assert_eq!(find_loose("Acme", "Acme").as_deref(), Some("Acme"));
        assert_eq!(find_loose("abc", "x"), None);
    }

    #[test]
    fn terms() {
        let t =
            pattern_terms("预算 30 万，见 https://x.example/a 和 报价.xlsx，事项 ST-12，涨了 15%");
        assert!(t.contains(&"30 万".to_string()), "{t:?}");
        assert!(t.contains(&"https://x.example/a".to_string()), "{t:?}");
        assert!(t.contains(&"报价.xlsx".to_string()), "{t:?}");
        assert!(t.contains(&"ST-12".to_string()), "{t:?}");
        assert!(t.contains(&"15%".to_string()), "{t:?}");
    }

    #[test]
    fn audience_inside_the_scope_makes_it_public() {
        let a = Audience {
            scopes: vec!["space:spc_1".into()],
            ..Default::default()
        };
        assert_eq!(
            effective(&k("x", Level::Reasoning, "space:spc_1"), &a),
            Level::Public
        );
        assert_eq!(
            effective(&k("x", Level::Reasoning, "space:spc_2"), &a),
            Level::Reasoning
        );
        assert_eq!(
            effective(&k("x", Level::Never, "space:spc_1"), &a),
            Level::Never
        );
    }

    #[test]
    fn rules_flag_only_what_only_hidden_knowledge_has() {
        let names = Names {
            parties: vec![("org_acme".into(), "Acme".into())],
            spaces: vec![("spc_2".into(), "Acme 客户群".into())],
        };
        let a = Audience {
            scopes: vec!["space:spc_1".into()],
            ..Default::default()
        };
        let ks = [k("Acme 的预算是 30万", Level::Reasoning, "space:spc_2")];
        let (s, f) = rule_check("听说 Acme 的预算有 30 万", &a, &ks, &names);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(f[0]);
        let (s, _) = rule_check("预算还没定", &a, &ks, &names);
        assert!(s.is_empty());
        // the same knowledge from this space: public, nothing flagged
        let own = [k("Acme 的预算是 30万", Level::Reasoning, "space:spc_1")];
        let (s, _) = rule_check("Acme 的预算有 30 万", &a, &own, &names);
        assert!(s.is_empty(), "{s:?}");
        // a public source says it too: fine
        let both = [
            ks[0].clone(),
            k("Acme 是我们的客户", Level::Public, "workspace"),
        ];
        let (s, _) = rule_check("Acme 那边", &a, &both, &names);
        assert!(s.is_empty(), "{s:?}");
        // the audience's own organization is not a disclosure
        let mine = Audience {
            parties: vec!["org_acme".into()],
            ..a.clone()
        };
        let (s, _) = rule_check("Acme 那边", &mine, &ks, &names);
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn answers() {
        assert_eq!(
            parse_check("{\"verdict\": \"pass\", \"spans\": []}"),
            Some((false, vec![]))
        );
        let (leak, s) = parse_check(
            "x {\"verdict\":\"leak\",\"spans\":[{\"text\":\"Acme\",\"reason\":\"别的客户\"}]}",
        )
        .unwrap();
        assert!(leak);
        assert_eq!(s[0].text, "Acme");
        assert_eq!(parse_check("{}"), None);
        assert_eq!(parse_check("好的"), None);
        assert_eq!(
            parse_rewrite("{\"text\": \"改好了\"}").as_deref(),
            Some("改好了")
        );
        assert_eq!(parse_rewrite("{\"text\": \"\"}"), None);
    }

    #[test]
    fn check_models_differ_from_the_draft_model() {
        let d = Disclosure::with_models(vec!["haiku".into(), "sonnet".into()]);
        assert_eq!(d.check_model(Some("haiku")).as_deref(), Some("sonnet"));
        assert_eq!(d.check_model(Some("opus")).as_deref(), Some("haiku"));
        assert_eq!(Disclosure::with_models(vec![]).check_model(None), None);
    }

    #[test]
    fn secrets_are_found_without_their_value() {
        let s = secret_spans(
            &NoSecrets,
            "ws",
            "令牌是 ghp_abcdefghijklmnopqrstuvwxyz0123",
        );
        assert_eq!(s.len(), 1);
        assert!(!s[0].text.contains("ghp_"));
    }
}
