//! Realtime output, independent of billing and calendar-day statistics. The collector writes
//! quarter-token estimates from streamed text, never from chunk counts or final usage reports.
//! Read an entire answer in one storage snapshot: late samples can revise the trend at their source
//! time, but can never become a burst at the time they arrived.

use std::collections::HashMap;

use serde::Serialize;
pub use steve_store::statistics::RealtimeScope as Scope;
use steve_store::{
    Read, StoreError,
    statistics::{Attempt, RealtimeReads},
};

pub const WINDOW_MS: i64 = 10_000;
pub const STEP_MS: i64 = 1_000;
pub const POINTS: i64 = 60;
/// Without a recent source event, silence is not evidence of zero output. No heartbeat is implied.
pub const FRESH_MS: i64 = 3_000;
pub const ESTIMATOR: &str = "text-v1";

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

/// Reads collector health, attempt ends and samples within one storage snapshot.
pub fn read(c: &mut impl Read, scope: Scope<'_>, now: i64) -> Result<Realtime, StoreError> {
    let at = now.div_euclid(STEP_MS) * STEP_MS;
    let lo = at - (POINTS - 1) * STEP_MS - WINDOW_MS;
    let attempts = RealtimeReads(c).attempts(scope, lo, at)?;
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

struct Aggregate {
    tps: Option<f64>,
    output_sessions: Option<usize>,
    state: Quality,
    coverage: Coverage,
    estimated_tps: Option<f64>,
    observed_output_sessions: usize,
}

fn aggregate(attempts: &[Attempt], at: i64) -> Aggregate {
    #[derive(Default)]
    struct Session {
        missing: bool,
        units: i64,
    }
    let lo = at - WINDOW_MS;
    let mut sessions: HashMap<&str, Session> = HashMap::new();
    for a in attempts {
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
