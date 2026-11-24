//! Durable daily aggregates for low-cardinality telemetry counters (`telemetry_daily`).
//!
//! Fed by the Hub's typed telemetry pipeline (e.g. `POST /api/system/web-perf` →
//! `steve_web_nav_opens_total`), aggregated by workspace local day and bounded label so
//! frequency data survives Hub restarts without creating a separate storage layer.

use sea_query::{Expr, ExprTrait, Func, OnConflict, Query};

use crate::{Read, Result, W};

/// Typed low-cardinality telemetry series persisted by local day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMetric {
    WebNavOpen,
}

impl TelemetryMetric {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebNavOpen => "web.nav.open",
        }
    }
}

/// Aggregated counts for one telemetry series over a local-day range (or all recorded days).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TelemetryTotals {
    pub since_day: Option<String>,
    pub rows: Vec<(String, i64)>,
}

pub struct TelemetryDaily<'a, C>(pub &'a mut C);

impl<C: Read> TelemetryDaily<'_, C> {
    pub fn totals(
        &mut self,
        ws: &str,
        metric: TelemetryMetric,
        range: Option<(&str, &str)>,
    ) -> Result<TelemetryTotals> {
        let mut since_q = Query::select();
        since_q
            .expr(Func::min(Expr::col("day")))
            .from("telemetry_daily")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("metric").eq(metric.as_str()));
        let mut rows_q = Query::select();
        rows_q
            .column("label")
            .expr(Expr::from(Func::sum(Expr::col("value"))).cast_as("BIGINT"))
            .from("telemetry_daily")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("metric").eq(metric.as_str()))
            .group_by_col("label");
        if let Some((from, to)) = range {
            since_q.and_where(Expr::col("day").between(from, to));
            rows_q.and_where(Expr::col("day").between(from, to));
        }
        let since_day = self
            .0
            .one(&since_q, |r| r.get::<Option<String>>(0))?
            .flatten();
        let rows = self.0.all(&rows_q, |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(TelemetryTotals { since_day, rows })
    }
}

impl TelemetryDaily<'_, W<'_>> {
    pub fn record_counts<'a>(
        &mut self,
        ws: &str,
        day: &str,
        metric: TelemetryMetric,
        counts: impl IntoIterator<Item = (&'a str, i64)>,
    ) -> Result<()> {
        for (label, delta) in counts {
            if delta <= 0 {
                continue;
            }
            self.0.exec(
                Query::insert()
                    .into_table("telemetry_daily")
                    .columns(["workspace_id", "day", "metric", "label", "value"])
                    .values_panic([
                        ws.into(),
                        day.into(),
                        metric.as_str().into(),
                        label.into(),
                        delta.into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["workspace_id", "day", "metric", "label"])
                            .value("value", Expr::col(("telemetry_daily", "value")).add(delta))
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }
}
