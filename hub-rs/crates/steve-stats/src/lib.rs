//! Statistics (modules/stats.md): token usage, estimated spend and how work flows through items.
//!
//! The detail rows and the daily cube are written by `steve_store::Stats` (migration 15); this crate holds
//! the pure parts every writer and reader shares: how adapters' token figures are brought to one
//! convention ([`normalize`]), the price table and model matching ([`prices`]), and which local day a
//! timestamp belongs to ([`day`]). On top of the stored rows: the per-item facts ([`items`]), their
//! refresh and the cube rebuild ([`refresh`]), the one-off backfill from the older tables ([`backfill`])
//! and every metric of stats.md §3 ([`metrics`]).
//!
//! Hub wiring (slice ST5): at boot `backfill::spawn(store)` and `tokio::spawn(Refresher::new().run(store))`;
//! `POST /api/stats/rebuild` calls `refresh::rebuild`; the REST reads call `metrics::summary` /
//! `metrics::breakdown` on a read connection.

pub mod backfill;
pub mod day;
pub mod items;
pub mod metrics;
pub mod normalize;
pub mod prices;
pub mod refresh;
pub mod views;

pub use day::{day, tz};
pub use normalize::{RawUsage, normalize};
pub use prices::{Price, Priced, base_model, default_prices, lookup, validate};
pub use refresh::{Refresher, rebuild};
pub use steve_store::Tokens;
