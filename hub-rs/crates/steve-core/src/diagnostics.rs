//! Observation never drives the domain. Authorization belongs to the authenticated caller;
//! this projection reuses the same lifecycle/dependency/release rules as normal execution.

use serde_json::{Value, json};
use steve_store::diagnostics::{Details, Record, Severity};

use crate::{Core, Result, Tx};

impl Core {
    /// Read-only facts, not a model's diagnosis. Core's state lock is not a SQL snapshot:
    /// release state is sampled separately and a changed item revision is reported explicitly.
    pub fn diagnostic_snapshot(&self, item_id: &str) -> Result<Value> {
        let (observed_at, cycle, revision, lifecycle, dependencies, last_stop) = {
            let t = self.tx();
            let item = t.item(item_id)?;
            // how its session's last turn ended, as the stall watch reads it (your Stop included)
            let stop = match t.active_session(item_id)? {
                Some(s) => t.db().store().sessions().stop(&s.id)?,
                None => None,
            };
            let (stopped, yours) = crate::turn_wait::stopped(stop.as_ref());
            (
                steve_store::now(),
                item.cycle,
                item.delivery_revision,
                crate::cycle::lifecycle_view(&t, &item)?,
                t.dependency_view(item_id)?,
                json!({"why": stopped, "yours": yours}),
            )
        };
        let current = self.store().item(item_id)?;
        let changed = current.is_none_or(|i| i.cycle != cycle || i.delivery_revision != revision);
        Ok(json!({
            "observed_at":observed_at,"item_id":item_id,"cycle":cycle,"revision":revision,
            "lifecycle":lifecycle,"dependencies":dependencies,"last_stop":last_stop,
            "sampled_until":steve_store::now(),"changed_during_read":changed
        }))
    }
}

impl Tx<'_> {
    /// Enqueue safe scalars only; do not look up a session, acquire the Store writer or
    /// publish a business event while Core holds its state lock. The recorder resolves ownership.
    pub(crate) fn diagnostic_session(
        &self,
        session: &str,
        attempt: Option<&str>,
        kind: &str,
        reason: &str,
        detail: bool,
        details: Details,
    ) {
        let _ = self.db().store().diagnostics().record(Record {
            session_id: Some(session.into()),
            attempt_id: attempt.map(str::to_owned),
            kind: kind.into(),
            reason_code: reason.into(),
            detail,
            details,
            ..Record::default()
        });
    }

    pub(crate) fn diagnostic_item(
        &self,
        item: &crate::repo::Item,
        kind: &str,
        reason: &str,
        detail: bool,
        mut details: Details,
    ) {
        details.item_cycle = Some(item.cycle);
        details.item_revision = Some(item.delivery_revision);
        let _ = self.db().store().diagnostics().record(Record {
            workspace_id: item.workspace_id.clone(),
            item_id: Some(item.id.clone()),
            kind: kind.into(),
            reason_code: reason.into(),
            severity: Severity::Info,
            detail,
            details,
            ..Record::default()
        });
    }
}
