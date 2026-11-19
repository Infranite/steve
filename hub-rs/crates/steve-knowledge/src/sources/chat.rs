//! The `chat` source: the main thread with Steve (`messages`, space `steve`), one
//! document per local day (`ref = chat:2026-10-31`), text only — no draft-card JSON, which lives in
//! `meta`. Today's document is refreshed hourly; yesterday is finalized at 03:30 and extracted
//! at 03:40, with up to seven completed local days revisited after downtime.

use std::time::Duration;

use chrono::{NaiveDate, Timelike};
use rusqlite::{Connection, params};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_runtime::tz::Zone;
use steve_store::base::Level;
use steve_store::base::mems::Mem;
use steve_store::uid;

use crate::candidates::{Origin, OriginKind};
use crate::chunk::TextKind;
use crate::extract;
use crate::extract_text::{Extracted, FrontMatter};
use crate::index::{self, BoxFuture, DocLoader, Loaded};
use crate::repo::{self, DocRow, NewDoc, SourceRow};
use crate::sources;
use crate::state::Knowledge;

pub const KIND: &str = "chat";
/// The main thread's `messages.space`.
pub const SPACE: &str = "steve";
/// How often the job wakes up; today's document is rebuilt every [`TODAY_EVERY`].
pub const TICK: Duration = Duration::from_secs(600);
pub const TODAY_EVERY: i64 = 3600 * 1000;
/// Local time after which yesterday is finalized, and after which it is extracted.
pub const FINALIZE_AT: (u32, u32) = (3, 30);
pub const EXTRACT_AT: (u32, u32) = (3, 40);

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
pub fn day_text(c: &Connection, ws: &str, day: &str) -> ApiResult<Option<String>> {
    let zone = Zone::parse(steve_modules::zone::workspace_tz(c, ws)?.name()).expect("IANA zone");
    let Some((start, end)) = day_bounds(day, zone) else {
        return Ok(None);
    };
    let msgs: Vec<(String, String, i64)> = c
        .prepare_cached(
            "SELECT author, text, created FROM messages WHERE workspace_id = ? AND space = ? AND created >= ? AND created < ? ORDER BY created, id",
        )?
        .query_map(params![ws, SPACE, start, end], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
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

fn extracted(text: String) -> Extracted {
    Extracted {
        text,
        kind: TextKind::Plain,
        front: FrontMatter::default(),
    }
}

/// `2026-10-31` → `10-31 的对话` (the label of its candidates).
fn short_label(day: &str) -> String {
    format!("{} 的对话", day.get(5..).unwrap_or(day))
}

/// Rebuilds the document of one day: written and indexed when there are messages, removed when there
/// are none. Returns the document id, if one exists afterwards.
pub fn sync_day(m: &Modules, kb: &Knowledge, ws: &str, day: &str) -> ApiResult<Option<String>> {
    let now = m.clock.now();
    let out = m.store.transaction(|tx| {
        let user = extract::owner(tx, ws)?;
        let src = sources::builtin(tx, ws, KIND, &user, now)?;
        let r#ref = format!("chat:{day}");
        let existing = repo::doc_by_ref(tx, &src.id, &r#ref)?;
        let Some(text) = day_text(tx, ws, day)? else {
            if let Some(d) = existing {
                index::delete_doc(tx, &d.id)?;
                return Ok::<_, ApiError>(Some((d.id, false)));
            }
            return Ok(None);
        };
        let title = format!("{day} 的对话");
        let id = match existing {
            Some(d) => {
                tx.prepare_cached(
                    "UPDATE kb_docs SET title = ?, size = ?, status = 'active', error = NULL, updated = ? WHERE id = ?",
                )?
                .execute(params![title, text.len() as i64, now, d.id])?;
                d.id
            }
            None => {
                let id = uid("kd");
                repo::insert_doc(
                    tx,
                    ws,
                    &NewDoc {
                        id: id.clone(),
                        source_id: src.id.clone(),
                        r#ref,
                        title,
                        mime: "text/plain".into(),
                        size: text.len() as i64,
                        created_by: user,
                        ..Default::default()
                    },
                    now,
                )?;
                id
            }
        };
        let doc = repo::doc(tx, &id)?.ok_or_else(|| ApiError::not_found("文档"))?;
        index::index_doc(tx, &doc, &src, &extracted(text), now, false)?;
        Ok(Some((id, true)))
    })?;
    Ok(out.and_then(|(id, alive)| {
        kb.notify(ws, &id);
        alive.then_some(id)
    }))
}

/// Extracts candidates from one day's conversation, once (`kb_marks extract:chat:<day>`). Returns the
/// candidates written.
pub async fn extract_day(m: &Modules, kb: &Knowledge, ws: &str, day: &str) -> ApiResult<Vec<Mem>> {
    let mark = format!("extract:chat:{day}");
    let now = m.clock.now();
    let (w, d) = (ws.to_string(), day.to_string());
    let todo = m
        .blocking(move |m| {
            m.store.transaction(|tx| {
                if repo::mark(tx, &w, &mark)?.is_some() {
                    return Ok::<_, ApiError>(None);
                }
                let user = extract::owner(tx, &w)?;
                Ok(Some((user, day_text(tx, &w, &d)?.unwrap_or_default())))
            })
        })
        .await?;
    let Some((user, text)) = todo else {
        return Ok(Vec::new());
    };
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
    m.store.transaction(|tx| {
        // Serialize concurrent extractions of the same day after their model work completes.
        if repo::mark(tx, &origin.ws, mark)?.is_some() {
            return Ok(Vec::new());
        }
        let made = extract::commit_in(tx, origin, prepared)?;
        before_mark();
        repo::set_mark(tx, &origin.ws, mark, "done", now)?;
        Ok(made)
    })
}

/// Completed local days revisited after downtime, oldest first, bounded to seven days per tick.
pub const CATCH_UP_DAYS: u64 = 7;

/// One run of the job at `now`: today's document of every workspace (when `today` is set), and after
/// 03:30 / 03:40 yesterday's final document / extraction, plus uncompleted days in the catch-up window.
pub async fn tick(m: &Modules, kb: &Knowledge, now: i64, today: bool) -> ApiResult<()> {
    let wss: Vec<String> = m
        .blocking(|m| {
            m.store.reader(|c| {
                Ok::<_, ApiError>(
                    c.prepare("SELECT id FROM workspaces ORDER BY id")?
                        .query_map([], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?,
                )
            })
        })
        .await?;
    for ws in wss {
        let w = ws.clone();
        let zone = m
            .blocking(move |m| {
                m.store.reader(|c| {
                    Ok::<_, ApiError>(
                        Zone::parse(steve_modules::zone::workspace_tz(c, &w)?.name())
                            .expect("IANA zone"),
                    )
                })
            })
            .await?;
        let Some(local) = zone.local(now).ok() else {
            continue;
        };
        let hm = (local.hour(), local.minute());
        if today {
            let (k, w) = (kb.clone(), ws.clone());
            m.blocking(move |m| sync_day(m, &k, &w, &day_of(now, zone)))
                .await?;
        }
        for ago in (1..=CATCH_UP_DAYS).rev() {
            let Some(day) = local.date().checked_sub_days(chrono::Days::new(ago)) else {
                continue;
            };
            let day = day.format("%Y-%m-%d").to_string();
            let (k, w, d) = (kb.clone(), ws.clone(), day.clone());
            if ago > 1 || hm >= FINALIZE_AT {
                m.blocking(move |m| {
                    let mark = format!("sync:chat:{d}");
                    if m.store.reader(|c| repo::mark(c, &w, &mark))?.is_none() {
                        sync_day(m, &k, &w, &d)?;
                        m.store
                            .transaction(|tx| repo::set_mark(tx, &w, &mark, "final", now))?;
                    }
                    Ok::<_, ApiError>(())
                })
                .await?;
            }
            if (ago > 1 || hm >= EXTRACT_AT)
                && let Err(e) = extract_day(m, kb, &ws, &day).await
            {
                tracing::warn!(ws = %ws, day, "kb.extract chat: {e}");
            }
        }
    }
    Ok(())
}

/// Starts the `kb.chat` job (today hourly; yesterday at 03:30 and its extraction at 03:40).
pub fn spawn_job(m: Modules, kb: Knowledge) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_today = i64::MIN;
        let mut every = tokio::time::interval(TICK);
        loop {
            every.tick().await;
            let now = m.clock.now();
            let today = now.saturating_sub(last_today) >= TODAY_EVERY;
            if today {
                last_today = now;
            }
            if let Err(e) = tick(&m, &kb, now, today).await {
                tracing::warn!("kb.chat: {e}");
            }
        }
    })
}

/// Re-reads a day document for the index queue.
pub struct ChatLoader;

impl DocLoader for ChatLoader {
    fn kinds(&self) -> &[&str] {
        &[KIND]
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        doc: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            let day = doc.r#ref.strip_prefix("chat:").unwrap_or(&doc.r#ref);
            match m.store.reader(|c| day_text(c, &doc.workspace_id, day)) {
                Ok(Some(t)) => Loaded::Text(extracted(t)),
                Ok(None) => Loaded::Gone,
                Err(e) => Loaded::Failed(format!("读不到这天的对话：{e}")),
            }
        })
    }
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
                .reader(|c| repo::mark(c, WS, mark))
                .unwrap()
                .is_none()
        );
        let count = || {
            env.modules
                .store
                .reader(|c| {
                    Ok::<_, ApiError>(c.query_row(
                        "SELECT count(*) FROM mem_candidates WHERE origin_id = 'chat:2026-10-31'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
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
        let notices: i64 = env
            .modules
            .store
            .reader(|c| {
                Ok::<_, ApiError>(c.query_row(
                    "SELECT count(*) FROM messages WHERE space = 'notify'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
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
