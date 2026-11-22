//! What a Node's usage reports may do to a session's cost and to the statistics (M3). A report is
//! the Node's word, and a remote Node is not trusted with the Hub's books: a figure that is not a
//! plain non-negative decimal (`NaN`, `inf`, `-1`, `1e9`) is refused, a running total that went
//! down adds nothing (a harness restart is a new meter epoch, never a lower total), and what one
//! report or one attempt adds stays under [`CostCaps`]. A refused figure is logged and changes
//! nothing; the rest of the event (tokens, context, model) still applies. So the cost of a session
//! only ever goes up, by at most the caps.

use steve_node_api::{Measurement, NodeEvent, NodeId, UsageReport, UsageScope};
use steve_store::{Ingest, MeterKey, StoreError, W};

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

/// What `report` adds to the cost of its attempt, which added `added` so far; `None`: nothing (no
/// figure, or a refused one, logged). `before`: the highest running total its meter epoch reported
/// earlier ([`mark`]).
pub(crate) fn admit(
    caps: CostCaps,
    report: &UsageReport,
    added: f64,
    before: Option<f64>,
) -> Option<f64> {
    let c = report_cost(report)?;
    let delta = if report.measurement == Measurement::Cumulative {
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
    if delta > caps.report {
        tracing::warn!(
            report = report.report_id,
            delta,
            cap = caps.report,
            "node usage: cost refused, more than one report may add"
        );
        return None;
    }
    if added + delta > caps.attempt {
        tracing::warn!(
            report = report.report_id,
            delta,
            added,
            cap = caps.attempt,
            "node usage: cost refused, more than one attempt may add"
        );
        return None;
    }
    Some(delta)
}

/// Validate the report's scope, currency and plain decimal.
fn report_cost(report: &UsageReport) -> Option<f64> {
    if report.scope == UsageScope::Background {
        return None;
    }
    let m = report.cost.as_ref()?;
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

/// The running total `report` gives, if it is one (cumulative, in dollars, plain money).
fn running_total(report: &UsageReport) -> Option<f64> {
    let m = report.cost.as_ref()?;
    (report.measurement == Measurement::Cumulative && m.currency.trim().eq_ignore_ascii_case("USD"))
        .then(|| money(&m.decimal))
        .flatten()
}

/// The harness process's meter (the report's meter epoch) on `ev`'s stream.
fn meter<'a>(node: &'a NodeId, ev: &'a NodeEvent, report: &'a UsageReport) -> MeterKey<'a> {
    MeterKey {
        node: node.as_str(),
        stream: ev.cursor.stream.as_str(),
        epoch: ev.cursor.epoch.as_str(),
        meter_epoch: report.meter_epoch.as_str(),
    }
}

/// The highest running total the same harness process (the report's meter epoch) reported before
/// `ev` on its stream; `None` for a report that is no running total.
pub(super) fn mark(
    w: &mut W<'_>,
    node: &NodeId,
    ev: &NodeEvent,
    report: &UsageReport,
) -> Result<Option<f64>, StoreError> {
    if report.measurement != Measurement::Cumulative {
        return Ok(None);
    }
    w.cost_marks().mark(meter(node, ev, report))
}

/// Keeps the highest running total of the report's harness process ([`mark`]).
pub(super) fn set_mark(
    w: &mut W<'_>,
    node: &NodeId,
    ev: &NodeEvent,
    report: &UsageReport,
) -> Result<(), StoreError> {
    let Some(c) = running_total(report) else {
        return Ok(());
    };
    w.cost_marks().raise(meter(node, ev, report), c)
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

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::{Epoch, Money, Tokens};

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
        let caps = CostCaps::default();
        let add = |r: &UsageReport, before| admit(caps, r, 0.0, before);
        for bad in ["NaN", "-1000", "inf"] {
            assert_eq!(add(&report(Measurement::Delta, bad, "e"), None), None);
        }
        for cur in ["EUR", "", "   "] {
            let mut r = report(Measurement::Delta, "1.25", "e");
            r.cost.as_mut().unwrap().currency = cur.into();
            assert_eq!(add(&r, None), None, "currency {cur:?}");
        }
        assert_eq!(
            add(&report(Measurement::Delta, "0.5", "e"), None),
            Some(0.5)
        );
        // a running total: its first figure counts whole, then only what it grew by; lower: nothing
        let c = Measurement::Cumulative;
        assert_eq!(add(&report(c, "2", "p"), None), Some(2.0));
        assert_eq!(add(&report(c, "3", "p"), Some(2.0)), Some(1.0));
        assert_eq!(add(&report(c, "1", "p"), Some(3.0)), Some(0.0));
    }

    #[test]
    fn caps_hold_per_report_and_per_attempt() {
        let caps = CostCaps::new(5.0, 8.0);
        let d = |cost| report(Measurement::Delta, cost, "e");
        assert_eq!(admit(caps, &d("6"), 0.0, None), None);
        assert_eq!(admit(caps, &d("5"), 0.0, None), Some(5.0));
        assert_eq!(admit(caps, &d("3"), 5.0, None), Some(3.0));
        assert_eq!(admit(caps, &d("0.01"), 8.0, None), None);
        // a cumulative jump past the cap is refused
        let c = report(Measurement::Cumulative, "50", "p");
        assert_eq!(admit(caps, &c, 0.0, None), None);
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
