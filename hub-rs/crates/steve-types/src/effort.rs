//! Canonical reasoning effort vocabulary and adapter-layer mapping contracts.
//!
//! Storage, Hub APIs, Node wire contracts, and the web client speak only Steve's five-level
//! [`ReasoningEffort`] scale (`default` / `low` / `medium` / `high` / `max`). Translating a level
//! into an adapter's native ACP `configOption`, a sibling model variant, or an explicit
//! unsupported fallback belongs exclusively to the harness adapter layer.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Steve's canonical reasoning effort scale.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ReasoningEffort {
    /// Leave reasoning effort to the harness / model default.
    #[default]
    Default,
    Low,
    Medium,
    High,
    Max,
}

impl ReasoningEffort {
    /// All five Steve reasoning effort levels in ascending order.
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Max,
    ];

    /// The four explicit (non-default) effort levels in ascending intensity order.
    pub const EXPLICIT: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Max];

    /// Canonical snake_case wire/storage value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Strictly parses Steve's canonical reasoning effort value. Harness-native strings
    /// (`xhigh`, `ultra`, `minimal`, `none`, `off`, etc.) return `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "default" => Self::Default,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            _ => return None,
        })
    }

    /// Deterministic migration mapping from pre-v138 harness-native effort strings onto Steve's
    /// five canonical levels. Unmappable values map to [`Self::Default`].
    pub fn from_legacy_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" | "low" => Self::Low,
            "medium" | "med" => Self::Medium,
            "high" => Self::High,
            "xhigh" | "max" | "ultra" => Self::Max,
            _ => Self::Default,
        }
    }

    /// True when this value represents [`Self::Default`].
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Default)
    }

    /// Returns `Some(self)` when explicit (`low..=max`), or `None` for [`Self::Default`].
    pub const fn non_default(self) -> Option<Self> {
        match self {
            Self::Default => None,
            other => Some(other),
        }
    }

    /// Zero-based index among [`Self::EXPLICIT`] (`Low=0, Medium=1, High=2, Max=3`).
    pub const fn explicit_ordinal(self) -> Option<usize> {
        match self {
            Self::Default => None,
            Self::Low => Some(0),
            Self::Medium => Some(1),
            Self::High => Some(2),
            Self::Max => Some(3),
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Declared adapter-level mechanism for applying [`ReasoningEffort`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum EffortMode {
    /// Apply via ACP `thought_level` / `effort` / `reasoning_effort` / `thinking` `configOption`
    /// using deterministic ordinal mapping over the model's reported options.
    #[default]
    AcpOption,
    /// Apply by switching to a sibling model variant in the same parsed model family.
    ModelVariant,
    /// Harness exposes no reasoning effort control; always resolves to `default`.
    Unsupported,
}

/// Resolved runtime strategy for a specific `(harness, model, effort)` tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum EffortStrategy {
    /// Mapped to an ACP `configOption` value.
    AcpOption,
    /// Mapped to a sibling model variant in the same family.
    ModelVariant,
    /// Model or harness does not expose effort control; runs at model default.
    Unsupported,
}

/// Deterministic mapping of one canonical [`ReasoningEffort`] on a specific harness and model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EffortMapping {
    /// Steve's requested canonical effort level.
    pub requested: ReasoningEffort,
    /// Strategy that applies for this harness and model.
    pub strategy: EffortStrategy,
    /// Whether this level is actively supported on this harness and model (`default` is always
    /// supported; explicit levels are false when unsupported).
    pub supported: bool,
    /// Native ACP config option value when `strategy == AcpOption` (e.g. `"xhigh"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub native_effort: Option<String>,
    /// Target sibling model id when `strategy == ModelVariant` (e.g. `"gemini-4-neon-xhigh"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub target_model: Option<String>,
    /// Canonical Steve effort actually in force (`requested` when supported, else `default`).
    pub effective: ReasoningEffort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_effort_roundtrips_and_rejects_native_names() {
        for e in ReasoningEffort::ALL {
            assert_eq!(ReasoningEffort::parse(e.as_str()), Some(e));
            let wire = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<ReasoningEffort>(&wire).unwrap(), e);
        }
        for bad in [
            "", "xhigh", "ultra", "minimal", "none", "off", "turbo", "HIGH",
        ] {
            assert_eq!(ReasoningEffort::parse(bad), None, "{bad}");
            assert!(
                serde_json::from_str::<ReasoningEffort>(&format!("\"{bad}\"")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn legacy_migration_mapping_is_deterministic() {
        assert_eq!(
            ReasoningEffort::from_legacy_str("minimal"),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("low"),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("medium"),
            ReasoningEffort::Medium
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("high"),
            ReasoningEffort::High
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("xhigh"),
            ReasoningEffort::Max
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("max"),
            ReasoningEffort::Max
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("ultra"),
            ReasoningEffort::Max
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("none"),
            ReasoningEffort::Default
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("off"),
            ReasoningEffort::Default
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("default"),
            ReasoningEffort::Default
        );
        assert_eq!(
            ReasoningEffort::from_legacy_str("unknown"),
            ReasoningEffort::Default
        );
    }
}
