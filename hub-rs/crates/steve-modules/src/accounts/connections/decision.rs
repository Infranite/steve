//! The `connection` decision handler: the “<账号> 的授权过期了” card with
//! `reauth` / `pause`.

use serde_json::{Value, json};
use steve_store::base;
use steve_store::{Decision, Tx};

use super::repo;
use crate::decisions::{DecisionCx, DecisionHandler};
use crate::{ApiError, ApiResult};

pub struct ConnectionDecision;

impl DecisionHandler for ConnectionDecision {
    /// `pause` pauses the connection; `reauth` leaves it in `reauth` and tells the card where to go
    /// (`account_id`: the account page has the re-authorize button, which needs the browser).
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let Some(mut r) = repo::get(tx, &d.subject_id)?.filter(|r| r.workspace_id == cx.ws) else {
            // disconnected meanwhile: nothing left to do
            return Ok(json!({"choice": cx.choice, "gone": true}));
        };
        match cx.choice {
            "pause" => {
                if r.status != "paused" {
                    r.status = "paused".into();
                    repo::update(tx, &r, steve_store::now())?;
                    base::audit(
                        tx,
                        cx.ws,
                        cx.actor,
                        "connection.patch",
                        &r.id,
                        &json!({"status": "paused", "decision": d.id}),
                    )?;
                    tx.publish(steve_types::DomainEvent::from(
                        steve_types::ChannelsEvent::Connection {
                            id: {
                                let value: String = r.id.clone();
                                steve_types::ConnectionId::from(value)
                            },
                            ws: {
                                let value: String = cx.ws.into();
                                steve_types::WorkspaceId::from(value)
                            },
                        },
                    ));
                }
                Ok(json!({"choice": "pause"}))
            }
            "reauth" => Ok(json!({
                "choice": "reauth",
                "connection_id": r.id,
                "account_id": r.account_id
            })),
            _ => Err(ApiError::invalid("choice")),
        }
    }
}
