//! Item and party projections assembled inside one storage snapshot.

use steve_store::base::links::Links;
use steve_store::{ItemReads, Read, Result, Tx};

/// The item ↔ party roles of `links`.
pub const ROLES: [&str; 5] = ["raised_by", "related", "waiting_on", "about", "attendee"];

pub use steve_store::ItemHead;

/// One item ↔ party link with the item's state.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemLink {
    pub item_id: String,
    pub party_id: String,
    pub role: String,
    pub source: String,
    pub confidence: f64,
    pub created: i64,
    pub archived: bool,
    pub title: String,
}

/// Links from existing items in the workspace, newest first.
pub fn item_links_to(
    c: &mut impl Read,
    ws: &str,
    parties: &[String],
    unarchived: bool,
) -> Result<Vec<ItemLink>> {
    let links = Links(c).item_links_to(ws, parties, &ROLES)?;
    let mut ids: Vec<&str> = links.iter().map(|l| l.subject_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let heads = ItemReads(c).heads(ws, &ids)?;
    Ok(links
        .into_iter()
        .filter_map(|l| {
            let head = heads
                .get(&l.subject_id)
                .filter(|h| !unarchived || !h.archived)?;
            Some(ItemLink {
                item_id: l.subject_id,
                party_id: l.object_id,
                role: l.predicate,
                source: l.source,
                confidence: l.confidence,
                created: l.created,
                archived: head.archived,
                title: head.title.clone(),
            })
        })
        .collect())
}

/// Adds a timeline line from a caller that still owns a legacy write transaction.
pub fn timeline_add(tx: &Tx, item: &str, actor: &str, kind: &str, text: &str) -> Result<()> {
    tx.write(|w| {
        let row = steve_store::ItemReads(w)
            .head(item)?
            .ok_or_else(|| steve_store::StoreError::Rejected(format!("missing item {item}")))?;
        steve_store::TimelineRows(w).add(&row.workspace_id, item, actor, kind, text)
    })
}
