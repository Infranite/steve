//! A declared pull request and the exact range accepted for delivery.

use serde::{Deserialize, Serialize};

use crate::ItemId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DeliveryRef {
    pub repository: String,
    pub number: i64,
    pub target: String,
    pub head: String,
    /// Absent on a historical association whose accepted range could not be recovered.
    pub base: Option<String>,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryBaseline {
    pub item: ItemId,
    pub cycle: i64,
    pub revision: i64,
    pub scope: i64,
    pub refs: Vec<DeliveryRef>,
}
