//! Knowledge in prompts: the extra context of an Agent session's
//! opening prompt ([`session_context`]), of Steve's main-thread answers ([`chat_context`]), and the
//! retrieval channel replies (C4) use ([`reply_hits`]).
//!
//! [`KnowledgeContext`] is the B1 [`PromptContext`] provider that steve-core already calls in
//! `startSession` and `answer`; the integrator only registers it. With nothing to say it returns
//! nothing, so prompts stay byte for byte what they are without the module.
//!
//! Visibility, `never` and paused sources are filtered by [`retrieve`]/[`anchor_mems`] and P3's
//! cards; nothing here builds its own conditions.

use std::collections::VecDeque;
use std::sync::Mutex;

use steve_modules::contacts::{self, ContactCard};
use steve_modules::context::{ChatCtx, ChatDone, Cite, PromptContext, SessionPrompt};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::{Contacts, Item, ItemEvidence, Members, R, Read, SessionEvidence, Store};

use crate::cite;
use crate::retrieve::{
    Hit, MEM_HREF_PREFIX, Purpose, Query, Retrieved, SEMANTIC_DOWN_NOTE, anchor_mems, record_use,
    retrieve_all,
};
use crate::state::Knowledge;
use crate::uses::{MemoryUseStage, record_memory_uses};

/// Retrieved hits per scene.
pub const SESSION_K: usize = 8;
pub const CHAT_K: usize = 6;
pub const REPLY_K: usize = 10;
/// Always-loaded personal context (`me`) character budget (L1).
pub const L1_ME_MAX_CHARS: usize = 1500;
/// Non-`me` anchor memories (item, project, people, space) character budget (L1).
pub const L1_ENTITY_MAX_CHARS: usize = 800;
/// Fixed personal-context injection budget per session, in estimated tokens.
pub const SESSION_CONTEXT_MAX_TOKENS: usize = 1200;
/// Conservative token estimate per non-ASCII (CJK) character (~1 token/char).
const TOKENS_PER_NON_ASCII_CHAR: usize = 1;
/// Conservative ASCII characters per token (~4 chars/token, ceiling division).
const ASCII_CHARS_PER_TOKEN: usize = 4;
/// The whole task-context block in a session, in characters; entries are dropped from the end beyond it.
pub const SESSION_MAX: usize = 6000;
/// One entry's text, in characters.
pub const SNIPPET_MAX: usize = 600;
/// The search text built from an item or a message, in characters.
pub const QUERY_MAX: usize = 500;

/// Deterministic conservative token estimator for prompt budget enforcement.
/// Non-ASCII (CJK) characters count at 1 token/char and ASCII at 4 chars/token (rounded up).
pub fn estimate_tokens(text: &str) -> usize {
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    non_ascii * TOKENS_PER_NON_ASCII_CHAR + ascii.div_ceil(ASCII_CHARS_PER_TOKEN)
}

fn cut_to_token_budget(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if estimate_tokens(&out) > max_tokens {
            out.pop();
            break;
        }
    }
    out
}

/// The block head, verbatim.
pub const SESSION_HEAD: &str =
    "相关知识（Steve 按这件事检索到的，可能不全；需要更多时调用 steve_knowledge_search）：";
pub const SESSION_RULES: &str = "使用规则：“仅用于推理”的内容不要原样写进任何会发给别人的东西（邮件、群消息、PR 评论）；“可转述结论”只说结论，不提来源和细节；用到哪条就在句末标 [K1]。";
/// Footnote-style numbers, cite only what was used, never invent a source.
pub const CHAT_HEAD: &str = "相关知识（Steve 按你的问题检索到的，可能不全）：";
pub const CHAT_RULES: &str = "使用规则：用到知识时在句末标 [n]，没用到就不标；知识里没有的不要编造来源。“仅用于推理”的内容不要原样写出；“可转述结论”只说结论，不提来源和细节。";
/// Heading of the contact cards in a session (layer 6).
pub const CARDS_HEAD: &str = "这件事相关的人（Steve 记得的，括号里是披露级别）：";

/// The prompt wording of a level; `never` never reaches a prompt.
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

/// The block: head, one line per hit, rules. `""` without hits.
pub fn render_snippets(head: &str, rules: &str, prefix: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut lines = vec![head.to_string()];
    lines.extend(hits.iter().map(|h| snippet_line(prefix, h)));
    lines.push(rules.to_string());
    lines.join("\n")
}

/// The text to search with: whitespace collapsed, cut to [`QUERY_MAX`] characters.
pub fn query_text(text: &str) -> String {
    text.split_whitespace()
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

/// The member who requested the item, matching the Hub's principal contract. Missing or revoked
/// requesters mean workspace service access, even in a one-person workspace; audit is not authority.
pub fn item_user(
    c: &mut impl Read,
    ws: &str,
    item_id: &str,
) -> steve_store::Result<Option<String>> {
    ItemEvidence(c).requester(ws, item_id)
}

/// The viewer an item's session (opening prompt, MCP tools) filters for.
pub fn item_viewer(c: &mut impl Read, ws: &str, item_id: &str) -> steve_store::Result<Viewer> {
    let user = item_user(c, ws, item_id)?.unwrap_or_default();
    Ok(Viewer::bare(ws, &user))
}

/// The session the opening prompt being built belongs to: steve-core creates the row just before.
pub fn latest_session(c: &mut impl Read, item_id: &str) -> ApiResult<Option<String>> {
    Ok(SessionEvidence(c).latest(item_id)?)
}

/// What an opening prompt gets: split personal [`Self::user_context`] and [`Self::task_context`],
/// their combined [`Self::text`] (`""` = nothing at all), and the numbered hits in them for
/// `kb_uses(session)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionCtx {
    /// Fixed personal context for the requester (`me` memories + address), bounded by
    /// [`L1_ME_MAX_CHARS`] and [`SESSION_CONTEXT_MAX_TOKENS`]. Delivered via native
    /// `append_system_context` or `## User Context` in the opening prompt.
    pub user_context: String,
    /// Task-specific context (contact cards, entity anchors, retrieved knowledge, semantic note).
    pub task_context: String,
    /// Combined text (`## User Context` + `task_context`, or just one when the other is empty).
    pub text: String,
    /// The search text (title + latest goal, whitespace collapsed).
    pub query: String,
    pub hits: Vec<Hit>,
    pub cards: Vec<ContactCard>,
    /// The hits were found by keywords only ([`Retrieved::semantic_down`]); `task_context` says so.
    pub semantic_down: bool,
}

/// Layers 6 and 7 for an Agent session on `item`: `me` personal context (priority-and-budget
/// bounded by [`L1_ME_MAX_CHARS`] and [`SESSION_CONTEXT_MAX_TOKENS`], workspace-filtered in
/// multi-member workspaces), contact cards of the item's people, active anchor memories about the
/// item/project/people (bounded by [`L1_ENTITY_MAX_CHARS`]), and `retrieve(title + latest goal,
/// k=8, Agent)`; all as the item's user sees them.
pub fn session_context(
    store: &Store,
    kb: &Knowledge,
    item: &Item,
    now: i64,
) -> ApiResult<SessionCtx> {
    let ws = item.workspace_id.as_str();
    crate::read_api(store, |c| {
        let viewer = item_viewer(c, ws, &item.id)?;
        let cards = contacts::cards::item_prompt_cards(store, &viewer, &item.id)?;
        session_snippets(c, kb, item, now, viewer, cards)
    })
}

/// Owner's `me` personal memory context (#573), bounded by [`L1_ME_MAX_CHARS`] and
/// [`SESSION_CONTEXT_MAX_TOKENS`], workspace-filtered in multi-member workspaces. Shared by
/// Agent session assembly ([`session_context`]) and owner-voice channel reply composition (#275).
pub fn owner_personal_mems(
    c: &mut R<'_>,
    viewer: &Viewer,
    now: i64,
) -> ApiResult<(Vec<Hit>, String)> {
    let ws = viewer.ws.as_str();
    let multi_member = Members::new(c).count(ws)? > 1;
    let me_party = if !viewer.user.is_empty() {
        Contacts::new(c).user_party(ws, &viewer.user)?
    } else {
        None
    };

    let mut me_hits = match me_party.as_deref() {
        Some(me) => anchor_mems(
            c,
            viewer,
            &[ObjRef::new("party", me)],
            L1_ME_MAX_CHARS,
            multi_member,
            now,
        )?,
        None => Vec::new(),
    };
    for (i, h) in me_hits.iter_mut().enumerate() {
        h.n = i + 1;
    }

    let address = if !multi_member && !viewer.user.is_empty() {
        Contacts::new(c)
            .internal_address(ws, &viewer.user)?
            .map(|name| {
                format!("对本人说话时称呼：{name}。对外介绍须使用对外称呼，不沿用这个昵称。")
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let build_user = |hits: &[Hit]| -> String {
        let mut parts = Vec::new();
        if !address.is_empty() {
            parts.push(address.clone());
        }
        if !hits.is_empty() {
            let mut lines: Vec<String> = hits.iter().map(|h| snippet_line("K", h)).collect();
            lines.push(SESSION_RULES.to_string());
            parts.push(lines.join("\n"));
        }
        parts.join("\n\n")
    };
    let mut user_context = build_user(&me_hits);
    while estimate_tokens(&user_context) > SESSION_CONTEXT_MAX_TOKENS && !me_hits.is_empty() {
        me_hits.pop();
        user_context = build_user(&me_hits);
    }
    if estimate_tokens(&user_context) > SESSION_CONTEXT_MAX_TOKENS {
        user_context = cut_to_token_budget(&user_context, SESSION_CONTEXT_MAX_TOKENS);
    }
    Ok((me_hits, user_context))
}

fn session_snippets(
    c: &mut R<'_>,
    kb: &Knowledge,
    item: &Item,
    now: i64,
    viewer: Viewer,
    cards: Vec<ContactCard>,
) -> ApiResult<SessionCtx> {
    let ws = item.workspace_id.as_str();
    let multi_member = Members::new(c).count(ws)? > 1;
    let me_party = if !viewer.user.is_empty() {
        Contacts::new(c).user_party(ws, &viewer.user)?
    } else {
        None
    };

    let mut entity_anchors = vec![ObjRef::new("item", &item.id)];
    if let Some(p) = item.project_id.as_deref().filter(|p| !p.is_empty()) {
        entity_anchors.push(ObjRef::new("project", p));
    }
    for k in &cards {
        if me_party.as_deref() != Some(k.party_id.as_str()) {
            entity_anchors.push(ObjRef::new("party", &k.party_id));
        }
    }
    let mut all_anchors = entity_anchors.clone();
    if let Some(ref me) = me_party {
        all_anchors.push(ObjRef::new("party", me));
    }

    let (me_hits, user_context) = owner_personal_mems(c, &viewer, now)?;

    let entity_hits = anchor_mems(
        c,
        &viewer,
        &entity_anchors,
        L1_ENTITY_MAX_CHARS,
        multi_member,
        now,
    )?;

    let query = query_text(&format!(
        "{} {}",
        item.title,
        ItemEvidence(c).latest_goal(&item.id)?.unwrap_or_default()
    ));
    let mut q = Query::new(kb.store(), &viewer, &query, Purpose::Agent);
    q.anchors = &all_anchors;
    q.k = SESSION_K;
    q.now = now;
    let found = retrieve_all(c, &q)?;
    let semantic_down = found.semantic_down;
    let mut task_hits: Vec<Hit> = merge(entity_hits, found.hits)
        .into_iter()
        .filter(|h| {
            !me_hits
                .iter()
                .any(|m| m.obj_type == h.obj_type && m.obj_id == h.obj_id && m.seq == h.seq)
        })
        .collect();
    let offset = me_hits.len();
    for (i, h) in task_hits.iter_mut().enumerate() {
        h.n = offset + i + 1;
    }

    let note = if semantic_down {
        SEMANTIC_DOWN_NOTE
    } else {
        ""
    };
    let cards_text = contacts::cards::render(&cards);
    let cards_block = if cards_text.is_empty() {
        String::new()
    } else {
        format!("{CARDS_HEAD}\n{cards_text}")
    };
    let build_task = |hits: &[Hit]| {
        [
            cards_block.clone(),
            render_snippets(SESSION_HEAD, SESSION_RULES, "K", hits),
            note.to_string(),
        ]
        .into_iter()
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
    };
    let mut task_context = build_task(&task_hits);
    while task_context.chars().count() > SESSION_MAX && !task_hits.is_empty() {
        task_hits.pop();
        task_context = build_task(&task_hits);
    }
    if task_context.chars().count() > SESSION_MAX {
        task_context = task_context.chars().take(SESSION_MAX).collect();
    }

    let hits: Vec<Hit> = me_hits.into_iter().chain(task_hits).collect();
    let text = SessionPrompt {
        user_context: user_context.clone(),
        task_context: task_context.clone(),
    }
    .into_protocol_text();
    Ok(SessionCtx {
        user_context,
        task_context,
        text,
        query,
        hits,
        cards,
        semantic_down,
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

/// Layers 6 and 7 for Steve's answer to `text` from `viewer`: active memories about “我”
/// (priority-and-budget assembled up to [`L1_ME_MAX_CHARS`]), then `retrieve(text, k=6, Chat)`;
/// numbered `[1] [2]`. In a workspace with more than one member only workspace-visible knowledge
/// is used. `None` = nothing found.
pub fn chat_context(
    c: &mut R<'_>,
    kb: &Knowledge,
    viewer: &Viewer,
    text: &str,
    now: i64,
) -> ApiResult<Option<ChatKnowledge>> {
    let query = query_text(text);
    let multi_member = Members::new(c).count(&viewer.ws)? > 1;
    let mut rules = Vec::new();
    if !viewer.user.is_empty()
        && let Some(me) = Contacts::new(c).user_party(&viewer.ws, &viewer.user)?
    {
        rules = anchor_mems(
            c,
            viewer,
            &[ObjRef::new("party", &me)],
            L1_ME_MAX_CHARS,
            multi_member,
            now,
        )?;
    }
    let mut q = Query::new(kb.store(), viewer, &query, Purpose::Chat);
    q.k = CHAT_K;
    q.now = now;
    let found = retrieve_all(c, &q)?;
    let hits = merge(rules, found.hits);
    // This context is used by the shared workspace chat. Keep its existing workspace-only
    // rule for multi-member workspaces: a member's private nickname must not enter it.
    let address = if !multi_member {
        Contacts::new(c).internal_address(&viewer.ws, &viewer.user)?
    } else {
        None
    };
    if hits.is_empty() && !found.semantic_down && address.is_none() {
        return Ok(None);
    }
    // keywords only: said even when nothing was found, so “no knowledge” is not taken as the whole
    let mut text = render_snippets(CHAT_HEAD, CHAT_RULES, "", &hits);
    if let Some(name) = address {
        text = format!("对本人说话时称呼：{name}。对外介绍使用对外称呼，不沿用内部昵称。\n{text}");
    }
    if found.semantic_down {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(SEMANTIC_DOWN_NOTE);
    }
    Ok(Some(ChatKnowledge {
        ctx: ChatCtx {
            text,
            cites: hits.iter().map(cite_of).collect(),
        },
        query,
        hits,
    }))
}

/// `cite::finish` and the reply's extra `meta`: `cites` with only what was cited.
/// Also emits `steve_memory_uses_total{consumer="chat", stage="cited"}` for cited memories.
pub fn chat_finish(answer: &str, cx: &ChatCtx) -> ChatDone {
    let f = cite::finish(answer, &cx.cites);
    let cited_mems = f
        .used
        .iter()
        .filter(|c| c.href.starts_with(MEM_HREF_PREFIX))
        .count() as u64;
    record_memory_uses("chat", MemoryUseStage::Cited, cited_mems);
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

/// Layer 7 of a channel reply: `retrieve(message + the last 3 messages, k=10, Reply)` for the
/// account owner's user. Layer 6 (cards, space summary) stays with C4; render the hits with
/// [`render_snippets`] and record them in `reply_basis`.
#[allow(clippy::too_many_arguments)]
pub fn reply_hits(
    c: &mut R<'_>,
    store: &dyn crate::port::KnowledgeStore,
    viewer: &Viewer,
    message: &str,
    recent: &[String],
    anchors: &[ObjRef],
    now: i64,
) -> steve_store::Result<Retrieved> {
    let start = recent.len().saturating_sub(3);
    let mut text = message.to_string();
    for r in &recent[start..] {
        text.push(' ');
        text.push_str(r);
    }
    let query = query_text(&text);
    let mut q = Query::new(store, viewer, &query, Purpose::Reply);
    q.anchors = anchors;
    q.k = REPLY_K;
    q.now = now;
    let found = retrieve_all(c, &q)?;
    let mem_hits = found.hits.iter().filter(|h| h.obj_type == "mem").count() as u64;
    record_memory_uses("reply", MemoryUseStage::Injected, mem_hits);
    Ok(found)
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

    fn pending(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, String, Vec<Hit>)>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, ws: &str, consumer: (&str, &str), query: &str, hits: &[Hit]) {
        if let Err(e) = self
            .store
            .write(|tx| record_use(tx, ws, consumer, query, hits))
        {
            tracing::warn!(consumer = consumer.0, id = consumer.1, "kb_uses: {e}");
        }
    }
}

impl PromptContext for KnowledgeContext {
    fn session(&self, store: &Store, _ws: &str, item: &Item) -> Option<SessionPrompt> {
        let cx = session_context(store, &self.kb, item, steve_store::now())
            .map_err(|e: ApiError| tracing::warn!(item = %item.id, "session knowledge: {e}"))
            .ok()?;
        if !cx.hits.is_empty() {
            match crate::read_api(store, |c| latest_session(c, &item.id)) {
                Ok(Some(sid)) => {
                    self.record(&item.workspace_id, ("session", &sid), &cx.query, &cx.hits)
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(item = %item.id, "session of item: {e}"),
            }
        }
        let prompt = SessionPrompt {
            user_context: cx.user_context,
            task_context: cx.task_context,
        };
        Some(prompt).filter(|p| !p.is_empty())
    }

    fn chat(&self, store: &Store, _ws: &str, viewer: &Viewer, text: &str) -> Option<ChatCtx> {
        let k = crate::read_api(store, |c| {
            chat_context(c, &self.kb, viewer, text, steve_store::now())
        })
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
        _store: &Store,
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
