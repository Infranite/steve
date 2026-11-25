//! Main-thread conversation extraction (`messages`, space `steve`): conversations are no longer
//! indexed into `kb_docs` / `kb_chunks` (they remain searchable as records), while completed local
//! days are extracted after 03:40 (with up to seven completed local days revisited after downtime).

use std::time::Duration;

use chrono::{NaiveDate, Timelike};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_runtime::tz::Zone;
use steve_store::base::Level;
use steve_store::base::mems::Mem;
use steve_store::knowledge::Documents;
use steve_store::{MessageEvidence, Read, Workspaces};

use crate::candidates::{Origin, OriginKind};
use crate::extract;
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

/// Extracts candidates and proposals from one day's conversation, once (`kb_marks extract:chat:<day>`).
/// Returns the memory candidates written.
pub async fn extract_day(m: &Modules, kb: &Knowledge, ws: &str, day: &str) -> ApiResult<Vec<Mem>> {
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
        default_level: Level::Conclusion,
        item_id: None,
        session_id: None,
    };
    let prepared = extract::prepare(m, kb, &origin, &text, None, extract::Task::default()).await?;
    let mark = format!("extract:chat:{day}");
    m.blocking(move |m| commit_extraction(m, &origin, &prepared, &mark, now, || {}))
        .await
}

fn commit_extraction(
    m: &Modules,
    origin: &Origin,
    prepared: &extract::Prepared,
    mark: &str,
    now: i64,
    before_mark: impl FnOnce(),
) -> ApiResult<Vec<Mem>> {
    m.store.write(|tx| {
        // Serialize concurrent extractions of the same day after their model work completes.
        if Documents(tx).mark(&origin.ws, mark)?.is_some() {
            return Ok(Vec::new());
        }
        let made = extract::commit_in(tx, origin, prepared)?;
        before_mark();
        Documents(tx).set_mark(&origin.ws, mark, "done", now)?;
        Ok(made)
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
        let origin = Origin {
            ws: WS.into(),
            kind: OriginKind::Chat,
            id: "chat:2026-10-31".into(),
            label: "10-31 的对话".into(),
            user: steve_modules::testkit::ME.into(),
            writer: "steve".into(),
            default_level: Level::Conclusion,
            item_id: None,
            session_id: None,
        };
        let candidate = NewCandidate {
            kind: "rule".into(),
            text: "周五不部署".into(),
            evidence: "以后周五不部署".into(),
            about: vec![],
            level: Level::Conclusion,
            supersedes: None,
            why: String::new(),
            scope: String::new(),
            owner_said: false,
        };
        let prepared = extract::Prepared {
            text: "我（10:00）：以后周五不部署".into(),
            candidates: vec![candidate],
            proposals: Vec::new(),
        };
        let mark = "extract:chat:2026-10-31";
        let path = env.dir.path().join("steve.sqlite3");
        let clock = steve_runtime::clock::FakeClock::new(1_793_491_200_000);
        let now = steve_runtime::clock::Clock::now(&clock);
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            commit_extraction(&env.modules, &origin, &prepared, mark, now, || {
                panic!("crash after candidates and notice, before receipt")
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
        let count = || {
            steve_store::testkit::knowledge::candidate_count(&env.modules.store, "chat:2026-10-31")
                .unwrap()
        };
        assert_eq!(count(), 0);
        assert_eq!(
            commit_extraction(&env.modules, &origin, &prepared, mark, now, || {})
                .unwrap()
                .len(),
            1
        );
        assert!(
            commit_extraction(&env.modules, &origin, &prepared, mark, now, || {})
                .unwrap()
                .is_empty()
        );
        assert_eq!(count(), 1);
        let notices = steve_store::testkit::knowledge::notice_text(&env.modules.store)
            .unwrap()
            .len();
        assert_eq!(notices, 1);
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
