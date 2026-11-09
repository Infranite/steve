//! `github.after`: what follows a committed state change and has to survive a crash: the signals
//! for steve-flow and the items to finish (a PR merged with `finish_on=merged`). Emitted in the
//! transaction that changed the state, delivered after the commit at least once. A repetition is
//! harmless: steve-flow answers a signal's `(connection, external_id)` with the event it has, and a
//! finished item is not finished again. Before, they went out in memory after the commit, and a
//! delivery processed again finds no change left to derive them from.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use steve_modules::ApiError;
use steve_store::Tx;
use steve_store::outbox::{self, Applied, ConsumeError, Consumer, Event, Since};

use crate::GitHub;
use crate::events::SignalEvent;
use crate::sync::Effects;

pub const TOPIC: &str = "github.after";
pub const VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct After {
    connection: String,
    /// `(item, the item's “now”)`.
    finish: Vec<(String, String)>,
    signals: Vec<SignalEvent>,
    /// After the signals: comments and reviews for the items of their PRs (absent before 2026-11-09).
    #[serde(default)]
    supplements: Vec<PrSupplement>,
}

/// A comment or review on a PR linked to an item, for that item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrSupplement {
    pub item: String,
    /// The signal's `external_id` (`comment:<id>`): a repeated delivery passes it on once.
    pub external_id: String,
    pub text: String,
}

/// Moves `fx`'s items to finish and signals into the outbox, in `tx` (the transaction that changed
/// what they follow). Nothing when there are none.
pub(crate) fn emit(tx: &Tx, ws: &str, connection: &str, fx: &mut Effects) -> Result<(), ApiError> {
    if fx.finish.is_empty() && fx.signals.is_empty() && fx.supplements.is_empty() {
        return Ok(());
    }
    let a = After {
        connection: connection.into(),
        finish: std::mem::take(&mut fx.finish),
        signals: std::mem::take(&mut fx.signals),
        supplements: std::mem::take(&mut fx.supplements),
    };
    let payload = serde_json::to_value(&a).map_err(|e| ApiError::Internal(e.to_string()))?;
    outbox::emit(tx, ws, TOPIC, VERSION, &payload)?;
    Ok(())
}

/// The consumer of [`TOPIC`].
pub struct GitHubAfter {
    gh: GitHub,
}

impl GitHubAfter {
    pub const ID: &'static str = "github.after";
}

fn after(ev: &Event) -> Result<After, ConsumeError> {
    if ev.version != VERSION {
        return Err(ConsumeError::Reject(format!(
            "{} version {}",
            ev.topic, ev.version
        )));
    }
    serde_json::from_value(ev.payload.clone())
        .map_err(|e| ConsumeError::Reject(format!("{}: {e}", ev.topic)))
}

impl Consumer for GitHubAfter {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        TOPIC
    }

    fn since(&self) -> Since {
        Since::Start
    }

    /// Items first, then the signals in order, as after the commit before. Async: the dispatcher
    /// runs consumers on a blocking thread of the Hub's runtime.
    fn effect(&self, ev: &Event) -> Result<(), ConsumeError> {
        let a = after(ev)?;
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| ConsumeError::Retry("没有运行时".into()))?;
        rt.block_on(async {
            let ws = ev.workspace_id.as_str();
            for (item, now) in &a.finish {
                self.gh.finish_item_checked(ws, item, now).await?;
            }
            let sink = self.gh.sink();
            for s in a.signals {
                sink.ingest(ws, &a.connection, s).await?;
            }
            for s in &a.supplements {
                self.gh.pass_on(ws, &a.connection, s).await?;
            }
            Ok::<(), String>(())
        })
        .map_err(ConsumeError::Retry)
    }

    fn apply(&self, _tx: &Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        let a = after(ev)?;
        Ok(Applied::noted(format!(
            "{} 个事项，{} 个信号",
            a.finish.len(),
            a.signals.len()
        )))
    }
}

impl GitHub {
    /// Passes a PR comment on to its item, once: done when the item already has it (a link from
    /// `gh_signal:<connection>:<external_id>`). A closed item does not take it; you are told instead.
    async fn pass_on(&self, ws: &str, connection: &str, s: &PrSupplement) -> Result<(), String> {
        let Some(flow) = self.flow() else {
            return Ok(());
        };
        let key = format!("{connection}:{}", s.external_id);
        let store = flow.store().clone();
        let (ws2, item, key2) = (ws.to_string(), s.item.clone(), key.clone());
        let done = store
            .reader::<_, steve_store::StoreError>(move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM links WHERE workspace_id = ? AND subject_type = 'item'
                       AND subject_id = ? AND predicate = 'from' AND object_type = 'gh_signal' AND object_id = ?)",
                    rusqlite::params![ws2, item, key2],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .map_err(|e| e.to_string())?;
        if done {
            return Ok(());
        }
        if let Err(e) = flow.core.supplement(ws, &s.item, &s.text, "auto").await {
            tracing::info!(item = %s.item, "PR comment not passed on: {e}");
            let note = format!(
                "{} 的 PR 有新评论，但它没有收下（{e}）：\n{}",
                s.item, s.text
            );
            flow.core
                .post_steve_message(ws, &note, serde_json::json!({}))
                .await?;
        }
        let (ws2, item) = (ws.to_string(), s.item.clone());
        store
            .transaction(move |tx| {
                steve_store::base::links::add(
                    tx,
                    &ws2,
                    &steve_store::base::links::NewLink::new(
                        steve_store::base::ObjRef::new("item", &item),
                        "from",
                        steve_store::base::ObjRef::new("gh_signal", &key),
                        "github",
                    ),
                )
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The outbox consumer of this module (registered by the Hub with its dispatcher).
    pub fn outbox_consumer(&self) -> Arc<dyn Consumer> {
        Arc::new(GitHubAfter { gh: self.clone() })
    }
}
