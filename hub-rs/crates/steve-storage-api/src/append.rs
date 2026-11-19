//! Append stream identities. Sequence allocation follows commit order within a stream.
use serde::{Deserialize, Serialize};
pub use steve_types::Seq;

/// Backend-independent name of an append-only stream in the request's domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamName(pub String);
