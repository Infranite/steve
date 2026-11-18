//! Per-connection SSE controls, never bus events. Ping is not JSON: it is the SSE frame
//! `event: ping\ndata: \n\n`, with no cursor, and does not belong to this enum.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum StreamControl {
    #[serde(rename = "stream.gap")]
    Gap,
    #[serde(rename = "stream.reset")]
    Reset,
}
