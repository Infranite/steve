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
    steve_node_api::usage::decimal_amount(d)
}

#[cfg(test)]
use steve_node_api::usage::token_count as tokens;

/// A context size from a Node (`context_used`, `context_limit`). `None`: past any real one.
pub(crate) fn context(x: u64) -> Option<i64> {
    i64::try_from(x)
        .ok()
        .filter(|_| steve_node_api::usage::token_count(x).is_some())
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
        let Some(c) = Self::report_cost(report) else {
            return Ok(None);
        };
        let delta = if report.measurement == Measurement::Cumulative {
            self.cumulative_delta(report, c, before)?
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

    /// Validate the report's scope, currency and plain decimal before touching the meter.
    fn report_cost(report: &UsageReport) -> Option<f64> {
        if report.scope == UsageScope::Background {
            return None;
        }
        let Some(m) = &report.cost else {
            return None;
        };
        if !m.currency.trim().eq_ignore_ascii_case("USD") {
            tracing::warn!(
                report = report.report_id,
                currency = m.currency.chars().take(16).collect::<String>(),
                "node usage: cost refused, currency is not USD"
            );
            return None;
        }
        let Some(c) = money(&m.decimal) else {
            tracing::warn!(
                report = report.report_id,
                cost = m.decimal.chars().take(48).collect::<String>(),
                "node usage: cost refused, not a non-negative decimal"
            );
            return None;
        };
        Some(c)
    }

    /// Advance an epoch's high water even if its increment is subsequently refused by a cap.
    fn cumulative_delta(
        &mut self,
        report: &UsageReport,
        c: f64,
        before: impl FnOnce() -> Result<Option<f64>, StoreError>,
    ) -> Result<f64, StoreError> {
        let epoch = report.meter_epoch.to_string();
        let before = match self.high.get(&epoch) {
            Some(b) => Some(*b),
            None => before()?,
        };
        self.high.insert(epoch, before.map_or(c, |b| b.max(c)));
        Ok(match before {
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
        })
    }
}

/// Compatibility at the Hub boundary for older context-only ACP meters: their cost was the
/// process total, not an attempt snapshot. Rebase using the existing admitted increment. Token
/// snapshots and final billing reports retain the UsageReport attempt contract unchanged.
pub(super) fn accounting_report(report: &UsageReport, admitted: Option<f64>) -> UsageReport {
    let mut out = report.clone();
    let context_meter = report.measurement == Measurement::Cumulative
        && report.tokens == steve_node_api::Tokens::default()
        && (report.context_used.is_some() || report.context_limit.is_some());
    if context_meter {
        out.measurement = Measurement::Delta;
        out.cost = admitted.map(|value| steve_node_api::Money {
            decimal: value.to_string(),
            currency: "USD".into(),
        });
    } else if admitted.is_none() {
        // Refusal is unknown accounting evidence. Dropping an invalid delta would let a
        // prior amount survive as though the whole sum were known. Session cost is unchanged.
        if let Some(cost) = &mut out.cost {
            cost.decimal = "unavailable".into();
        }
    }
    out
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
        "SELECT json_extract(payload,'$.body.report.cost.decimal'), json_extract(payload,'$.body.report.cost.currency') FROM node_event_inbox
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
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        },
    )?;
    let mut high: Option<f64> = None;
    for row in rows {
        let (d, cur) = row?;
        if !cur
            .as_deref()
            .map(str::trim)
            .is_some_and(|c| c.eq_ignore_ascii_case("USD"))
        {
            continue;
        }
        if let Some(v) = d.as_deref().and_then(money) {
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
        let mut eur = report(Measurement::Delta, "1.25", "e");
        eur.cost.as_mut().unwrap().currency = "EUR".into();
        assert_eq!(add(&mut m, &eur), None);
        for blank in ["", "   "] {
            let mut no_cur = report(Measurement::Delta, "1.25", "e");
            no_cur.cost.as_mut().unwrap().currency = blank.into();
            assert_eq!(add(&mut m, &no_cur), None, "currency {blank:?}");
        }
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
