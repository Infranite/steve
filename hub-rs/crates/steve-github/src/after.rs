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
}

/// Moves `fx`'s items to finish and signals into the outbox, in `tx` (the transaction that changed
/// what they follow). Nothing when there are none.
pub(crate) fn emit(tx: &Tx, ws: &str, connection: &str, fx: &mut Effects) -> Result<(), ApiError> {
    if fx.finish.is_empty() && fx.signals.is_empty() {
        return Ok(());
    }
    let a = After {
        connection: connection.into(),
        finish: std::mem::take(&mut fx.finish),
        signals: std::mem::take(&mut fx.signals),
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
    /// The outbox consumer of this module (registered by the Hub with its dispatcher).
    pub fn outbox_consumer(&self) -> Arc<dyn Consumer> {
        Arc::new(GitHubAfter { gh: self.clone() })
    }
}
