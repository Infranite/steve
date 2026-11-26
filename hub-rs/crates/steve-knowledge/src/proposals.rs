//! Project-level `AGENTS.md` proposals: codebase engineering lessons stay with their project
//! instead of entering personal memories. Each proposal opens a `confirm` decision (`subject_type =
//! "agents_proposal"`) and appears on the project page; confirming records acceptance for a later
//! `AGENTS.md` PR without directly modifying the repository.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_modules::decisions::{self, DecisionCx, DecisionHandler, DecisionOutcome};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules};
use steve_store::base::ObjRef;
use steve_store::knowledge::{NewProjectProposal, ProjectProposal, ProjectProposals};
use steve_store::{Decision, DecisionRows, Read, References, W};

use crate::candidates::{EVIDENCE_MAX, Origin};

/// `subject_type` of project `AGENTS.md` / `SKILL.md` proposal decisions.
pub const SUBJECT: &str = "agents_proposal";
/// `project_proposals.kind` for `AGENTS.md` update suggestions.
pub const KIND_AGENTS_MD: &str = "agents_md";
/// `project_proposals.kind` for cross-repo reusable `SKILL.md` methodology suggestions.
pub const KIND_SKILL_MD: &str = "skill_md";
const NOUN: &str = "提议";

/// A proposal ready to be persisted by extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProposal {
    pub project_id: Option<String>,
    pub kind: String,
    pub target_path: String,
    pub text: String,
    pub reason: String,
    pub evidence: String,
}

/// Public REST shape for one `AGENTS.md` or `SKILL.md` proposal (`GET /kb/proposals`).
#[derive(Debug, Clone, Serialize)]
pub struct ProposalOut {
    pub id: String,
    pub project_id: Option<String>,
    pub project_name: String,
    pub kind: String,
    pub target_path: String,
    pub text: String,
    pub reason: String,
    pub evidence: String,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub origin_label: String,
    pub origin_href: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub writer: String,
    pub created: i64,
    pub updated: i64,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn origin_href(kind: &str, id: Option<&str>) -> String {
    let Some(id) = id.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    match kind {
        "item" => format!("#/items/{id}"),
        "mail" => format!("#/mail/t/{id}"),
        "space" => format!("#/channels/space/{id}"),
        _ => String::new(),
    }
}

pub(crate) fn proposal_out(c: &mut impl Read, p: &ProjectProposal) -> ApiResult<ProposalOut> {
    let project_name = match p.project_id.as_deref() {
        Some(pid) => References(c)
            .name(&ObjRef::new("project", pid))?
            .unwrap_or_default(),
        None => steve_i18n::t!("knowledge.proposal.skill_target"),
    };
    Ok(ProposalOut {
        id: p.id.clone(),
        project_id: p.project_id.clone(),
        project_name,
        kind: p.kind.clone(),
        target_path: p.target_path.clone(),
        text: p.text.clone(),
        reason: p.reason.clone(),
        evidence: p.evidence.clone(),
        origin_kind: p.origin_kind.clone(),
        origin_id: p.origin_id.clone(),
        origin_label: crate::sources::label_shown(&p.origin_label),
        origin_href: origin_href(&p.origin_kind, p.origin_id.as_deref()),
        status: p.status.clone(),
        decision_id: p.decision_id.clone(),
        writer: p.writer.clone(),
        created: p.created,
        updated: p.updated,
    })
}

/// Persists a project `AGENTS.md` or cross-repo `SKILL.md` proposal and opens its `confirm` decision.
pub fn propose_in(tx: &mut W<'_>, o: &Origin, p: &NewProposal) -> ApiResult<ProjectProposal> {
    let evidence = cut(&p.evidence, EVIDENCE_MAX);
    let origin_id = (!o.id.is_empty()).then_some(o.id.as_str());
    let target_path = if p.target_path.is_empty() {
        if p.kind == KIND_SKILL_MD {
            "SKILL.md"
        } else {
            "AGENTS.md"
        }
    } else {
        p.target_path.as_str()
    };
    let mut row = ProjectProposals(tx).create(
        &o.ws,
        &NewProjectProposal {
            project_id: p.project_id.as_deref(),
            kind: &p.kind,
            target_path,
            text: &p.text,
            reason: &p.reason,
            evidence: &evidence,
            origin_kind: o.kind.code(),
            origin_id,
            origin_label: &o.label,
            writer: &o.writer,
            created_by: &o.user,
        },
    )?;
    let target_label = match p.project_id.as_deref() {
        Some(pid) => References(tx)
            .name(&ObjRef::new("project", pid))?
            .unwrap_or_else(|| pid.to_string()),
        None => steve_i18n::stored(|| steve_i18n::t!("knowledge.proposal.skill_target")),
    };
    let mut draft = format!("项目：{target_label}\n目标文件：{target_path}");
    if !p.reason.trim().is_empty() {
        draft.push_str(&format!("\n为什么值得写入：{}", p.reason.trim()));
    }
    if !o.label.trim().is_empty() {
        draft.push_str(&format!("\n来源：{}", o.label.trim()));
    }
    if !evidence.is_empty() {
        draft.push_str(&format!("\n原话：{evidence}"));
    }
    let question = format!("建议写入 {target_label} 的 {target_path}：{}", p.text);
    let mut nd = decisions::confirm(&o.ws, SUBJECT, &row.id, &question, &o.writer);
    nd.draft = Some(draft);
    nd.session_id = o.session_id.clone();
    nd.source = Some(steve_store::DecisionSource::Record);
    let d = DecisionRows(tx).open(&nd)?;
    ProjectProposals(tx).set_decision_id(&row.id, Some(&d.id))?;
    row.decision_id = Some(d.id);
    tx.audit(
        &o.ws,
        &o.writer,
        "project_proposal.create",
        &row.id,
        &json!({
            "project_id": p.project_id,
            "kind": p.kind,
            "origin": o.kind.code(),
            "origin_id": o.id,
        }),
    )?;
    crate::state::emit_change(tx, &o.ws, &row.id);
    metrics::counter!("steve_knowledge_proposals_total", "action" => "created").increment(1);
    Ok(row)
}

fn pending_proposal(c: &mut impl Read, ws: &str, id: &str) -> ApiResult<ProjectProposal> {
    match ProjectProposals(c).get(id)? {
        Some(p) if p.workspace_id == ws && p.status == "pending" => Ok(p),
        Some(p) if p.workspace_id == ws => Err(ApiError::conflict(steve_i18n::t!(
            "knowledge.proposal.already_handled"
        ))),
        _ => Err(ApiError::not_found(NOUN)),
    }
}

/// Applies `yes` (`accepted`) or `no` (`dismissed`) to a pending proposal.
pub fn answer(
    tx: &mut W<'_>,
    ws: &str,
    actor: &str,
    id: &str,
    choice: &str,
) -> ApiResult<ProjectProposal> {
    let row = pending_proposal(tx, ws, id)?;
    let (status, action) = match choice {
        "yes" => ("accepted", "accepted"),
        "no" => ("dismissed", "dismissed"),
        _ => return Err(ApiError::invalid("choice")),
    };
    ProjectProposals(tx).set_status(&row.id, status)?;
    tx.audit(
        ws,
        actor,
        &format!("project_proposal.{action}"),
        &row.id,
        &json!({ "project_id": row.project_id, "kind": row.kind }),
    )?;
    crate::state::emit_change(tx, ws, &row.id);
    metrics::counter!("steve_knowledge_proposals_total", "action" => action).increment(1);
    ProjectProposals(tx)
        .get(&row.id)?
        .ok_or_else(|| ApiError::not_found(NOUN))
}

/// Decision handler for `subject_type = "agents_proposal"`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentsProposalDecisions;

impl DecisionHandler for AgentsProposalDecisions {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        answer(tx, cx.ws, cx.actor, &d.subject_id, cx.choice)?;
        Ok(decisions::choice_result(cx).into())
    }

    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        let Some(p) = ProjectProposals(tx).get(&d.subject_id)? else {
            return Err(ApiError::not_found(NOUN));
        };
        if p.workspace_id != d.workspace_id {
            return Err(ApiError::not_found(NOUN));
        }
        ProjectProposals(tx).set_status(&p.id, "pending")?;
        crate::state::emit_change(tx, &d.workspace_id, &p.id);
        Ok(())
    }

    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(p) = ProjectProposals(tx).get(&d.subject_id)?
            && p.workspace_id == d.workspace_id
            && p.status == "pending"
        {
            ProjectProposals(tx).set_status(&p.id, "dismissed")?;
            crate::state::emit_change(tx, &d.workspace_id, &p.id);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- REST

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/kb/proposals", get(list_route))
        .route("/kb/proposals/{id}/resolve", post(resolve_route))
}

#[derive(Debug, Default, Deserialize)]
pub struct ProposalsParams {
    pub project_id: Option<String>,
    pub status: Option<String>,
}

async fn list_route(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<ProposalsParams>,
) -> ApiResult<Json<Vec<ProposalOut>>> {
    m.blocking(move |m| list(m, &who, &p).map(Json)).await
}

#[derive(Debug, Deserialize)]
pub struct ResolveProposalBody {
    pub choice: String,
}

async fn resolve_route(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<ResolveProposalBody>,
) -> ApiResult<Json<ProposalOut>> {
    m.blocking(move |m| resolve_one(m, &who, &id, &b.choice).map(Json))
        .await
}

pub fn list(m: &Modules, who: &Caller, p: &ProposalsParams) -> ApiResult<Vec<ProposalOut>> {
    if let Some(st) = p.status.as_deref().filter(|s| !s.is_empty()) {
        Check::new()
            .one_of("status", st, &["pending", "accepted", "dismissed"])
            .done()?;
    }
    crate::read_api(&m.store, |c| {
        let rows =
            ProjectProposals(c).list(&who.ws, p.project_id.as_deref(), p.status.as_deref())?;
        rows.iter().map(|r| proposal_out(c, r)).collect()
    })
}

pub fn resolve_one(m: &Modules, who: &Caller, id: &str, choice: &str) -> ApiResult<ProposalOut> {
    Check::new()
        .one_of("choice", choice, &["yes", "no"])
        .done()?;
    let existing = crate::read_api(&m.store, |c| pending_proposal(c, &who.ws, id))?;
    if let Some(dec_id) = existing.decision_id.as_deref() {
        let dec = crate::read_api(&m.store, |c| {
            Ok::<_, ApiError>(DecisionRows(c).get(dec_id)?)
        })?;
        if let Some(d) = dec
            && d.status == steve_types::DecisionStatus::Open
        {
            decisions::resolve(m, who, dec_id, d.revision, choice, "")?;
        } else {
            m.store
                .write(|tx| answer(tx, &who.ws, &who.me, id, choice))?;
        }
    } else {
        m.store
            .write(|tx| answer(tx, &who.ws, &who.me, id, choice))?;
    }
    crate::read_api(&m.store, |c| {
        let row = ProjectProposals(c)
            .get(id)?
            .ok_or_else(|| ApiError::not_found(NOUN))?;
        proposal_out(c, &row)
    })
}
