//! One attempt's accounting primitive, shared by durable meters and one-shot calls.
//! No clocks, collector I/O, or live-rate estimates. Raw model identity is retained until projection.
use serde::{Deserialize, Serialize};

use crate::{Measurement, Money, Tokens, UsageReport, UsageScope};

/// A finite, non-negative reported amount. This does not perform currency conversion.
pub fn amount(m: &Money) -> Option<f64> {
    decimal_amount(&m.decimal)
}

/// Plain non-negative decimals, bounded at both local and remote accounting edges.
/// Session admission may apply stricter per-report/attempt caps independently.
pub fn decimal_amount(d: &str) -> Option<f64> {
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
    d.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && (0.0..=1e9).contains(v))
}

/// Shared sanity bound; rejected counts remain unknown instead of overflowing SQL integers.
pub fn token_count(n: u64) -> Option<i64> {
    (n <= 1_000_000_000_000).then(|| n as i64)
}

/// Durable source state. A context notification cannot seal accounting fields.
/// Persist this state, rather than reusing a materialized snapshot as an incremental meter head.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageMeter {
    pub report: UsageReport,
    fields: [Option<Measurement>; 5],
    cost_measurement: Option<Measurement>,
    unknown_cost: bool,
}

fn fields(t: Tokens) -> [Option<u64>; 5] {
    [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
}
fn tokens(f: [Option<u64>; 5]) -> Tokens {
    Tokens {
        input: f[0],
        output: f[1],
        cache_read: f[2],
        cache_write: f[3],
        reasoning: f[4],
    }
}

/// Fold an already-deduplicated report. Report IDs are scoped to the attempt by the caller.
/// The requested model is a display fallback, never evidence that two reported models are aliases.
pub fn accumulate_usage(totals: &mut Vec<UsageMeter>, mut r: UsageReport) {
    if r.scope == UsageScope::Background {
        return;
    }
    if r.model.as_deref() == Some("") {
        r.model = None;
    }
    let i = totals
        .iter()
        .position(|t| {
            t.report.meter_epoch == r.meter_epoch
                && t.report.model == r.model
                && t.report.scope == r.scope
        })
        .unwrap_or_else(|| {
            let mut empty = r.clone();
            empty.tokens = Tokens::default();
            empty.cost = None;
            totals.push(UsageMeter {
                report: empty,
                fields: [None; 5],
                cost_measurement: None,
                unknown_cost: false,
            });
            totals.len() - 1
        });
    let state = &mut totals[i];
    let total = &mut state.report;
    total.context_used = r.context_used.or(total.context_used);
    total.context_limit = r.context_limit.or(total.context_limit);
    let finalized = state.fields.contains(&Some(Measurement::Final))
        || state.cost_measurement == Some(Measurement::Final);
    if finalized && r.measurement != Measurement::Final {
        return;
    }
    let mut f = fields(total.tokens);
    for (i, value) in fields(r.tokens).into_iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        if state.fields[i] == Some(Measurement::Final) && r.measurement != Measurement::Final {
            continue;
        }
        f[i] = Some(if r.measurement == Measurement::Delta {
            f[i].unwrap_or(0).saturating_add(value)
        } else {
            value
        });
        state.fields[i] = Some(r.measurement);
    }
    total.tokens = tokens(f);
    if let Some(cost) = r.cost {
        if state.cost_measurement != Some(Measurement::Final) || r.measurement == Measurement::Final
        {
            if r.measurement != Measurement::Delta {
                state.unknown_cost = amount(&cost).is_none();
                total.cost = Some(cost);
            } else if !state.unknown_cost {
                total.cost = match total.cost.take() {
                    Some(old) => {
                        let sum = amount(&old)
                            .zip(amount(&cost))
                            .filter(|_| old.currency == cost.currency)
                            .map(|(a, b)| a + b)
                            .filter(|v| v.is_finite() && (0.0..=1e9).contains(v));
                        state.unknown_cost = sum.is_none();
                        sum.map(|v| Money {
                            decimal: v.to_string(),
                            currency: cost.currency,
                        })
                    }
                    None => {
                        state.unknown_cost = amount(&cost).is_none();
                        Some(cost)
                    }
                };
            }
            state.cost_measurement = Some(r.measurement);
        }
    }
    total.report_id = r.report_id;
    // Exported rows are snapshots; only the per-field state above controls late reports.
    total.measurement = if state
        .fields
        .iter()
        .flatten()
        .any(|m| *m == Measurement::Final)
        || state.cost_measurement == Some(Measurement::Final)
    {
        Measurement::Final
    } else {
        Measurement::Cumulative
    };
}

/// Materialize a bill. All covers Main only in the same epoch and model, field by field.
/// An anonymous Main is the adapter's main-loop aggregate (prompt `usage`); when that epoch
/// names exactly one All model (quota `model_usage`), it has a unique canonical owner. This
/// also handles requested aliases without string heuristics or inventing a reported model.
/// With several All models, retain uncovered anonymous field residuals without naming a model.
/// Named A and named B are always independent; different epochs are never inferred to overlap.
pub fn accounting_usage(totals: &[UsageMeter], fallback_model: &str) -> Vec<UsageReport> {
    let mut rows: Vec<_> = totals
        .iter()
        .filter_map(|t| {
            let mut r = t.report.clone();
            if t.unknown_cost {
                r.cost = None;
            }
            (r.tokens != Tokens::default() || r.cost.is_some()).then_some(r)
        })
        .collect();
    // Accumulate into All at its original slot, then emit it at the earliest contributor's
    // position. Mutating the output position during matching would let an anonymous Main and
    // a named Main consume the same All twice (or lose a complement) in interleaved input.
    let mut owners: Vec<usize> = (0..rows.len()).collect();
    let mut main_order: Vec<_> = (0..rows.len())
        .filter(|i| rows[*i].scope == UsageScope::Main)
        .collect();
    // First complete named All fields from their named Main. Only then subtract coverage from
    // the anonymous aggregate; source order still determines where each bill is emitted below.
    main_order.sort_by_key(|i| rows[*i].model.is_none());
    for i in main_order {
        let main = &rows[i];
        let candidates: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, all)| all.scope == UsageScope::All && all.meter_epoch == main.meter_epoch)
            .map(|(j, _)| j)
            .collect();
        let exact = candidates
            .iter()
            .copied()
            .find(|j| rows[*j].model == main.model);
        let owner = exact
            .or_else(|| (main.model.is_none() && candidates.len() == 1).then(|| candidates[0]));
        let Some(j) = owner else {
            if main.model.is_none() && !candidates.is_empty() {
                // The adapter's anonymous prompt usage is the Main aggregate, while quota
                // rows cover the fuller per-model bill. Retain only fields not evidenced by
                // that aggregate; residuals have no invented canonical model. Materialize
                // a residual as All so normalizing these snapshots again is idempotent.
                let mut residual = fields(main.tokens);
                for (field, value) in residual.iter_mut().enumerate() {
                    if let Some(m) = *value {
                        let known = candidates
                            .iter()
                            .filter_map(|j| fields(rows[*j].tokens)[field])
                            .reduce(u64::saturating_add);
                        *value = match known {
                            Some(known) => (m > known).then_some(m.saturating_sub(known)),
                            None => Some(m),
                        };
                    }
                }
                if let Some(main_cost) = &main.cost {
                    if let Some(m) = amount(main_cost) {
                        let known = candidates
                            .iter()
                            .filter_map(|j| rows[*j].cost.as_ref())
                            .filter(|c| c.currency == main_cost.currency)
                            .filter_map(amount)
                            .reduce(|a, b| a + b);
                        if let Some(known) = known {
                            rows[i].cost = (m > known).then(|| Money {
                                decimal: (m - known).to_string(),
                                currency: main_cost.currency.clone(),
                            });
                        }
                    }
                }
                rows[i].tokens = tokens(residual);
                if rows[i].tokens != Tokens::default() {
                    rows[i].scope = UsageScope::All;
                }
            }
            continue;
        };
        let main = rows[i].clone();
        let all = &mut rows[j];
        let mut f = fields(all.tokens);
        for (i, m) in fields(main.tokens).into_iter().enumerate() {
            f[i] = f[i].or(m);
        }
        all.tokens = tokens(f);
        all.cost = all.cost.take().or(main.cost);
        owners[i] = j;
    }
    let mut emitted = std::collections::HashSet::new();
    let materialized: Vec<_> = owners
        .into_iter()
        .filter_map(|j| emitted.insert(j).then(|| rows[j].clone()))
        .collect();
    materialized
        .into_iter()
        .filter(|r| r.tokens != Tokens::default() || r.cost.is_some())
        .map(|mut r| {
            if r.model.is_none()
                && !fallback_model.is_empty()
                && !totals.iter().any(|t| {
                    t.report.scope == UsageScope::All && t.report.meter_epoch == r.meter_epoch
                })
            {
                r.model = Some(fallback_model.into());
            }
            r
        })
        .collect()
}

/// Source-order replay; duplicate report IDs are ignored within this attempt. Results are snapshots
/// and may cross the same boundary again (for example LocalAcp -> Tried::of) without doubling.
pub fn merged_usage(
    reports: impl IntoIterator<Item = UsageReport>,
    fallback_model: &str,
) -> Vec<UsageReport> {
    let mut seen = std::collections::HashSet::new();
    let mut totals = vec![];
    for r in reports {
        if r.scope != UsageScope::Background && seen.insert(r.report_id.clone()) {
            accumulate_usage(&mut totals, r);
        }
    }
    accounting_usage(&totals, fallback_model)
}

/// A context cost companion is not another token meter. An anonymous Main cost accompanying
/// the same epoch's All bill has the same role; a separate named cost-only model stays missing.
pub fn accounting_tokens(r: &UsageReport, batch: &[UsageReport]) -> bool {
    [
        r.tokens.input,
        r.tokens.output,
        r.tokens.cache_read,
        r.tokens.cache_write,
    ]
    .iter()
    .any(Option::is_some)
        || (r.context_used.is_none()
            && r.context_limit.is_none()
            && !(r.scope == UsageScope::Main
                && r.model.is_none()
                && batch
                    .iter()
                    .any(|all| all.scope == UsageScope::All && all.meter_epoch == r.meter_epoch)))
}
