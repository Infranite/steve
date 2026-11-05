//! The `chat` source: the main thread with Steve (`messages`, space `steve`), one
//! document per local day (`ref = chat:2026-10-31`), text only — no draft-card JSON, which lives in
//! `meta`. Today's document is refreshed hourly, yesterday's is finalized at 03:30, and yesterday's
//! conversation is extracted into candidate memories at 03:40.

use std::time::Duration;

use chrono::{Local, NaiveDate, TimeZone, Timelike};
use rusqlite::{Connection, params};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::Level;
use steve_store::base::mems::Mem;
use steve_store::uid;

use crate::candidates::{Origin, OriginKind};
use crate::chunk::TextKind;
use crate::extract::{self, MAX_INPUT, clip};
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
const DAY_MS: i64 = 24 * 3600 * 1000;

/// The local day (`YYYY-MM-DD`) of a Unix-ms time.
pub fn day_of(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `[start, end)` in Unix ms of a local day.
pub fn day_bounds(day: &str) -> Option<(i64, i64)> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let at = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|t| t.timestamp_millis())
    };
    Some((at(d)?, at(d.succ_opt()?)?))
}

fn hhmm(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The day's conversation as text, `我（10:31）：…` / `Steve（10:32）：…`; `None` if nothing was said.
pub fn day_text(c: &Connection, ws: &str, day: &str) -> ApiResult<Option<String>> {
    let Some((start, end)) = day_bounds(day) else {
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
            format!("{who}（{}）：{}", hhmm(at), t.trim())
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
        index::index_doc(tx, kb.guard.as_ref(), &doc, &src, &extracted(text), now, false)?;
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
                repo::set_mark(tx, &w, &mark, "done", now)?;
                let user = extract::owner(tx, &w)?;
                Ok(day_text(tx, &w, &d)?.map(|t| (user, t)))
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
    extract::run(
        m,
        kb,
        &origin,
        &clip(&text, MAX_INPUT),
        None,
        extract::Task::default(),
    )
    .await
}

/// One run of the job at `now`: today's document of every workspace (when `today` is set), and after
/// 03:30 / 03:40 yesterday's final document / extraction if not done yet.
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
    let local = Local.timestamp_millis_opt(now).earliest();
    let hm = local.map(|t| (t.hour(), t.minute())).unwrap_or((0, 0));
    let yesterday = day_of(now - DAY_MS);
    for ws in wss {
        // a day's document is rebuilt and chunked in one write: off the runtime
        let (k, w, y) = (kb.clone(), ws.clone(), yesterday.clone());
        m.blocking(move |m| {
            if today {
                sync_day(m, &k, &w, &day_of(now))?;
            }
            if hm >= FINALIZE_AT {
                let mark = format!("sync:chat:{y}");
                if m.store.reader(|c| repo::mark(c, &w, &mark))?.is_none() {
                    sync_day(m, &k, &w, &y)?;
                    m.store
                        .transaction(|tx| repo::set_mark(tx, &w, &mark, "final", now))?;
                }
            }
            Ok(())
        })
        .await?;
        if hm >= EXTRACT_AT
            && let Err(e) = extract_day(m, kb, &ws, &yesterday).await
        {
            tracing::warn!(ws = %ws, "kb.extract chat: {e}");
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
    use super::*;

    #[test]
    fn days_round_trip() {
        let (s, e) = day_bounds("2026-10-31").unwrap();
        assert!(e > s);
        assert_eq!(day_of(s), "2026-10-31");
        assert_eq!(day_of(e - 1), "2026-10-31");
        assert_eq!(day_of(e), "2026-11-01");
        assert_eq!(short_label("2026-10-31"), "10-31 的对话");
        assert!(day_bounds("昨天").is_none());
    }
}
