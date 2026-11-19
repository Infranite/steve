//! The read tools of the calendar (resource `calendar`). Each reads the session owner's own cache;
//! `steve_calendar_get` also asks the source for the event's current body. They change nothing at a
//! provider, and the profile methods behind them are never tools of their own.
//!
//! Times are resolved by rule in the owner's zone and echoed back (`range`) so the model can check
//! what was understood.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_modules::contacts::mcp::McpCx;

use crate::query::{self, Range};
use crate::repo;
use crate::sync::{People, me_party, normalize};
use crate::{Calendar, sync};

pub const AGENDA: &str = "steve_calendar_agenda";
pub const SEARCH: &str = "steve_calendar_search";
pub const GET: &str = "steve_calendar_get";
pub const FIND_TIME: &str = "steve_calendar_find_time";

/// Every tool here only reads.
pub const READ_ONLY: [&str; 4] = [AGENDA, SEARCH, GET, FIND_TIME];

/// How long a live read of one event may take before the cache answers.
const LIVE_TIMEOUT: Duration = Duration::from_secs(20);

const TIME: &str = "A date (2026-11-18), a local time (2026-11-18T14:00) in the owner's time zone, RFC 3339, or now / today / tomorrow.";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": properties,
        "required": required, "additionalProperties": false}})
}

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            AGENDA,
            "Read the owner's calendar for a range (default: today) from Steve's calendar cache: every meeting once (also_in names the other calendars it is in), cancelled ones left out, overlapping events in conflicts_with, local times in the owner's time zone. calendars says how fresh each calendar is. Only the owner's own calendars; other people's free/busy is for steve_calendar_find_time.",
            json!({"from": {"type": "string", "description": TIME}, "to": {"type": "string", "description": format!("{TIME} A date means through the end of that day.")}}),
            &[],
        ),
        tool(
            SEARCH,
            "Find events in the owner's calendars by words in the title, place, organizer or description start. Default range: 30 days back to 180 days ahead.",
            json!({"query": {"type": "string", "minLength": 1, "maxLength": 200}, "from": {"type": "string", "description": TIME}, "to": {"type": "string", "description": TIME}}),
            &["query"],
        ),
        tool(
            GET,
            "Read one event (an event id or a meeting id from the other calendar tools) with its attendees and replies, asking its calendar source for the current version and body first; when the source cannot answer, the cached version is returned with the time it is from.",
            json!({"event": {"type": "string"}}),
            &["event"],
        ),
        tool(
            FIND_TIME,
            "Find free times for the owner and other people, by rule: working days and hours, a fixed grid, not before now, the earliest that do not overlap each other, each with why. People are contact ids (steve_contacts_find) or exact contact names; their busy times come from calendars bound to them under Connections (free/busy unless they share details). People without a calendar are listed in unknown and not counted.",
            json!({"people": {"type": "array", "maxItems": 20, "items": {"type": "string"}},
                "duration_min": {"type": "integer", "minimum": 5, "maximum": 1440},
                "from": {"type": "string", "description": format!("{TIME} Default: now.")},
                "to": {"type": "string", "description": format!("{TIME} Default: 7 days ahead.")}}),
            &["duration_min"],
        ),
    ]
}

pub fn handles(name: &str) -> bool {
    READ_ONLY.contains(&name)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeArgs {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    event: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    #[serde(default)]
    people: Vec<String>,
    duration_min: i64,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

fn args<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("Invalid calendar arguments: {e}"))
}

fn text(e: ApiError) -> String {
    e.to_string()
}

/// Runs a calendar tool for the session `cx`.
pub async fn call(cal: &Calendar, cx: &McpCx, name: &str, a: &Value) -> Result<String, String> {
    if cx.user.is_empty() {
        return Err("日历只对本人的会话开放".into());
    }
    let (ws, user) = (cx.ws.clone(), cx.user.clone());
    let s = cal.settings().clone();
    let out: Value = match name {
        AGENDA => {
            let a: RangeArgs = args(a)?;
            cal.modules()
                .blocking(move |m| {
                    m.store.reader(|c| {
                        let tz = query::user_tz(c, &ws, &user)?;
                        let r = query::range(a.from.as_deref(), a.to.as_deref(), tz, m.clock.now())
                            .map_err(ApiError::Rule)?;
                        query::agenda(c, &ws, &user, &r, tz)
                    })
                })
                .await
                .map_err(text)?
        }
        SEARCH => {
            let a: SearchArgs = args(a)?;
            cal.modules()
                .blocking(move |m| {
                    m.store.reader(|c| {
                        let now = m.clock.now();
                        let tz = query::user_tz(c, &ws, &user)?;
                        let from = match &a.from {
                            Some(f) => query::parse_time(f, tz, now, false),
                            None => Ok(now - s.past_days * 86_400_000),
                        }
                        .map_err(ApiError::Rule)?;
                        let to = match &a.to {
                            Some(t) => query::parse_time(t, tz, now, true),
                            None => Ok(now + s.future_days * 86_400_000),
                        }
                        .map_err(ApiError::Rule)?;
                        if to <= from {
                            return Err(ApiError::Rule("结束时间要晚于开始时间".into()));
                        }
                        query::search(c, &ws, &user, &a.query, &Range::new(from, to, tz), tz, 50)
                    })
                })
                .await
                .map_err(text)?
        }
        GET => {
            let a: GetArgs = args(a)?;
            cal.live_event(&ws, &user, &a.event).await.map_err(text)?
        }
        FIND_TIME => {
            let a: FindArgs = args(a)?;
            cal.modules()
                .blocking(move |m| {
                    m.store.reader(|c| {
                        let now = m.clock.now();
                        let tz = query::user_tz(c, &ws, &user)?;
                        let to_default = query::local(now + 7 * 86_400_000, tz).replace(' ', "T");
                        let r = query::range(
                            Some(a.from.as_deref().unwrap_or("now")),
                            Some(a.to.as_deref().unwrap_or(&to_default)),
                            tz,
                            now,
                        )
                        .map_err(ApiError::Rule)?;
                        let people = a
                            .people
                            .iter()
                            .map(|p| query::person(c, &ws, p))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(ApiError::Rule)?;
                        query::find_time(c, &ws, &user, &people, a.duration_min, &r, tz, now, &s)
                    })
                })
                .await
                .map_err(text)?
        }
        _ => return Err(format!("unknown calendar tool {name}")),
    };
    Ok(out.to_string())
}

impl Calendar {
    /// One event of `user`'s, refreshed from its source first (the body is read, not kept beyond
    /// the cache's excerpt); the cache when the source does not answer in time.
    pub async fn live_event(&self, ws: &str, user: &str, id: &str) -> Result<Value, ApiError> {
        let (w2, u2, i2) = (ws.to_string(), user.to_string(), id.to_string());
        let (row, tz) = self
            .modules()
            .blocking(move |m| {
                m.store.reader(|c| {
                    let tz = query::user_tz(c, &w2, &u2)?;
                    Ok((query::event(c, &w2, &u2, &i2, tz)?.0, tz))
                })
            })
            .await?;
        let cal = self
            .modules()
            .store
            .reader(|c| repo::calendar(c, &row.calendar_id).map_err(steve_store::StoreError::from))?
            .ok_or_else(|| ApiError::not_found("日历"))?;
        let cached = |note: Option<String>| -> Result<Value, ApiError> {
            let (w, u, i) = (ws.to_string(), user.to_string(), row.id.clone());
            let mut v = self
                .modules()
                .store
                .reader(|c| query::event(c, &w, &u, &i, tz).map(|x| x.1))?;
            v["live"] = json!(false);
            if let Some(n) = note {
                v["note"] = json!(n);
            }
            Ok(v)
        };
        if row.busy_only || cal.detail == "busy" || row.deleted_ms.is_some() {
            return cached(None);
        }
        let Some((cx, _)) = self.is_source(ws, &cal.connection_id).await? else {
            return cached(Some("这个日历现在没有连接，内容来自上次同步".into()));
        };
        let src = self
            .source(cx.adapter())
            .ok_or_else(|| ApiError::not_found("日历来源"))?;
        let got = tokio::time::timeout(LIVE_TIMEOUT, src.get(&cx, &cal.ext_id, &row.ext_id)).await;
        let remote = match got {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return cached(Some(format!("实时读取失败（{e}），内容来自上次同步"))),
            Err(_) => return cached(Some("实时读取超时，内容来自上次同步".into())),
        };
        let now = self.now();
        let Some(remote) = remote else {
            self.modules().store.transaction(|tx| {
                repo::delete_event(tx, &row.id, now)?;
                tx.publish(steve_types::DomainEvent::from(
                    steve_types::AutomationEvent::Calendar {
                        id: cal.id.clone().into(),
                        ws: ws.into(),
                    },
                ));
                Ok::<_, ApiError>(())
            })?;
            return cached(Some("来源里已经没有这个日程了".into()));
        };
        let description = remote.description.clone();
        self.modules().store.transaction(|tx| {
            let me = me_party(tx, ws, user)?;
            let mut people = People {
                c: tx,
                ws,
                me: me.as_deref(),
                cache: HashMap::new(),
            };
            if let Ok((mut fresh, atts)) = normalize(&cal, &remote, false, &mut people)
                && fresh.id == row.id
            {
                fresh.meeting_id = row.meeting_id.clone();
                repo::write_event(tx, &fresh, &atts, now)?;
                tx.publish(steve_types::DomainEvent::from(
                    steve_types::AutomationEvent::Calendar {
                        id: cal.id.clone().into(),
                        ws: ws.into(),
                    },
                ));
            }
            Ok::<_, ApiError>(())
        })?;
        let mut v = cached(None)?;
        v["live"] = json!(true);
        if let Some(d) = description {
            v["description"] = json!(sync::cut_text(&d, 4000));
        }
        Ok(v)
    }
}
