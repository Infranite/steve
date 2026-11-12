//! Candidate memories: writing them with their `confirm` decision, answering them
//! (the `mem` decision handler, one at a time or in a batch), the 待确认 list, the 30-day expiry and
//! the agent tool `steve_remember`.
//!
//! A candidate is a `mems` row with `status=candidate` plus its `mem_candidates` row (origin, quote,
//! the memory it would replace). It takes effect only with a `yes` — the owner's, or, when the
//! owner's own words in their conversation say to keep it (“记住…”, “以后…”) and it replaces nothing,
//! Steve's at once (settled as a `yes` the owner can undo). An expired candidate can be restored.

use std::time::Duration;

use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::contacts::memory::{MAX_OPEN_CONFIRMS, confirm_question, record_name};
use steve_modules::contacts::people_repo::open_mem_confirms;
use steve_modules::decisions::{self, DecisionCx, DecisionHandler};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules};
use steve_store::base::mems::{self, Mem, MemPatch, NewMem};
use steve_store::base::{self, Level, ObjRef, Viewer, Visibility};
use steve_store::{Decision, Tx};

use crate::extract::Names;
use crate::service::{mem_json, prepare};
use crate::sync;

/// `subject_type` of memory confirm decisions.
pub const SUBJECT: &str = "mem";
/// Candidates kept per origin (one item completion, one day of chat, one mail thread …): few, so
/// each is worth a look; most origins give none.
pub const MAX_PER_ORIGIN: usize = 3;
/// Unanswered candidates expire after this long.
pub const EXPIRE_AFTER_MS: i64 = 30 * 24 * 3600 * 1000;
/// How often the expiry job runs.
pub const EXPIRE_EVERY: Duration = Duration::from_secs(3600);
/// Quotes are cut to this many characters (`mem_candidates.evidence`).
pub const EVIDENCE_MAX: usize = 300;
const NOUN: &str = "记忆";

/// Where candidates come from (`mem_candidates.origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    Item,
    Chat,
    Mail,
    Tool,
    Space,
}

impl OriginKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Chat => "chat",
            Self::Mail => "mail",
            Self::Tool => "tool",
            Self::Space => "space",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Item, Self::Chat, Self::Mail, Self::Tool, Self::Space]
            .into_iter()
            .find(|k| k.code() == s)
    }

    /// `mems.source_type` of what this origin writes.
    fn source_type(self, has_item: bool) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Tool if has_item => "item",
            Self::Tool => "steve",
            Self::Chat | Self::Mail | Self::Space => "message",
        }
    }
}

/// One origin of candidates: who they belong to and how they are labelled.
#[derive(Debug, Clone)]
pub struct Origin {
    pub ws: String,
    pub kind: OriginKind,
    /// `ST-12`, `chat:2026-10-31`, a thread or session id.
    pub id: String,
    /// “ST-12 交付时”, “10-31 的对话”: shown on the card and the 待确认 page.
    pub label: String,
    /// The user of the workspace the candidates are for (`created_by`; they are private to them).
    pub user: String,
    /// `steve`, or the bot that saved it (`writer`, `requested_by`).
    pub writer: String,
    /// The level a candidate gets at least (item and chat: `conclusion`, mail: `reasoning`).
    pub default_level: Level,
    /// The item behind it, if any (`source_id` of tool candidates).
    pub item_id: Option<String>,
    pub session_id: Option<String>,
}

/// A candidate ready to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCandidate {
    /// `fact` | `preference` | `rule` | `event`
    pub kind: String,
    pub text: String,
    pub about: Vec<ObjRef>,
    pub level: Level,
    pub evidence: String,
    /// A confirmed memory this one replaces once confirmed.
    pub supersedes: Option<String>,
    /// Why it is worth keeping, for the card (empty from `steve_remember`).
    pub why: String,
    /// Where it applies, for the card.
    pub scope: String,
    /// The owner's own words say to keep it (“记住…”, “以后…”): it is confirmed at once, unless it
    /// replaces a memory (that still asks). See [`owner_said`].
    pub owner_said: bool,
}

/// Words with which the owner says to keep something.
pub const SAY_KEEP: [&str; 2] = ["记住", "以后"];

/// `evidence` (quotes joined with “ / ”) is all the owner's own words in `text`, the lines that start
/// with `owner_head` (`我（` in a day of chat), and one quote says to keep it ([`SAY_KEEP`]).
pub fn owner_said(text: &str, owner_head: &str, evidence: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let owner: Vec<String> = text
        .lines()
        .filter(|l| l.trim_start().starts_with(owner_head))
        .map(norm)
        .collect();
    let quotes: Vec<&str> = evidence
        .split(" / ")
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .collect();
    !quotes.is_empty()
        && quotes.iter().all(|q| {
            let pieces: Vec<String> = q
                .split(['…', '⋯'])
                .flat_map(|p| p.split("..."))
                .map(norm)
                .filter(|p| p.chars().count() >= 2)
                .collect();
            !pieces.is_empty()
                && owner
                    .iter()
                    .any(|l| pieces.iter().all(|p| l.contains(p.as_str())))
        })
        && quotes
            .iter()
            .any(|q| SAY_KEEP.iter().any(|w| q.contains(w)))
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Writes a candidate: the private `mems` row, its `about` rows, its `mem_candidates` row and, while
/// fewer than 20 memory decisions are open in the workspace, a `confirm` decision. Publishes `mm`
/// (and `decision`). Returns the memory.
pub fn propose(tx: &Tx, o: &Origin, c: &NewCandidate) -> ApiResult<Mem> {
    let source_id = match o.kind {
        OriginKind::Tool => o.item_id.clone().or_else(|| o.session_id.clone()),
        _ => Some(o.id.clone()).filter(|s| !s.is_empty()),
    };
    let mem = mems::create(
        tx,
        &o.ws,
        &NewMem {
            kind: c.kind.clone(),
            text: c.text.clone(),
            source_type: o.kind.source_type(o.item_id.is_some()).into(),
            source_id,
            source_label: o.label.clone(),
            visibility: Visibility::Private,
            level: c.level,
            status: "candidate".into(),
            writer: o.writer.clone(),
            created_by: o.user.clone(),
            about: c.about.clone(),
            ..Default::default()
        },
    )?;
    let evidence = cut(&c.evidence, EVIDENCE_MAX);
    tx.prepare_cached(
        "INSERT OR REPLACE INTO mem_candidates (mem_id, workspace_id, origin, origin_id, origin_label, evidence, supersedes, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        mem.id,
        o.ws,
        o.kind.code(),
        o.id,
        o.label,
        evidence,
        c.supersedes,
        mem.created
    ])?;
    let mut mem = mem;
    let auto = c.owner_said && c.supersedes.is_none();
    if auto || open_mem_confirms(tx, &o.ws)? < MAX_OPEN_CONFIRMS {
        let mut draft = format!("类型：{} · {}", record_name(&c.kind), kind_name(&c.kind));
        for (head, v) in [(WHY, &c.why), (SCOPE, &c.scope)] {
            if !v.trim().is_empty() {
                draft.push_str(&format!("\n{head}{}", v.trim()));
            }
        }
        draft.push_str(&format!("\n来源：{}", o.label));
        if !evidence.is_empty() {
            draft.push_str(&format!("\n原话：{evidence}"));
        }
        if let Some(id) = c.supersedes.as_deref()
            && let Some(old) = mems::get(tx, id)?
        {
            draft.push_str(&format!("\n会替换：{}", old.text));
        }
        let mut nd = decisions::confirm(
            &o.ws,
            SUBJECT,
            &mem.id,
            &confirm_question(&c.kind, &c.text),
            &o.writer,
        );
        nd.draft = Some(draft);
        nd.session_id = o.session_id.clone();
        let d = base::decisions::open(tx, &nd)?;
        mem = mems::patch(
            tx,
            &mem.id,
            &MemPatch {
                decision_id: Some(Some(d.id)),
                ..Default::default()
            },
        )?
        .ok_or_else(|| ApiError::not_found(NOUN))?;
    }
    if auto {
        // you said so yourself: kept now, the card settled as yours, undoable like your own yes
        answer(tx, &o.ws, "steve", &mem.id, "yes", "")?;
        if let Some(d) = mem.decision_id.as_deref() {
            base::decisions::settle(
                tx,
                d,
                &json!({"choice": "yes", "auto": "owner_said"}),
                "resolved",
                &json!({"auto": true}),
            )?;
        }
        base::audit(
            tx,
            &o.ws,
            "steve",
            "mm.auto_confirm",
            &mem.id,
            &json!({"origin": o.kind.code(), "origin_id": o.id}),
        )?;
        mem = mems::get(tx, &mem.id)?.ok_or_else(|| ApiError::not_found(NOUN))?;
    }
    base::audit(
        tx,
        &o.ws,
        &o.writer,
        "mm.propose",
        &mem.id,
        &json!({"origin": o.kind.code(), "origin_id": o.id, "kind": c.kind}),
    )?;
    Ok(mem)
}

/// Line heads of the confirm card's draft; the 待确认 list reads them back.
const WHY: &str = "为什么值得保留：";
const SCOPE: &str = "适用：";

fn kind_name(kind: &str) -> &'static str {
    match kind {
        "preference" => "偏好",
        "rule" => "规则",
        "event" => "事件",
        "summary" => "摘要",
        _ => "事实",
    }
}

/// `(why, scope)` written on a candidate's decision (empty without one, or from before ST-41).
fn reasons(c: &Connection, decision: Option<&str>) -> ApiResult<(String, String)> {
    let Some(d) = decision else {
        return Ok(Default::default());
    };
    let draft = base::decisions::get(c, d)?
        .map(|d| d.draft)
        .unwrap_or_default();
    let line = |head: &str| {
        draft
            .lines()
            .find_map(|l| l.strip_prefix(head))
            .unwrap_or_default()
            .to_string()
    };
    Ok((line(WHY), line(SCOPE)))
}

/// A `mem_candidates` row: `(origin, origin_label, evidence, supersedes)`.
type CandidateRow = (String, String, String, Option<String>);

fn candidate_row(c: &Connection, mem_id: &str) -> ApiResult<Option<CandidateRow>> {
    Ok(c.prepare_cached(
        "SELECT origin, origin_label, evidence, supersedes FROM mem_candidates WHERE mem_id = ?",
    )?
    .query_row([mem_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()?)
}

/// The candidate memory `id` of `ws`, still waiting for an answer.
fn pending(c: &Connection, ws: &str, id: &str) -> ApiResult<Mem> {
    match mems::get(c, id)? {
        Some(m) if m.workspace_id == ws && m.status == "candidate" => Ok(m),
        Some(m) if m.workspace_id == ws => Err(ApiError::conflict(steve_i18n::t!(
            "knowledge.mem.already_handled"
        ))),
        _ => Err(ApiError::not_found(NOUN)),
    }
}

/// Answers a candidate: `yes` confirms it (with the edited wording `text`, if any) and makes the memory
/// it supersedes `stale`; `no` deletes it with its candidate row. Re-indexes what changed.
/// The decision itself is the caller's to settle.
pub fn answer(tx: &Tx, ws: &str, actor: &str, id: &str, choice: &str, text: &str) -> ApiResult<()> {
    let mem = pending(tx, ws, id)?;
    let now = steve_store::now();
    match choice {
        "yes" => {
            let text = text.trim();
            if text.chars().count() > 2000 {
                return Err(ApiError::invalid("text"));
            }
            mems::patch(
                tx,
                &mem.id,
                &MemPatch {
                    text: (!text.is_empty()).then(|| text.to_string()),
                    status: Some("confirmed".into()),
                    ..Default::default()
                },
            )?;
            sync::sync_mem(tx, &mem.id, now)?;
            let supersedes = candidate_row(tx, &mem.id)?.and_then(|r| r.3);
            if let Some(old) = supersedes
                && let Some(o) = mems::get(tx, &old)?
                && o.workspace_id == ws
                && o.status == "confirmed"
            {
                mems::patch(
                    tx,
                    &o.id,
                    &MemPatch {
                        status: Some("stale".into()),
                        ..Default::default()
                    },
                )?;
                sync::sync_mem(tx, &o.id, now)?;
            }
            base::audit(
                tx,
                ws,
                actor,
                "mm.confirm",
                &mem.id,
                &json!({"edited": !text.is_empty()}),
            )?;
        }
        "no" => {
            tx.prepare_cached("DELETE FROM mem_candidates WHERE mem_id = ?")?
                .execute([&mem.id])?;
            mems::delete(tx, &mem.id)?;
            sync::sync_mem(tx, &mem.id, now)?;
            base::audit(tx, ws, actor, "mm.reject", &mem.id, &json!({}))?;
        }
        _ => return Err(ApiError::invalid("choice")),
    }
    Ok(())
}

/// The `DecisionHandler` of `subject_type = "mem"` (moved here from contacts P4). Register it
/// with `registry.decision(SUBJECT, Box::new(MemDecisions))`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MemDecisions;

impl DecisionHandler for MemDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        answer(tx, cx.ws, cx.actor, &d.subject_id, cx.choice, cx.text)?;
        Ok(decisions::choice_result(cx))
    }

    /// Undoing a `yes` puts the candidate back and restores the memory it replaced; a `no` deleted
    /// the memory, so it cannot be undone.
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if d.result.get("choice").and_then(Value::as_str) != Some("yes") {
            return Err(ApiError::rule(steve_i18n::t!(
                "knowledge.mem.deleted_no_undo"
            )));
        }
        let Some(mem) = mems::get(tx, &d.subject_id)? else {
            return Err(ApiError::rule(steve_i18n::t!(
                "knowledge.mem.deleted_no_undo"
            )));
        };
        let now = steve_store::now();
        mems::patch(
            tx,
            &mem.id,
            &MemPatch {
                status: Some("candidate".into()),
                ..Default::default()
            },
        )?;
        sync::sync_mem(tx, &mem.id, now)?;
        if let Some(old) = candidate_row(tx, &mem.id)?.and_then(|r| r.3)
            && mems::get(tx, &old)?.is_some_and(|o| o.status == "stale")
        {
            mems::patch(
                tx,
                &old,
                &MemPatch {
                    status: Some("confirmed".into()),
                    ..Default::default()
                },
            )?;
            sync::sync_mem(tx, &old, now)?;
        }
        Ok(())
    }

    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        stale_candidate(tx, &d.subject_id)
    }
}

/// An unanswered candidate becomes `stale` (the 30-day rule); anything else is left alone.
fn stale_candidate(tx: &Tx, id: &str) -> ApiResult<()> {
    if mems::get(tx, id)?.is_some_and(|m| m.status == "candidate") {
        mems::patch(
            tx,
            id,
            &MemPatch {
                status: Some("stale".into()),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- REST

/// `GET /kb/candidates` and `POST /kb/candidates/resolve`, merged into [`crate::routes`].
pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/kb/candidates", get(list_route))
        .route("/kb/candidates/resolve", post(resolve_route))
        .route("/kb/candidates/expired", get(expired_route))
        .route("/kb/candidates/{id}/restore", post(restore_route))
}

async fn expired_route(
    axum::extract::State(m): axum::extract::State<Modules>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| expired(m, &who).map(Json)).await
}

async fn restore_route(
    axum::extract::State(m): axum::extract::State<Modules>,
    who: Caller,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| restore(m, &who, &id).map(Json)).await
}

async fn list_route(
    axum::extract::State(m): axum::extract::State<Modules>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| list(m, &who).map(Json)).await
}

async fn resolve_route(
    axum::extract::State(m): axum::extract::State<Modules>,
    who: Caller,
    Body(b): Body<ResolveBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| resolve_many(m, &who, &b).map(Json))
        .await
}

/// `Candidate[]` of the caller, newest first.
pub fn list(m: &Modules, who: &Caller) -> ApiResult<Value> {
    prepare(m, who)?;
    let viewer = m.viewer(who)?;
    m.store
        .reader(|c| Ok(Value::Array(candidates_of(c, &viewer)?)))
}

fn candidates_of(c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Value>> {
    let scope = viewer.sql("m");
    let sql = format!(
        "SELECT m.id FROM mem_candidates k JOIN mems m ON m.id = k.mem_id
         WHERE k.workspace_id = ? AND m.workspace_id = ? AND m.status = 'candidate' AND {}
         ORDER BY k.created DESC, m.created DESC, m.id",
        scope.sql
    );
    let mut p: Vec<rusqlite::types::Value> =
        vec![viewer.ws.clone().into(), viewer.ws.clone().into()];
    p.extend(scope.params);
    let ids: Vec<String> = c
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(p), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(mem) = mems::get(c, &id)? else {
            continue;
        };
        let Some((origin, label, evidence, sup)) = candidate_row(c, &id)? else {
            continue;
        };
        let supersedes = match sup {
            Some(s) => mems::visible(c, viewer, &s)?.map(|o| mem_json(c, &o)),
            None => None,
        };
        let (why, scope) = reasons(c, mem.decision_id.as_deref())?;
        out.push(json!({
            "mem": mem_json(c, &mem),
            "record": record_name(&mem.kind),
            "why": why,
            "scope": scope,
            "origin": origin,
            "origin_label": label,
            "evidence": evidence,
            "supersedes": supersedes,
            "decision_id": mem.decision_id,
        }));
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveBody {
    pub ids: Vec<String>,
    pub choice: String,
}

/// `POST /kb/candidates/resolve {ids, choice}`: every id must be a memory the caller can see
/// (`404 没有这个记忆` before anything changes). Candidates with an open decision are answered through
/// the decision dispatch (the same path as `POST /api/decisions/:id`, so audit and decision state stay
/// consistent); the rest directly. Ones already answered are skipped. `{ok:true, done:n}`.
pub fn resolve_many(m: &Modules, who: &Caller, b: &ResolveBody) -> ApiResult<Value> {
    Check::new()
        .that("ids", (1..=50).contains(&b.ids.len()))
        .one_of("choice", &b.choice, &["yes", "no"])
        .done()?;
    let viewer = m.viewer(who)?;
    let found: Vec<Mem> = m.store.reader(|c| {
        b.ids
            .iter()
            .map(|id| mems::visible(c, &viewer, id)?.ok_or_else(|| ApiError::not_found(NOUN)))
            .collect::<ApiResult<Vec<_>>>()
    })?;
    let mut done = 0;
    for mem in found {
        if mem.status != "candidate" {
            continue;
        }
        let open = match &mem.decision_id {
            Some(d) => m
                .store
                .reader(|c| base::decisions::get(c, d))?
                .filter(|d| d.status == "open")
                .map(|d| d.id),
            None => None,
        };
        if let Some(dec) = open
            && let Some(r) = decisions::dispatch(m, who, &dec, &b.choice, "")
        {
            r?;
            done += 1;
            continue;
        }
        // No open decision (or no handler registered): answer directly, settling any decision too.
        m.store.transaction(|tx| {
            answer(tx, &who.ws, &who.me, &mem.id, &b.choice, "")?;
            if let Some(d) = &mem.decision_id {
                base::decisions::settle(
                    tx,
                    d,
                    &json!({"choice": b.choice}),
                    "resolved",
                    &Value::Null,
                )?;
            }
            Ok::<_, ApiError>(())
        })?;
        done += 1;
    }
    Ok(json!({"ok": true, "done": done}))
}

// ---------------------------------------------------------------- expiry

/// Candidates of the caller that expired unanswered (and were not confirmed since), newest first:
/// `[{mem, origin_label, evidence, expired}]`, at most 50.
pub fn expired(m: &Modules, who: &Caller) -> ApiResult<Value> {
    prepare(m, who)?;
    let viewer = m.viewer(who)?;
    m.store.reader(|c| {
        let scope = viewer.sql("m");
        let sql = format!(
            "SELECT m.id, max(a.ts) FROM mems m JOIN mem_candidates k ON k.mem_id = m.id
               JOIN audit a ON a.workspace_id = m.workspace_id AND a.target = m.id AND a.action = 'mm.expire'
              WHERE m.workspace_id = ? AND m.status = 'stale' AND {}
                AND NOT EXISTS (SELECT 1 FROM audit b WHERE b.target = m.id AND b.action = 'mm.confirm' AND b.id > a.id)
              GROUP BY m.id ORDER BY max(a.ts) DESC LIMIT 50",
            scope.sql
        );
        let mut p: Vec<rusqlite::types::Value> = vec![viewer.ws.clone().into()];
        p.extend(scope.params);
        let rows: Vec<(String, i64)> = c
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(p), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, at) in rows {
            let (Some(mem), Some((_, label, evidence, _))) = (mems::get(c, &id)?, candidate_row(c, &id)?) else {
                continue;
            };
            out.push(json!({ "mem": mem_json(c, &mem), "origin_label": label, "evidence": evidence, "expired": at }));
        }
        Ok(Value::Array(out))
    })
}

/// `POST /kb/candidates/:id/restore`: an expired candidate waits for an answer again (for another 30
/// days); `{ok:true}`. Anything else that is not a candidate any more: `409`.
pub fn restore(m: &Modules, who: &Caller, id: &str) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    let listed = expired(m, who)?;
    let ok = listed
        .as_array()
        .is_some_and(|xs| xs.iter().any(|x| x["mem"]["id"] == id));
    m.store.transaction(|tx| {
        if mems::visible(tx, &viewer, id)?.is_none() {
            return Err(ApiError::not_found(NOUN));
        }
        if !ok {
            return Err(ApiError::conflict(steve_i18n::t!(
                "knowledge.mem.already_handled"
            )));
        }
        let now = steve_store::now();
        mems::patch(
            tx,
            id,
            &MemPatch {
                status: Some("candidate".into()),
                ..Default::default()
            },
        )?;
        tx.prepare_cached("UPDATE mem_candidates SET created = ? WHERE mem_id = ?")?
            .execute(params![now, id])?;
        base::audit(tx, &who.ws, &who.me, "mm.restore", id, &json!({}))?;
        Ok(json!({"ok": true}))
    })
}

/// `kb.candidates_expire`: candidates older than 30 days at `now` become `stale`, their open
/// decisions `expired`. Returns how many.
pub fn expire(m: &Modules, now: i64) -> ApiResult<usize> {
    let wss: Vec<String> = m.store.reader(|c| {
        Ok::<_, ApiError>(
            c.prepare("SELECT id FROM workspaces ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?,
        )
    })?;
    let mut n = 0;
    for ws in wss {
        let old = m
            .store
            .reader(|c| mems::candidates_before(c, &ws, now - EXPIRE_AFTER_MS))?;
        for (id, dec) in old {
            if let Some(d) = dec {
                decisions::expire(m, &d)?;
            }
            m.store.transaction(|tx| {
                stale_candidate(tx, &id)?;
                base::audit(tx, &ws, "steve", "mm.expire", &id, &json!({}))?;
                Ok::<_, ApiError>(())
            })?;
            n += 1;
        }
    }
    Ok(n)
}

/// Starts the hourly expiry job.
pub fn spawn_expirer(m: Modules) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EXPIRE_EVERY);
        loop {
            tick.tick().await;
            let now = m.clock.now();
            match m.blocking(move |m| expire(m, now)).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("kb.candidates_expire: {n} expired"),
                Err(e) => tracing::warn!("kb.candidates_expire: {e}"),
            }
        }
    })
}

// ---------------------------------------------------------------- steve_remember

pub const REMEMBER: &str = "steve_remember";
/// The answer of `steve_remember` (verbatim; does not end the turn).
pub const SAVED: &str = "Saved as a candidate; the user will confirm it.";
const TOOL_KINDS: [&str; 3] = ["fact", "preference", "rule"];

/// The tool definition (`{name, description, inputSchema}`, like steve-mcp's `tools.json`).
pub fn remember_tool() -> Value {
    json!({
        "name": REMEMBER,
        "description": "Save something that will matter beyond this task: a rule or preference the user said holds from now on (\"以后…\", \"每次…\"), or a verified fact reusable in other work. Not this task's requirements, progress, decisions or results — those stay in the item. It becomes a candidate the user confirms.",
        "inputSchema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "kind": { "type": "string", "enum": TOOL_KINDS },
                "about": { "type": "string", "description": "a person, organization, project or bot, by exact name or id" }
            },
            "required": ["text", "kind"]
        }
    })
}

/// `steve_remember`: no model call, the text becomes a candidate of the user the
/// agent works for. `Ok` is the result text, `Err` the `isError` text. Synchronous: call it from a
/// blocking task.
pub fn remember(m: &Modules, cx: &McpCx, args: &Value) -> Result<String, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let mut issues = Vec::new();
    if s("text").is_none() {
        issues.push("Invalid input: expected string at text".to_string());
    }
    match s("kind") {
        Some(k) if TOOL_KINDS.contains(&k) => {}
        _ => issues.push(
            "Invalid option: expected one of \"fact\"|\"preference\"|\"rule\" at kind".to_string(),
        ),
    }
    if args
        .get("about")
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        issues.push("Invalid input: expected string at about".to_string());
    }
    if !issues.is_empty() {
        return Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {REMEMBER}: {}",
            issues.join("; ")
        ));
    }
    let text = s("text").unwrap_or_default().trim().to_string();
    let n = text.chars().count();
    if n == 0 || n > 2000 {
        return Err("text must be 1 to 2000 characters.".into());
    }
    let kind = s("kind").unwrap_or("fact").to_string();
    let about = s("about").map(str::trim).filter(|a| !a.is_empty());
    let who = Caller {
        ws: cx.ws.clone(),
        me: cx.user.clone(),
    };
    prepare(m, &who).map_err(|e| e.to_string())?;
    let viewer = m.viewer(&who).map_err(|e| e.to_string())?;
    let origin = Origin {
        ws: cx.ws.clone(),
        kind: OriginKind::Tool,
        id: cx
            .item_id
            .clone()
            .or_else(|| cx.session_id.clone())
            .unwrap_or_default(),
        label: match &cx.item_id {
            Some(i) => format!("{i} 的会话"),
            None => "Agent 会话".into(),
        },
        user: cx.user.clone(),
        writer: cx.bot_id.clone(),
        default_level: Level::Conclusion,
        item_id: cx.item_id.clone(),
        session_id: cx.session_id.clone(),
    };
    m.store
        .transaction(|tx| {
            let about = match about {
                None => Vec::new(),
                Some(a) => {
                    let names = Names::load(tx, &viewer, None)?;
                    match names.resolve(a) {
                        Some(r) => vec![r],
                        None => {
                            return Err(ApiError::rule(format!(
                                "No person, organization, project or bot is called \"{a}\". Pass the exact name or id."
                            )));
                        }
                    }
                }
            };
            propose(
                tx,
                &origin,
                &NewCandidate {
                    kind: kind.clone(),
                    text: text.clone(),
                    about,
                    level: Level::Conclusion,
                    evidence: String::new(),
                    supersedes: None,
                    why: String::new(),
                    scope: String::new(),
                    owner_said: false,
                },
            )
        })
        .map(|_| SAVED.to_string())
        .map_err(|e: ApiError| e.to_string())
}
