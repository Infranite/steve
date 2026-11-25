//! Extracting candidate memories and project `AGENTS.md` proposals with the model gateway.
//!
//! One call per origin (an item completion, a day of chat, a mail thread): the text, the memories it
//! resembles (confirmed and still pending, so the model can say “duplicate of” or “replaces”) and the
//! names it may use for “关于”.
//!
//! Knowledge is strictly about the owner's world, reused across tasks, confirmed by the owner, and
//! attached to a person, organization, team or project:
//! - Long-term preferences and rules (`preference`, `rule`): what the owner explicitly said holds
//!   from now on (or repeated in two places).
//! - Facts (`fact`): stable facts, relationships or commitments attached to an existing person,
//!   organization, team or project. Missing or unresolved `about` is deterministically dropped.
//! - Events (`event`, mail/space origins only): explicit dates or commitments.
//!
//! Codebase engineering conclusions (build/test commands, repository constraints, architecture rules)
//! do NOT enter personal memory: they are routed to project-level `AGENTS.md` proposals
//! (`agents_proposal`) for owner confirmation on the project page.
//!
//! Work-item process records, conversation records and one-off debugging conclusions are never
//! extracted. Every decision is deterministic and metered (`steve_knowledge_extract_*`).

use std::time::Duration;

use serde_json::Value;
use steve_modules::{ApiError, ApiResult, Modules, Tier};
use steve_store::base::mems::Mem;
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::knowledge::{CandidateRows, Memories, ProjectProposals};
use steve_store::{Contacts, ItemEvidence, Members, Read, References, W};
use tokio::sync::Semaphore;

use crate::candidates::{self, MAX_PER_ORIGIN, NewCandidate, Origin, OriginKind};
use crate::proposals::{self, KIND_AGENTS_MD, NewProposal};
use crate::retrieve::{self, Purpose, Query};
use crate::state::Knowledge;

/// Model time limit (smart tier).
pub const TIMEOUT: Duration = Duration::from_secs(90);
/// Source text given to the model.
pub const MAX_INPUT: usize = 12_000;
/// Names listed in the prompt.
pub const MAX_NAMES: usize = 50;
/// Similar confirmed memories listed in the prompt.
pub const SIMILAR_K: usize = 8;
/// What of the text the similar-memory search uses (the query builder ORs every bigram).
const SIMILAR_QUERY_CHARS: usize = 1000;
/// The first line of every extraction prompt (tests script the gateway on it).
pub const PROMPT_HEAD: &str = "你在帮用户整理以后还会用到的记忆。";
/// Model output kind for a project `AGENTS.md` proposal.
pub const KIND_PROPOSAL: &str = "agents_proposal";
const BASE_KINDS: [&str; 4] = ["fact", "preference", "rule", KIND_PROPOSAL];
const ALL_KINDS: [&str; 5] = ["fact", "preference", "rule", KIND_PROPOSAL, "event"];

/// At most two extraction calls run at a time.
static SLOTS: Semaphore = Semaphore::const_new(2);

// ---------------------------------------------------------------- names

/// Entity classification for resolved `about` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Person,
    Org,
    Team,
    /// Workspace project; `has_repo` is true when bound to a git repository (`projects.kind == "git"`).
    Project {
        has_repo: bool,
    },
    Bot,
}

impl EntityKind {
    /// Whether this entity can serve as the required subject of a `fact` memory.
    pub fn is_fact_subject(self) -> bool {
        !matches!(self, Self::Bot)
    }
}

/// Strongly-typed knowledge scope emitted by the extraction model.
/// Deterministic server routing uses this enum rather than text or keyword heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractScope {
    Person,
    Organization,
    Project,
    Codebase,
}

impl ExtractScope {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "person" => Some(Self::Person),
            "organization" => Some(Self::Organization),
            "project" => Some(Self::Project),
            "codebase" => Some(Self::Codebase),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Codebase => "codebase",
        }
    }

    /// Whether a resolved entity satisfies the target type required by this scope.
    pub fn matches(self, entity: EntityKind) -> bool {
        match self {
            Self::Person => entity == EntityKind::Person,
            Self::Organization => matches!(entity, EntityKind::Org | EntityKind::Team),
            Self::Project => matches!(entity, EntityKind::Project { .. }),
            Self::Codebase => matches!(entity, EntityKind::Project { has_repo: true }),
        }
    }
}

/// The objects “关于” may name: contacts, organizations, teams (names and aliases), projects and bots
/// of the workspace that the user can see; “我” is the user's own contact.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// `(display name, label, ref, entity kind)`, in prompt order (item people first).
    entries: Vec<(String, &'static str, ObjRef, EntityKind)>,
    /// `(lowercased name or alias, ref, entity kind)`.
    keys: Vec<(String, ObjRef, EntityKind)>,
}

impl Names {
    /// Loads the names for `viewer`; the parties linked to `item` come first in the prompt list.
    pub fn load(c: &mut impl Read, viewer: &Viewer, item: Option<&str>) -> ApiResult<Self> {
        let mut n = Self::default();
        let mut parties = Contacts::new(c).names(viewer, &["contact", "org", "team"])?;
        if let Some(item) = item {
            let linked = ItemEvidence(c).linked_parties(&viewer.ws, item)?;
            parties.sort_by_key(|p| !linked.contains(&p.id));
        }
        for p in &parties {
            let r = ObjRef::new("party", &p.id);
            let (label, entity) = match p.kind.as_str() {
                "org" => ("组织", EntityKind::Org),
                "team" => ("团队", EntityKind::Team),
                _ => ("联系人", EntityKind::Person),
            };
            if p.user_id.as_deref() == Some(viewer.user.as_str()) {
                n.keys.push(("我".into(), r.clone(), entity));
            }
            n.add(&p.name, label, &r, entity);
            for a in &p.aliases {
                n.keys.push((a.trim().to_lowercase(), r.clone(), entity));
            }
        }
        for p in References(c).projects(&viewer.ws)? {
            let label = if p.has_repo {
                "项目（代码仓库）"
            } else {
                "项目"
            };
            n.add(
                &p.name,
                label,
                &ObjRef::new("project", &p.id),
                EntityKind::Project {
                    has_repo: p.has_repo,
                },
            );
        }
        for (id, name) in References(c).bots(&viewer.ws)? {
            n.add(&name, "Bot", &ObjRef::new("bot", &id), EntityKind::Bot);
        }
        Ok(n)
    }

    fn add(&mut self, name: &str, label: &'static str, r: &ObjRef, entity: EntityKind) {
        self.entries
            .push((name.to_string(), label, r.clone(), entity));
        self.keys
            .push((name.trim().to_lowercase(), r.clone(), entity));
    }

    /// True if `id` is a workspace project bound to a git repository.
    pub fn is_repo_project(&self, id: &str) -> bool {
        self.entries.iter().any(|(_, _, r, kind)| {
            r.kind == "project"
                && r.id == id
                && matches!(kind, EntityKind::Project { has_repo: true })
        })
    }

    /// Resolves `name` to `(ObjRef, EntityKind)`: an id of a listed object, or the single object
    /// whose name or alias equals it (case-insensitive). None or several → `None`: never a guess.
    pub fn resolve_entity(&self, name: &str) -> Option<(ObjRef, EntityKind)> {
        let w = name.trim();
        if w.is_empty() {
            return None;
        }
        if let Some((_, _, r, kind)) = self.entries.iter().find(|(_, _, r, _)| r.id == w) {
            return Some((r.clone(), *kind));
        }
        let wl = w.to_lowercase();
        let mut hits: Vec<(&ObjRef, EntityKind)> = self
            .keys
            .iter()
            .filter(|(k, _, _)| *k == wl)
            .map(|(_, r, kind)| (r, *kind))
            .collect();
        hits.sort_by(|a, b| a.0.cmp(b.0));
        hits.dedup_by(|a, b| a.0 == b.0);
        match hits.as_slice() {
            [(one, kind)] => Some(((*one).clone(), *kind)),
            _ => None,
        }
    }

    /// The object `name` names.
    pub fn resolve(&self, name: &str) -> Option<ObjRef> {
        self.resolve_entity(name).map(|(r, _)| r)
    }

    /// The prompt lines, at most [`MAX_NAMES`]: “- Sarah Chen（联系人）”.
    fn prompt_lines(&self) -> String {
        self.entries
            .iter()
            .take(MAX_NAMES)
            .map(|(name, label, _, _)| format!("- {name}（{label}）"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------- prompt and answer

/// A memory the model is shown so it does not propose it again, by id: confirmed ones (found by
/// retrieval) and the user's candidates still waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Similar {
    pub id: String,
    pub text: String,
    pub pending: bool,
}

/// What the extraction is about, beyond the text itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Task<'a> {
    /// The goal versions of the item behind the text: restating them is not a memory.
    pub goals: &'a [String],
    /// Whether `event` candidates are wanted (mail threads and channel spaces; not items or chat).
    pub events: bool,
    /// The project the origin belongs to (e.g. an item's `project_id`), used as the default target
    /// for codebase `agents_proposal` suggestions.
    pub project: Option<&'a str>,
}

/// The extraction prompt. Knowledge covers only the owner's world (people, organizations, projects,
/// stable preferences/rules, commitments). Codebase engineering conclusions produce `agents_proposal`
/// instead of personal memories; process records and one-off troubleshooting are never extracted.
pub fn prompt(label: &str, text: &str, similar: &[Similar], names: &Names, task: Task) -> String {
    let names = names.prompt_lines();
    let similar = if similar.is_empty() {
        "（没有）".to_string()
    } else {
        similar
            .iter()
            .map(|s| {
                let mark = if s.pending { "（待确认）" } else { "" };
                format!("- [{}]{mark} {}", s.id, s.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (event_kind, event_line) = if task.events {
        (
            "|event",
            "\n6. 日程与承诺（kind = event）：对方明确说的、以后需要记着的日期、截止时间或安排，比如“3 月 1 日起由王五对接”。basis 填 explicit。",
        )
    } else {
        ("", "")
    };
    format!(
        "{PROMPT_HEAD}下面是{label}的原文。先分清每段内容属于哪一类，只输出第 3、4、5 类{}：\n\
1. 事项记录：这次要做什么、改哪里、进度、交付了什么、怎么验收，以及一次性的要求、批准、指令和一次性排查结论。它们已经保存在事项和执行记录里，不要输出。\n\
2. 会话记录：这次聊了什么、做了哪些决定。对话原文已经保存、可以检索，不要输出。\n\
3. 偏好与规则（kind = preference 或 rule）：主人明确表达、以后在别的事情上也适用的稳定偏好或长期规则。必须满足其一：原话明确说了长期适用（“以后”“后续”“每次”“总是”“一律”“这类”“不要再”“之前说过”之类），basis 填 explicit；或原文里不同场合至少两次表达了同一倾向，basis 填 repeated，evidence 列出这两处原话。只凭一次具体反馈不能推断成偏好：“这个页面的按钮太挤了，改宽松点”是这件事的修改要求，不是“用户偏好宽松的界面”。\n\
4. 关于人、组织或项目的事实（kind = fact）：关于主人世界里某个人、组织、团队或项目的稳定事实、关系、角色或长期承诺，以后会被反复用到。about 必须填写下面列表里已有的联系人、组织、团队或项目名字，没有明确对象的 fact 会被直接丢弃。basis 填 verified。不要把代码仓库的工程实现细节、构建/测试命令或排障过程写成个人记忆。\n\
5. 项目 AGENTS.md 提议（kind = agents_proposal，scope = codebase）：仅当原文总结了某个代码仓库长期适用的工程约定、架构约束、实现机制、测试/发布规范或踩坑教训（适合写进该仓库的 AGENTS.md，供后续改该仓库的 Agent 遵守）时输出。它不会进入个人记忆，而是路由为对应代码仓库项目的 AGENTS.md 提议请主人确认；about 填对应代码仓库项目名（若来源已属于该仓库项目可留空），basis 填 verified。{event_line}\n\n\
每条输出都必须标注强类型 scope（只能取以下四个值之一，服务端完全按 scope 确定性路由）：\n\
- person：关于某个人（含主人自己的偏好/规则，或某个联系人）\n\
- organization：关于某个组织或团队\n\
- project：关于某个项目的业务背景、目标或非代码事实\n\
- codebase：关于某个代码仓库的工程实现/架构/构建/测试/发布结论（仅能落到标注了“代码仓库”的项目，走 AGENTS.md 提议）\n\n\
复述事项目标、换个说法总结需求、把功能需求写成“用户希望……”，都不要输出。没有符合的就返回 {{\"items\":[]}}，这是常见的正常结果；宁可少，不要凑数，最多 {MAX_PER_ORIGIN} 条。\n\n\
可识别的名字（about 只能用这些名字，认不出就留空，不要猜）：\n{names}\n\n\
已有的长期记忆和知识（含待确认的；和某条重复就在 dup_of 填它的 id；和已确认的某条冲突、应该替换它就在 conflicts 填它的 id）：\n{similar}\n\n\
原文：\n<<<\n{text}\n>>>\n\n\
只返回 JSON：{{\"items\":[{{\"kind\":\"preference|rule|fact|{KIND_PROPOSAL}{event_kind}\",\"scope\":\"person|organization|project|codebase\",\"text\":\"一句话，写成以后可以直接照做或引用的规则、偏好、事实或 AGENTS.md 建议\",\"when\":\"适用场景：在什么场景下用\",\"why\":\"为什么值得长期保留：以后在什么场景会用到\",\"basis\":\"explicit|repeated|verified\",\"evidence\":[\"原文摘录，逐字照抄\"],\"about\":[\"名字\"],\"level\":\"public|conclusion|reasoning|never\",\"dup_of\":null,\"conflicts\":null}}]}}",
        if task.events { "和第 6 类" } else { "" }
    )
}

/// One item of the model's answer, as given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raw {
    pub kind: String,
    /// Strongly-typed routing scope (`person` | `organization` | `project` | `codebase`).
    pub scope: Option<ExtractScope>,
    /// True when the model emitted a non-empty `scope` outside [`ExtractScope`].
    pub invalid_scope: bool,
    pub text: String,
    pub about: Vec<String>,
    pub level: Option<Level>,
    /// The quotes it rests on (a string or a list in the answer).
    pub evidence: Vec<String>,
    /// `explicit` | `repeated` | `verified`, or empty.
    pub basis: String,
    /// Where it applies (human-readable hint shown on 待确认).
    pub when: String,
    /// Why it is worth keeping.
    pub why: String,
    pub dup_of: Option<String>,
    pub conflicts: Option<String>,
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|n| opt_str(Some(n))).collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => Vec::new(),
    }
}

/// Parses the answer: the first JSON object in it (code fences and chatter around it are tolerated).
/// Anything unreadable gives no items.
pub fn parse(out: &str) -> Vec<Raw> {
    let (Some(a), Some(b)) = (out.find('{'), out.rfind('}')) else {
        return Vec::new();
    };
    let Some(v) = out
        .get(a..=b)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    else {
        return Vec::new();
    };
    let Some(items) = v.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|x| x.is_object())
        .map(|x| {
            let (scope, invalid_scope) = match opt_str(x.get("scope")).as_deref() {
                None => (None, false),
                Some(s) => match ExtractScope::parse(s) {
                    Some(parsed) => (Some(parsed), false),
                    None => (None, true),
                },
            };
            Raw {
                kind: opt_str(x.get("kind")).unwrap_or_default(),
                scope,
                invalid_scope,
                text: opt_str(x.get("text")).unwrap_or_default(),
                about: str_list(x.get("about")),
                level: x
                    .get("level")
                    .and_then(Value::as_str)
                    .and_then(Level::parse),
                evidence: str_list(x.get("evidence")),
                basis: opt_str(x.get("basis")).unwrap_or_default(),
                when: opt_str(x.get("when")).unwrap_or_default(),
                why: opt_str(x.get("why")).unwrap_or_default(),
                dup_of: opt_str(x.get("dup_of")),
                conflicts: opt_str(x.get("conflicts")),
            }
        })
        .collect()
}

// ---------------------------------------------------------------- the server's filter

/// Words that say a preference or rule is meant to last (ST-41). One plain request (“这里改成那样”)
/// has none of them and stays with its item.
pub const LASTING: [&str; 33] = [
    "以后",
    "今后",
    "往后",
    "后续",
    "从现在起",
    "下次",
    "每次",
    "每回",
    "每一次",
    "总是",
    "一直",
    "始终",
    "永远",
    "一律",
    "凡是",
    "这类",
    "同类",
    "类似的",
    "不要再",
    "别再",
    "再也不",
    "之前说过",
    "上次说过",
    "说过了",
    "说过很多次",
    "习惯",
    "一般都",
    "通常",
    "always",
    "never",
    "every time",
    "from now on",
    "going forward",
];

/// Text reduced to what comparisons look at: letters and digits, lowercased.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn bigrams(s: &str) -> std::collections::HashSet<(char, char)> {
    let cs: Vec<char> = norm(s).chars().collect();
    cs.windows(2).map(|w| (w[0], w[1])).collect()
}

/// The share of `a`'s character pairs that are also in `b` (0 when `a` has none).
pub fn contained(a: &str, b: &str) -> f64 {
    let (x, y) = (bigrams(a), bigrams(b));
    if x.is_empty() {
        return 0.0;
    }
    x.intersection(&y).count() as f64 / x.len() as f64
}

/// Two wordings of the same thing: most of the shorter one's pairs are in the longer one.
pub fn near_duplicate(a: &str, b: &str) -> bool {
    let (s, l) = if norm(a).chars().count() <= norm(b).chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    contained(s, l) >= NEAR_DUP
}

/// [`near_duplicate`] threshold.
const NEAR_DUP: f64 = 0.7;
/// A candidate this much made of the item's goal only restates it.
const RESTATES: f64 = 0.6;

/// Is `quote` in `source`? Compared on [`norm`]; a quote shortened with “…” counts when every piece is.
fn quoted(quote: &str, source: &str) -> bool {
    let pieces: Vec<String> = quote
        .split(['…', '⋯'])
        .flat_map(|p| p.split("..."))
        .map(norm)
        .filter(|p| p.chars().count() >= 2)
        .collect();
    !pieces.is_empty() && pieces.iter().all(|p| source.contains(p.as_str()))
}

fn lasting(s: &str) -> bool {
    let l = s.to_lowercase();
    LASTING.iter().any(|w| l.contains(w))
}

/// Why a raw item was dropped (tests, logs and metrics say which rule did it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// The model marked it a duplicate, or it is close to a memory/proposal already there.
    Duplicate,
    /// Not a kind this origin proposes, or no usable text.
    Shape,
    /// Missing, unrecognized or contradictory `scope` (must be `person|organization|project|codebase`).
    InvalidScope,
    /// No quote, or a quote that is not in the text.
    NoEvidence,
    /// A preference or rule from one plain request: no word that it lasts, not said twice.
    OneOff,
    /// It restates the item's goal.
    RestatesGoal,
    /// Missing required `about` target for the declared scope.
    MissingAbout,
    /// `about` named objects that did not resolve to an eligible existing entity for the declared scope
    /// (e.g. unknown name, wrong entity type, or non-repository project for `codebase`).
    UnresolvedAbout,
    /// Origin output cap [`MAX_PER_ORIGIN`] was already reached.
    CapReached,
}

impl Dropped {
    pub fn code(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Shape => "shape",
            Self::InvalidScope => "invalid_scope",
            Self::NoEvidence => "no_evidence",
            Self::OneOff => "one_off",
            Self::RestatesGoal => "restates_goal",
            Self::MissingAbout => "missing_about",
            Self::UnresolvedAbout => "unresolved_about",
            Self::CapReached => "cap_reached",
        }
    }
}

/// The deterministic outcome of [`select`]: personal memory candidates, project `AGENTS.md`
/// proposals, and the drop reasons recorded for rejected items.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selected {
    pub candidates: Vec<NewCandidate>,
    pub proposals: Vec<NewProposal>,
    pub dropped: Vec<Dropped>,
}

/// The server's rules over one raw item. `source` is the text the model saw,
/// `known` the texts of the user's confirmed and pending memories. `Ok` = the evidence quotes kept.
pub fn check(r: &Raw, source: &str, task: Task, known: &[String]) -> Result<Vec<String>, Dropped> {
    let kinds: &[&str] = if task.events { &ALL_KINDS } else { &BASE_KINDS };
    if !kinds.contains(&r.kind.as_str()) {
        return Err(Dropped::Shape);
    }
    if r.invalid_scope {
        return Err(Dropped::InvalidScope);
    }
    let Some(scope) = r.scope else {
        return Err(Dropped::InvalidScope);
    };
    if r.kind == KIND_PROPOSAL && scope != ExtractScope::Codebase {
        return Err(Dropped::InvalidScope);
    }
    let n = r.text.chars().count();
    if n == 0 || n > 2000 {
        return Err(Dropped::Shape);
    }
    if r.dup_of.is_some() {
        return Err(Dropped::Duplicate);
    }
    let src = norm(source);
    let quotes: Vec<String> = r
        .evidence
        .iter()
        .filter(|q| quoted(q, &src))
        .cloned()
        .collect();
    if quotes.is_empty() {
        return Err(Dropped::NoEvidence);
    }
    let says_lasting = quotes.iter().any(|q| lasting(q));
    if scope != ExtractScope::Codebase && matches!(r.kind.as_str(), "preference" | "rule") {
        let distinct = {
            let mut q: Vec<String> = quotes.iter().map(|q| norm(q)).collect();
            q.sort();
            q.dedup();
            q.len()
        };
        if !says_lasting && !(r.basis == "repeated" && distinct >= 2) {
            return Err(Dropped::OneOff);
        }
    }
    if !says_lasting {
        for g in task.goals {
            let g = norm(g);
            if contained(&r.text, &g) >= RESTATES || quotes.iter().all(|q| quoted(q, &g)) {
                return Err(Dropped::RestatesGoal);
            }
        }
    }
    if scope != ExtractScope::Codebase
        && r.conflicts.is_none()
        && known.iter().any(|k| near_duplicate(&r.text, k))
    {
        return Err(Dropped::Duplicate);
    }
    Ok(quotes)
}

fn record_drop(out: &mut Selected, why: Dropped, text: &str) {
    tracing::debug!(?why, text, "kb.extract: dropped");
    metrics::counter!("steve_knowledge_extract_dropped_total", "reason" => why.code()).increment(1);
    out.dropped.push(why);
}

fn record_produced(kind: &'static str) {
    metrics::counter!("steve_knowledge_extract_produced_total", "kind" => kind).increment(1);
}

fn kind_metric(kind: &str) -> &'static str {
    match kind {
        "fact" => "fact",
        "preference" => "preference",
        "rule" => "rule",
        "event" => "event",
        _ => KIND_PROPOSAL,
    }
}

/// Applies all deterministic server rules over the model's items:
/// - [`check`] validates kind, strongly-typed [`ExtractScope`], text length, quotes, lasting signal,
///   goal restatement, and duplicates.
/// - `about` names resolve strictly against existing workspace entities (never guessed).
/// - `scope = codebase` routes deterministically to a project `AGENTS.md` proposal and MUST resolve
///   to a repository-bound project (`projects.kind == "git"` with path), else dropped and counted.
/// - `scope ∈ {person, organization, project}` MUST resolve to an existing entity matching that
///   scope (with empty-`about` `person` preferences/rules attaching to the owner `"我"` when present);
///   mismatched or unresolved entities are dropped and counted.
/// - At most [`MAX_PER_ORIGIN`] total outputs (candidates + proposals) are kept.
#[allow(clippy::too_many_arguments)]
pub fn select(
    raw: Vec<Raw>,
    names: &Names,
    default_level: Level,
    source: &str,
    task: Task,
    known: &[String],
    known_proposals: &[(String, String)],
    mut valid_conflict: impl FnMut(&str) -> bool,
) -> Selected {
    let mut out = Selected::default();
    for r in raw {
        if out.candidates.len() + out.proposals.len() >= MAX_PER_ORIGIN {
            record_drop(&mut out, Dropped::CapReached, &r.text);
            continue;
        }
        let quotes = match check(&r, source, task, known) {
            Ok(q) => q,
            Err(why) => {
                record_drop(&mut out, why, &r.text);
                continue;
            }
        };
        let scope = r.scope.expect("validated by check");
        let mut resolved: Vec<(ObjRef, EntityKind)> = Vec::new();
        for name in &r.about {
            if let Some((x, kind)) = names.resolve_entity(name)
                && !resolved.iter().any(|(r, _)| r == &x)
            {
                resolved.push((x, kind));
            }
        }
        if !r.about.is_empty() && resolved.is_empty() {
            record_drop(&mut out, Dropped::UnresolvedAbout, &r.text);
            continue;
        }

        if scope == ExtractScope::Codebase {
            let repo_project = resolved
                .iter()
                .find(|(_, k)| scope.matches(*k))
                .map(|(r, _)| r.id.clone())
                .or_else(|| {
                    r.about
                        .is_empty()
                        .then_some(task.project)
                        .flatten()
                        .filter(|pid| names.is_repo_project(pid))
                        .map(String::from)
                });
            let Some(project_id) = repo_project else {
                let why = if r.about.is_empty() && task.project.is_none() {
                    Dropped::MissingAbout
                } else {
                    Dropped::UnresolvedAbout
                };
                record_drop(&mut out, why, &r.text);
                continue;
            };
            let dup = known_proposals
                .iter()
                .any(|(pid, txt)| pid == &project_id && near_duplicate(txt, &r.text))
                || out
                    .proposals
                    .iter()
                    .any(|p| p.project_id == project_id && near_duplicate(&p.text, &r.text));
            if dup {
                record_drop(&mut out, Dropped::Duplicate, &r.text);
                continue;
            }
            out.proposals.push(NewProposal {
                project_id,
                kind: KIND_AGENTS_MD.into(),
                text: r.text,
                reason: r.why,
                evidence: quotes.join(" / "),
            });
            record_produced(KIND_PROPOSAL);
            continue;
        }

        let has_matching = resolved.iter().any(|(_, k)| scope.matches(*k));
        let about = if has_matching {
            resolved
                .into_iter()
                .filter(|(_, k)| k.is_fact_subject())
                .map(|(r, _)| r)
                .collect()
        } else if scope == ExtractScope::Person
            && r.about.is_empty()
            && matches!(r.kind.as_str(), "preference" | "rule")
        {
            names
                .resolve_entity("我")
                .map(|(r, _)| vec![r])
                .unwrap_or_default()
        } else {
            let why = if r.about.is_empty() {
                Dropped::MissingAbout
            } else {
                Dropped::UnresolvedAbout
            };
            record_drop(&mut out, why, &r.text);
            continue;
        };

        if out
            .candidates
            .iter()
            .any(|c| near_duplicate(&c.text, &r.text))
        {
            record_drop(&mut out, Dropped::Duplicate, &r.text);
            continue;
        }
        let metric_kind = kind_metric(&r.kind);
        out.candidates.push(NewCandidate {
            kind: r.kind,
            text: r.text,
            about,
            level: r.level.unwrap_or(default_level).max(default_level),
            evidence: quotes.join(" / "),
            supersedes: r.conflicts.filter(|id| valid_conflict(id)),
            why: r.why,
            scope: r.when,
            owner_said: false,
        });
        record_produced(metric_kind);
    }
    out
}

/// Cuts to `n` characters.
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------- running

/// The user's candidates still waiting for an answer, and the texts every new candidate is compared
/// with: confirmed memories of the workspace plus those candidates (newest first, bounded).
fn known(c: &mut impl Read, ws: &str, user: &str) -> ApiResult<(Vec<Similar>, Vec<String>)> {
    let rows = CandidateRows(c).known(ws, user)?;
    let pending = rows
        .iter()
        .filter(|(_, _, s)| s == "candidate")
        .map(|(id, t, _)| Similar {
            id: id.clone(),
            text: t.clone(),
            pending: true,
        })
        .collect();
    Ok((pending, rows.into_iter().map(|(_, t, _)| t).collect()))
}

/// Runs one extraction for `origin` over `text` and writes the candidates and project proposals.
/// Returns the memory candidates written — often none.
pub async fn run(
    m: &Modules,
    kb: &Knowledge,
    origin: &Origin,
    text: &str,
    item: Option<&str>,
    task: Task<'_>,
) -> ApiResult<Vec<Mem>> {
    let prepared = prepare(m, kb, origin, text, item, task).await?;
    if prepared.candidates.is_empty() && prepared.proposals.is_empty() {
        return Ok(Vec::new());
    }
    let origin = origin.clone();
    m.blocking(move |m| m.store.write(|tx| commit_in(tx, &origin, &prepared)))
        .await
}

/// Model output checked against the source text, ready for one domain transaction.
pub(crate) struct Prepared {
    pub(crate) text: String,
    pub(crate) candidates: Vec<NewCandidate>,
    pub(crate) proposals: Vec<NewProposal>,
}

/// Retrieval, model work and filtering, outside the writer. All sources share `SLOTS`.
pub(crate) async fn prepare(
    m: &Modules,
    kb: &Knowledge,
    origin: &Origin,
    text: &str,
    item: Option<&str>,
    task: Task<'_>,
) -> ApiResult<Prepared> {
    let text = clip(text, MAX_INPUT);
    if text.trim().is_empty() {
        return Ok(Prepared {
            text,
            candidates: Vec::new(),
            proposals: Vec::new(),
        });
    }
    let now = m.clock.now();
    // retrieval (which may ask a remote store) and the name list: off the runtime
    let (k, o, t, it) = (
        kb.clone(),
        origin.clone(),
        text.clone(),
        item.map(str::to_string),
    );
    let (names, similar, known, known_proposals) = m
        .blocking(move |m| {
            crate::read_api(&m.store, |c| {
                let (kb, origin, text, item) = (&k, &o, &t, it.as_deref());
                let viewer = Viewer::bare(&origin.ws, &origin.user);
                let names = Names::load(c, &viewer, item)?;
                let q_text = clip(text, SIMILAR_QUERY_CHARS);
                let kinds = ["mems"];
                let mut q = Query::new(kb.store(), &viewer, &q_text, Purpose::Agent);
                q.kinds = Some(&kinds);
                q.k = SIMILAR_K;
                q.now = now;
                let mut similar: Vec<Similar> = retrieve::retrieve(c, &q)?
                    .into_iter()
                    .filter(|h| h.obj_type == "mem")
                    .map(|h| Similar {
                        id: h.obj_id,
                        text: h.text,
                        pending: false,
                    })
                    .collect();
                let (pending, known) = known(c, &origin.ws, &origin.user)?;
                // pending candidates are not in the index: the ones most made of this text's words
                let mut close: Vec<(f64, Similar)> = pending
                    .into_iter()
                    .map(|p| (contained(&p.text, text), p))
                    .filter(|(s, _)| *s >= 0.3)
                    .collect();
                close.sort_by(|a, b| b.0.total_cmp(&a.0));
                similar.extend(close.into_iter().take(SIMILAR_K).map(|(_, s)| s));
                let known_proposals = ProjectProposals(c).known_texts(&origin.ws)?;
                Ok::<_, ApiError>((names, similar, known, known_proposals))
            })
        })
        .await?;
    let p = prompt(&origin.label, &text, &similar, &names, task);
    let answer = {
        let _slot = SLOTS
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        steve_modules::gateway::tagged(
            "extract",
            &origin.ws,
            m.gateway.run(Tier::Smart, &p, TIMEOUT),
        )
        .await
    };
    let answer = answer.map_err(|e| {
        tracing::warn!(origin = %origin.id, "kb.extract: gateway failed: {e}");
        ApiError::Internal(format!("gateway: {e}"))
    })?;
    if origin.kind == OriginKind::Chat {
        steve_modules::gateway::record_periodic_usage(
            crate::sources::chat::PERIODIC_TASK_EXTRACT,
            &answer.usage,
        );
    }
    let raw = parse(&answer.text);
    let offered = raw.len();
    let ws = origin.ws.clone();
    let user = origin.user.clone();
    let selected = crate::read_api(&m.store, |c| {
        let viewer = Viewer::bare(&ws, &user);
        Ok::<_, ApiError>(select(
            raw,
            &names,
            origin.default_level,
            &text,
            task,
            &known,
            &known_proposals,
            |id| {
                Memories(c)
                    .visible(&viewer, id)
                    .ok()
                    .flatten()
                    .is_some_and(|x| x.status == "confirmed")
            },
        ))
    })?;
    tracing::info!(
        origin = %origin.id,
        offered,
        kept = selected.candidates.len(),
        proposals = selected.proposals.len(),
        dropped = selected.dropped.len(),
        "kb.extract: done"
    );
    Ok(Prepared {
        text,
        candidates: selected.candidates,
        proposals: selected.proposals,
    })
}

/// Writes candidates, project proposals, decisions and notices in the caller's transaction. A source
/// can atomically include its completion receipt, including when nothing was selected.
pub(crate) fn commit_in(
    tx: &mut W<'_>,
    origin: &Origin,
    prepared: &Prepared,
) -> ApiResult<Vec<Mem>> {
    let made = prepared
        .candidates
        .iter()
        .map(|c| {
            let mut c = c.clone();
            c.owner_said = origin.kind == OriginKind::Chat
                && c.kind != "event"
                && candidates::owner_said(&prepared.text, OWNER_LINE, &c.evidence);
            candidates::propose_in(tx, origin, &c)
        })
        .collect::<ApiResult<Vec<_>>>()?;
    for prop in &prepared.proposals {
        proposals::propose_in(tx, origin, prop)?;
    }
    kept_notice(tx, &origin.ws, &made)?;
    Ok(made)
}

/// How a line of the owner starts in a day of chat (`sources::chat::day_text`).
const OWNER_LINE: &str = "我（";

/// Memories kept at once because the owner said so: one notice says which, and where to take them back.
fn kept_notice(tx: &mut W<'_>, ws: &str, made: &[Mem]) -> ApiResult<()> {
    let kept: Vec<String> = made
        .iter()
        .filter(|x| x.status == "confirmed")
        .map(|x| format!("「{}」", x.text.chars().take(80).collect::<String>()))
        .collect();
    if kept.is_empty() {
        return Ok(());
    }
    let text = format!(
        "按你在对话里说的，记住了 {} 条：{}。不对的话到 知识 › 偏好与规则 里改或删掉。",
        kept.len(),
        kept.join("；")
    );
    steve_store::notify::notify_in(tx, ws, "memory", "info", &text, None)?;
    Ok(())
}

/// The entry point for other modules: mail threads (`OriginKind::Mail`, default
/// `reasoning`), channel spaces (`OriginKind::Space`); both may propose events. `user` = the workspace user the text is for;
/// `None` = the workspace owner.
#[allow(clippy::too_many_arguments)]
pub async fn submit(
    m: &Modules,
    kb: &Knowledge,
    ws: &str,
    origin: OriginKind,
    origin_id: &str,
    label: &str,
    text: &str,
    default_level: Level,
    user: Option<&str>,
) -> ApiResult<Vec<Mem>> {
    let user = match user {
        Some(u) => u.to_string(),
        None => crate::read_api(&m.store, |c| owner(c, ws))?,
    };
    let o = Origin {
        ws: ws.into(),
        kind: origin,
        id: origin_id.into(),
        label: label.into(),
        user,
        writer: "steve".into(),
        default_level,
        item_id: None,
        session_id: None,
    };
    let task = Task {
        goals: &[],
        events: true,
        project: None,
    };
    run(m, kb, &o, text, None, task).await
}

/// The workspace's owner (else its first member): whom workspace-wide origins (main thread chat)
/// write their candidates for.
pub fn owner(c: &mut impl Read, ws: &str) -> ApiResult<String> {
    Members::new(c)
        .first(ws)?
        .ok_or_else(|| ApiError::not_found("工作空间成员"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        let mut n = Names::default();
        n.add(
            "Sarah Chen",
            "联系人",
            &ObjRef::new("party", "ct_1"),
            EntityKind::Person,
        );
        n.keys.push((
            "sarah".into(),
            ObjRef::new("party", "ct_1"),
            EntityKind::Person,
        ));
        n.add(
            "Acme",
            "组织",
            &ObjRef::new("party", "org_1"),
            EntityKind::Org,
        );
        n.add(
            "李雷",
            "联系人",
            &ObjRef::new("party", "ct_2"),
            EntityKind::Person,
        );
        n.add(
            "李雷",
            "联系人",
            &ObjRef::new("party", "ct_3"),
            EntityKind::Person,
        );
        n.add(
            "Steve",
            "项目（代码仓库）",
            &ObjRef::new("project", "prj_steve"),
            EntityKind::Project { has_repo: true },
        );
        n.add(
            "NotesDir",
            "项目",
            &ObjRef::new("project", "prj_notes"),
            EntityKind::Project { has_repo: false },
        );
        n.add(
            "HelperBot",
            "Bot",
            &ObjRef::new("bot", "bot_1"),
            EntityKind::Bot,
        );
        n
    }

    #[test]
    fn resolves_only_exact_and_unique_names() {
        let n = names();
        assert_eq!(
            n.resolve_entity("sarah chen"),
            Some((ObjRef::new("party", "ct_1"), EntityKind::Person))
        );
        assert_eq!(n.resolve(" Sarah "), Some(ObjRef::new("party", "ct_1")));
        assert_eq!(n.resolve("org_1"), Some(ObjRef::new("party", "org_1")));
        assert!(n.is_repo_project("prj_steve"));
        assert!(!n.is_repo_project("prj_notes"));
        assert_eq!(n.resolve("Sarah C"), None);
        assert_eq!(n.resolve("李雷"), None, "two people of that name: no guess");
        assert_eq!(n.resolve(""), None);
    }

    #[test]
    fn parses_fenced_answers_and_ignores_garbage() {
        let out = "好的：\n```json\n{\"items\":[{\"kind\":\"fact\",\"scope\":\"organization\",\"text\":\" A \",\"about\":\"Acme\",\"level\":\"reasoning\",\"dup_of\":\"\",\"conflicts\":null}]}\n```";
        let r = parse(out);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "A");
        assert_eq!(r[0].scope, Some(ExtractScope::Organization));
        assert!(!r[0].invalid_scope);
        assert_eq!(r[0].about, vec!["Acme".to_string()]);
        assert_eq!(r[0].level, Some(Level::Reasoning));
        assert_eq!(r[0].dup_of, None);
        assert!(r[0].evidence.is_empty());
        let r = parse(
            r#"{"items":[{"kind":"rule","scope":"person","text":"T","evidence":["甲"," 乙 "],"basis":"repeated","when":"写邮件时","why":"以后写信用"},{"kind":"fact","scope":"bogus","text":"U","evidence":"丙"}]}"#,
        );
        assert_eq!(r[0].evidence, vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(
            (
                r[0].scope,
                r[0].basis.as_str(),
                r[0].when.as_str(),
                r[0].why.as_str()
            ),
            (
                Some(ExtractScope::Person),
                "repeated",
                "写邮件时",
                "以后写信用"
            )
        );
        assert_eq!(r[1].evidence, vec!["丙".to_string()]);
        assert_eq!(r[1].scope, None);
        assert!(r[1].invalid_scope);
        assert!(
            parse(r#"{"items":[]}"#).is_empty(),
            "none is a normal answer"
        );
        assert!(parse("没有").is_empty());
        assert!(parse("{\"items\": 3}").is_empty());
        assert!(parse("{broken").is_empty());
    }

    fn raw(text: &str) -> Raw {
        Raw {
            kind: "fact".into(),
            scope: Some(ExtractScope::Organization),
            text: text.into(),
            about: vec!["Acme".into()],
            evidence: vec![text.into()],
            basis: "verified".into(),
            ..Default::default()
        }
    }

    /// The source text the `raw` items quote.
    fn source(items: &[Raw]) -> String {
        items
            .iter()
            .flat_map(|r| r.evidence.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn select_enforces_scope_and_entity_matching_and_caps_total() {
        let n = names();
        let mut items = vec![
            Raw {
                dup_of: Some("mm_1".into()),
                ..raw("重复的")
            },
            Raw {
                scope: None,
                ..raw("缺少 scope 的条目")
            },
            Raw {
                scope: None,
                invalid_scope: true,
                ..raw("非法 scope 的条目")
            },
            Raw {
                about: vec![],
                ..raw("没有关于对象的事实")
            },
            Raw {
                scope: Some(ExtractScope::Person),
                about: vec!["王五".into()],
                ..raw("认不出的人")
            },
            Raw {
                scope: Some(ExtractScope::Person),
                about: vec!["Acme".into()],
                ..raw("scope 写 person 却只指了组织")
            },
            Raw {
                scope: Some(ExtractScope::Organization),
                about: vec!["HelperBot".into()],
                ..raw("只挂到 Bot 的事实不合法")
            },
            Raw {
                about: vec!["Acme".into(), "王五".into()],
                level: Some(Level::Public),
                conflicts: Some("mm_old".into()),
                ..raw("部分认出")
            },
            Raw {
                kind: "summary".into(),
                ..raw("种类不对")
            },
            Raw {
                kind: "event".into(),
                ..raw("事项和对话不抽事件")
            },
            Raw {
                level: Some(Level::Never),
                conflicts: Some("mm_bogus".into()),
                ..raw("更严的级别")
            },
        ];
        for i in 0..10 {
            items.push(raw(&format!(
                "第 {i} 条不一样的内容{}",
                "甲乙丙丁戊己庚辛壬癸".chars().nth(i).unwrap()
            )));
        }
        let src = source(&items);
        let got = select(
            items,
            &n,
            Level::Conclusion,
            &src,
            Task::default(),
            &[],
            &[],
            |id| id == "mm_old",
        );
        assert_eq!(got.candidates.len(), MAX_PER_ORIGIN);
        assert!(got.proposals.is_empty());
        assert_eq!(got.candidates[0].text, "部分认出");
        assert_eq!(got.candidates[0].about, vec![ObjRef::new("party", "org_1")]);
        assert_eq!(
            got.candidates[0].level,
            Level::Conclusion,
            "public < origin default"
        );
        assert_eq!(got.candidates[0].supersedes.as_deref(), Some("mm_old"));
        assert_eq!(got.candidates[1].text, "更严的级别");
        assert_eq!(got.candidates[1].level, Level::Never);
        assert_eq!(got.candidates[1].supersedes, None);
        assert!(got.dropped.contains(&Dropped::InvalidScope));
        assert!(got.dropped.contains(&Dropped::MissingAbout));
        assert!(got.dropped.contains(&Dropped::UnresolvedAbout));
        assert!(got.dropped.contains(&Dropped::CapReached));
        assert!(got.candidates.iter().all(|c| {
            c.text != "重复的"
                && c.text != "缺少 scope 的条目"
                && c.text != "非法 scope 的条目"
                && c.text != "没有关于对象的事实"
                && c.text != "认不出的人"
                && c.text != "scope 写 person 却只指了组织"
                && c.text != "只挂到 Bot 的事实不合法"
                && c.kind != "event"
        }));
    }

    #[test]
    fn codebase_scope_routes_deterministically_to_repo_bound_project_proposals() {
        let n = names();
        let items = vec![
            // Chinese engineering conclusion with `scope = codebase` (even with `kind = "fact"`):
            // routed deterministically without any keyword heuristic.
            Raw {
                kind: "fact".into(),
                scope: Some(ExtractScope::Codebase),
                text: "输入框由原生 textarea 绘制光标".into(),
                about: vec!["Steve".into()],
                evidence: vec!["输入框由原生 textarea 绘制光标".into()],
                basis: "verified".into(),
                why: "避免自绘光标导致输入法偏移".into(),
                ..Default::default()
            },
            // Pure-Chinese engineering conclusion with zero ASCII words using `task.project` fallback
            Raw {
                kind: KIND_PROPOSAL.into(),
                scope: Some(ExtractScope::Codebase),
                text: "消息列表在流式输出期间保持底部锚定，不要整段重排历史".into(),
                about: vec![],
                evidence: vec!["消息列表在流式输出期间保持底部锚定，不要整段重排历史".into()],
                basis: "verified".into(),
                why: "防止长对话滚动抖动".into(),
                ..Default::default()
            },
            // Normal fact about a person → stays as personal memory candidate
            Raw {
                kind: "fact".into(),
                scope: Some(ExtractScope::Person),
                text: "Sarah Chen 在 Acme 负责年度商务合作与采购".into(),
                about: vec!["Sarah Chen".into(), "Acme".into()],
                evidence: vec!["Sarah Chen 在 Acme 负责年度商务合作与采购".into()],
                basis: "verified".into(),
                ..Default::default()
            },
        ];
        let src = source(&items);
        let got = select(
            items,
            &n,
            Level::Conclusion,
            &src,
            Task {
                goals: &[],
                events: false,
                project: Some("prj_steve"),
            },
            &[],
            &[],
            |_| false,
        );
        assert_eq!(got.candidates.len(), 1);
        assert_eq!(
            got.candidates[0].text,
            "Sarah Chen 在 Acme 负责年度商务合作与采购"
        );
        assert_eq!(got.proposals.len(), 2);
        assert_eq!(got.proposals[0].text, "输入框由原生 textarea 绘制光标");
        assert!(got.proposals.iter().all(|p| p.project_id == "prj_steve"));
        assert!(got.proposals.iter().all(|p| p.kind == KIND_AGENTS_MD));

        // Codebase targeting a plain directory project (no git repo) is dropped & counted.
        let non_repo = vec![
            Raw {
                kind: KIND_PROPOSAL.into(),
                scope: Some(ExtractScope::Codebase),
                text: "普通目录项目没有代码仓库".into(),
                about: vec!["NotesDir".into()],
                evidence: vec!["普通目录项目没有代码仓库".into()],
                basis: "verified".into(),
                ..Default::default()
            },
            // Fallback task.project is also rejected if that project has no repo.
            Raw {
                kind: KIND_PROPOSAL.into(),
                scope: Some(ExtractScope::Codebase),
                text: "从无仓库项目回退也应被拒绝".into(),
                about: vec![],
                evidence: vec!["从无仓库项目回退也应被拒绝".into()],
                basis: "verified".into(),
                ..Default::default()
            },
            // Codebase targeting only a person with no repo project -> dropped, never memory.
            Raw {
                kind: "fact".into(),
                scope: Some(ExtractScope::Codebase),
                text: "只挂到联系人不能落成代码库提议".into(),
                about: vec!["Sarah Chen".into()],
                evidence: vec!["只挂到联系人不能落成代码库提议".into()],
                basis: "verified".into(),
                ..Default::default()
            },
        ];
        let src2 = source(&non_repo);
        let got2 = select(
            non_repo,
            &n,
            Level::Conclusion,
            &src2,
            Task {
                goals: &[],
                events: false,
                project: Some("prj_notes"),
            },
            &[],
            &[],
            |_| false,
        );
        assert!(got2.candidates.is_empty());
        assert!(got2.proposals.is_empty());
        assert_eq!(
            got2.dropped,
            vec![
                Dropped::UnresolvedAbout,
                Dropped::UnresolvedAbout,
                Dropped::UnresolvedAbout,
            ]
        );

        // Codebase with empty `about` and no `task.project` -> MissingAbout.
        let missing = vec![Raw {
            kind: KIND_PROPOSAL.into(),
            scope: Some(ExtractScope::Codebase),
            text: "未指明代码仓库项目".into(),
            about: vec![],
            evidence: vec!["未指明代码仓库项目".into()],
            basis: "verified".into(),
            ..Default::default()
        }];
        let src3 = source(&missing);
        let got3 = select(
            missing,
            &n,
            Level::Conclusion,
            &src3,
            Task::default(),
            &[],
            &[],
            |_| false,
        );
        assert!(got3.proposals.is_empty());
        assert_eq!(got3.dropped, vec![Dropped::MissingAbout]);

        // Plain directory project IS valid for `scope = project` (non-codebase project fact).
        let dir_fact = vec![Raw {
            kind: "fact".into(),
            scope: Some(ExtractScope::Project),
            text: "NotesDir 用来整理季度复盘资料".into(),
            about: vec!["NotesDir".into()],
            evidence: vec!["NotesDir 用来整理季度复盘资料".into()],
            basis: "verified".into(),
            ..Default::default()
        }];
        let src4 = source(&dir_fact);
        let got4 = select(
            dir_fact,
            &n,
            Level::Conclusion,
            &src4,
            Task::default(),
            &[],
            &[],
            |_| false,
        );
        assert_eq!(got4.candidates.len(), 1);
        assert_eq!(
            got4.candidates[0].about,
            vec![ObjRef::new("project", "prj_notes")]
        );
    }

    #[test]
    fn duplicate_proposals_are_dropped() {
        let n = names();
        let items = vec![Raw {
            kind: KIND_PROPOSAL.into(),
            scope: Some(ExtractScope::Codebase),
            text: "修改 Rust 代码前先跑 cargo nextest 确认基线。".into(),
            about: vec!["Steve".into()],
            evidence: vec!["修改 Rust 代码前先跑 cargo nextest 确认基线。".into()],
            basis: "verified".into(),
            ..Default::default()
        }];
        let src = source(&items);
        let known_proposals = vec![(
            "prj_steve".to_string(),
            "修改 Rust 代码前先跑 cargo nextest 确认基线".to_string(),
        )];
        let got = select(
            items,
            &n,
            Level::Conclusion,
            &src,
            Task::default(),
            &[],
            &known_proposals,
            |_| false,
        );
        assert!(got.proposals.is_empty());
        assert_eq!(got.dropped, vec![Dropped::Duplicate]);
    }

    #[test]
    fn evidence_must_be_in_the_text() {
        let task = Task::default();
        let r = Raw {
            evidence: vec!["原文里没有这句".into()],
            ..raw("Acme 的合同每年 3 月续签")
        };
        assert_eq!(
            check(&r, "Acme 的合同每年 3 月续签。", task, &[]),
            Err(Dropped::NoEvidence)
        );
        let r = Raw {
            evidence: vec![],
            ..raw("Acme 的合同每年 3 月续签")
        };
        assert_eq!(
            check(&r, "Acme 的合同每年 3 月续签。", task, &[]),
            Err(Dropped::NoEvidence)
        );
        // punctuation, spacing and a “…” cut do not matter
        let r = Raw {
            evidence: vec!["Acme 的合同 …… 3 月续签".into()],
            ..raw("Acme 的合同每年 3 月续签")
        };
        assert!(check(&r, "我记得 Acme 的合同，每年3月续签。", task, &[]).is_ok());
    }

    #[test]
    fn events_only_where_asked_for() {
        let r = Raw {
            kind: "event".into(),
            ..raw("3 月 1 日起由王五对接")
        };
        let src = "3 月 1 日起由王五对接";
        assert_eq!(check(&r, src, Task::default(), &[]), Err(Dropped::Shape));
        let mail = Task {
            goals: &[],
            events: true,
            project: None,
        };
        assert!(check(&r, src, mail, &[]).is_ok());
    }

    #[test]
    fn near_duplicates_of_known_memories_are_dropped() {
        let known = vec!["Steve 发版走 GitHub Release 流程，不在本机发布".to_string()];
        let r = raw("Steve 发版走 GitHub Release 流程，不在本机发布。");
        assert_eq!(
            check(&r, &r.text, Task::default(), &known),
            Err(Dropped::Duplicate)
        );
        // a replacement names what it replaces and is kept
        let r = Raw {
            conflicts: Some("mm_old".into()),
            ..r
        };
        assert!(check(&r, &r.text, Task::default(), &known).is_ok());
        assert!(!near_duplicate(
            "周五下午不安排部署",
            "Acme 的邮件一律用英文"
        ));
    }

    /// One historical candidate (ST-41 review of the first 18): what the model proposed, the item's
    /// goal if it came from one, the text it was given, and whether it should survive.
    struct Case {
        what: &'static str,
        kind: &'static str,
        text: &'static str,
        evidence: &'static str,
        basis: &'static str,
        goal: &'static str,
        source: &'static str,
        keep: Result<(), Dropped>,
    }

    #[test]
    fn the_historical_candidates_are_sorted_the_way_the_review_says() {
        let st39_goal = "修复卡片详情中排队信息重复展示的问题：排队信息只保留在评论区展示，输入框顶部不再显示该排队信息。\n\n（用户原话：卡片详情这个ui有问题，重复展示了，其实卡片详情里，这个排队信息只展示在评论就够了，输入框顶上其实就不需要了）";
        let cases = [
            Case {
                what: "ST-39 截图里的反例：修复要求被拔高成偏好",
                kind: "preference",
                text: "用户希望卡片详情中的排队信息只在评论区展示，不在输入框顶部重复显示。",
                evidence: "卡片详情这个ui有问题，重复展示了，其实卡片详情里，这个排队信息只展示在评论就够了，输入框顶上其实就不需要了",
                basis: "explicit",
                goal: st39_goal,
                source: st39_goal,
                keep: Err(Dropped::OneOff),
            },
            Case {
                what: "ST-39 反例写成 fact 也不行：它只是复述目标",
                kind: "fact",
                text: "卡片详情中的排队信息只在评论区展示，输入框顶部不再显示。",
                evidence: "这个排队信息只展示在评论就够了，输入框顶上其实就不需要了",
                basis: "verified",
                goal: st39_goal,
                source: st39_goal,
                keep: Err(Dropped::RestatesGoal),
            },
            Case {
                what: "ST-30 功能需求",
                kind: "preference",
                text: "用户希望事项产出支持多种格式的内容预览，而不只是展示产出列表。",
                evidence: "这些产出咋看不到内容？得支持各种格式产物的预览",
                basis: "explicit",
                goal: "",
                source: "这些产出咋看不到内容？得支持各种格式产物的预览",
                keep: Err(Dropped::OneOff),
            },
            Case {
                what: "11-02 对话：一次抱怨泛化成偏好",
                kind: "preference",
                text: "用户希望审批界面简洁、有序，避免频繁审批导致展示杂乱。",
                evidence: "频繁审批这个样式非常乱，得优化",
                basis: "explicit",
                goal: "",
                source: "我（10:00）：频繁审批这个样式非常乱，得优化",
                keep: Err(Dropped::OneOff),
            },
            Case {
                what: "ST-27 修复规格写成规则",
                kind: "rule",
                text: "事项恢复执行、重试成功或完成后，应自动清除对应旧异常提示，历史错误保留在时间线；关闭的提示刷新后不应再次出现。",
                evidence: "恢复执行、重试成功或事项完成后自动解除对应旧异常，历史错误保留在时间线；支持关闭后刷新不再出现",
                basis: "explicit",
                goal: "恢复执行、重试成功或事项完成后自动解除对应旧异常，历史错误保留在时间线；支持关闭后刷新不再出现",
                source: "恢复执行、重试成功或事项完成后自动解除对应旧异常，历史错误保留在时间线；支持关闭后刷新不再出现",
                keep: Err(Dropped::OneOff),
            },
            Case {
                what: "ST-16 一次性指令",
                kind: "preference",
                text: "发布 steve/st-16 这类分支前，要求先修复、解冲突并重新验证，再批准发布。",
                evidence: "修一下，解一下冲突，然后重新验证发布",
                basis: "explicit",
                goal: "",
                source: "你补充：修一下，解一下冲突，然后重新验证发布",
                keep: Err(Dropped::OneOff),
            },
            Case {
                what: "ST-33 明确说了“每次”：真正的长期规则",
                kind: "preference",
                text: "主会话收到需求时直接给出方案，不再每次询问是在当前会话出方案还是创建卡片。",
                evidence: "主会话就不用每次都问在这出方案还是创建卡片了，直接出方案就行。",
                basis: "explicit",
                goal: "",
                source: "你补充：主会话就不用每次都问在这出方案还是创建卡片了，直接出方案就行。",
                keep: Ok(()),
            },
            Case {
                what: "ST-26 “之前说过”：重复表达过的长期规则",
                kind: "rule",
                text: "Steve 发版走 GitHub 正常的 Release 流程，不在本机发版，也不调用 steve_publish。",
                evidence: "你之前说过「不要直接自己本地发，走 GitHub 正常的 release」",
                basis: "explicit",
                goal: "",
                source: "问题：你之前说过「不要直接自己本地发，走 GitHub 正常的 release」，这次怎么发？\n你的回答：不调用 steve_publish",
                keep: Ok(()),
            },
            Case {
                what: "说了两次的倾向（repeated，两处原话都在原文里）",
                kind: "preference",
                text: "回复先给结论，再给细节。",
                evidence: "先说结论\u{1f}结论放前面",
                basis: "repeated",
                goal: "",
                source: "我（10:00）：先说结论\n我（15:00）：结论放前面，细节后面再说",
                keep: Ok(()),
            },
            Case {
                what: "关于客户组织的稳定事实",
                kind: "fact",
                text: "Acme 的年度合同每年 3 月续签。",
                evidence: "Acme 的年度合同每年 3 月续签。",
                basis: "verified",
                goal: "整理客户台账",
                source: "交付摘要：Acme 的年度合同每年 3 月续签。",
                keep: Ok(()),
            },
        ];
        for c in &cases {
            let goals: Vec<String> = [c.goal]
                .into_iter()
                .filter(|g| !g.is_empty())
                .map(String::from)
                .collect();
            let r = Raw {
                kind: c.kind.into(),
                scope: Some(if c.kind == "fact" {
                    ExtractScope::Organization
                } else {
                    ExtractScope::Person
                }),
                text: c.text.into(),
                evidence: c.evidence.split('\u{1f}').map(String::from).collect(),
                basis: c.basis.into(),
                ..Default::default()
            };
            let task = Task {
                goals: &goals,
                events: false,
                project: None,
            };
            assert_eq!(
                check(&r, c.source, task, &[]).map(|_| ()),
                c.keep,
                "{}",
                c.what
            );
        }
    }
}
