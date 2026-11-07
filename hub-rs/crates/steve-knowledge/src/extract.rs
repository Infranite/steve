//! Extracting candidate memories with the model gateway.
//!
//! One call per origin (an item completion, a day of chat, a mail thread): the text, the memories it
//! resembles (confirmed and still pending, so the model can say “duplicate of” or “replaces”) and the
//! names it may use for “关于”.
//!
//! Only two kinds of record are proposed (ST-41): long-term memories (`preference`, `rule`: what the
//! user said holds from now on) and reusable knowledge (`fact`: what is confirmed and useful beyond
//! this one task). Item records (goals, requests, progress, deliveries) and conversation records stay
//! where they are. The server then checks what the model cannot be trusted with ([`check`]): every
//! candidate quotes the text; a preference or rule from one plain request is dropped; restating the
//! item's goal is dropped; near-duplicates are dropped. Names resolve only exactly (never guessed), the
//! stricter level wins, at most [`MAX_PER_ORIGIN`] are kept, and none is the usual outcome. Other
//! modules call [`submit`].

use std::time::Duration;

use rusqlite::Connection;
use serde_json::Value;
use steve_modules::contacts::people_repo;
use steve_modules::{ApiError, ApiResult, Modules, Tier};
use steve_store::base::mems::{self, Mem};
use steve_store::base::{Level, ObjRef, Viewer};
use tokio::sync::Semaphore;

use crate::candidates::{self, MAX_PER_ORIGIN, NewCandidate, Origin, OriginKind};
use crate::retrieve::{self, Purpose, Query};
use crate::state::{Knowledge, redact};

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
const KINDS: [&str; 4] = ["fact", "preference", "rule", "event"];

/// At most two extraction calls run at a time.
static SLOTS: Semaphore = Semaphore::const_new(2);

// ---------------------------------------------------------------- names

/// The objects “关于” may name: contacts, organizations, teams (names and aliases), projects and bots
/// of the workspace that the user can see; “我” is the user's own contact.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// `(display name, label, ref)`, in prompt order (item people first).
    entries: Vec<(String, &'static str, ObjRef)>,
    /// `(lowercased name or alias, ref)`.
    keys: Vec<(String, ObjRef)>,
}

impl Names {
    /// Loads the names for `viewer`; the parties linked to `item` come first in the prompt list.
    pub fn load(c: &Connection, viewer: &Viewer, item: Option<&str>) -> ApiResult<Self> {
        let mut n = Self::default();
        let mut parties = people_repo::parties(c, viewer, &["contact", "org", "team"], false)?;
        if let Some(item) = item {
            let linked: Vec<String> = c
                .prepare_cached(
                    "SELECT object_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND subject_id = ? AND object_type = 'party' ORDER BY created",
                )?
                .query_map([&viewer.ws, &item.to_string()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            parties.sort_by_key(|p| !linked.contains(&p.id));
        }
        for p in &parties {
            let r = ObjRef::new("party", &p.id);
            let label = match p.kind.as_str() {
                "org" => "组织",
                "team" => "团队",
                _ => "联系人",
            };
            if p.user_id.as_deref() == Some(viewer.user.as_str()) {
                n.keys.push(("我".into(), r.clone()));
            }
            n.add(&p.name, label, &r);
            for a in &p.aliases {
                n.keys.push((a.trim().to_lowercase(), r.clone()));
            }
        }
        for (sql, kind, label) in [
            (
                "SELECT id, name FROM projects WHERE workspace_id = ? ORDER BY name, id",
                "project",
                "项目",
            ),
            (
                "SELECT b.id, b.name FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id WHERE w.workspace_id = ? ORDER BY b.name, b.id",
                "bot",
                "Bot",
            ),
        ] {
            let rows: Vec<(String, String)> = c
                .prepare_cached(sql)?
                .query_map([&viewer.ws], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, name) in rows {
                n.add(&name, label, &ObjRef::new(kind, &id));
            }
        }
        Ok(n)
    }

    fn add(&mut self, name: &str, label: &'static str, r: &ObjRef) {
        self.entries.push((name.to_string(), label, r.clone()));
        self.keys.push((name.trim().to_lowercase(), r.clone()));
    }

    /// The object `name` names: an id of a listed object, or the single object whose name or alias
    /// equals it (case-insensitive). None or several → `None`: never a guess.
    pub fn resolve(&self, name: &str) -> Option<ObjRef> {
        let w = name.trim();
        if w.is_empty() {
            return None;
        }
        if let Some((_, _, r)) = self.entries.iter().find(|(_, _, r)| r.id == w) {
            return Some(r.clone());
        }
        let wl = w.to_lowercase();
        let mut hits: Vec<&ObjRef> = self
            .keys
            .iter()
            .filter(|(k, _)| *k == wl)
            .map(|(_, r)| r)
            .collect();
        hits.dedup();
        hits.sort();
        hits.dedup();
        match hits.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    }

    /// The prompt lines, at most [`MAX_NAMES`]: “- Sarah Chen（联系人；别名 Sarah）”.
    fn prompt_lines(&self) -> String {
        self.entries
            .iter()
            .take(MAX_NAMES)
            .map(|(name, label, _)| format!("- {name}（{label}）"))
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
}

/// The extraction prompt. It sorts the text into the four kinds of record first (ST-41):
/// item records and conversation records are kept already and are never proposed; only
/// long-term memories (preference, rule) and reusable knowledge (fact) are, and none is the usual answer.
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
            "\n5. 日程与事件（kind = event）：对方明确说的、以后需要记着的日期或安排，比如“3 月 1 日起由王五对接”。basis 填 explicit。",
        )
    } else {
        ("", "")
    };
    format!(
        "{PROMPT_HEAD}下面是{label}的原文。先分清每段内容属于哪一类，只输出第 3、4 类：\n\
1. 事项记录：这次要做什么、改哪里、进度、交付了什么、怎么验收，以及一次性的要求、批准和指令（“修一下”“这里改成那样”“要支持某功能”）。它们已经保存在事项和执行记录里，不要输出。\n\
2. 会话记录：这次聊了什么、做了哪些决定。对话原文已经保存、可以检索，不要输出。\n\
3. 长期记忆（kind = preference 或 rule）：用户明确表达、以后在别的事情上也适用的稳定偏好或长期规则。必须满足其一：原话明确说了长期适用（“以后”“后续”“每次”“总是”“一律”“这类”“不要再”“之前说过”之类），basis 填 explicit；或原文里不同场合至少两次表达了同一倾向，basis 填 repeated，evidence 列出这两处原话。只凭一次具体反馈不能推断成偏好：“这个页面的按钮太挤了，改宽松点”是这件事的修改要求，不是“用户偏好宽松的界面”。\n\
4. 可复用知识（kind = fact）：以后做别的事也用得上、原文已经确认的事实或做法，比如某类问题的原因和避免办法、系统或外部环境的约束、某人或某组织的稳定信息。basis 填 verified。“这次把 X 改成了 Y”“用户希望有 X 功能”不是知识，它们属于事项记录。{event_line}\n\n\
复述事项目标、换个说法总结需求、把功能需求写成“用户希望……”，都不要输出。没有符合的就返回 {{\"items\":[]}}，这是常见的正常结果；宁可少，不要凑数，最多 {MAX_PER_ORIGIN} 条。\n\n\
可识别的名字（about 只能用这些名字，认不出就留空，不要猜）：\n{names}\n\n\
已有的长期记忆和知识（含待确认的；和某条重复就在 dup_of 填它的 id；和已确认的某条冲突、应该替换它就在 conflicts 填它的 id）：\n{similar}\n\n\
原文：\n<<<\n{text}\n>>>\n\n\
只返回 JSON：{{\"items\":[{{\"kind\":\"preference|rule|fact{event_kind}\",\"text\":\"一句话，写成以后可以直接照做或引用的规则、偏好或事实\",\"scope\":\"适用范围：在什么场景下用\",\"why\":\"为什么值得长期保留：以后在什么场景会用到\",\"basis\":\"explicit|repeated|verified\",\"evidence\":[\"原文摘录，逐字照抄\"],\"about\":[\"名字\"],\"level\":\"public|conclusion|reasoning|never\",\"dup_of\":null,\"conflicts\":null}}]}}"
    )
}

/// One item of the model's answer, as given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raw {
    pub kind: String,
    pub text: String,
    pub about: Vec<String>,
    pub level: Option<Level>,
    /// The quotes it rests on (a string or a list in the answer).
    pub evidence: Vec<String>,
    /// `explicit` | `repeated` | `verified`, or empty.
    pub basis: String,
    /// Where it applies.
    pub scope: String,
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
        .map(|x| Raw {
            kind: opt_str(x.get("kind")).unwrap_or_default(),
            text: opt_str(x.get("text")).unwrap_or_default(),
            about: str_list(x.get("about")),
            level: x
                .get("level")
                .and_then(Value::as_str)
                .and_then(Level::parse),
            evidence: str_list(x.get("evidence")),
            basis: opt_str(x.get("basis")).unwrap_or_default(),
            scope: opt_str(x.get("scope")).unwrap_or_default(),
            why: opt_str(x.get("why")).unwrap_or_default(),
            dup_of: opt_str(x.get("dup_of")),
            conflicts: opt_str(x.get("conflicts")),
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

/// Why a raw item was dropped (tests and the log say which rule did it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// The model marked it a duplicate, or it is close to a memory already there or proposed.
    Duplicate,
    /// Not a kind this origin proposes, or no usable text.
    Shape,
    /// No quote, or a quote that is not in the text.
    NoEvidence,
    /// A preference or rule from one plain request: no word that it lasts, not said twice.
    OneOff,
    /// It restates the item's goal.
    RestatesGoal,
}

/// The server's rules over one raw item. `source` is the text the model saw,
/// `known` the texts of the user's confirmed and pending memories. `Ok` = the evidence quotes kept.
pub fn check(r: &Raw, source: &str, task: Task, known: &[String]) -> Result<Vec<String>, Dropped> {
    let kinds: &[&str] = if task.events { &KINDS } else { &KINDS[..3] };
    if !kinds.contains(&r.kind.as_str()) {
        return Err(Dropped::Shape);
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
    if matches!(r.kind.as_str(), "preference" | "rule") {
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
    if r.conflicts.is_none() && known.iter().any(|k| near_duplicate(&r.text, k)) {
        return Err(Dropped::Duplicate);
    }
    Ok(quotes)
}

/// The server's rules over the model's items (ST-41): [`check`] first; then `about` names
/// resolved exactly, unknown ones dropped — and a candidate whose named objects are all unknown is
/// dropped (it would be about the wrong thing, or nothing); the stricter of the model's and the
/// origin's level; `conflicts` kept only if `valid_conflict` accepts it; near-duplicates within the
/// answer dropped; at most [`MAX_PER_ORIGIN`]. None is a normal result.
pub fn select(
    raw: Vec<Raw>,
    names: &Names,
    default_level: Level,
    source: &str,
    task: Task,
    known: &[String],
    valid_conflict: impl Fn(&str) -> bool,
) -> Vec<NewCandidate> {
    let mut out: Vec<NewCandidate> = Vec::new();
    for r in raw {
        if out.len() >= MAX_PER_ORIGIN {
            break;
        }
        let quotes = match check(&r, source, task, known) {
            Ok(q) => q,
            Err(why) => {
                tracing::debug!(?why, text = %r.text, "kb.extract: dropped");
                continue;
            }
        };
        let mut about: Vec<ObjRef> = Vec::new();
        for name in &r.about {
            if let Some(x) = names.resolve(name)
                && !about.contains(&x)
            {
                about.push(x);
            }
        }
        if !r.about.is_empty() && about.is_empty() {
            continue;
        }
        if out.iter().any(|c| near_duplicate(&c.text, &r.text)) {
            continue;
        }
        out.push(NewCandidate {
            kind: r.kind,
            text: r.text,
            about,
            level: r.level.unwrap_or(default_level).max(default_level),
            evidence: quotes.join(" / "),
            supersedes: r.conflicts.filter(|id| valid_conflict(id)),
            why: r.why,
            scope: r.scope,
            owner_said: false,
        });
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
fn known(c: &Connection, ws: &str, user: &str) -> ApiResult<(Vec<Similar>, Vec<String>)> {
    let rows: Vec<(String, String, String)> = c
        .prepare_cached(
            "SELECT id, text, status FROM mems WHERE workspace_id = ?1
               AND (status = 'confirmed' OR (status = 'candidate' AND created_by = ?2))
             ORDER BY updated DESC, id LIMIT 500",
        )?
        .query_map([ws, user], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
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

/// Runs one extraction for `origin` over `text` and writes the candidates. `item` puts the item's
/// people first in the name list; `task` holds what the text is about (its goals) and whether events
/// are wanted. Returns the candidates written — often none.
pub async fn run(
    m: &Modules,
    kb: &Knowledge,
    origin: &Origin,
    text: &str,
    item: Option<&str>,
    task: Task<'_>,
) -> ApiResult<Vec<Mem>> {
    let text = redact(kb.guard.as_ref(), &clip(text, MAX_INPUT));
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let now = m.clock.now();
    // retrieval (which may ask a remote store) and the name list: off the runtime
    let (k, o, t, it) = (
        kb.clone(),
        origin.clone(),
        text.clone(),
        item.map(str::to_string),
    );
    let (names, similar, known) = m
        .blocking(move |m| {
            m.store.reader(|c| {
                let (kb, origin, text, item) = (&k, &o, &t, it.as_deref());
                let viewer = Viewer::load(c, &origin.ws, &origin.user)?;
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
                Ok::<_, ApiError>((names, similar, known))
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
    let raw = parse(&answer.text);
    let offered = raw.len();
    let ws = origin.ws.clone();
    let user = origin.user.clone();
    let chosen = m.store.reader(|c| {
        let viewer = Viewer::load(c, &ws, &user)?;
        Ok::<_, ApiError>(select(
            raw,
            &names,
            origin.default_level,
            &text,
            task,
            &known,
            |id| {
                mems::visible(c, &viewer, id)
                    .ok()
                    .flatten()
                    .is_some_and(|x| x.status == "confirmed")
            },
        ))
    })?;
    tracing::info!(origin = %origin.id, offered, kept = chosen.len(), "kb.extract: candidates");
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    let (guard, origin) = (kb.guard.clone(), origin.clone());
    m.blocking(move |m| {
        let made = m.store.transaction(|tx| {
            chosen
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    // only the owner's own conversation tells their words apart (`我（…）：`)
                    c.owner_said = origin.kind == OriginKind::Chat
                        && c.kind != "event"
                        && candidates::owner_said(&text, OWNER_LINE, &c.evidence);
                    c.evidence = redact(guard.as_ref(), &c.evidence);
                    candidates::propose(tx, &origin, &c)
                })
                .collect::<ApiResult<Vec<_>>>()
        })?;
        kept_notice(m, &origin.ws, &made);
        Ok(made)
    })
    .await
}

/// How a line of the owner starts in a day of chat (`sources::chat::day_text`).
const OWNER_LINE: &str = "我（";

/// Memories kept at once because the owner said so: one notice says which, and where to take them back.
fn kept_notice(m: &Modules, ws: &str, made: &[Mem]) {
    let kept: Vec<String> = made
        .iter()
        .filter(|x| x.status == "confirmed")
        .map(|x| format!("「{}」", x.text.chars().take(80).collect::<String>()))
        .collect();
    if kept.is_empty() {
        return;
    }
    let text = format!(
        "按你在对话里说的，记住了 {} 条：{}。不对的话到 知识 › 长期记忆 里改或删掉。",
        kept.len(),
        kept.join("；")
    );
    let meta = serde_json::json!({"notice": {"kind": "memory", "level": "info"}});
    if let Err(e) = m
        .store
        .messages()
        .add(ws, "steve", &text, None, &meta, "notify")
    {
        tracing::warn!("kb.extract: kept notice: {e}");
    }
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
        None => m.store.reader(|c| owner(c, ws))?,
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
    };
    run(m, kb, &o, text, None, task).await
}

/// The workspace's owner (else its first member): whom workspace-wide origins (main thread chat)
/// write their candidates for.
pub fn owner(c: &Connection, ws: &str) -> ApiResult<String> {
    use rusqlite::OptionalExtension;
    c.prepare_cached(
        "SELECT user_id FROM workspace_members WHERE workspace_id = ? ORDER BY role = 'owner' DESC, created, user_id LIMIT 1",
    )?
    .query_row([ws], |r| r.get(0))
    .optional()?
    .ok_or_else(|| ApiError::not_found("工作空间成员"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        let mut n = Names::default();
        n.add("Sarah Chen", "联系人", &ObjRef::new("party", "ct_1"));
        n.keys.push(("sarah".into(), ObjRef::new("party", "ct_1")));
        n.add("Acme", "组织", &ObjRef::new("party", "org_1"));
        n.add("李雷", "联系人", &ObjRef::new("party", "ct_2"));
        n.add("李雷", "联系人", &ObjRef::new("party", "ct_3"));
        n
    }

    #[test]
    fn resolves_only_exact_and_unique_names() {
        let n = names();
        assert_eq!(n.resolve("sarah chen"), Some(ObjRef::new("party", "ct_1")));
        assert_eq!(n.resolve(" Sarah "), Some(ObjRef::new("party", "ct_1")));
        assert_eq!(n.resolve("org_1"), Some(ObjRef::new("party", "org_1")));
        assert_eq!(n.resolve("Sarah C"), None);
        assert_eq!(n.resolve("李雷"), None, "two people of that name: no guess");
        assert_eq!(n.resolve(""), None);
    }

    #[test]
    fn parses_fenced_answers_and_ignores_garbage() {
        let out = "好的：\n```json\n{\"items\":[{\"kind\":\"fact\",\"text\":\" A \",\"about\":\"Acme\",\"level\":\"reasoning\",\"dup_of\":\"\",\"conflicts\":null}]}\n```";
        let r = parse(out);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "A");
        assert_eq!(r[0].about, vec!["Acme".to_string()]);
        assert_eq!(r[0].level, Some(Level::Reasoning));
        assert_eq!(r[0].dup_of, None);
        assert!(r[0].evidence.is_empty());
        let r = parse(
            r#"{"items":[{"kind":"rule","text":"T","evidence":["甲"," 乙 "],"basis":"repeated","scope":"写邮件时","why":"以后写信用"},{"kind":"fact","text":"U","evidence":"丙"}]}"#,
        );
        assert_eq!(r[0].evidence, vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(
            (r[0].basis.as_str(), r[0].scope.as_str(), r[0].why.as_str()),
            ("repeated", "写邮件时", "以后写信用")
        );
        assert_eq!(r[1].evidence, vec!["丙".to_string()]);
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
            text: text.into(),
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
    fn select_applies_the_server_rules() {
        let n = names();
        let mut items = vec![
            Raw {
                dup_of: Some("mm_1".into()),
                ..raw("重复的")
            },
            Raw {
                about: vec!["王五".into()],
                ..raw("认不出的人")
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
            |id| id == "mm_old",
        );
        assert_eq!(got.len(), MAX_PER_ORIGIN);
        assert_eq!(got[0].text, "部分认出");
        assert_eq!(got[0].about, vec![ObjRef::new("party", "org_1")]);
        assert_eq!(got[0].level, Level::Conclusion, "public < origin default");
        assert_eq!(got[0].supersedes.as_deref(), Some("mm_old"));
        assert_eq!(got[1].text, "更严的级别");
        assert_eq!(got[1].level, Level::Never);
        assert_eq!(got[1].supersedes, None);
        assert!(
            got.iter()
                .all(|c| c.text != "重复的" && c.text != "认不出的人" && c.kind != "event")
        );
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
                what: "ST-27 可复用知识：仓库与发布的约束",
                kind: "fact",
                text: "Infranite/steve 仓库是公开仓库；PR 不运行 CI，只有推送版本标签才触发发布流程。",
                evidence: "仓库是公开的；这个仓库的 PR 不跑 CI，只有打版本标签才会触发发布流程。",
                basis: "verified",
                goal: "恢复执行、重试成功或事项完成后自动解除对应旧异常",
                source: "交付摘要：仓库是公开的；这个仓库的 PR 不跑 CI，只有打版本标签才会触发发布流程。",
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
                text: c.text.into(),
                evidence: c.evidence.split('\u{1f}').map(String::from).collect(),
                basis: c.basis.into(),
                ..Default::default()
            };
            let task = Task {
                goals: &goals,
                events: false,
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
