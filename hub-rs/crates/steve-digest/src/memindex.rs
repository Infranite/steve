//! A memory as an index of key information: besides its text, the conversations it came from, the people and
//! the matter it is about, and the message range it rests on.
//!
//! Extends the existing memory rows, no second store: the conversation and the matter are `mem_about` rows
//! (object types `space` / `mail_thread` and `matter`, next to `party`, `project`, `item` …), the message range
//! a `mem_ranges` row (conversation, first and last Steve message id and time, the packet). `steve_memory_list`
//! returns them; `steve_im_read` reads the range again.

use steve_store::{AddedMemoryIndex, StoreError, W};

pub use steve_store::MemoryRange as Range;

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

/// Indexes memory `mem` of `ws`: about the conversation, the matter and the parties, resting on `range`.
pub fn link(
    w: &mut W<'_>,
    ws: &str,
    mem: &str,
    conv: &ConvRef,
    matter: Option<&str>,
    parties: &[String],
    range: Option<&Range>,
    packet: Option<&str>,
) -> Result<AddedMemoryIndex> {
    let tx = &w.legacy();
    if !w.memory_index().exists(ws, mem)? || crate::conv::info(tx, ws, conv)?.is_none() {
        return Err(StoreError::Hook(
            "unknown digest memory or conversation".into(),
        ));
    }
    let matter = match matter {
        Some(m) => Some(
            crate::matter::resolve(tx, ws, m)?
                .ok_or_else(|| StoreError::Hook("unknown digest matter".into()))?
                .0,
        ),
        None => None,
    };
    for p in parties {
        if !w.contacts().contains(ws, p)? {
            return Err(StoreError::Hook("unknown digest party".into()));
        }
    }
    if let Some(p) = packet {
        let p = w
            .digest_packets()
            .get(ws, p)?
            .ok_or_else(|| StoreError::Hook("unknown digest packet".into()))?;
        if p.conv != conv.key() {
            return Err(StoreError::Hook(
                "digest packet conversation mismatch".into(),
            ));
        }
    }
    if let Some(r) = range {
        let msgs = crate::conv::by_ids(tx, ws, conv, &[r.first_id.clone(), r.last_id.clone()])?;
        if r.conv != conv.key()
            || msgs.len() != 2
            || msgs[0].at != r.first_at
            || msgs[1].at != r.last_at
            || (r.first_at, &r.first_id) > (r.last_at, &r.last_id)
        {
            return Err(StoreError::Hook("invalid digest message range".into()));
        }
    }
    let mut about: Vec<(String, String)> = vec![(conv.kind.clone(), conv.id.clone())];
    if let Some(m) = matter {
        about.push(("matter".into(), m));
    }
    for p in parties {
        about.push(("party".into(), p.clone()));
    }
    w.memory_index()
        .add(ws, mem, about, range, packet, steve_store::now())
}
