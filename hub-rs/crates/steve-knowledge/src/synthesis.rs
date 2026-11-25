//! Event-driven `## Overview` synthesis, quiet weekly Feed summary, and observation buffer expiry.
//!
//! Hard invariants:
//! - Zero-change days make **0 model calls** (`steve_memory_refresh_skipped_total`).
//! - Overview synthesis reads ONLY active atomic blocks (never old overview prose) to prevent
//!   recursive summary drift.
//! - Replaces the legacy 03:40 `kept_notice` interruption with a single quiet weekly Feed digest
//!   plus a revert-rate health alert (`MEMORY_REVERT_ALERT_RATIO`).

use std::collections::HashSet;

use steve_modules::{ApiError, ApiResult, Modules, Tier};
use steve_store::knowledge::{
    Documents, MemoryEntries, MemoryKind, MemoryRevisions, Observations, SourceRole, SourceRow,
};
use steve_store::{Read, W};

use crate::memory_file::{self, parse_memory_file, read_memory_file_from_disk};
use crate::policy::{DAY_MS, MemorySettings};

/// Deterministic decision returned by [`evaluate_overview_trigger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewTrigger {
    SkipNoChange,
    SkipBelowThreshold,
    RefreshDirtyCount,
    RefreshStaleAge,
}

impl OverviewTrigger {
    pub fn should_refresh(self) -> bool {
        matches!(self, Self::RefreshDirtyCount | Self::RefreshStaleAge)
    }

    pub fn skip_reason(self) -> Option<&'static str> {
        match self {
            Self::SkipNoChange => Some("no_change"),
            Self::SkipBelowThreshold => Some("below_threshold"),
            _ => None,
        }
    }
}

/// Pure deterministic gate for event-driven overview refresh:
/// - `dirty_count == 0` -> `SkipNoChange` (0 model calls)
/// - `dirty_count >= settings.synthesis_min_new_facts` -> `RefreshDirtyCount`
/// - `dirty_count >= 1` and oldest dirty age >= `settings.synthesis_max_stale_days` -> `RefreshStaleAge`
/// - else -> `SkipBelowThreshold` (0 model calls)
pub fn evaluate_overview_trigger(
    dirty_count: i64,
    oldest_dirty_at: Option<i64>,
    now: i64,
    settings: &MemorySettings,
) -> OverviewTrigger {
    if dirty_count <= 0 {
        return OverviewTrigger::SkipNoChange;
    }
    if dirty_count >= settings.synthesis_min_new_facts {
        return OverviewTrigger::RefreshDirtyCount;
    }
    if let Some(oldest) = oldest_dirty_at
        && now.saturating_sub(oldest) >= settings.synthesis_max_stale_days * DAY_MS
    {
        return OverviewTrigger::RefreshStaleAge;
    }
    OverviewTrigger::SkipBelowThreshold
}

/// Summary of one overview refresh check/execution (for telemetry and tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewRefreshResult {
    pub trigger: OverviewTrigger,
    pub model_called: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub updated: bool,
}

/// Evaluates and, if triggered, refreshes `## Overview` for one memory Markdown file.
/// Feeds ONLY active atomic blocks (never old overview text) to avoid recursive summary drift.
pub async fn maybe_refresh_overview(
    m: &Modules,
    source: &SourceRow,
    rel_path: &str,
    now: i64,
) -> ApiResult<OverviewRefreshResult> {
    if source.role != SourceRole::Memory {
        return Ok(OverviewRefreshResult {
            trigger: OverviewTrigger::SkipNoChange,
            model_called: false,
            input_tokens: 0,
            output_tokens: 0,
            updated: false,
        });
    }
    let src_id = source.id.clone();
    let ws = source.workspace_id.clone();
    let rel = rel_path.to_string();
    let (trigger, _settings) = m
        .blocking(move |m| {
            crate::read_api(&m.store, |c| {
                let settings = MemorySettings::load(c, &ws);
                let last_refresh = MemoryRevisions(c)
                    .last_overview_refresh(&src_id, &rel)?
                    .unwrap_or(0);
                let (dirty_count, oldest_dirty) =
                    MemoryEntries(c).count_dirty_atomic_since(&src_id, &rel, last_refresh)?;
                let trig = evaluate_overview_trigger(dirty_count, oldest_dirty, now, &settings);
                Ok::<_, ApiError>((trig, settings))
            })
        })
        .await?;

    if let Some(reason) = trigger.skip_reason() {
        metrics::counter!(
            "steve_memory_refresh_skipped_total",
            "kind" => "overview",
            "reason" => reason
        )
        .increment(1);
        return Ok(OverviewRefreshResult {
            trigger,
            model_called: false,
            input_tokens: 0,
            output_tokens: 0,
            updated: false,
        });
    }

    // Read ONLY active atomic blocks from disk (never old `## Overview` prose).
    let Some(raw_md) = read_memory_file_from_disk(source, rel_path)? else {
        return Ok(OverviewRefreshResult {
            trigger: OverviewTrigger::SkipNoChange,
            model_called: false,
            input_tokens: 0,
            output_tokens: 0,
            updated: false,
        });
    };
    let parsed = parse_memory_file(rel_path, &raw_md);
    let atomic_bullets: Vec<String> = parsed
        .blocks
        .iter()
        .filter(|b| b.kind != MemoryKind::Overview)
        .map(|b| format!("- [{}] {}", b.kind.code(), b.text))
        .collect();
    if atomic_bullets.is_empty() {
        metrics::counter!(
            "steve_memory_refresh_skipped_total",
            "kind" => "overview",
            "reason" => "no_change"
        )
        .increment(1);
        return Ok(OverviewRefreshResult {
            trigger: OverviewTrigger::SkipNoChange,
            model_called: false,
            input_tokens: 0,
            output_tokens: 0,
            updated: false,
        });
    }

    let prompt = format!(
        "Write a concise 2-4 sentence overview for `{rel_path}` strictly summarizing ONLY the atomic facts/rules below. Do not invent anything not listed.\n\n{}",
        atomic_bullets.join("\n")
    );
    let answer = steve_modules::gateway::tagged(
        "memory_overview",
        &source.workspace_id,
        m.gateway.run(Tier::Fast, &prompt, crate::extract::TIMEOUT),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("gateway: {e}")))?;

    let input_tokens: u64 = answer
        .usage
        .iter()
        .map(|u| {
            u.tokens.input.unwrap_or(0).max(0) as u64
                + u.tokens.cache_read.unwrap_or(0).max(0) as u64
        })
        .sum();
    let output_tokens: u64 = answer
        .usage
        .iter()
        .map(|u| u.tokens.output.unwrap_or(0).max(0) as u64)
        .sum();
    metrics::counter!(
        "steve_memory_synthesis_tokens_total",
        "kind" => "overview",
        "direction" => "input"
    )
    .increment(input_tokens);
    metrics::counter!(
        "steve_memory_synthesis_tokens_total",
        "kind" => "overview",
        "direction" => "output"
    )
    .increment(output_tokens);

    let overview = answer.text.trim().to_string();
    if overview.is_empty() {
        return Ok(OverviewRefreshResult {
            trigger,
            model_called: true,
            input_tokens,
            output_tokens,
            updated: false,
        });
    }

    let src = source.clone();
    let rel = rel_path.to_string();
    let updated = m
        .blocking(move |m| {
            m.store.write(|tx| {
                let rev =
                    memory_file::apply_overview_refresh(tx, &src, &rel, &overview, "steve", now)?;
                Ok::<_, ApiError>(rev.is_some())
            })
        })
        .await?;

    Ok(OverviewRefreshResult {
        trigger,
        model_called: true,
        input_tokens,
        output_tokens,
        updated,
    })
}

/// Checks every active file in every unpaused `role = memory` folder source of `ws` and refreshes
/// overviews only for files that pass [`evaluate_overview_trigger`]. Zero-change days make 0 calls.
pub async fn maybe_refresh_ws_overviews(
    m: &Modules,
    ws: &str,
    now: i64,
) -> ApiResult<Vec<OverviewRefreshResult>> {
    let ws_owned = ws.to_string();
    let targets: Vec<(SourceRow, String)> = m
        .blocking(move |m| {
            crate::read_api(&m.store, |c| {
                let mut out = Vec::new();
                for src in Documents(c).memory_folder_sources(&ws_owned)? {
                    if src.paused {
                        continue;
                    }
                    for doc in Documents(c).docs_of_source(&src.id)? {
                        if doc.status == "active" {
                            out.push((src.clone(), doc.r#ref));
                        }
                    }
                }
                Ok::<_, ApiError>(out)
            })
        })
        .await?;

    let mut results = Vec::with_capacity(targets.len());
    for (src, rel) in targets {
        results.push(maybe_refresh_overview(m, &src, &rel, now).await?);
    }
    Ok(results)
}

/// Emits at most one quiet weekly Feed summary of silent memory writes (replaces legacy 03:40
/// `kept_notice` interruptions) and evaluates the rolling revert-rate health alert.
pub fn maybe_emit_weekly_feed_summary(tx: &mut W<'_>, ws: &str, now: i64) -> ApiResult<bool> {
    const MARK_FEED: &str = "memory:feed_summary_last";
    const MARK_ALERT: &str = "memory:revert_alert_last";

    let settings = MemorySettings::load(tx, ws);
    let interval_ms = settings.feed_summary_interval_days * DAY_MS;
    let last_feed: i64 = Documents(tx)
        .mark(ws, MARK_FEED)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut emitted = false;
    if last_feed == 0 || now.saturating_sub(last_feed) >= interval_ms {
        let since = if last_feed == 0 {
            now.saturating_sub(interval_ms)
        } else {
            last_feed
        };
        let applied = MemoryRevisions(tx).list_applied_since(ws, since)?;
        if !applied.is_empty() {
            let files: HashSet<&str> = applied.iter().map(|r| r.rel_path.as_str()).collect();
            let text = steve_i18n::t!(
                "knowledge.feed.weekly_summary",
                count = applied.len(),
                files = files.len()
            );
            steve_store::notify::notify_in(tx, ws, "memory_digest", "info", &text, None)?;
            emitted = true;
        }
        Documents(tx).set_mark(ws, MARK_FEED, &now.to_string(), now)?;
    }

    // Rolling revert-rate health alert.
    let window_ms = settings.revert_alert_window_days * DAY_MS;
    let last_alert: i64 = Documents(tx)
        .mark(ws, MARK_ALERT)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if now.saturating_sub(last_alert) >= window_ms {
        let (total, reverted) =
            MemoryRevisions(tx).revert_window_counts(ws, now.saturating_sub(window_ms))?;
        if total >= settings.revert_alert_min_writes
            && (reverted as f64 / total as f64) > settings.revert_alert_ratio
        {
            metrics::counter!(
                "steve_memory_reverted_total",
                "reason" => "alert_threshold_exceeded"
            )
            .increment(1);
            let text = steve_i18n::t!(
                "knowledge.feed.revert_alert",
                days = settings.revert_alert_window_days,
                reverted = reverted,
                total = total
            );
            steve_store::notify::notify_in(tx, ws, "memory_health", "warn", &text, None)?;
            Documents(tx).set_mark(ws, MARK_ALERT, &now.to_string(), now)?;
        }
    }

    Ok(emitted)
}

/// Expires buffered observations whose `expires_at <= now` (90-day TTL by default).
pub fn expire_observations_in(tx: &mut W<'_>, ws: &str, now: i64) -> steve_store::Result<usize> {
    Observations(tx).expire_due(ws, now)
}

/// Counts active vs pending observations in a workspace (used by diagnostics/tests).
pub fn pending_observation_count(c: &mut impl Read, ws: &str) -> steve_store::Result<usize> {
    Ok(Observations(c).list_pending(ws)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_overview_trigger_skips_zero_change_and_fires_on_threshold_or_age() {
        let settings = MemorySettings::default();
        let now = 1_800_000_000_000i64;
        assert_eq!(
            evaluate_overview_trigger(0, None, now, &settings),
            OverviewTrigger::SkipNoChange
        );
        assert_eq!(
            evaluate_overview_trigger(2, Some(now - DAY_MS), now, &settings),
            OverviewTrigger::SkipBelowThreshold
        );
        assert_eq!(
            evaluate_overview_trigger(3, Some(now - DAY_MS), now, &settings),
            OverviewTrigger::RefreshDirtyCount
        );
        assert_eq!(
            evaluate_overview_trigger(1, Some(now - 8 * DAY_MS), now, &settings),
            OverviewTrigger::RefreshStaleAge
        );
    }
}
