use sea_query::{Expr, ExprTrait, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Row;
use crate::{Read, Result, W};

pub struct Catalog<'a, C>(pub &'a mut C);

/// Rates are USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub tiered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub provider: String,
    pub model: String,
    pub name: String,
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Version {
    pub since: i64,
    pub name: String,
    pub rates: Rates,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SyncState {
    pub synced: Option<i64>,
    pub tried: Option<i64>,
    pub error: Option<String>,
    #[serde(skip)]
    pub etag: Option<String>,
    pub models: i64,
}

fn prices(provider: &str, model: &str) -> SelectStatement {
    Query::select()
        .columns([
            "since",
            "name",
            "input",
            "output",
            "cache_read",
            "cache_write",
            "tiered",
        ])
        .from("price_catalog")
        .and_where(Expr::col("provider").eq(provider))
        .and_where(Expr::col("model").eq(model))
        .take()
}

fn version(r: &Row<'_>) -> Result<Version> {
    Ok(Version {
        since: r.get(0)?,
        name: r.get(1)?,
        rates: Rates {
            input: r.get(2)?,
            output: r.get(3)?,
            cache_read: r.get(4)?,
            cache_write: r.get(5)?,
            tiered: r.get(6)?,
        },
    })
}

impl<C: Read> Catalog<'_, C> {
    pub fn versions(&mut self, provider: &str, model: &str) -> Result<Vec<Version>> {
        self.0.all(
            prices(provider, model).order_by("since", Order::Asc),
            version,
        )
    }

    fn latest(&mut self, provider: &str, model: &str) -> Result<Option<Version>> {
        self.0.one(
            prices(provider, model)
                .order_by("since", Order::Desc)
                .limit(1),
            version,
        )
    }

    pub fn state(&mut self, source: &str) -> Result<SyncState> {
        Ok(self
            .0
            .one(
                Query::select()
                    .columns(["synced", "tried", "error", "etag", "models"])
                    .from("price_sync")
                    .and_where(Expr::col("source").eq(source)),
                |r| {
                    Ok(SyncState {
                        synced: r.get(0)?,
                        tried: r.get(1)?,
                        error: r.get(2)?,
                        etag: r.get(3)?,
                        models: r.get(4)?,
                    })
                },
            )?
            .unwrap_or_default())
    }
}

impl Catalog<'_, W<'_>> {
    /// The first observed price is effective for all prior days. Each later change starts a new version.
    pub fn save(&mut self, entries: &[Entry], now: i64) -> Result<usize> {
        let mut written = 0;
        const KEY: [&str; 3] = ["provider", "model", "since"];
        const RATES: [&str; 6] = [
            "input",
            "output",
            "cache_read",
            "cache_write",
            "tiered",
            "name",
        ];
        for entry in entries {
            let since = match self.latest(&entry.provider, &entry.model)? {
                Some(v) if v.rates == entry.rates => continue,
                Some(_) => now,
                None => 0,
            };
            let r = entry.rates;
            written += self.0.exec(
                Query::insert()
                    .into_table("price_catalog")
                    .columns(KEY.into_iter().chain(RATES))
                    .values_panic([
                        entry.provider.as_str().into(),
                        entry.model.as_str().into(),
                        since.into(),
                        r.input.into(),
                        r.output.into(),
                        r.cache_read.into(),
                        r.cache_write.into(),
                        i64::from(r.tiered).into(),
                        entry.name.as_str().into(),
                    ])
                    .on_conflict(OnConflict::columns(KEY).update_columns(RATES).to_owned()),
            )?;
        }
        Ok(written as usize)
    }

    pub fn record_ok(
        &mut self,
        source: &str,
        now: i64,
        etag: Option<&str>,
        models: Option<usize>,
    ) -> Result<()> {
        let prior = self.state(source)?;
        let models = models.map(|n| n as i64).unwrap_or(prior.models);
        self.0.exec(
            Query::insert()
                .into_table("price_sync")
                .columns(["source", "synced", "tried", "error", "etag", "models"])
                .values_panic([
                    source.into(),
                    now.into(),
                    now.into(),
                    Option::<&str>::None.into(),
                    etag.or(prior.etag.as_deref()).into(),
                    models.into(),
                ])
                .on_conflict(
                    OnConflict::column("source")
                        .update_columns(["synced", "tried", "error", "etag", "models"])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn record_err(&mut self, source: &str, now: i64, error: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("price_sync")
                .columns(["source", "tried", "error"])
                .values_panic([source.into(), now.into(), error.into()])
                .on_conflict(
                    OnConflict::column("source")
                        .update_columns(["tried", "error"])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }
}
