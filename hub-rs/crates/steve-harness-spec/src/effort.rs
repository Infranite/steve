//! Deterministic mapping between Steve's five canonical [`ReasoningEffort`] levels and
//! harness-native mechanisms.
//!
//! Three adapter-layer strategies are supported without hardcoding model names:
//! 1. [`EffortStrategy::AcpOption`]: ordinal mapping over a model's ACP `thought_level` /
//!    `effort` / `reasoning_effort` / `thinking` option list.
//! 2. [`EffortStrategy::ModelVariant`]: rule-based `(family_base, effort_token)` parsing over
//!    the harness's available models, switching to a sibling variant when the family exposes
//!    at least [`MIN_VARIANT_SIBLINGS`] distinct effort-suffixed models.
//! 3. [`EffortStrategy::Unsupported`]: falls back to model default without failing the turn.

use std::collections::BTreeMap;

use steve_node_api::{EffortMapping, EffortMode, EffortStrategy, ReasoningEffort};

/// Minimum distinct effort-suffixed siblings required in one model family before
/// [`EffortMode::ModelVariant`] treats the family as variant-controlled.
/// Prevents standalone models whose name happens to end in an effort word (e.g.
/// `gpt-oss-120b-medium`) from being misclassified as effort-switchable.
pub const MIN_VARIANT_SIBLINGS: usize = 2;

/// Native ACP sentinel value meaning "leave effort to the model / adapter default".
const DEFAULT_OPTION_VALUE: &str = "default";

/// Ordered effort tokens recognized in model variant identifiers (ascending intensity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffortToken {
    Minimal = 0,
    Low = 1,
    Medium = 2,
    High = 3,
    XHigh = 4,
    Max = 5,
    Ultra = 6,
}

impl EffortToken {
    /// Parse a single effort suffix token (case-insensitive).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }
}

/// A model id split into its family base and trailing effort token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVariant<'a> {
    /// Family identifier with any route prefix preserved (e.g. `"gemini-4-neon"` or
    /// `"astergate/antigravity/gemini-4-neon"`).
    pub family_base: &'a str,
    pub token: EffortToken,
    /// Original suffix token as written in the model id (e.g. `"xhigh"`).
    pub raw_token: &'a str,
}

/// One effort-suffixed sibling belonging to a model family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingVariant {
    pub token: EffortToken,
    pub model_id: String,
}

/// Inputs required to map a [`ReasoningEffort`] for one harness + model pair.
#[derive(Debug, Clone, Copy)]
pub struct EffortContext<'a> {
    pub mode: EffortMode,
    pub model: Option<&'a str>,
    /// Native ACP option values reported for `model` (may include `"default"`, which is
    /// filtered out before ordinal indexing). Empty means the model reports no ACP effort option.
    pub acp_options: &'a [String],
    /// Concrete model ids known for this harness, used when `mode == EffortMode::ModelVariant`.
    pub available_models: &'a [String],
}

/// Rule-based parser for `(family_base, effort_token)` model variant ids.
///
/// Recognizes:
/// - bracket syntax: `<base>[<token>]` (e.g. `gpt-6.2-sol[xhigh]`)
/// - hyphen/underscore suffix: `<base>-<token>` or `<base>_<token>` (e.g. `gemini-4-neon-xhigh`)
///
/// Route prefixes (`provider/family-high`) are preserved inside `family_base`.
pub fn parse_model_variant(model_id: &str) -> Option<ParsedVariant<'_>> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 1. Bracket syntax: `<base>[<token>]`
    if let Some(inner_start) = trimmed.rfind('[')
        && trimmed.ends_with(']')
    {
        let base = &trimmed[..inner_start];
        let raw_token = &trimmed[inner_start + 1..trimmed.len() - 1];
        if !base.is_empty()
            && !base.ends_with('/')
            && let Some(token) = EffortToken::parse(raw_token)
        {
            return Some(ParsedVariant {
                family_base: base,
                token,
                raw_token,
            });
        }
    }
    // 2. Trailing `-<token>` or `_<token>` inside the leaf segment (after any `/`).
    let leaf_start = trimmed.rfind('/').map_or(0, |i| i + 1);
    let leaf = &trimmed[leaf_start..];
    let sep_rel = leaf.rfind(['-', '_'])?;
    let base_end = leaf_start + sep_rel;
    let base = &trimmed[..base_end];
    let raw_token = &trimmed[base_end + 1..];
    if base.is_empty() || base.ends_with(['/', '-', '_']) || raw_token.is_empty() {
        return None;
    }
    let token = EffortToken::parse(raw_token)?;
    Some(ParsedVariant {
        family_base: base,
        token,
        raw_token,
    })
}

/// Discover ordered effort-suffixed siblings sharing `model`'s family base in `available_models`.
///
/// Returns an empty vector when fewer than [`MIN_VARIANT_SIBLINGS`] distinct effort tokens exist
/// for that family.
pub fn sibling_variants(model: &str, available_models: &[String]) -> Vec<SiblingVariant> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let family = parse_model_variant(trimmed).map_or(trimmed, |p| p.family_base);
    let mut by_token: BTreeMap<EffortToken, String> = BTreeMap::new();
    for candidate in available_models {
        let Some(parsed) = parse_model_variant(candidate) else {
            continue;
        };
        if parsed.family_base == family {
            by_token
                .entry(parsed.token)
                .or_insert_with(|| candidate.trim().to_owned());
        }
    }
    if by_token.len() < MIN_VARIANT_SIBLINGS {
        return Vec::new();
    }
    by_token
        .into_iter()
        .map(|(token, model_id)| SiblingVariant { token, model_id })
        .collect()
}

/// Filter out `"default"` and empty/duplicate entries while preserving ascending order.
pub fn explicit_acp_options(raw: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(raw.len());
    for opt in raw {
        let s = opt.trim();
        if s.is_empty() || s.eq_ignore_ascii_case(DEFAULT_OPTION_VALUE) {
            continue;
        }
        if !out.iter().any(|existing: &String| existing == s) {
            out.push(s.to_owned());
        }
    }
    out
}

/// Deterministic monotonic ordinal index from Steve's four explicit levels (`Low..=Max`) onto
/// `option_count` ascending native levels:
/// - `0` → `None` (unsupported)
/// - `1` → `0` for all explicit levels
/// - `2` → `Low|Medium → 0`, `High|Max → 1`
/// - `3` → `Low → 0`, `Medium → 1`, `High|Max → 2`
/// - `4` → exact bijection (`0, 1, 2, 3`)
/// - `≥ 5` → `Low → 0`, `Medium → (n - 1) / 3`, `High → (2 * (n - 1)) / 3`, `Max → n - 1`
///   - `n = 5` (e.g. Claude/Pi `[low, medium, high, xhigh, max]`): `0, 1, 2, 4` (`low, medium, high, max`)
///   - `n = 6` (e.g. Codex `[low, medium, high, xhigh, max, ultra]`): `0, 1, 3, 5` (`low, medium, xhigh, ultra`)
pub const fn ordinal_index(effort: ReasoningEffort, option_count: usize) -> Option<usize> {
    if option_count == 0 {
        return None;
    }
    let Some(ord) = effort.explicit_ordinal() else {
        return None;
    };
    let idx = match option_count {
        1 => 0,
        2 => match effort {
            ReasoningEffort::Low | ReasoningEffort::Medium => 0,
            ReasoningEffort::High | ReasoningEffort::Max => 1,
            ReasoningEffort::Default => return None,
        },
        3 => match effort {
            ReasoningEffort::Low => 0,
            ReasoningEffort::Medium => 1,
            ReasoningEffort::High | ReasoningEffort::Max => 2,
            ReasoningEffort::Default => return None,
        },
        4 => ord,
        n => match effort {
            ReasoningEffort::Low => 0,
            ReasoningEffort::Medium => (n - 1) / 3,
            ReasoningEffort::High => (2 * (n - 1)) / 3,
            ReasoningEffort::Max => n - 1,
            ReasoningEffort::Default => return None,
        },
    };
    Some(idx)
}

/// Resolve how a single canonical [`ReasoningEffort`] executes under `ctx`.
/// Never fails: unsupported combinations resolve to [`EffortStrategy::Unsupported`] (`supported: false`)
/// so the runtime runs at model default with an explicit record.
pub fn resolve_for_model(requested: ReasoningEffort, ctx: &EffortContext<'_>) -> EffortMapping {
    let options = explicit_acp_options(ctx.acp_options);
    let siblings = if ctx.mode == EffortMode::ModelVariant {
        ctx.model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| sibling_variants(m, ctx.available_models))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let model_strategy = match ctx.mode {
        EffortMode::AcpOption if !options.is_empty() => EffortStrategy::AcpOption,
        EffortMode::ModelVariant if !siblings.is_empty() => EffortStrategy::ModelVariant,
        _ => EffortStrategy::Unsupported,
    };

    if requested.is_default() {
        return EffortMapping {
            requested,
            strategy: model_strategy,
            supported: true,
            native_effort: None,
            target_model: None,
            effective: ReasoningEffort::Default,
        };
    }

    match model_strategy {
        EffortStrategy::AcpOption => {
            if let Some(idx) = ordinal_index(requested, options.len()) {
                EffortMapping {
                    requested,
                    strategy: EffortStrategy::AcpOption,
                    supported: true,
                    native_effort: Some(options[idx].clone()),
                    target_model: None,
                    effective: requested,
                }
            } else {
                EffortMapping {
                    requested,
                    strategy: EffortStrategy::Unsupported,
                    supported: false,
                    native_effort: None,
                    target_model: None,
                    effective: ReasoningEffort::Default,
                }
            }
        }
        EffortStrategy::ModelVariant => {
            if let Some(idx) = ordinal_index(requested, siblings.len()) {
                EffortMapping {
                    requested,
                    strategy: EffortStrategy::ModelVariant,
                    supported: true,
                    native_effort: None,
                    target_model: Some(siblings[idx].model_id.clone()),
                    effective: requested,
                }
            } else {
                EffortMapping {
                    requested,
                    strategy: EffortStrategy::Unsupported,
                    supported: false,
                    native_effort: None,
                    target_model: None,
                    effective: ReasoningEffort::Default,
                }
            }
        }
        EffortStrategy::Unsupported => EffortMapping {
            requested,
            strategy: EffortStrategy::Unsupported,
            supported: false,
            native_effort: None,
            target_model: None,
            effective: ReasoningEffort::Default,
        },
    }
}

/// Resolve all five canonical [`ReasoningEffort`] levels in fixed order (`Default..=Max`).
pub fn resolve_all_levels(ctx: &EffortContext<'_>) -> Vec<EffortMapping> {
    ReasoningEffort::ALL
        .into_iter()
        .map(|level| resolve_for_model(level, ctx))
        .collect()
}

/// Map a harness-native ACP effort string observed in a live notification back to Steve's
/// canonical [`ReasoningEffort`].
pub fn reverse_map_native(native: &str, acp_options: &[String]) -> ReasoningEffort {
    let trimmed = native.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(DEFAULT_OPTION_VALUE) {
        return ReasoningEffort::Default;
    }
    let options = explicit_acp_options(acp_options);
    if !options.is_empty() {
        for level in ReasoningEffort::EXPLICIT {
            if let Some(idx) = ordinal_index(level, options.len())
                && options[idx].eq_ignore_ascii_case(trimmed)
            {
                return level;
            }
        }
    }
    ReasoningEffort::from_legacy_str(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn codex_six_level_fixture_maps_high_to_xhigh_and_max_to_ultra() {
        // Real recorded fixture from codex-acp 2.12.0 + codex-cli 0.170.0 (`gpt-6.2-sol`)
        let opts = s(&["low", "medium", "high", "xhigh", "max", "ultra"]);
        let ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("gpt-6.2-sol"),
            acp_options: &opts,
            available_models: &[],
        };
        let all = resolve_all_levels(&ctx);
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].requested, ReasoningEffort::Default);
        assert_eq!(all[0].effective, ReasoningEffort::Default);
        assert_eq!(all[0].strategy, EffortStrategy::AcpOption);
        assert!(all[0].supported);
        assert_eq!(all[1].native_effort.as_deref(), Some("low"));
        assert_eq!(all[2].native_effort.as_deref(), Some("medium"));
        assert_eq!(all[3].native_effort.as_deref(), Some("xhigh"));
        assert_eq!(all[4].native_effort.as_deref(), Some("ultra"));
        assert!(all.iter().all(|m| m.supported));
    }

    #[test]
    fn claude_code_fixtures_cover_five_four_and_unsupported_models() {
        // Real recorded fixtures from claude-agent-acp 0.112.0 + claude 2.1.311
        let opus5 = s(&["default", "low", "medium", "high", "xhigh", "max"]);
        let ctx5 = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("claude-opus-5-5"),
            acp_options: &opus5,
            available_models: &[],
        };
        let mapped5 = resolve_all_levels(&ctx5);
        assert_eq!(mapped5[1].native_effort.as_deref(), Some("low"));
        assert_eq!(mapped5[2].native_effort.as_deref(), Some("medium"));
        assert_eq!(mapped5[3].native_effort.as_deref(), Some("high"));
        assert_eq!(mapped5[4].native_effort.as_deref(), Some("max"));

        // `claude-opus-4-5-20251101` reports ["default", "low", "medium", "high"] -> 3 explicit levels
        let opus45 = s(&["default", "low", "medium", "high"]);
        let ctx45 = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("claude-opus-4-5-20251101"),
            acp_options: &opus45,
            available_models: &[],
        };
        let mapped45 = resolve_all_levels(&ctx45);
        assert_eq!(mapped45[1].native_effort.as_deref(), Some("low"));
        assert_eq!(mapped45[2].native_effort.as_deref(), Some("medium"));
        assert_eq!(mapped45[3].native_effort.as_deref(), Some("high"));
        assert_eq!(mapped45[4].native_effort.as_deref(), Some("high"));

        // `claude-haiku-4-5-20251001` omits `effort` configOption completely
        let haiku_ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("claude-haiku-4-5-20251001"),
            acp_options: &[],
            available_models: &[],
        };
        let mapped_haiku = resolve_all_levels(&haiku_ctx);
        assert!(mapped_haiku[0].supported);
        for m in &mapped_haiku[1..] {
            assert_eq!(m.strategy, EffortStrategy::Unsupported);
            assert!(!m.supported);
            assert_eq!(m.native_effort, None);
            assert_eq!(m.target_model, None);
            assert_eq!(m.effective, ReasoningEffort::Default);
        }
    }

    #[test]
    fn antigravity_fixtures_separate_supported_from_orchestrator_and_standalone_models() {
        // Real recorded fixtures from agy 1.5.17 (`agy acp`)
        let neon_opts = s(&["low", "medium", "high", "xhigh"]);
        let neon_ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("gemini-4-neon"),
            acp_options: &neon_opts,
            available_models: &[],
        };
        let neon = resolve_all_levels(&neon_ctx);
        assert_eq!(neon[1].native_effort.as_deref(), Some("low"));
        assert_eq!(neon[2].native_effort.as_deref(), Some("medium"));
        assert_eq!(neon[3].native_effort.as_deref(), Some("high"));
        assert_eq!(neon[4].native_effort.as_deref(), Some("xhigh"));

        // `gemini-pro-orchestrator-neon` omits `thought_level` -> Unsupported (never errors)
        let orch_ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("gemini-pro-orchestrator-neon"),
            acp_options: &[],
            available_models: &[],
        };
        let orch = resolve_for_model(ReasoningEffort::High, &orch_ctx);
        assert_eq!(orch.strategy, EffortStrategy::Unsupported);
        assert!(!orch.supported);
        assert_eq!(orch.effective, ReasoningEffort::Default);
    }

    #[test]
    fn pi_fixtures_map_five_and_three_level_models() {
        // Real recorded fixtures from @openma/pi-acp 0.1.8 + pi 1.3.0
        let sol = s(&["low", "medium", "high", "xhigh", "max"]);
        let sol_ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("astergate/gpt-6.2-sol"),
            acp_options: &sol,
            available_models: &[],
        };
        assert_eq!(
            resolve_for_model(ReasoningEffort::High, &sol_ctx)
                .native_effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            resolve_for_model(ReasoningEffort::Max, &sol_ctx)
                .native_effort
                .as_deref(),
            Some("max")
        );

        let flash = s(&["low", "medium", "high"]);
        let flash_ctx = EffortContext {
            mode: EffortMode::AcpOption,
            model: Some("astergate/antigravity/gemini-2.5-flash"),
            acp_options: &flash,
            available_models: &[],
        };
        assert_eq!(
            resolve_for_model(ReasoningEffort::Max, &flash_ctx)
                .native_effort
                .as_deref(),
            Some("high")
        );
    }

    #[test]
    fn model_variant_strategy_uses_rule_based_siblings_and_rejects_singleton_names() {
        // Real agy `models.availableModels` list (subset with families + standalone models)
        let available = s(&[
            "gemini-4-neon-low",
            "gemini-4-neon-medium",
            "gemini-4-neon-high",
            "gemini-4-neon-xhigh",
            "gemini-4-carbon-low",
            "gemini-4-carbon-medium",
            "gemini-4-carbon-high",
            "gemini-pro-orchestrator-neon",
            "gpt-oss-120b-medium",
            "route/gpt-6.2-sol[low]",
            "route/gpt-6.2-sol[medium]",
            "route/gpt-6.2-sol[high]",
            "route/gpt-6.2-sol[xhigh]",
            "route/gpt-6.2-sol[max]",
            "route/gpt-6.2-sol[ultra]",
        ]);

        // 4-sibling family: `gemini-4-neon` + Max -> `gemini-4-neon-xhigh`
        let neon_ctx = EffortContext {
            mode: EffortMode::ModelVariant,
            model: Some("gemini-4-neon"),
            acp_options: &[],
            available_models: &available,
        };
        let mapped = resolve_all_levels(&neon_ctx);
        assert_eq!(mapped[0].requested, ReasoningEffort::Default);
        assert_eq!(mapped[0].strategy, EffortStrategy::ModelVariant);
        assert!(mapped[0].supported);
        assert_eq!(mapped[1].target_model.as_deref(), Some("gemini-4-neon-low"));
        assert_eq!(
            mapped[2].target_model.as_deref(),
            Some("gemini-4-neon-medium")
        );
        assert_eq!(
            mapped[3].target_model.as_deref(),
            Some("gemini-4-neon-high")
        );
        assert_eq!(
            mapped[4].target_model.as_deref(),
            Some("gemini-4-neon-xhigh")
        );
        assert!(
            mapped[1..]
                .iter()
                .all(|m| m.strategy == EffortStrategy::ModelVariant && m.supported)
        );

        // Route-prefixed bracket family: 6 siblings -> High maps to `[xhigh]`, Max to `[ultra]`
        let bracket_ctx = EffortContext {
            mode: EffortMode::ModelVariant,
            model: Some("route/gpt-6.2-sol"),
            acp_options: &[],
            available_models: &available,
        };
        assert_eq!(
            resolve_for_model(ReasoningEffort::High, &bracket_ctx)
                .target_model
                .as_deref(),
            Some("route/gpt-6.2-sol[xhigh]")
        );
        assert_eq!(
            resolve_for_model(ReasoningEffort::Max, &bracket_ctx)
                .target_model
                .as_deref(),
            Some("route/gpt-6.2-sol[ultra]")
        );

        // Standalone `gpt-oss-120b-medium` has only 1 entry (not >= 2 siblings) -> Unsupported!
        let singleton_ctx = EffortContext {
            mode: EffortMode::ModelVariant,
            model: Some("gpt-oss-120b-medium"),
            acp_options: &[],
            available_models: &available,
        };
        let singleton = resolve_for_model(ReasoningEffort::High, &singleton_ctx);
        assert_eq!(singleton.strategy, EffortStrategy::Unsupported);
        assert!(!singleton.supported);
        assert_eq!(singleton.target_model, None);

        // Model without siblings -> Unsupported
        let orch_ctx = EffortContext {
            mode: EffortMode::ModelVariant,
            model: Some("gemini-pro-orchestrator-neon"),
            acp_options: &[],
            available_models: &available,
        };
        assert!(!resolve_for_model(ReasoningEffort::Low, &orch_ctx).supported);
    }

    #[test]
    fn unsupported_mode_always_keeps_default_supported_and_marks_explicit_unsupported() {
        let opts = s(&["low", "high"]);
        let ctx = EffortContext {
            mode: EffortMode::Unsupported,
            model: Some("any-model"),
            acp_options: &opts,
            available_models: &[],
        };
        let all = resolve_all_levels(&ctx);
        assert!(all[0].supported && all[0].strategy == EffortStrategy::Unsupported);
        assert_eq!(all[0].effective, ReasoningEffort::Default);
        for m in &all[1..] {
            assert!(!m.supported && m.strategy == EffortStrategy::Unsupported);
            assert_eq!(m.effective, ReasoningEffort::Default);
        }
    }

    #[test]
    fn reverse_map_recovers_canonical_effort_from_native_values() {
        let codex = s(&["low", "medium", "high", "xhigh", "max", "ultra"]);
        assert_eq!(
            reverse_map_native("default", &codex),
            ReasoningEffort::Default
        );
        assert_eq!(reverse_map_native("low", &codex), ReasoningEffort::Low);
        assert_eq!(
            reverse_map_native("medium", &codex),
            ReasoningEffort::Medium
        );
        assert_eq!(reverse_map_native("xhigh", &codex), ReasoningEffort::High);
        assert_eq!(reverse_map_native("ultra", &codex), ReasoningEffort::Max);
    }
}
