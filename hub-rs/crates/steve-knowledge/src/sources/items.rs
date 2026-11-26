//! Extraction on item completion: work items are no longer indexed into `kb_docs` / `kb_chunks`
//! (they remain searchable as records), but finishing an item still runs candidate memory and
//! project `AGENTS.md` proposal extraction once per completion (`kb_marks extract:item:<id>`).
//!
//! steve-core calls [`KnowledgeHooks`] after `finish` (B1 `item_hooks`); a reopened and finished
//! again item is extracted again.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::Value;
use steve_modules::hooks::ItemHooks;
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::{Level, ObjRef};
use steve_store::knowledge::{Documents, OriginClass, SourceTrustTier};
use steve_store::{Item, ItemEvidence, Members, Read, References};

use crate::candidates::{Origin, OriginKind};
use crate::extract::{self, MAX_INPUT, clip};
use crate::ingest::IngestOutcome;
use crate::state::Knowledge;

/// What item completion extraction reads from the store.
#[derive(Debug, Clone, Default)]
pub struct ItemFacts {
    pub id: String,
    pub ws: String,
    pub title: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    /// Goal versions, oldest first.
    pub goals: Vec<String>,
    /// What you wrote on the timeline (supplements), oldest first.
    pub said: Vec<String>,
    /// `(question, your answer)` of answered questions.
    pub answers: Vec<(String, String)>,
    /// Rework notes given at acceptance.
    pub reviews: Vec<String>,
    /// The summary of the latest delivery.
    pub summary: String,
    /// The user who started the item (else the workspace owner).
    pub user: String,
    /// Changes whenever the item is finished again (`items.updated`).
    pub stamp: i64,
}

/// Reads an item's facts; `None` if there is no such item.
pub fn facts(c: &mut impl Read, id: &str) -> ApiResult<Option<ItemFacts>> {
    let head = ItemEvidence(c).header(id)?;
    let Some((ws, title, _cwd, stamp)) = head else {
        return Ok(None);
    };
    let project_id = ItemEvidence(c).project_id(id)?;
    let project_name = match project_id.as_deref() {
        Some(pid) => References(c).name(&ObjRef::new("project", pid))?,
        None => None,
    };
    let goals = ItemEvidence(c).goals(id)?;
    let said: Vec<String> = ItemEvidence(c)
        .user_text(id)?
        .into_iter()
        // answers and reviews come from their decisions below; bare actions say nothing to remember
        .filter(|t: &String| {
            !t.starts_with("回答：")
                && !t.starts_with("返工：")
                && !matches!(
                    t.as_str(),
                    "验收通过"
                        | "暂停"
                        | "继续"
                        | "放弃"
                        | "重新打开"
                        | "归档"
                        | "恢复"
                        | "撤销了决定"
                )
        })
        .collect();
    let decs = ItemEvidence(c).completion_cards(id)?;
    let mut f = ItemFacts {
        id: id.into(),
        ws: ws.clone(),
        title,
        project_id,
        project_name,
        goals,
        said,
        stamp,
        ..Default::default()
    };
    for card in decs {
        let result = card.result;
        let s = |k: &str| {
            result
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let by_steve = result.get("decided_by").and_then(Value::as_str) == Some("steve");
        match card.kind.as_str() {
            "ask" if card.status == "resolved" && !by_steve => {
                if let Some(a) = s("answer") {
                    f.answers.push((card.question, a));
                }
            }
            "accept" => {
                f.summary = card.draft;
                if card.status == "resolved"
                    && !by_steve
                    && s("choice").as_deref() == Some("rework")
                    && let Some(t) = s("text")
                {
                    f.reviews.push(t);
                }
            }
            _ => {}
        }
    }
    f.user = initiator(c, &ws, id)?;
    Ok(Some(f))
}

/// The workspace user who created the item (audit `item.create`), else the workspace owner.
pub fn initiator(c: &mut impl Read, ws: &str, item: &str) -> ApiResult<String> {
    Ok(match ItemEvidence(c).creator(ws, item)? {
        Some(actor) => actor,
        None => Members::new(c)
            .first(ws)?
            .ok_or_else(|| ApiError::not_found("工作空间成员"))?,
    })
}

/// Section marker identifying the Agent's delivery summary (eligible ONLY for codebase/skill
/// `AGENTS.md`/`SKILL.md` proposals, NEVER as owner preference/rule evidence).
pub const AGENT_SUMMARY_TAG: &str = "[AGENT_SUMMARY]";
/// Section marker identifying owner rework feedback (high-signal Tier 1 evidence).
pub const OWNER_REWORK_TAG: &str = "[OWNER_REWORK]";

/// The extraction input: project, goal versions, owner supplements/answers/rework notes, and
/// tagged `[AGENT_SUMMARY]` (kept strictly separated from owner preference evidence).
pub fn extract_text(f: &ItemFacts) -> String {
    let mut s = format!("Item {}: {}\n", f.id, f.title);
    if let Some(project) = f.project_name.as_deref().filter(|p| !p.is_empty()) {
        s.push_str(&format!("Project: {project}\n"));
    }
    if !f.goals.is_empty() {
        s.push_str("\n[ITEM_GOAL]\n");
    }
    for (i, g) in f.goals.iter().enumerate() {
        s.push_str(&format!("Goal v{}: {g}\n", i + 1));
    }
    for t in &f.said {
        s.push_str(&format!("\n[OWNER_SUPPLEMENT]\n{t}\n"));
    }
    for (q, a) in &f.answers {
        s.push_str(&format!("\n[OWNER_ANSWER]\nQ: {q}\nA: {a}\n"));
    }
    for r in &f.reviews {
        s.push_str(&format!("\n{OWNER_REWORK_TAG}\n{r}\n"));
    }
    if !f.summary.trim().is_empty() {
        s.push_str(&format!("\n{AGENT_SUMMARY_TAG}\n{}\n", f.summary.trim()));
    }
    clip(&s, MAX_INPUT)
}

/// After `finish`: runs unified extraction for personal memories (silent file writes / buffered
/// observations, zero Inbox cards) and project `AGENTS.md` / `SKILL.md` proposals once per
/// completion. Does not index the item as a knowledge document.
pub async fn finished(m: &Modules, kb: &Knowledge, item_id: &str) -> ApiResult<Vec<IngestOutcome>> {
    let id = item_id.to_string();
    let f = m
        .blocking(move |m| crate::read_api(&m.store, |c| facts(c, &id)))
        .await?;
    let Some(f) = f else {
        return Ok(Vec::new());
    };
    let mark = format!("extract:item:{}", f.id);
    let stamp = f.stamp.to_string();
    let (ws_check, mark_check, stamp_check) = (f.ws.clone(), mark.clone(), stamp.clone());
    let already = m
        .blocking(move |m| {
            Ok(m.store
                .read(|c| Documents(c).mark(&ws_check, &mark_check))?
                .as_deref()
                == Some(stamp_check.as_str()))
        })
        .await?;
    if already {
        return Ok(Vec::new());
    }
    let origin = Origin {
        ws: f.ws.clone(),
        kind: OriginKind::Item,
        id: f.id.clone(),
        label: format!("{} 交付时", f.id),
        user: f.user.clone(),
        writer: "steve".into(),
        default_level: Level::Reasoning,
        item_id: Some(f.id.clone()),
        session_id: None,
        origin_class: if !f.reviews.is_empty() {
            OriginClass::OwnerRework
        } else {
            OriginClass::ImplicitPattern
        },
        source_tier: SourceTrustTier::Tier1Owner,
    };
    let task = extract::Task {
        goals: &f.goals,
        events: false,
        project: f.project_id.as_deref(),
    };
    let prepared = extract::prepare(m, kb, &origin, &extract_text(&f), Some(&f.id), task).await?;
    let now = m.clock.now();
    let policy = kb.folder_policy().clone();
    m.blocking(move |m| {
        m.store.write(|tx| {
            if Documents(tx).mark(&origin.ws, &mark)?.as_deref() == Some(stamp.as_str()) {
                return Ok(Vec::new());
            }
            let report = extract::commit_in(tx, &policy, &origin, &prepared, now)?;
            Documents(tx).set_mark(&origin.ws, &mark, &stamp, now)?;
            Ok(report.outcomes)
        })
    })
    .await
}

/// The knowledge module's item hooks: `on_finished` extracts candidates and project proposals in a
/// spawned task. Register with `registry.item_hooks(Box::new(hooks.clone()))` and call
/// [`KnowledgeHooks::bind`] once the `Modules` exist (the registry is built before them).
#[derive(Clone)]
pub struct KnowledgeHooks {
    kb: Knowledge,
    m: Arc<OnceLock<Modules>>,
}

impl KnowledgeHooks {
    pub fn new(kb: Knowledge) -> Self {
        Self {
            kb,
            m: Arc::default(),
        }
    }

    /// Connects the hooks to the module state. Later calls are ignored.
    pub fn bind(&self, m: Modules) {
        let _ = self.m.set(m);
    }
}

#[async_trait]
impl ItemHooks for KnowledgeHooks {
    fn name(&self) -> &'static str {
        "knowledge"
    }

    async fn on_finished(&self, item: &Item) {
        let Some(m) = self.m.get().cloned() else {
            tracing::warn!(item = %item.id, "knowledge hooks not bound");
            return;
        };
        let kb = self.kb.clone();
        let id = item.id.clone();
        tokio::spawn(async move {
            if let Err(e) = finished(&m, &kb, &id).await {
                tracing::warn!(item = %id, "kb items: {e}");
            }
        });
    }
}
