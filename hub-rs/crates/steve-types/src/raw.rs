//! Validated JSON text with no Rust field access; TypeScript consumers must narrow unknown.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(type = "unknown"))]
pub struct RawJson(Box<RawValue>);

impl RawJson {
    /// Validate JSON without exposing a structured representation.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

impl std::str::FromStr for RawJson {
    type Err = serde_json::Error;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_str(text)
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for RawJson {}

// Tagged/untagged serde derives buffer their input into Content, which cannot carry opaque raw
// JSON. Decode directly from validated text instead, preserving the payload's exact bytes.
pub(crate) fn deserialize_with<'de, D, T>(
    d: D,
    decode: impl FnOnce(&str) -> Result<T, serde_json::Error>,
) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = RawJson::deserialize(d)?;
    decode(raw.0.get()).map_err(serde::de::Error::custom)
}
