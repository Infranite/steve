//! Execution adapters for upgrade v1. The permanent data module imports none of these ports.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use steve_node_api::{Node, upgrade_bridge, upgrade_v1 as v1};

pub fn decode<T: DeserializeOwned>(value: Value) -> v1::Result<T> {
    serde_json::from_value(value)
        .map_err(|e| upgrade_bridge::invalid(format!("upgrade payload: {e}")))
}

pub fn encode<T: Serialize>(value: T) -> v1::Result<Value> {
    serde_json::to_value(value).map_err(|e| v1::Error::new(v1::Code::Internal, e.to_string()))
}

pub async fn serve_node<N: Node + ?Sized>(
    node: &N,
    method: &str,
    value: Value,
) -> v1::Result<Value> {
    match method {
        v1::PROBE_BUILD => {
            let _: v1::ProbeBuildRequest = decode(value)?;
            encode(node.probe_build().await.map_err(v1::Error::from)?)
        }
        v1::UPGRADE_STAGE => {
            let request: v1::StageRequest = decode(value)?;
            let state = node
                .upgrade_stage(request.try_into()?)
                .await
                .map_err(v1::Error::from)?;
            encode(v1::StageResponse::try_from(state)?)
        }
        v1::UPGRADE_SWITCH => {
            let request: v1::SwitchRequest = decode(value)?;
            let request: steve_node_api::UpgradeSwitch = request.try_into()?;
            let state = node
                .upgrade_switch(request)
                .await
                .map_err(v1::Error::from)?;
            encode(v1::SwitchResponse::try_from(state)?)
        }
        v1::UPGRADE_STATUS => {
            let request: v1::StatusRequest = decode(value)?;
            let state = node
                .upgrade_status(upgrade_bridge::operation(request.operation)?)
                .await
                .map_err(v1::Error::from)?;
            encode(v1::StatusResponse::try_from(state)?)
        }
        _ => Err(v1::Error::incompatible()),
    }
}
