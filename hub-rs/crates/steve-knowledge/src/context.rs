//! Knowledge in prompts (knowledge.md §4.1–4.3, slice K4): the extra context of an Agent session's
//! opening prompt ([`session_context`]), of Steve's main-thread answers ([`chat_context`]), and the
//! retrieval channel replies (C4) use ([`reply_hits`]).
//!
//! [`KnowledgeContext`] is the B1 [`PromptContext`] provider that steve-core already calls in
//! `startSession` and `answer`; the integrator only registers it. With nothing to say it returns
//! nothing, so prompts stay byte for byte what they are without the module.
//!
//! Visibility, `never` and paused sources are filtered by [`retrieve`]/[`anchor_mems`] and P3's
//! cards (§11); nothing here builds its own conditions.

use std::collections::VecDeque;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use steve_modules::contacts::{self, ContactCard};
use steve_modules::context::{ChatCtx, ChatDone, Cite, PromptContext};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::{Item, Store};

use crate::cite;
use crate::retrieve::{Hit, Purpose, Query, anchor_mems, record_use, retrieve};
use crate::state::{Guard, Knowledge};

/// Retrieved hits per scene (§4.1 layer 7).
pub const SESSION_K: usize = 8;
pub const CHAT_K: usize = 6;
pub const REPLY_K: usize = 10;
/// Rules and preferences about the anchors (layer 6), before the retrieved hits.
pub const ANCHOR_MEMS: usize = 5;
/// The whole session context, in characters; entries are dropped from the end beyond it (§4.2).
pub const SESSION_MAX: usize = 6000;
/// One entry's text, in characters (§4.2).
pub const SNIPPET_MAX: usize = 600;
/// The search text built from an item or a message, in characters.
pub const QUERY_MAX: usize = 500;

/// §4.2, verbatim.
pub const SESSION_HEAD: &str =
    "相关知识（Steve 按这件事检索到的，可能不全；需要更多时调用 steve_knowledge_search）：";
pub const SESSION_RULES: &str = "使用规则：“仅用于推理”的内容不要原样写进任何会发给别人的东西（邮件、群消息、PR 评论）；“可转述结论”只说结论，不提来源和细节；用到哪条就在句末标 [K1]。";
/// §4.3: footnote-style numbers, cite only what was used, never invent a source.
pub const CHAT_HEAD: &str = "相关知识（Steve 按你的问题检索到的，可能不全）：";
pub const CHAT_RULES: &str = "使用规则：用到知识时在句末标 [n]，没用到就不标；知识里没有的不要编造来源。“仅用于推理”的内容不要原样写出；“可转述结论”只说结论，不提来源和细节。";
/// Heading of the contact cards in a session (layer 6).
pub const CARDS_HEAD: &str = "这件事相关的人（Steve 记得的，括号里是披露级别）：";

/// The prompt wording of a level (§4.2); `never` never reaches a prompt.
pub fn level_label(l: Level) -> &'static str {
    contacts::cards::level_label(l)
}

/// Whitespace runs collapsed (one entry = one line), cut to [`SNIPPET_MAX`] characters.
fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(SNIPPET_MAX - 1).collect();
    out.push('…');
    out
}

/// `[K1] <正文> —— 来源：<label>；级别：<…>` (`prefix` = `K` for sessions and replies, `""` for chat).
pub fn snippet_line(prefix: &str, h: &Hit) -> String {
    format!(
        "[{prefix}{}] {} —— 来源：{}；级别：{}",
        h.n,
        one_line(&h.text),
        h.label,
        level_label(h.level)
    )
}

/// The §4.2 block: head, one line per hit, rules. `""` without hits.
pub fn render_snippets(head: &str, rules: &str, prefix: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut lines = vec![head.to_string()];
    lines.extend(hits.iter().map(|h| snippet_line(prefix, h)));
    lines.push(rules.to_string());
    lines.join("\n")
}

/// The text to search with: secrets the guard finds are blanked out (never searched, never stored in
/// `kb_uses`, §11), whitespace collapsed, cut to [`QUERY_MAX`] characters.
pub fn scrub_query(guard: &dyn Guard, text: &str) -> String {
    let mut s = text.to_string();
    let mut ranges = guard.scan(text);
    ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
    for r in ranges {
        let (mut a, mut b) = (r.start.min(s.len()), r.end.min(s.len()));
        while !s.is_char_boundary(a) {
            a -= 1;
        }
        while !s.is_char_boundary(b) {
            b += 1;
        }
        if a < b {
            s.replace_range(a..b, " ");
        }
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(QUERY_MAX)
        .collect()
}

/// Anchor memories first, then the retrieved hits that are not among them, numbered `1..` again.
fn merge(first: Vec<Hit>, then: Vec<Hit>) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::new();
    for h in first.into_iter().chain(then) {
        if out
            .iter()
            .any(|x| x.obj_type == h.obj_type && x.obj_id == h.obj_id && x.seq == h.seq)
        {
            continue;
        }
        out.push(h);
    }
    for (i, h) in out.iter_mut().enumerate() {
        h.n = i + 1;
    }
    out
}

// ---------------------------------------------------------------- whose session

/// The user an item's agent works for: who created the item (`audit item.create`) when that is a
/// member of the workspace, else the only member of a one-person workspace. `None` otherwise (items
/// made by Steve in a shared workspace): the session then sees only what the whole workspace may.
pub fn item_user(c: &Connection, ws: &str, item_id: &str) -> ApiResult<Option<String>> {
    let actor: Option<String> = c
        .prepare_cached(
            "SELECT a.actor FROM audit a JOIN workspace_members w ON w.workspace_id = a.workspace_id AND w.user_id = a.actor
              WHERE a.workspace_id = ? AND a.action = 'item.create' AND a.target = ? ORDER BY a.id LIMIT 1",
        )?
        .query_row(params![ws, item_id], |r| r.get(0))
        .optional()?;
    if actor.is_some() {
        return Ok(actor);
    }
    let members: Vec<String> = c
        .prepare_cached("SELECT user_id FROM workspace_members WHERE workspace_id = ? LIMIT 2")?
        .query_map([ws], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match members.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    })
}

/// The viewer an item's session (opening prompt, MCP tools) filters for.
pub fn item_viewer(c: &Connection, ws: &str, item_id: &str) -> ApiResult<Viewer> {
    Ok(match item_user(c, ws, item_id)? {
        Some(u) => Viewer::load(c, ws, &u)?,
        None => Viewer::bare(ws, ""),
    })
}

fn latest_goal(c: &Connection, item_id: &str) -> ApiResult<String> {
    Ok(c.prepare_cached(
        "SELECT text FROM item_goals WHERE item_id = ? ORDER BY version DESC LIMIT 1",
    )?
    .query_row([item_id], |r| r.get(0))
    .optional()?
    .unwrap_or_default())
}

/// The session the opening prompt being built belongs to: steve-core creates the row just before.
pub fn latest_session(c: &Connection, item_id: &str) -> ApiResult<Option<String>> {
    Ok(c
        .prepare_cached(
            "SELECT id FROM sessions WHERE item_id = ? ORDER BY no DESC, created DESC, rowid DESC LIMIT 1",
        )?
        .query_row([item_id], |r| r.get(0))
        .optional()?)
}

// ---------------------------------------------------------------- sessions

/// What an opening prompt gets: `text` (`""` = nothing at all) and the numbered hits in it, for
/// `kb_uses(session)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionCtx {
    pub text: String,
    /// The search text (title + latest goal, scrubbed).
    pub query: String,
    pub hits: Vec<Hit>,
    pub cards: Vec<ContactCard>,
}

/// Layers 6 and 7 for an Agent session on `item` (§4.1): the cards of the item's people, the rules
/// and preferences about the item, its project and those people, and `retrieve(title + latest goal,
/// k=8, Agent)`; all as the item's user sees them. ≤[`SESSION_MAX`] characters.
pub fn session_context(
    c: &Connection,
    guard: &dyn Guard,
    item: &Item,
    now: i64,
) -> ApiResult<SessionCtx> {
    let ws = item.workspace_id.as_str();
    let viewer = item_viewer(c, ws, &item.id)?;
    let cards = contacts::item_contact_cards(c, &viewer, &item.id)?;

    let mut anchors = vec![ObjRef::new("item", &item.id)];
    if let Some(p) = item.project_id.as_deref().filter(|p| !p.is_empty()) {
        anchors.push(ObjRef::new("project", p));
    }
    anchors.extend(cards.iter().map(|k| ObjRef::new("party", &k.party_id)));
    let rules = anchor_mems(c, &viewer, &anchors, ANCHOR_MEMS, now)?;

    let query = scrub_query(
        guard,
        &format!("{} {}", item.title, latest_goal(c, &item.id)?),
    );
    let mut q = Query::new(&viewer, &query, Purpose::Agent);
    q.anchors = &anchors;
    q.k = SESSION_K;
    q.now = now;
    let found = retrieve(c, &q)?;
    let mut hits = merge(rules, found);

    let cards_text = contacts::cards::render(&cards);
    let cards_block = if cards_text.is_empty() {
        String::new()
    } else {
        format!("{CARDS_HEAD}\n{cards_text}")
    };
    let build = |hits: &[Hit]| {
        [
            cards_block.clone(),
            render_snippets(SESSION_HEAD, SESSION_RULES, "K", hits),
        ]
        .into_iter()
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
    };
    let mut text = build(&hits);
    while text.chars().count() > SESSION_MAX && !hits.is_empty() {
        hits.pop();
        text = build(&hits);
    }
    if text.chars().count() > SESSION_MAX {
        text = text.chars().take(SESSION_MAX).collect();
    }
    Ok(SessionCtx {
        text,
        query,
        hits,
        cards,
    })
}

// ---------------------------------------------------------------- main thread

/// What a main-thread answer gets, plus what `kb_uses(chat)` needs afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatKnowledge {
    pub ctx: ChatCtx,
    pub query: String,
    pub hits: Vec<Hit>,
}

/// The citation entry of a hit (`messages.meta.cites`).
pub fn cite_of(h: &Hit) -> Cite {
    Cite {
        n: h.n as u32,
        label: h.label.clone(),
        href: h.href.clone(),
        level: h.level,
    }
}

/// Layers 6 and 7 for Steve's answer to `text` from `viewer` (§4.1): rules and preferences about
/// “我”, then `retrieve(text, k=6, Chat)`; numbered `[1] [2]` (§4.3). In a workspace with more than
/// one member only workspace-visible knowledge is used (§3.3). `None` = nothing found.
pub fn chat_context(
    c: &Connection,
    guard: &dyn Guard,
    viewer: &Viewer,
    text: &str,
    now: i64,
) -> ApiResult<Option<ChatKnowledge>> {
    let query = scrub_query(guard, text);
    let mut rules = Vec::new();
    if !viewer.user.is_empty()
        && let Some(me) = contacts::repo::me_of(c, &viewer.ws, &viewer.user)?
    {
        rules = anchor_mems(c, viewer, &[ObjRef::new("party", &me.id)], ANCHOR_MEMS, now)?;
        // anchor memories ignore the query, so the shared-thread rule is applied here too
        if crate::repo::member_count(c, &viewer.ws)? > 1 {
            rules.retain(|h| h.visibility == steve_store::base::Visibility::Workspace);
        }
    }
    let mut q = Query::new(viewer, &query, Purpose::Chat);
    q.k = CHAT_K;
    q.now = now;
    let hits = merge(rules, retrieve(c, &q)?);
    if hits.is_empty() {
        return Ok(None);
    }
    Ok(Some(ChatKnowledge {
        ctx: ChatCtx {
            text: render_snippets(CHAT_HEAD, CHAT_RULES, "", &hits),
            cites: hits.iter().map(cite_of).collect(),
        },
        query,
        hits,
    }))
}

/// `cite::finish` and the reply's extra `meta` (§4.3): `cites` with only what was cited.
pub fn chat_finish(answer: &str, cx: &ChatCtx) -> ChatDone {
    let f = cite::finish(answer, &cx.cites);
    let mut meta = serde_json::Map::new();
    meta.insert(
        "cites".into(),
        serde_json::to_value(&f.used).unwrap_or_default(),
    );
    ChatDone {
        text: f.text,
        meta: Some(meta),
    }
}

// ---------------------------------------------------------------- channel replies (C4)

/// Layer 7 of a channel reply (§4.1): `retrieve(message + the last 3 messages, k=10, Reply)` for the
/// account owner's user. Layer 6 (cards, space summary) stays with C4; render the hits with
/// [`render_snippets`] and record them in `reply_basis`.
pub fn reply_hits(
    c: &Connection,
    guard: &dyn Guard,
    viewer: &Viewer,
    message: &str,
    recent: &[String],
    anchors: &[ObjRef],
    now: i64,
) -> ApiResult<Vec<Hit>> {
    let start = recent.len().saturating_sub(3);
    let mut text = message.to_string();
    for r in &recent[start..] {
        text.push(' ');
        text.push_str(r);
    }
    let query = scrub_query(guard, &text);
    let mut q = Query::new(viewer, &query, Purpose::Reply);
    q.anchors = anchors;
    q.k = REPLY_K;
    q.now = now;
    Ok(retrieve(c, &q)?)
}

// ---------------------------------------------------------------- the B1 provider

/// Main-thread contexts handed out and not yet finished, newest last (to find the hits again in
/// `chat_done`, which only gets the [`ChatCtx`] back).
const PENDING: usize = 64;

/// The knowledge module's [`PromptContext`]: register it with `Registry::prompt_context`.
pub struct KnowledgeContext {
    store: Store,
    kb: Knowledge,
    pending: Mutex<VecDeque<(String, String, Vec<Hit>)>>,
}

impl KnowledgeContext {
    /// `store` is the Hub's store (the same one `Modules` gets): `kb_uses` rows are written with it.
    pub fn new(store: Store, kb: Knowledge) -> Self {
        Self {
            store,
            kb,
            pending: Mutex::new(VecDeque::new()),
        }
    }

    fn guard(&self) -> &dyn Guard {
        self.kb.guard.as_ref()
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, String, Vec<Hit>)>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, ws: &str, consumer: (&str, &str), query: &str, hits: &[Hit]) {
        if let Err(e) = self
            .store
            .transaction(|tx| record_use(tx, ws, consumer, query, hits))
        {
            tracing::warn!(consumer = consumer.0, id = consumer.1, "kb_uses: {e}");
        }
    }
}

impl PromptContext for KnowledgeContext {
    fn session(&self, c: &Connection, _ws: &str, item: &Item) -> Option<String> {
        let cx = session_context(c, self.guard(), item, steve_store::now())
            .map_err(|e: ApiError| tracing::warn!(item = %item.id, "session knowledge: {e}"))
            .ok()?;
        if !cx.hits.is_empty() {
            match latest_session(c, &item.id) {
                Ok(Some(sid)) => {
                    self.record(&item.workspace_id, ("session", &sid), &cx.query, &cx.hits)
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(item = %item.id, "session of item: {e}"),
            }
        }
        Some(cx.text).filter(|t| !t.is_empty())
    }

    fn chat(&self, c: &Connection, _ws: &str, viewer: &Viewer, text: &str) -> Option<ChatCtx> {
        let k = chat_context(c, self.guard(), viewer, text, steve_store::now())
            .map_err(|e| tracing::warn!(ws = %viewer.ws, "chat knowledge: {e}"))
            .ok()??;
        let mut p = self.pending();
        if p.len() >= PENDING {
            p.pop_front();
        }
        p.push_back((k.ctx.text.clone(), k.query, k.hits));
        Some(k.ctx)
    }

    fn chat_done(
        &self,
        _c: &Connection,
        ws: &str,
        message_id: &str,
        answer: &str,
        cx: &ChatCtx,
    ) -> ChatDone {
        let found = {
            let mut p = self.pending();
            p.iter()
                .rposition(|(t, _, _)| *t == cx.text)
                .and_then(|i| p.remove(i))
        };
        if let Some((_, query, hits)) = found {
            self.record(ws, ("chat", message_id), &query, &hits);
        }
        chat_finish(answer, cx)
    }
}

/// A boxed provider for `Registry::prompt_context`.
pub fn provider(store: Store, kb: Knowledge) -> Box<dyn PromptContext> {
    Box::new(KnowledgeContext::new(store, kb))
}
