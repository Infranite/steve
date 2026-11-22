//! Read views (`steve_types::view`): what the Hub's projection, its snapshot and its paged reads
//! are made of. Each kind gives batch reads whose statement count does not grow with the number of
//! ids asked for: the hot set (all of it, or one workspace's), the views of given ids, and pages
//! of what is outside the hot set.

mod decision_chat;
mod decisions;

pub use decision_chat::{PROXY_MARK, looks_automated};
pub use decisions::{ABOUT_CHAT_MESSAGE, DecisionPage, DecisionViews, SettledCursor};

use crate::{R, Read, W};

/// Ids per `IN (…)` list.
const CHUNK: usize = 500;

/// The view repositories, on a read snapshot and on the write transaction.
pub trait Views: Read + Sized {
    fn decision_views(&mut self) -> DecisionViews<'_, Self> {
        DecisionViews(self)
    }
}
impl Views for R<'_> {}
impl Views for W<'_> {}
