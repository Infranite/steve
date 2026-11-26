//! Named constants, workspace-configurable policy settings, and deterministic rules for
//! confidence calculation, external trust isolation, and disclosure ceilings.
//!
//! Zero magic numbers and zero LLM confidence scoring: every threshold is named here with
//! its rationale and can be overridden via typed [`MemorySettings`].

use serde::{Deserialize, Serialize};
use steve_store::base::Level;
use steve_store::knowledge::{
    ConfidenceTier, MemoryEntryRow, MemoryKind, OriginClass, SourceTrustTier,
};
use steve_store::{Read, WorkspaceSettings};

/// Milliseconds in one day.
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Minimum Sørensen–Dice character bigram similarity required to rebind an edited block to its
/// existing sidecar row (Step 2 of anchor reconciliation).
pub const ANCHOR_REBIND_MIN_SIMILARITY: f64 = 0.65;

/// Minimum character bigram similarity required to consider a candidate blocked by a tombstone
/// in `rejected_patterns`.
pub const TOMBSTONE_MATCH_MIN_SIMILARITY: f64 = 0.65;

/// Minimum character bigram similarity used to detect near-duplicate memories / observations.
pub const NEAR_DUPLICATE_MIN_SIMILARITY: f64 = 0.72;

/// Maximum Hamming distance on 64-bit SimHash to qualify as a fast near-duplicate prefilter hit.
pub const SIMHASH_NEAR_MAX_DISTANCE: u32 = 8;

/// Minimum distinct independent episodes (`episode_key`) before an implicit observation promotes
/// into a corroborated file-backed memory entry.
pub const PROMOTE_MIN_EPISODES: i64 = 2;

/// Minimum distinct independent episodes before an implicit preference observation promotes.
pub const PROMOTE_PREFERENCE_MIN_EPISODES: i64 = 2;

/// Days an unpromoted observation stays in the `observations` buffer before automatic expiry.
pub const OBSERVATION_TTL_DAYS: i64 = 90;

/// Minimum new or updated atomic blocks in a file since its last `## Overview` refresh before
/// event-driven overview synthesis triggers immediately.
pub const SYNTHESIS_MIN_NEW_FACTS: i64 = 3;

/// Maximum days an existing `## Overview` waits with at least 1 dirty atomic block before refresh.
pub const SYNTHESIS_MAX_STALE_DAYS: i64 = 7;

/// Maximum lines rendered in the L0 memory directory index.
pub const L0_INDEX_MAX_LINES: usize = 120;

/// Character budget for L1 `me.md` personal context injection.
pub const L1_ME_MAX_CHARS: usize = 1_500;

/// Character budget for L1 per-entity (`people/`, `projects/`) context injection.
pub const L1_ENTITY_MAX_CHARS: usize = 800;

/// Character budget for L1 `repo_map` injection (used by Card 4).
pub const L1_REPO_MAP_MAX_CHARS: usize = 1_500;

/// Total token budget for automatic session context assembly.
pub const SESSION_CONTEXT_MAX_TOKENS: usize = 1_200;

/// Number of surrounding context messages attached when extracting evidence.
pub const EVIDENCE_CONTEXT_MESSAGES: usize = 5;

/// Revert-rate alert threshold: if `reverted / writes > 15%` in the rolling window, raise health alert.
pub const MEMORY_REVERT_ALERT_RATIO: f64 = 0.15;

/// Rolling window (days) for computing the silent-write revert ratio.
pub const MEMORY_REVERT_ALERT_WINDOW_DAYS: i64 = 30;

/// Minimum writes in the rolling window before the revert-rate alert evaluates.
pub const MEMORY_REVERT_ALERT_MIN_WRITES: i64 = 10;

/// Days without verification or use after which a non-verified entry is treated as stale.
pub const STALE_AFTER_DAYS: i64 = 180;

/// Quiet Feed digest cadence (days) summarizing recent silent writes with 1-click undo links.
pub const FEED_SUMMARY_INTERVAL_DAYS: i64 = 7;

/// Minimum character edits required for a human edit on an AI draft to count as a meaningful
/// `OwnerDraftDiff` learning signal (filters out typo/whitespace tweaks).
pub const DRAFT_DIFF_MIN_CHANGED_CHARS: usize = 6;

/// Maximum bigram similarity for a human draft edit to count as substantive (above this it is
/// treated as a minor typo tweak rather than a reusable preference/rule signal).
pub const DRAFT_DIFF_MAX_SIMILARITY: f64 = 0.96;

/// Maximum characters per extraction window when scanning long daily conversations.
pub const CHAT_WINDOW_MAX_CHARS: usize = 12_000;

/// Trailing context lines preserved across consecutive chat extraction windows.
pub const CHAT_WINDOW_OVERLAP_LINES: usize = 4;

/// Maximum characters passed into mail-thread extraction (trimmed from the oldest messages first
/// so the latest exchange and complete recent thread context are preserved).
pub const MAIL_THREAD_MAX_CHARS: usize = 12_000;

/// Maximum characters stored in `memory_evidence.rationale`.
pub const RATIONALE_MAX_CHARS: usize = 240;

/// Disclosure ceiling for every automatic silent write (`Level::Public < Conclusion < Reasoning < Never`).
pub const DEFAULT_SILENT_WRITE_LEVEL: Level = Level::Reasoning;

/// Workspace settings key storing typed [`MemorySettings`].
pub const SETTINGS_KEY: &str = "knowledge.memory_policy";

/// Typed workspace overrides for memory policy thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    pub promote_min_episodes: i64,
    pub promote_preference_min_episodes: i64,
    pub observation_ttl_days: i64,
    pub synthesis_min_new_facts: i64,
    pub synthesis_max_stale_days: i64,
    pub stale_after_days: i64,
    pub feed_summary_interval_days: i64,
    pub revert_alert_ratio: f64,
    pub revert_alert_window_days: i64,
    pub revert_alert_min_writes: i64,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            promote_min_episodes: PROMOTE_MIN_EPISODES,
            promote_preference_min_episodes: PROMOTE_PREFERENCE_MIN_EPISODES,
            observation_ttl_days: OBSERVATION_TTL_DAYS,
            synthesis_min_new_facts: SYNTHESIS_MIN_NEW_FACTS,
            synthesis_max_stale_days: SYNTHESIS_MAX_STALE_DAYS,
            stale_after_days: STALE_AFTER_DAYS,
            feed_summary_interval_days: FEED_SUMMARY_INTERVAL_DAYS,
            revert_alert_ratio: MEMORY_REVERT_ALERT_RATIO,
            revert_alert_window_days: MEMORY_REVERT_ALERT_WINDOW_DAYS,
            revert_alert_min_writes: MEMORY_REVERT_ALERT_MIN_WRITES,
        }
    }
}

impl MemorySettings {
    pub fn load(c: &mut impl Read, ws: &str) -> Self {
        let mut s: Self = WorkspaceSettings(c)
            .get(ws, SETTINGS_KEY)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        s.clamp();
        s
    }

    pub fn clamp(&mut self) {
        self.promote_min_episodes = self.promote_min_episodes.clamp(1, 10);
        self.promote_preference_min_episodes = self.promote_preference_min_episodes.clamp(1, 10);
        self.observation_ttl_days = self.observation_ttl_days.clamp(7, 365);
        self.synthesis_min_new_facts = self.synthesis_min_new_facts.clamp(1, 50);
        self.synthesis_max_stale_days = self.synthesis_max_stale_days.clamp(1, 90);
        self.stale_after_days = self.stale_after_days.clamp(30, 730);
        self.feed_summary_interval_days = self.feed_summary_interval_days.clamp(1, 30);
        self.revert_alert_ratio = self.revert_alert_ratio.clamp(0.05, 0.90);
        self.revert_alert_window_days = self.revert_alert_window_days.clamp(7, 180);
        self.revert_alert_min_writes = self.revert_alert_min_writes.clamp(3, 500);
    }

    /// Required independent episode count before an implicit observation promotes.
    pub fn min_episodes_for(&self, kind: MemoryKind) -> i64 {
        match kind {
            MemoryKind::Preference => self.promote_preference_min_episodes,
            _ => self.promote_min_episodes,
        }
    }
}

/// Deterministic input to [`compute_confidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfidenceInput {
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub episode_count: i64,
    pub correction_count: i64,
    pub has_conflict: bool,
    /// True when the owner explicitly stated a future-behavior rule/correction (e.g. rework with
    /// explicit scope + rule, or direct `steve_remember` / file edit).
    pub is_explicit_rule: bool,
    /// Minimum episodes required for `Corroborated` (default [`PROMOTE_MIN_EPISODES`]).
    pub promote_min_episodes: i64,
}

/// Computes [`ConfidenceTier`] purely from typed provenance rules — never from LLM scoring.
pub fn compute_confidence(input: &ConfidenceInput) -> ConfidenceTier {
    if input.has_conflict {
        return ConfidenceTier::Contested;
    }
    if input.correction_count > 0 {
        return ConfidenceTier::Verified;
    }
    match input.origin_class {
        OriginClass::OwnerFile | OriginClass::OwnerEdited => ConfidenceTier::Verified,
        OriginClass::OwnerExplicit if input.source_tier == SourceTrustTier::Tier1Owner => {
            ConfidenceTier::Verified
        }
        // Tier 3 external untrusted sources remain `Contested` until explicitly confirmed by owner.
        _ if input.source_tier == SourceTrustTier::Tier3External => ConfidenceTier::Contested,
        OriginClass::OwnerRework if input.source_tier == SourceTrustTier::Tier1Owner => {
            if input.episode_count >= input.promote_min_episodes.max(1) {
                ConfidenceTier::Corroborated
            } else {
                ConfidenceTier::Provisional
            }
        }
        OriginClass::OwnerDraftDiff
        | OriginClass::OwnerRework
        | OriginClass::OwnerExplicit
        | OriginClass::ImplicitPattern
        | OriginClass::ExternalEntity => {
            if input.episode_count >= input.promote_min_episodes.max(1) {
                ConfidenceTier::Corroborated
            } else {
                ConfidenceTier::Provisional
            }
        }
    }
}

/// Hard code-level security isolation: Tier 3 external sources (inbound emails, third-party IM,
/// untrusted docs) can NEVER write `me`/owner entries or any `Rule`/`Preference` entries.
pub fn is_external_blocked(
    source_tier: SourceTrustTier,
    subject_type: &str,
    kind: MemoryKind,
) -> bool {
    if source_tier != SourceTrustTier::Tier3External {
        return false;
    }
    subject_type == "me" || matches!(kind, MemoryKind::Rule | MemoryKind::Preference)
}

/// Enforces the disclosure ceiling on silent automatic writes:
/// `Level` orders `Public < Conclusion < Reasoning < Never`, so automatic silent writes are
/// clamped to at least [`DEFAULT_SILENT_WRITE_LEVEL`] (`Reasoning`).
pub fn enforce_disclosure_ceiling(requested: Level, is_auto_write: bool) -> Level {
    if is_auto_write {
        requested.max(DEFAULT_SILENT_WRITE_LEVEL)
    } else {
        requested
    }
}

/// Whether an entry is stale due to age without verification or recent use.
pub fn is_stale(entry: &MemoryEntryRow, now_ms: i64, stale_after_days: i64) -> bool {
    if entry.pinned || entry.confidence_tier == ConfidenceTier::Verified {
        return false;
    }
    let last_touch = entry
        .last_used_at
        .into_iter()
        .chain(entry.verified_at)
        .chain(std::iter::once(entry.updated_at))
        .max()
        .unwrap_or(entry.updated_at);
    now_ms.saturating_sub(last_touch) >= stale_after_days.max(1) * DAY_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_confidence_rules() {
        let base = ConfidenceInput {
            origin_class: OriginClass::ImplicitPattern,
            source_tier: SourceTrustTier::Tier2Indirect,
            episode_count: 1,
            correction_count: 0,
            has_conflict: false,
            is_explicit_rule: false,
            promote_min_episodes: PROMOTE_MIN_EPISODES,
        };
        assert_eq!(compute_confidence(&base), ConfidenceTier::Provisional);
        assert_eq!(
            compute_confidence(&ConfidenceInput {
                episode_count: 2,
                ..base
            }),
            ConfidenceTier::Corroborated
        );
        assert_eq!(
            compute_confidence(&ConfidenceInput {
                origin_class: OriginClass::OwnerExplicit,
                source_tier: SourceTrustTier::Tier1Owner,
                ..base
            }),
            ConfidenceTier::Verified
        );
        assert_eq!(
            compute_confidence(&ConfidenceInput {
                origin_class: OriginClass::OwnerFile,
                source_tier: SourceTrustTier::Tier1Owner,
                ..base
            }),
            ConfidenceTier::Verified
        );
        assert_eq!(
            compute_confidence(&ConfidenceInput {
                correction_count: 1,
                ..base
            }),
            ConfidenceTier::Verified
        );
        assert_eq!(
            compute_confidence(&ConfidenceInput {
                has_conflict: true,
                origin_class: OriginClass::OwnerExplicit,
                ..base
            }),
            ConfidenceTier::Contested
        );
    }

    #[test]
    fn external_isolation_and_disclosure_ceiling() {
        assert!(is_external_blocked(
            SourceTrustTier::Tier3External,
            "me",
            MemoryKind::Fact
        ));
        assert!(is_external_blocked(
            SourceTrustTier::Tier3External,
            "party",
            MemoryKind::Rule
        ));
        assert!(is_external_blocked(
            SourceTrustTier::Tier3External,
            "party",
            MemoryKind::Preference
        ));
        assert!(!is_external_blocked(
            SourceTrustTier::Tier3External,
            "party",
            MemoryKind::Fact
        ));
        assert!(!is_external_blocked(
            SourceTrustTier::Tier3External,
            "party",
            MemoryKind::Event
        ));
        assert!(!is_external_blocked(
            SourceTrustTier::Tier1Owner,
            "me",
            MemoryKind::Rule
        ));

        assert_eq!(
            enforce_disclosure_ceiling(Level::Public, true),
            Level::Reasoning
        );
        assert_eq!(
            enforce_disclosure_ceiling(Level::Conclusion, true),
            Level::Reasoning
        );
        assert_eq!(
            enforce_disclosure_ceiling(Level::Reasoning, true),
            Level::Reasoning
        );
        assert_eq!(enforce_disclosure_ceiling(Level::Never, true), Level::Never);
        assert_eq!(
            enforce_disclosure_ceiling(Level::Public, false),
            Level::Public
        );
    }
}
