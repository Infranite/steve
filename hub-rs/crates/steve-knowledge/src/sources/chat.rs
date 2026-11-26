//! Main-thread conversation extraction (`messages`, space `steve`): conversations are no longer
//! indexed into `kb_docs` / `kb_chunks` (they remain searchable as records), while completed local
//! days are extracted after 03:40 (with up to seven completed local days revisited after downtime).

use std::time::Duration;

use chrono::{NaiveDate, Timelike};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_runtime::tz::Zone;
use steve_store::base::Level;
use steve_store::knowledge::{Documents, OriginClass, SourceTrustTier};
use steve_store::{MessageEvidence, Read, Workspaces};

use crate::candidates::{Origin, OriginKind};
use crate::extract;
use crate::ingest::IngestOutcome;
use crate::policy::{CHAT_WINDOW_MAX_CHARS, CHAT_WINDOW_OVERLAP_LINES};
use crate::sources::folder::FolderPolicy;
use crate::state::Knowledge;

/// The main thread's `messages.space`.
pub const SPACE: &str = "steve";
/// How often the job wakes up.
pub const TICK: Duration = Duration::from_secs(600);
/// Local time after which yesterday's conversation is extracted.
pub const EXTRACT_AT: (u32, u32) = (3, 40);
/// Metric task label for daily main-thread memory extraction.
pub const PERIODIC_TASK_EXTRACT: &str = "kb.chat_extract";

/// The local day (`YYYY-MM-DD`) of a Unix-ms time.
pub fn day_of(ms: i64, zone: Zone) -> String {
    zone.local(ms)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `[start, end)` in Unix ms of a local day on the workspace clock.
pub fn day_bounds(day: &str, zone: Zone) -> Option<(i64, i64)> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    zone.day(zone.instant(d.and_hms_opt(0, 0, 0)?).ok()?).ok()
}

fn hhmm(ms: i64, zone: Zone) -> String {
    zone.local(ms)
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The day's conversation as text, `我（10:31）：…` / `Steve（10:32）：…`; `None` if nothing was said.
pub(crate) fn workspace_zone(c: &mut impl Read, ws: &str) -> steve_store::Result<Zone> {
    let zone = steve_modules::zone::Zone::for_user(c, ws, None)?;
    Ok(Zone::parse(zone.name()).expect("IANA zone"))
}

pub fn day_text(c: &mut impl Read, ws: &str, day: &str) -> steve_store::Result<Option<String>> {
    let zone = workspace_zone(c, ws)?;
    let Some((start, end)) = day_bounds(day, zone) else {
        return Ok(None);
    };
    let msgs = MessageEvidence(c).in_range(ws, SPACE, start, end)?;
    let lines: Vec<String> = msgs
        .into_iter()
        .filter(|(_, t, _)| !t.trim().is_empty())
        .map(|(a, t, at)| {
            let who = if a == "me" { "我" } else { "Steve" };
            format!("{who}（{}）：{}", hhmm(at, zone), t.trim())
        })
        .collect();
    Ok((!lines.is_empty()).then(|| lines.join("\n\n")))
}

/// `2026-10-31` → `10-31 的对话` (the label of its candidates).
fn short_label(day: &str) -> String {
    format!("{} 的对话", day.get(5..).unwrap_or(day))
}

/// Splits a long day's transcript into overlapping windows capped at `max_chars`, so evening
/// conversations are never silently truncated.
pub fn split_chat_windows(text: &str, max_chars: usize, overlap_lines: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.chars().count() <= max_chars {
        return vec![trimmed.to_string()];
    }
    let blocks: Vec<&str> = trimmed
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .collect();
    let mut windows = Vec::new();
    let mut idx = 0usize;
    while idx < blocks.len() {
        let mut cur = String::new();
        let mut count = 0usize;
        let start = idx;
        while idx < blocks.len() {
            let sep = if cur.is_empty() { 0 } else { 2 };
            let b_len = blocks[idx].chars().count();
            if !cur.is_empty() && count + sep + b_len > max_chars {
                break;
            }
            if !cur.is_empty() {
                cur.push_str("\n\n");
                count += 2;
            }
            if b_len > max_chars {
                let clipped: String = blocks[idx].chars().take(max_chars).collect();
                cur.push_str(&clipped);
                idx += 1;
                break;
            }
            cur.push_str(blocks[idx]);
            count += b_len;
            idx += 1;
        }
        windows.push(cur);
        if idx >= blocks.len() {
            break;
        }
        let back = overlap_lines.min(idx.saturating_sub(start + 1));
        idx -= back;
    }
    windows
}

/// Extracts memories and proposals from one day's conversation, once (`kb_marks extract:chat:<day>`).
/// Zero-change days make 0 model calls. Long days are windowed instead of truncated.
/// Writes silently to file-backed personal context (zero Inbox cards, zero 03:40 notices).
pub async fn extract_day(
    m: &Modules,
    kb: &Knowledge,
    ws: &str,
    day: &str,
) -> ApiResult<Vec<IngestOutcome>> {
    let mark = format!("extract:chat:{day}");
    let now = m.clock.now();
    let (w, d) = (ws.to_string(), day.to_string());
    let todo = m
        .blocking(move |m| {
            m.store.write(|tx| {
                if Documents(tx).mark(&w, &mark)?.is_some() {
                    return Ok::<_, ApiError>(None);
                }
                let Some(text) = day_text(tx, &w, &d)?.filter(|t| !t.trim().is_empty()) else {
                    Documents(tx).set_mark(&w, &mark, "done", now)?;
                    steve_runtime::jobs::record_periodic_skipped(
                        PERIODIC_TASK_EXTRACT,
                        steve_runtime::jobs::SkipReason::NoChange,
                    );
                    steve_runtime::jobs::record_model_calls_avoided(
                        PERIODIC_TASK_EXTRACT,
                        steve_runtime::jobs::AvoidReason::NoChange,
                        1,
                    );
                    return Ok(None);
                };
                let user = extract::owner(tx, &w)?;
                Ok(Some((user, text)))
            })
        })
        .await?;
    let Some((user, text)) = todo else {
        return Ok(Vec::new());
    };
    steve_runtime::jobs::record_periodic_triggered(PERIODIC_TASK_EXTRACT);
    let origin = Origin {
        ws: ws.into(),
        kind: OriginKind::Chat,
        id: format!("chat:{day}"),
        label: short_label(day),
        user,
        writer: "steve".into(),
        default_level: Level::Reasoning,
        item_id: None,
        session_id: None,
        origin_class: OriginClass::ImplicitPattern,
        source_tier: SourceTrustTier::Tier1Owner,
    };
    let windows = split_chat_windows(&text, CHAT_WINDOW_MAX_CHARS, CHAT_WINDOW_OVERLAP_LINES);
    let mut prepared_all = extract::Prepared::default();
    for win in windows {
        let p = extract::prepare(m, kb, &origin, &win, None, extract::Task::default()).await?;
        prepared_all.extend(p);
    }
    let mark = format!("extract:chat:{day}");
    let policy = kb.folder_policy().clone();
    let ws_for_synth = ws.to_string();
    let outcomes = m
        .blocking(move |m| commit_extraction(m, &policy, &origin, &prepared_all, &mark, now, || {}))
        .await?;
    // Event-driven overview refresh: 0 model calls unless dirty thresholds are crossed.
    let _ = crate::synthesis::maybe_refresh_ws_overviews(m, &ws_for_synth, now).await;
    Ok(outcomes)
}

fn commit_extraction(
    m: &Modules,
    policy: &FolderPolicy,
    origin: &Origin,
    prepared: &extract::Prepared,
    mark: &str,
    now: i64,
    before_mark: impl FnOnce(),
) -> ApiResult<Vec<IngestOutcome>> {
    m.store.write(|tx| {
        // Serialize concurrent extractions of the same day after their model work completes.
        if Documents(tx).mark(&origin.ws, mark)?.is_some() {
            return Ok(Vec::new());
        }
        let report = extract::commit_in(tx, policy, origin, prepared, now)?;
        before_mark();
        Documents(tx).set_mark(&origin.ws, mark, "done", now)?;
        let _ = crate::synthesis::maybe_emit_weekly_feed_summary(tx, &origin.ws, now)?;
        Ok(report.outcomes)
    })
}

/// Completed local days revisited after downtime, oldest first, bounded to seven days per tick.
pub const CATCH_UP_DAYS: u64 = 7;

/// One run of the job at `now`: after 03:40 extracts yesterday and any uncompleted days in the
/// catch-up window. Does not index chat transcripts into `kb_docs`.
pub async fn tick(m: &Modules, kb: &Knowledge, now: i64) -> ApiResult<()> {
    let mut wss = m
        .blocking(|m| Ok(m.store.read(|c| Workspaces(c).ids())?))
        .await?;
    wss.sort();
    for ws in wss {
        let w = ws.clone();
        let zone = m
            .blocking(move |m| Ok(m.store.read(|c| workspace_zone(c, &w))?))
            .await?;
        let Some(local) = zone.local(now).ok() else {
            continue;
        };
        let hm = (local.hour(), local.minute());
        for ago in (1..=CATCH_UP_DAYS).rev() {
            let Some(day) = local.date().checked_sub_days(chrono::Days::new(ago)) else {
                continue;
            };
            let day = day.format("%Y-%m-%d").to_string();
            if (ago > 1 || hm >= EXTRACT_AT)
                && let Err(e) = extract_day(m, kb, &ws, &day).await
            {
                tracing::warn!(ws = %ws, day, "kb.extract chat: {e}");
            }
        }
    }
    Ok(())
}

/// Starts the `kb.chat` job (extracts completed days after 03:40).
pub fn task(m: Modules, kb: Knowledge) -> steve_runtime::Task {
    let clock = m.clock.clone();
    steve_runtime::every(
        "kb.chat",
        clock,
        TICK,
        steve_runtime::FirstRun::Immediate,
        move || {
            let now = m.clock.now();
            let (m, kb) = (m.clone(), kb.clone());
            async move {
                if let Err(e) = tick(&m, &kb, now).await {
                    tracing::warn!("kb.chat: {e}");
                }
            }
        },
    )
}

pub async fn run(m: Modules, kb: Knowledge) {
    task(m, kb).await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(day_bounds(&day_of(ms, z), z), z.day(ms).ok());
            assert_eq!(
                hhmm(ms, z),
                z.local(ms).unwrap().format("%H:%M").to_string()
            );
        }
    }

    use super::*;
    use crate::candidates::{self, NewCandidate};

    #[test]
    fn extraction_crash_before_receipt_rolls_back_and_reopens_once() {
        use steve_modules::testkit::{TestEnv, WS};
        let mut env = TestEnv::new(|r| {
            r.decision(candidates::SUBJECT, Box::new(candidates::MemDecisions));
        });
        let policy = FolderPolicy::new(env.dir.path(), env.dir.path().join("steve_home"));
        let origin = Origin {
            ws: WS.into(),
            kind: OriginKind::Chat,
            id: "chat:2026-10-31".into(),
            label: "10-31 的对话".into(),
            user: steve_modules::testkit::ME.into(),
            writer: "steve".into(),
            default_level: Level::Reasoning,
            item_id: None,
            session_id: None,
            origin_class: OriginClass::OwnerExplicit,
            source_tier: SourceTrustTier::Tier1Owner,
        };
        let candidate = NewCandidate {
            kind: "rule".into(),
            text: "Never deploy on Fridays".into(),
            evidence: "Never deploy on Fridays".into(),
            about: vec![],
            level: Level::Reasoning,
            supersedes: None,
            why: "Avoids weekend production incidents".into(),
            scope: "deployment".into(),
            changes_future_behavior: true,
            origin_class: OriginClass::OwnerExplicit,
            source_tier: SourceTrustTier::Tier1Owner,
            is_explicit_rule: true,
        };
        let prepared = extract::Prepared {
            text: "我（10:00）：Never deploy on Fridays".into(),
            candidates: vec![candidate],
            proposals: Vec::new(),
        };
        let mark = "extract:chat:2026-10-31";
        let path = env.dir.path().join("steve.sqlite3");
        let clock = steve_runtime::clock::FakeClock::new(1_793_491_200_000);
        let now = steve_runtime::clock::Clock::now(&clock);
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            commit_extraction(&env.modules, &policy, &origin, &prepared, mark, now, || {
                panic!("crash before receipt")
            })
        }));
        assert!(crashed.is_err());
        let reopened = steve_store::Store::open(&path, steve_store::NoPublisher).unwrap();
        let mut registry = steve_modules::Registry::new();
        registry.decision(candidates::SUBJECT, Box::new(candidates::MemDecisions));
        env.modules =
            Modules::with_clock(reopened, env.gateway.clone(), registry, env.clock.clone());
        assert!(
            env.modules
                .store
                .read(|c| Documents(c).mark(WS, mark))
                .unwrap()
                .is_none()
        );
        let entries_count = || {
            env.modules
                .store
                .read(|c| steve_store::knowledge::MemoryEntries(c).list_active_ws(WS))
                .unwrap()
                .len()
        };
        assert_eq!(entries_count(), 0);
        let first =
            commit_extraction(&env.modules, &policy, &origin, &prepared, mark, now, || {}).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].is_written());
        assert!(
            commit_extraction(&env.modules, &policy, &origin, &prepared, mark, now, || {})
                .unwrap()
                .is_empty()
        );
        assert_eq!(entries_count(), 1);
        // Zero Inbox confirmation cards opened for memories!
        let open_mem_cards = env
            .modules
            .store
            .read(|c| steve_store::DecisionRows(c).open_confirmations(WS, candidates::SUBJECT))
            .unwrap();
        assert_eq!(open_mem_cards, 0);
    }

    #[test]
    fn windows_cover_long_day_without_truncating_evening() {
        let mut parts = Vec::new();
        for i in 0..40 {
            parts.push(format!("我（{:02}:00）：{}", i % 24, "x".repeat(400)));
        }
        parts.push("我（23:55）：EVENING_MARKER_RULE".to_string());
        let full = parts.join("\n\n");
        assert!(full.chars().count() > CHAT_WINDOW_MAX_CHARS);
        let wins = split_chat_windows(&full, CHAT_WINDOW_MAX_CHARS, CHAT_WINDOW_OVERLAP_LINES);
        assert!(wins.len() >= 2);
        assert!(wins.last().unwrap().contains("EVENING_MARKER_RULE"));
    }

    #[test]
    fn days_round_trip() {
        let zone = Zone::parse("Europe/London").unwrap();
        let (s, e) = day_bounds("2026-10-31", zone).unwrap();
        assert!(e > s);
        assert_eq!(day_of(s, zone), "2026-10-31");
        assert_eq!(day_of(e - 1, zone), "2026-10-31");
        assert_eq!(day_of(e, zone), "2026-11-01");
        assert_eq!(short_label("2026-10-31"), "10-31 的对话");
        assert!(day_bounds("昨天", Zone::system()).is_none());
    }
}
