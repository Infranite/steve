//! The notes left by a flow for the next report.

use steve_store::DigestNotes;

use crate::{FlowCx, FlowResult};

/// Marks the pending notes as taken in the same transaction that reads them.
pub fn take_now(cx: &FlowCx, ws: &str, since: i64) -> FlowResult<Vec<steve_store::DigestEntry>> {
    let now = cx.now();
    cx.store().write(|w| {
        let mut notes = DigestNotes(w);
        let pending = notes.pending(ws, since)?;
        let ids: Vec<_> = pending.iter().map(|entry| entry.id.clone()).collect();
        notes.mark_taken(&ids, now)?;
        Ok(pending)
    })
}
