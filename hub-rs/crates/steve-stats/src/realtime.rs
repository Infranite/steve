//! Realtime output, independent of billing and calendar-day statistics. The collector writes
//! quarter-token estimates from streamed text, never from chunk counts or final usage reports.
//! Read an entire answer in one SQLite snapshot: late samples can revise the trend at their source
//! time, but can never become a burst at the time they arrived.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde::Serialize;
use steve_store::StoreError;

#[cfg(test)]
#[path = "realtime_query_tests.rs"]
mod query_tests;

pub const WINDOW_MS: i64 = 10_000;
pub const STEP_MS: i64 = 1_000;
pub const POINTS: i64 = 60;
/// Without a recent source event, silence is not evidence of zero output. No heartbeat is implied.
pub const FRESH_MS: i64 = 3_000;
pub const ESTIMATOR: &str = "text-v1";

// Keep the persistent gap lookup and the bounded tail separate. An OR with the predecessor
// defeats the lower index bound and makes every one-second poll scan the entire attempt.
const GAP_SQL: &str =
    "SELECT MIN(at) FROM realtime_output_samples WHERE attempt_id=? AND kind='gap'";
const WINDOW_SQL: &str = "SELECT at,units FROM realtime_output_samples WHERE attempt_id=?1 AND at>?2 AND at<=?3 ORDER BY at,seq";
const PRECEDING_SQL: &str = "SELECT at,0 FROM realtime_output_samples WHERE attempt_id=?1 AND at<=?2 ORDER BY at DESC LIMIT 1";
/// A deliberately approximate, additive text estimator. ASCII scalars weigh 1/4 token; every
/// other Unicode scalar weighs 1 token. Fractional tokens avoid dependence on stream chunking.
/// Includes visible assistant/thought text, excludes tools, images and unexposed reasoning.
pub fn text_units(text: &str) -> i64 {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 4 }).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    // Reserved for a collector with a verified, timed output-token contract. No current adapter has a verified timed
    // output-token contract; streamed billing facts are not generation-time measurements.
    Measured,
    Estimated,
    Idle,
    Unavailable,
    Partial,
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub measured: usize,
    pub estimated: usize,
    pub unavailable: usize,
}

#[derive(Debug, Serialize)]
pub struct Point {
    pub at: i64,
    pub tps: Option<f64>,
    pub state: Quality,
}

#[derive(Debug, Serialize)]
pub struct Realtime {
    pub at: i64,
    pub window_ms: i64,
    pub step_ms: i64,
    pub tps: Option<f64>,
    pub output_sessions: Option<usize>,
    pub state: Quality,
    pub coverage: Coverage,
    pub series: Vec<Point>,
    pub estimator: &'static str,
    /// Available evidence only, explicitly separate from the unknown total in a partial answer.
    pub estimated_tps: Option<f64>,
    pub measured_tps: Option<f64>,
    pub observed_output_sessions: usize,
}

#[derive(Debug)]
struct Sample {
    at: i64,
    units: i64,
}

struct Attempt {
    session: String,
    started: i64,
    ended: Option<i64>,
    tracked: bool,
    baseline: bool,
    confirmed_end: bool,
    unknown_at: Option<i64>,
    gap_at: Option<i64>,
    samples: Vec<Sample>,
}

/// Already-authorized workspace and optional project/Bot references. There are deliberately no
/// historical range, model, source, item, harness or client-supplied clock parameters.
pub struct Scope<'a> {
    pub workspace: &'a str,
    pub project: Option<&'a str>,
    pub bot: Option<&'a str>,
}

/// Reads from persisted Node collection records. The transaction also keeps stream health and
/// attempt ends consistent with the samples when a collector commits concurrently.
pub fn read(c: &Connection, scope: Scope<'_>, now: i64) -> Result<Realtime, StoreError> {
    let tx = c.unchecked_transaction()?;
    let at = now.div_euclid(STEP_MS) * STEP_MS;
    let lo = at - (POINTS - 1) * STEP_MS - WINDOW_MS;
    let mut stmt = tx.prepare_cached(
        "SELECT a.attempt_id, t.session_id, COALESCE(r.started,a.created),
                r.ended,
                r.attempt_id IS NOT NULL, r.ended IS NOT NULL,
                CASE WHEN a.state='unknown' OR
                     (a.state='finished' AND r.attempt_id IS NOT NULL AND r.ended IS NULL) THEN a.updated END,
                COALESCE(r.baseline,0)
           FROM runtime_attempts a JOIN runtime_turns t USING(turn_id)
           JOIN sessions s ON s.id=t.session_id
           LEFT JOIN items i ON i.id=s.item_id AND i.workspace_id=s.workspace_id
           LEFT JOIN realtime_output_attempts r ON r.attempt_id=a.attempt_id AND r.workspace_id=s.workspace_id
          WHERE s.workspace_id=?1 AND s.readonly=0 AND COALESCE(r.not_started,0)=0
            AND COALESCE(r.started,a.created)<=?4
            AND (r.ended IS NULL OR r.ended>?5)
            AND (?2 IS NULL OR (CASE WHEN r.attempt_id IS NULL THEN i.project_id ELSE r.project_id END)=?2)
            AND (?3 IS NULL OR (CASE WHEN r.attempt_id IS NULL THEN s.bot_id ELSE r.bot_id END)=?3)",
    )?;
    let mut attempts = stmt
        .query_map(
            params![scope.workspace, scope.project, scope.bot, at, lo],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Attempt {
                        session: r.get(1)?,
                        started: r.get(2)?,
                        ended: r.get(3)?,
                        tracked: r.get(4)?,
                        baseline: r.get(7)?,
                        confirmed_end: r.get(5)?,
                        unknown_at: r.get(6)?,
                        gap_at: None,
                        samples: vec![],
                    },
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_attempt_samples(&tx, &mut attempts, lo, at)?;
    // Old non-Node sessions have no compatible collection path. Their persisted ended lifecycle
    // bounds the unknown interval, but does not erase its historical windows or prove coverage.
    let mut st = tx.prepare_cached(
        "SELECT s.id,s.created,CASE WHEN s.status='ended' THEN MAX(s.created,s.updated) END
           FROM sessions s LEFT JOIN items i ON i.id=s.item_id AND i.workspace_id=s.workspace_id
          WHERE s.workspace_id=?1 AND s.readonly=0 AND s.created<=?4
            AND (s.status IN ('running','starting','busy') OR (s.status='ended' AND MAX(s.created,s.updated)>?5))
            AND NOT EXISTS(SELECT 1 FROM runtime_turns t WHERE t.session_id=s.id)
            AND (?2 IS NULL OR i.project_id=?2) AND (?3 IS NULL OR s.bot_id=?3)",
    )?;
    for a in st.query_map(
        params![scope.workspace, scope.project, scope.bot, at, lo],
        |r| {
            Ok(Attempt {
                session: r.get(0)?,
                started: r.get(1)?,
                ended: r.get(2)?,
                tracked: false,
                baseline: false,
                confirmed_end: false,
                unknown_at: None,
                gap_at: None,
                samples: vec![],
            })
        },
    )? {
        attempts.push((String::new(), a?));
    }
    drop(st);
    tx.commit()?;
    let current = aggregate(&attempts, at);
    let series = (0..POINTS)
        .rev()
        .map(|i| {
            let p = aggregate(&attempts, at - i * STEP_MS);
            Point {
                at: at - i * STEP_MS,
                tps: p.tps,
                state: p.state,
            }
        })
        .collect();
    Ok(Realtime {
        at,
        window_ms: WINDOW_MS,
        step_ms: STEP_MS,
        tps: current.tps,
        output_sessions: current.output_sessions,
        state: current.state,
        coverage: current.coverage,
        series,
        estimator: ESTIMATOR,
        estimated_tps: current.estimated_tps,
        measured_tps: None,
        observed_output_sessions: current.observed_output_sessions,
    })
}

fn load_attempt_samples(
    tx: &Connection,
    attempts: &mut [(String, Attempt)],
    lo: i64,
    at: i64,
) -> Result<(), StoreError> {
    for (id, a) in attempts {
        if !a.tracked {
            continue; // no compatible collection record in this workspace, including a moved session
        }
        a.gap_at = tx.query_row(GAP_SQL, [id.as_str()], |r| r.get(0))?;
        // One preceding activity is enough for the first point's freshness check. No need to
        // reread a long attempt's entire journal every second.
        let mut st = tx.prepare_cached(WINDOW_SQL)?;
        a.samples = st
            .query_map(params![id.as_str(), lo, at], |r| {
                Ok(Sample {
                    at: r.get(0)?,
                    units: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut preceding = tx.prepare_cached(PRECEDING_SQL)?;
        a.samples.extend(
            preceding
                .query_map(params![id.as_str(), lo], |r| {
                    Ok(Sample {
                        at: r.get(0)?,
                        units: 0,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    Ok(())
}

struct Aggregate {
    tps: Option<f64>,
    output_sessions: Option<usize>,
    state: Quality,
    coverage: Coverage,
    estimated_tps: Option<f64>,
    observed_output_sessions: usize,
}

fn aggregate(attempts: &[(String, Attempt)], at: i64) -> Aggregate {
    #[derive(Default)]
    struct Session {
        missing: bool,
        units: i64,
    }
    let lo = at - WINDOW_MS;
    let mut sessions: HashMap<&str, Session> = HashMap::new();
    for (_, a) in attempts {
        if a.started > at || a.ended.is_some_and(|end| end <= lo) {
            continue;
        }
        let session = sessions.entry(&a.session).or_default();
        let latest = a.samples.iter().filter(|s| s.at <= at).map(|s| s.at).max();
        let closed = a.confirmed_end && a.ended.is_some_and(|end| end <= at);
        session.missing |= !a.baseline
            || a.gap_at.is_some_and(|g| g <= at)
            || a.unknown_at.is_some_and(|u| u <= at)
            || (a.ended.is_some_and(|end| end <= at) && !a.confirmed_end)
            || (!closed && latest.is_none_or(|last| at - last > FRESH_MS));
        session.units += a
            .samples
            .iter()
            .filter(|s| s.at > lo && s.at <= at)
            .map(|s| s.units)
            .sum::<i64>();
    }
    let mut coverage = Coverage::default();
    let mut units = 0i64;
    let mut output = 0;
    for s in sessions.values() {
        if s.missing {
            coverage.unavailable += 1;
        } else {
            coverage.estimated += 1;
        }
        units += s.units;
        output += usize::from(s.units > 0);
    }
    let known = coverage.estimated > 0 || units > 0;
    let missing = coverage.unavailable > 0;
    let state = match (missing, known, units > 0) {
        (true, true, _) => Quality::Partial,
        (true, false, _) => Quality::Unavailable,
        (false, _, true) => Quality::Estimated,
        (false, _, false) => Quality::Idle,
    };
    let rate = units as f64 / 4.0 / (WINDOW_MS as f64 / 1_000.0);
    Aggregate {
        tps: (!missing).then_some(rate),
        output_sessions: (!missing).then_some(output),
        state,
        coverage,
        estimated_tps: known.then_some(rate),
        observed_output_sessions: output,
    }
}
