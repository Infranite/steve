//! Persistence for conversation digest.

mod alerts;
mod briefings;
mod changes;
mod matters;
mod packets;
mod profiles;
mod segments;
mod summaries;
mod todos;
mod windows;

pub use alerts::{Alert, Alerts, Rollup};
pub use briefings::{Briefings, NewBriefing};
pub use changes::{Change, Changes, NewChange};
pub use matters::{Matter, MatterEvent, Matters};
pub use packets::{Batching, Chunk, ChunkRow, NewPacket, Packet, Packets};
pub use profiles::{Profiles, Stored};
pub use segments::{Segment, SegmentInput, Segments};
pub use summaries::{NewSummary, Summaries, Summary};
pub use todos::{Todo, TodoFields, TodoFilter, TodoInput, Todos};
pub use windows::{Cursor, Windows};

use crate::W;
use steve_types::{ChannelsEvent, DomainEvent};

fn emit(w: &mut W<'_>, ws: &str, id: &str) {
    w.emit(DomainEvent::from(ChannelsEvent::Digest {
        id: id.into(),
        ws: ws.into(),
    }));
}
