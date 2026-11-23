//! Human merge consent outlives a delivery, but only for the exact target shown on its card.
//! Requests stay on their original operation; resolved decisions are the authority, not cached
//! `pr_merge_ok` flags. No target is inferred from mutable, pre-upgrade PR observations.
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::PR_AWAITING_MERGE;
use crate::Tx;
use crate::cycle::OP_RELEASE;
use crate::error::Result;
use crate::repo::{Decision, Item};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct MergeTarget {
    pub repo: String,
    pub pr: u64,
    pub head: String,
}

impl MergeTarget {
    pub fn new(repo: &str, pr: u64, head: &str) -> Option<Self> {
        let parts: Vec<_> = repo.split('/').collect();
        (parts.len() == 2
            && parts.iter().all(|s| !s.is_empty())
            && pr > 0
            && head.len() == 40
            && head.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| Self {
            repo: repo.to_owned(),
            pr,
            head: head.to_owned(),
        })
    }

    pub fn observed(o: &steve_store::ItemOperation) -> Option<Self> {
        Self::new(
            o.evidence["pr_slug"].as_str()?,
            o.evidence["pr"].as_u64()?,
            o.evidence["pr_head"].as_str()?,
        )
    }

    pub fn requested(o: &steve_store::ItemOperation, decision: &str) -> Option<Self> {
        let target: Self =
            serde_json::from_value(o.evidence["pr_merge_requests"][decision].clone()).ok()?;
        Self::new(&target.repo, target.pr, &target.head)
    }
}

impl Tx<'_> {
    /// The latest answer for this target in this item's cycle. An expired, reopened or revoked
    /// decision blocks fallback to an older yes. Another item/cycle never lends its authority.
    pub(super) fn pr_merge_receipt(
        &self,
        i: &Item,
        target: &MergeTarget,
    ) -> Result<Option<Decision>> {
        let operations = self.db().store().items().operations(&i.id, i.cycle)?;
        let mut candidates: Vec<_> = self
            .db()
            .decisions_of_item(&i.id)?
            .into_iter()
            .filter_map(|d| {
                let o = operations
                    .iter()
                    .find(|o| o.id == d.subject_id && o.kind == OP_RELEASE && o.item_id == i.id)?;
                (d.workspace_id == i.workspace_id
                    && d.kind == "approve"
                    && d.subject_type == PR_AWAITING_MERGE
                    && MergeTarget::requested(o, &d.id).as_ref() == Some(target))
                .then_some((o.revision, d))
            })
            .collect();
        candidates.sort_by(|a, b| (a.0, a.1.created, &a.1.id).cmp(&(b.0, b.1.created, &b.1.id)));
        let Some((_, d)) = candidates.pop() else {
            return Ok(None);
        };
        Ok((d.status == "resolved"
            && d.result["approved"].is_boolean()
            && d.result["merge_target"] == json!(target)
            && d.result["actor"]
                .as_str()
                .is_some_and(|a| !a.is_empty() && a != "yolo"))
        .then_some(d))
    }

    /// A card for another head/PR must not approve a newly observed target, even within one result.
    pub(super) fn pr_expire_merge_requests(
        &self,
        i: &Item,
        o: &steve_store::ItemOperation,
        target: Option<&MergeTarget>,
    ) -> Result<()> {
        for d in self.db().decisions_of_item(&i.id)? {
            if d.status == "open"
                && d.subject_type == PR_AWAITING_MERGE
                && d.subject_id == o.id
                && (target.is_none() || MergeTarget::requested(o, &d.id).as_ref() != target)
            {
                self.db().decision_resolve(
                    &d.id,
                    json!({"superseded": "PR 合并目标已改变或无法核验"}),
                    "expired",
                    None,
                )?;
            }
        }
        Ok(())
    }
}
