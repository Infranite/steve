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
    fn item_views(&mut self) -> ItemViews<'_, Self> {
        ItemViews(self)
    }
    fn session_views(&mut self) -> SessionViews<'_, Self> {
        SessionViews(self)
    }
    fn message_views(&mut self) -> MessageViews<'_, Self> {
        MessageViews(self)
    }
    fn decision_views(&mut self) -> DecisionViews<'_, Self> {
        DecisionViews(self)
    }
}
impl Views for R<'_> {}
impl Views for W<'_> {}

mod closeout;
mod items;
mod sessions;
mod thread;

pub use closeout::closeout_from_evidence;
pub use items::ItemViews;
pub use sessions::SessionViews;
pub use thread::{FIRST_PAGE, MessageViews, TAIL};

use crate::{Result, contract::Row};
use sea_query::{Alias, Expr, ExprTrait, Order, SelectStatement};

#[derive(Clone)]
pub struct Page {
    pub before: Option<SettledCursor>,
    pub limit: u32,
}

fn column(name: &str) -> Expr {
    Expr::col(Alias::new(name))
}

fn read_ids<C: Read, T>(
    r: &mut C,
    q: SelectStatement,
    ws: &str,
    ids: &[String],
    decode: fn(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let mut rows = Vec::with_capacity(ids.len());
    for ids in ids.chunks(CHUNK) {
        rows.extend(
            r.all(
                q.clone()
                    .and_where(column("workspace_id").eq(ws))
                    .and_where(column("id").is_in(ids.iter().map(String::as_str))),
                decode,
            )?,
        );
    }
    Ok(rows)
}

fn page<C: Read, T>(
    r: &mut C,
    q: &mut SelectStatement,
    ws: &str,
    options: &Page,
    time: &str,
    decode: fn(&Row<'_>) -> Result<T>,
    position: impl Fn(&T) -> (i64, &str),
) -> Result<(Vec<T>, Option<String>)> {
    q.and_where(column("workspace_id").eq(ws));
    if let Some(b) = &options.before {
        q.and_where(
            column(time).lt(b.resolved).or(column(time)
                .eq(b.resolved)
                .and(column("id").lt(b.id.as_str()))),
        );
    }
    q.order_by(Alias::new(time), Order::Desc)
        .order_by(Alias::new("id"), Order::Desc)
        .limit(u64::from(options.limit) + 1);
    let mut rows = r.all(q, decode)?;
    let next = if rows.len() > options.limit as usize {
        rows.pop();
        rows.last().map(|row| {
            let (at, id) = position(row);
            format!("{at}:{id}")
        })
    } else {
        None
    };
    Ok((rows, next))
}
