//! Legacy candidate memory inspection routes, `Origin` types, and the `steve_remember` tool
//! entry point (which now routes directly through [`crate::ingest`] with zero Inbox confirmation
//! cards).

use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::contacts::memory::record_name;
use steve_modules::decisions::{self, DecisionCx, DecisionHandler, DecisionOutcome};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules};
use steve_store::base::mems::{Mem, MemPatch};
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::knowledge::{
    CandidateInfo, CandidateRows, Memories, OriginClass, SourceTrustTier,
};
use steve_store::{Decision, DecisionRows, Read, W};

use crate::service::{mem_json_for, prepare};
use crate::state::Knowledge;
use crate::sync;

/// `subject_type` of memory confirm decisions.
pub const SUBJECT: &str = "mem";
/// Candidates kept per origin (one item completion, one day of chat, one mail thread …): few, so
/// each is worth a look; most origins give none.
pub const MAX_PER_ORIGIN: usize = 3;
/// Unanswered candidates expire after this long.
pub const EXPIRE_AFTER_MS: i64 = 30 * 24 * 3600 * 1000;
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
}

/// One origin of extracted memories/proposals: who they belong to and their trust provenance.
#[derive(Debug, Clone)]
pub struct Origin {
    pub ws: String,
    pub kind: OriginKind,
    /// `ST-12`, `chat:2026-10-31`, a thread or session id.
    pub id: String,
    /// “ST-12 交付时”, “10-31 的对话”: provenance label.
    pub label: String,
    /// The user of the workspace the memories belong to (`created_by`; private by default).
    pub user: String,
    /// `steve`, or the bot that saved it (`writer`, `requested_by`).
    pub writer: String,
    /// Minimum disclosure level enforced on writes from this origin (automatic writes clamp >= `reasoning`).
    pub default_level: Level,
    /// The item behind it, if any.
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    /// Deterministic origin class for confidence & fast-path/buffer routing.
    pub origin_class: OriginClass,
    /// Deterministic source trust tier (Tier 3 external is blocked from writing `me`/global rules).
    pub source_tier: SourceTrustTier,
}

/// A validated extraction item ready to pass into [`crate::ingest::ingest_in`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewCandidate {
    /// `fact` | `preference` | `rule` | `event`
    pub kind: String,
    pub text: String,
    pub about: Vec<ObjRef>,
    pub level: Level,
    pub evidence: String,
    /// A memory this one supersedes/updates, if any.
    pub supersedes: Option<String>,
    /// Why this changes agent behavior in another different task.
    pub why: String,
    /// Where it applies (`scope_hint`).
    pub scope: String,
    /// Hard admission flag: must be `true` to pass ingestion (default NOOP).
    pub changes_future_behavior: bool,
    /// Effective origin class (may be refined per-item, e.g. OwnerRework vs ImplicitPattern).
    pub origin_class: OriginClass,
    /// Effective trust tier (e.g. Tier3External when quote comes from `[EXTERNAL_RECEIVED]`).
    pub source_tier: SourceTrustTier,
    /// Whether the owner explicitly stated a standing rule/correction.
    pub is_explicit_rule: bool,
}

/// Line heads of legacy confirm card drafts (retained read-only for pre-141 rows).
const WHY: &str = "为什么值得保留：";
const SCOPE: &str = "适用：";

/// `(why, scope)` written on a candidate's decision (empty without one, or from before ST-41).
fn reasons(c: &mut impl Read, decision: Option<&str>) -> ApiResult<(String, String)> {
    let Some(d) = decision else {
        return Ok(Default::default());
    };
    let draft = DecisionRows(c).get(d)?.map(|d| d.draft).unwrap_or_default();
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
fn candidate_row(c: &mut impl Read, id: &str) -> ApiResult<Option<CandidateInfo>> {
    Ok(CandidateRows(c).info(id)?)
}

/// The candidate memory `id` of `ws`, still waiting for an answer.
fn pending(c: &mut impl Read, ws: &str, id: &str) -> ApiResult<Mem> {
    match Memories(c).get(id)? {
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
pub fn answer(
    tx: &mut W<'_>,
    ws: &str,
    actor: &str,
    id: &str,
    choice: &str,
    text: &str,
) -> ApiResult<()> {
    let mem = pending(tx, ws, id)?;
    let now = steve_store::now();
    match choice {
        "yes" => {
            let text = text.trim();
            if text.chars().count() > 2000 {
                return Err(ApiError::invalid("text"));
            }
            Memories(tx).patch(
                &mem.id,
                &MemPatch {
                    text: (!text.is_empty()).then(|| text.to_string()),
                    status: Some("confirmed".into()),
                    ..Default::default()
                },
            )?;
            sync::sync_mem(tx, &mem.id, now)?;
            for old in replaced(tx, &mem.id)? {
                let Some(o) = Memories(tx).get(&old)? else {
                    continue;
                };
                if o.workspace_id != ws || o.status != "confirmed" {
                    continue;
                }
                Memories(tx).patch(
                    &o.id,
                    &MemPatch {
                        status: Some("stale".into()),
                        ..Default::default()
                    },
                )?;
                sync::sync_mem(tx, &o.id, now)?;
            }
            tx.audit(
                ws,
                actor,
                "mm.confirm",
                &mem.id,
                &json!({"edited": !text.is_empty()}),
            )?;
        }
        "no" => {
            CandidateRows(tx).remove(&mem.id)?;
            Memories(tx).delete(&mem.id)?;
            sync::sync_mem(tx, &mem.id, now)?;
            tx.audit(ws, actor, "mm.reject", &mem.id, &json!({}))?;
        }
        _ => return Err(ApiError::invalid("choice")),
    }
    Ok(())
}

/// The confirmed memories a candidate replaces once confirmed: its `supersedes`, and every memory a
/// merge names (`mem_supersedes`).
fn replaced(c: &mut impl Read, id: &str) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = candidate_row(c, id)?
        .and_then(|r| r.3)
        .into_iter()
        .collect();
    let more = CandidateRows(c).supersedes(id)?;
    for x in more {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    Ok(out)
}

/// The `DecisionHandler` of `subject_type = "mem"` (moved here from contacts P4). Register it
/// with `registry.decision(SUBJECT, Box::new(MemDecisions))`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MemDecisions;

impl DecisionHandler for MemDecisions {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        answer(tx, cx.ws, cx.actor, &d.subject_id, cx.choice, cx.text)?;
        Ok(decisions::choice_result(cx).into())
    }

    /// Undoing a `yes` puts the candidate back and restores the memory it replaced; a `no` deleted
    /// the memory, so it cannot be undone.
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        undo_confirmation(tx, d)
    }

    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        stale_candidate(tx, &d.subject_id)
    }
}

fn undo_confirmation(tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
    if d.result.get("choice").and_then(Value::as_str) != Some("yes") {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.mem.deleted_no_undo"
        )));
    }
    let Some(mem) = Memories(tx).get(&d.subject_id)? else {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.mem.deleted_no_undo"
        )));
    };
    let now = steve_store::now();
    Memories(tx).patch(
        &mem.id,
        &MemPatch {
            status: Some("candidate".into()),
            ..Default::default()
        },
    )?;
    sync::sync_mem(tx, &mem.id, now)?;
    for old in replaced(tx, &mem.id)? {
        if !Memories(tx).get(&old)?.is_some_and(|o| o.status == "stale") {
            continue;
        }
        Memories(tx).patch(
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

/// An unanswered candidate becomes `stale` (the 30-day rule); anything else is left alone.
fn stale_candidate(tx: &mut W<'_>, id: &str) -> ApiResult<()> {
    if Memories(tx)
        .get(id)?
        .is_some_and(|m| m.status == "candidate")
    {
        Memories(tx).patch(
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
    crate::read_api(&m.store, |c| Ok(Value::Array(candidates_of(c, &viewer)?)))
}

fn candidates_of(c: &mut impl Read, viewer: &Viewer) -> ApiResult<Vec<Value>> {
    let ids = CandidateRows(c).pending(viewer)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(mem) = Memories(c).get(&id)? else {
            continue;
        };
        let Some((origin, label, evidence, sup)) = candidate_row(c, &id)? else {
            continue;
        };
        let supersedes = match sup {
            Some(s) => Memories(c)
                .visible(viewer, &s)?
                .map(|o| mem_json_for(c, viewer, &o)),
            None => None,
        };
        let (why, scope) = reasons(c, mem.decision_id.as_deref())?;
        out.push(json!({
            "mem": mem_json_for(c, viewer, &mem),
            "record": match record_name(&mem.kind) {
                "长期记忆" => steve_i18n::t!("knowledge.record.memory"),
                _ => steve_i18n::t!("knowledge.record.entry"),
            },
            "why": why,
            "scope": scope,
            "origin": origin,
            "origin_label": crate::sources::label_shown(&label),
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
    let found: Vec<Mem> = crate::read_api(&m.store, |c| {
        b.ids
            .iter()
            .map(|id| {
                Memories(c)
                    .visible(&viewer, id)?
                    .ok_or_else(|| ApiError::not_found(NOUN))
            })
            .collect::<ApiResult<Vec<_>>>()
    })?;
    let mut done = 0;
    for mem in found {
        if mem.status != "candidate" {
            continue;
        }
        m.store.write(|tx| {
            if let Some(id) = &mem.decision_id {
                let d = DecisionRows(tx)
                    .get(id)?
                    .ok_or_else(|| ApiError::not_found("决定"))?;
                decisions::resolve(m, who, id, d.revision, &b.choice, "")?;
            } else {
                answer(tx, &who.ws, &who.me, &mem.id, &b.choice, "")?;
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
    crate::read_api(&m.store, |c| {
        let rows = CandidateRows(c).expired(&viewer, 50)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, at) in rows {
            let (Some(mem), Some((_, label, evidence, _))) =
                (Memories(c).get(&id)?, candidate_row(c, &id)?)
            else {
                continue;
            };
            out.push(json!({ "mem": mem_json_for(c, &viewer, &mem), "origin_label": crate::sources::label_shown(&label), "evidence": evidence, "expired": at }));
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
    m.store.write(|tx| {
        let mem = Memories(tx)
            .visible(&viewer, id)?
            .ok_or_else(|| ApiError::not_found(NOUN))?;
        if !ok || mem.status != "stale" {
            return Err(ApiError::conflict(steve_i18n::t!(
                "knowledge.mem.already_handled"
            )));
        }
        let previous = mem
            .decision_id
            .as_deref()
            .map(|id| DecisionRows(tx).get(id))
            .transpose()?
            .flatten()
            .filter(|d| {
                d.workspace_id == who.ws
                    && d.subject_type == SUBJECT
                    && d.subject_id == mem.id
                    && d.status == steve_types::DecisionStatus::Expired
            });
        let now = steve_store::now();
        let decision_id = if let Some(d) = previous {
            let mut spec = decisions::confirm(&who.ws, SUBJECT, id, &d.question, &d.requested_by);
            spec.draft = Some(d.draft);
            spec.session_id = d.session_id;
            spec.source = Some(steve_store::DecisionSource::Revision { revision: d.id });
            spec.wait = steve_types::WaitContract::bounded(
                steve_types::WaitOwner::Owner,
                now,
                EXPIRE_AFTER_MS,
                steve_types::WaitTimeoutAction::Close,
            )
            .ok();
            Some(DecisionRows(tx).open(&spec)?.id)
        } else {
            None
        };
        Memories(tx).patch(
            id,
            &MemPatch {
                status: Some("candidate".into()),
                decision_id: Some(decision_id),
                ..Default::default()
            },
        )?;
        CandidateRows(tx).restore(id, now)?;
        tx.audit(&who.ws, &who.me, "mm.restore", id, &json!({}))?;
        Ok(json!({"ok": true}))
    })
}

/// The first candidate deadline, including candidates without a confirmation card.
pub fn next_expiry(r: &mut impl Read) -> steve_store::Result<Option<i64>> {
    Ok(Memories(r)
        .first_candidate_at()?
        .map(|at| at + EXPIRE_AFTER_MS))
}

/// Settle a bounded transaction batch of candidates that have reached their deadline.
pub fn expire(m: &Modules, now: i64) -> ApiResult<usize> {
    m.store.write(|tx| {
        let due = Memories(tx).due_candidates(now - EXPIRE_AFTER_MS, 64)?;
        for (id, ws, decision) in &due {
            if let Some(id) = decision {
                decisions::expire(m, id)?;
            }
            stale_candidate(tx, id)?;
            tx.audit(ws, "steve", "mm.expire", id, &json!({}))?;
        }
        Ok(due.len())
    })
}

// ---------------------------------------------------------------- steve_remember

pub const REMEMBER: &str = "steve_remember";
const TOOL_KINDS: [&str; 3] = ["fact", "preference", "rule"];

/// The tool definition (`{name, description, inputSchema}`, like steve-mcp's `tools.json`).
pub fn remember_tool() -> Value {
    json!({
        "name": REMEMBER,
        "description": "Save personal context that will change how an agent behaves in another different task: a preference or rule the user explicitly stated, or a stable fact about a person, organization or project (`about` is required for kind=fact). Do not save one-off task records, troubleshooting notes or repository engineering details here. Writes silently to the personal context Markdown files (respecting the folder write_mode) without opening an Inbox confirmation card.",
        "inputSchema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "kind": { "type": "string", "enum": TOOL_KINDS },
                "about": { "type": "string", "description": "a person, organization, team or project, by exact name or id (required when kind is fact)" },
                "scope": { "type": "string", "description": "optional scope where this rule/preference applies" },
                "why": { "type": "string", "description": "optional reason why this changes future agent behavior" }
            },
            "required": ["text", "kind"]
        }
    })
}

/// `steve_remember`: unified single-pipeline silent memory write via [`crate::ingest::remember_tool`].
pub fn remember(m: &Modules, kb: &Knowledge, cx: &McpCx, args: &Value) -> Result<String, String> {
    crate::ingest::remember_tool(m, kb, cx, args)
}
