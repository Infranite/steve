//! Durable outbox facts are independent of the lossy live bus. Wave 2 fills this contract
//! alongside storage; there are deliberately no variants in the W1.1 skeleton.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DurableFact {}
