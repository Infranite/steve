//! What a Node's usage reports may do to a session's cost and to the statistics (M3). A report is
//! the Node's word, and a remote Node is not trusted with the Hub's books: a figure that is not a
//! plain non-negative decimal (`NaN`, `inf`, `-1`, `1e9`) is refused, a running total that went
//! down adds nothing (a harness restart is a new meter epoch, never a lower total), and what one
//! report or one attempt adds stays under [`CostCaps`]. A refused figure is logged and changes
//! nothing; the rest of the event (tokens, context, model) still applies. So the cost of a session
//! only ever goes up, by at most the caps.

use std::collections::HashMap;

use steve_node_api::{Epoch, Measurement, NodeEvent, NodeId, UsageReport, UsageScope};
use steve_store::StoreError;

/// The largest figure a report may carry at all (a harness process's running total included).
const ABSURD: f64 = 1e9;

/// The largest token count of one report the statistics take.
const MAX_TOKENS: u64 = 1_000_000_000_000;

/// How much one usage report, and one attempt in all, may add to a session's cost (the harness's
/// own currency, dollars for the ones Steve runs). `STEVE_NODE_COST_REPORT_MAX` and
/// `STEVE_NODE_COST_ATTEMPT_MAX`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostCaps {
    pub report: f64,
    pub attempt: f64,
}

impl Default for CostCaps {
    fn default() -> CostCaps {
        CostCaps {
            report: 100.0,
            attempt: 1000.0,
        }
    }
}

impl CostCaps {
    /// Finite and positive caps only; anything else keeps the default of that cap.
    pub fn new(report: f64, attempt: f64) -> CostCaps {
        let d = CostCaps::default();
        let ok = |x: f64| x.is_finite() && x > 0.0;
        CostCaps {
            report: if ok(report) { report } else { d.report },
            attempt: if ok(attempt) { attempt } else { d.attempt },
        }
    }
}

/// A money figure from a Node: a plain decimal (`12`, `0.034`), finite, non-negative and not
/// absurd. `None`: anything else (a sign, an exponent, `NaN`, `inf`, spaces, an empty part).
pub(crate) fn money(d: &str) -> Option<f64> {
    if d.is_empty() || d.len() > 40 {
        return None;
    }
    let (int, frac) = match d.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (d, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || frac.is_some_and(|f| !digits(f)) {
        return None;
    }
    let v: f64 = d.parse().ok()?;
    (v.is_finite() && (0.0..=ABSURD).contains(&v)).then_some(v)
}

/// A token count from a Node, as the statistics store it. `None`: past any real report.
pub(crate) fn tokens(x: u64) -> Option<i64> {
    if x > MAX_TOKENS {
        tracing::warn!(tokens = x, "node usage: token count refused");
        return None;
    }
    i64::try_from(x).ok()
}

/// A context size from a Node (`context_used`, `context_limit`). `None`: past any real one.
pub(crate) fn context(x: u64) -> Option<i64> {
    i64::try_from(x).ok().filter(|_| x <= MAX_TOKENS)
}

/// The costs of a sequence of usage reports, in order: those of one attempt (its caps hold across
/// them), or one report alone.
pub(crate) struct Meter {
    caps: CostCaps,
    /// The highest running total seen per meter epoch.
    high: HashMap<String, f64>,
    /// What this attempt added so far.
    added: f64,
}

impl Meter {
    pub(crate) fn new(caps: CostCaps) -> Meter {
        Meter {
            caps,
            high: HashMap::new(),
            added: 0.0,
        }
    }

    /// What `report` adds to the cost; `None`: nothing (no figure, or a refused one, logged).
    /// `before`: the highest running total its meter epoch reported earlier, from the store, asked
    /// only for the first report of an epoch in this sequence.
    pub(crate) fn add(
        &mut self,
        report: &UsageReport,
        before: impl FnOnce() -> Result<Option<f64>, StoreError>,
    ) -> Result<Option<f64>, StoreError> {
        if report.scope == UsageScope::Background {
            return Ok(None);
        }
        let Some(m) = &report.cost else {
            return Ok(None);
        };
        let Some(c) = money(&m.decimal) else {
            tracing::warn!(
                report = report.report_id,
                cost = m.decimal.chars().take(48).collect::<String>(),
                "node usage: cost refused, not a non-negative decimal"
            );
            return Ok(None);
        };
        let delta = if report.measurement == Measurement::Cumulative {
            let epoch = report.meter_epoch.to_string();
            let before = match self.high.get(&epoch) {
                Some(b) => Some(*b),
                None => before()?,
            };
            self.high.insert(epoch, before.map_or(c, |b| b.max(c)));
            match before {
                Some(b) if c < b => {
                    tracing::warn!(
                        report = report.report_id,
                        before = b,
                        now = c,
                        "node usage: a running total went down; nothing added"
                    );
                    0.0
                }
                Some(b) => c - b,
                None => c,
            }
        } else {
            c
        };
        if delta > self.caps.report {
            tracing::warn!(
                report = report.report_id,
                delta,
                cap = self.caps.report,
                "node usage: cost refused, more than one report may add"
            );
            return Ok(None);
        }
        if self.added + delta > self.caps.attempt {
            tracing::warn!(
                report = report.report_id,
                delta,
                added = self.added,
                cap = self.caps.attempt,
                "node usage: cost refused, more than one attempt may add"
            );
            return Ok(None);
        }
        self.added += delta;
        Ok(Some(delta))
    }
}

/// The highest valid running total the same harness process (`epoch`, the report's meter epoch)
/// reported before `ev` on its stream. Figures that are no money ([`money`]) do not count.
pub(crate) fn earlier_cost_in(
    tx: &rusqlite::Connection,
    node: &NodeId,
    ev: &NodeEvent,
    epoch: &Epoch,
) -> Result<Option<f64>, StoreError> {
    let mut st = tx.prepare_cached(
        "SELECT json_extract(payload,'$.body.report.cost.decimal') FROM node_event_inbox
           WHERE node_id=? AND stream=? AND epoch=? AND seq<? AND json_extract(payload,'$.body.type')='usage'
             AND json_extract(payload,'$.body.report.meter_epoch')=? AND json_extract(payload,'$.body.report.cost') IS NOT NULL
             AND json_extract(payload,'$.body.report.measurement')='cumulative'",
    )?;
    let rows = st.query_map(
        rusqlite::params![
            node.as_str(),
            ev.cursor.stream.as_str(),
            ev.cursor.epoch.as_str(),
            ev.cursor.seq as i64,
            epoch.as_str()
        ],
        |r| r.get::<_, Option<String>>(0),
    )?;
    let mut high: Option<f64> = None;
    for d in rows {
        if let Some(v) = d?.as_deref().and_then(money) {
            high = Some(high.map_or(v, |h| h.max(v)));
        }
    }
    Ok(high)
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::{Money, Tokens};

    fn report(m: Measurement, cost: &str, epoch: &str) -> UsageReport {
        UsageReport {
            report_id: format!("r-{cost}"),
            model: None,
            meter_epoch: Epoch::new(epoch).unwrap(),
            scope: UsageScope::Main,
            measurement: m,
            tokens: Tokens::default(),
            context_used: None,
            context_limit: None,
            cost: Some(Money {
                decimal: cost.into(),
                currency: "USD".into(),
            }),
        }
    }

    #[test]
    fn only_plain_non_negative_decimals_are_money() {
        for ok in ["0", "12", "0.034", "1000000", "007.50"] {
            assert!(money(ok).is_some(), "{ok}");
        }
        for bad in [
            "",
            "NaN",
            "nan",
            "inf",
            "-inf",
            "infinity",
            "-1",
            "-0",
            "+1",
            "1e3",
            "1E3",
            "-1e300",
            "1e300",
            ".5",
            "5.",
            "1.2.3",
            " 1",
            "1 ",
            "0x10",
            "1_000",
            "2000000000",
        ] {
            assert_eq!(money(bad), None, "{bad}");
        }
    }

    #[test]
    fn refused_figures_add_nothing_and_totals_never_go_down() {
        let none = || Ok(None);
        let mut m = Meter::new(CostCaps::default());
        let add = |m: &mut Meter, r: &UsageReport| m.add(r, || Ok(None)).unwrap();
        assert_eq!(add(&mut m, &report(Measurement::Delta, "NaN", "e")), None);
        assert_eq!(add(&mut m, &report(Measurement::Delta, "-1000", "e")), None);
        assert_eq!(add(&mut m, &report(Measurement::Delta, "inf", "e")), None);
        assert_eq!(
            add(&mut m, &report(Measurement::Delta, "0.5", "e")),
            Some(0.5)
        );
        // a running total: its first figure counts whole, then only what it grew by
        let c = Measurement::Cumulative;
        assert_eq!(m.add(&report(c, "2", "p"), none).unwrap(), Some(2.0));
        assert_eq!(m.add(&report(c, "3", "p"), none).unwrap(), Some(1.0));
        // lower: nothing, and the next one counts from the highest
        assert_eq!(m.add(&report(c, "1", "p"), none).unwrap(), Some(0.0));
        assert_eq!(m.add(&report(c, "3.5", "p"), none).unwrap(), Some(0.5));
        // an earlier figure in the store is where a new sequence starts
        let mut m = Meter::new(CostCaps::default());
        assert_eq!(
            m.add(&report(c, "10", "q"), || Ok(Some(12.0))).unwrap(),
            Some(0.0)
        );
        assert_eq!(m.add(&report(c, "13", "q"), none).unwrap(), Some(1.0));
    }

    #[test]
    fn caps_hold_per_report_and_per_attempt() {
        let none = || Ok(None);
        let mut m = Meter::new(CostCaps::new(5.0, 8.0));
        assert_eq!(
            m.add(&report(Measurement::Delta, "6", "e"), none).unwrap(),
            None
        );
        assert_eq!(
            m.add(&report(Measurement::Delta, "5", "e"), none).unwrap(),
            Some(5.0)
        );
        assert_eq!(
            m.add(&report(Measurement::Delta, "3", "e"), none).unwrap(),
            Some(3.0)
        );
        assert_eq!(
            m.add(&report(Measurement::Delta, "0.01", "e"), none)
                .unwrap(),
            None
        );
        // a cumulative jump past the cap is refused, and the total still moves on from it
        let mut m = Meter::new(CostCaps::new(5.0, 100.0));
        let c = Measurement::Cumulative;
        assert_eq!(m.add(&report(c, "50", "p"), none).unwrap(), None);
        assert_eq!(m.add(&report(c, "51", "p"), none).unwrap(), Some(1.0));
        // bad caps keep the defaults
        assert_eq!(CostCaps::new(f64::NAN, -1.0), CostCaps::default());
    }

    #[test]
    fn token_and_context_counts_stay_sane() {
        assert_eq!(tokens(12), Some(12));
        assert_eq!(tokens(u64::MAX), None);
        assert_eq!(context(u64::MAX), None);
        assert_eq!(context(200_000), Some(200_000));
    }
}
