//! Statistics: token usage, estimated spend and how work flows through items.
//!
//! The detail rows and the daily cube are written by `steve_store::Stats` (migration 15); this crate holds
//! the pure parts every writer and reader shares: how adapters' token figures are brought to one
//! convention ([`normalize`]), the price table and model matching ([`prices`]) with the prices synced
//! from Models.dev ([`catalog`]), and which local day a
//! timestamp belongs to ([`day`]). On top of the stored rows: the per-item facts ([`items`]), their
//! refresh and the cube rebuild ([`refresh`]) and every metric ([`metrics`]).
//!
//! Hub wiring (slice ST5): at boot `tokio::spawn(Refresher::new().run(store))`;
//! `POST /api/stats/rebuild` calls `refresh::rebuild`; the REST reads call `metrics::summary` /
//! `metrics::breakdown` on a read connection.

steve_i18n::init!();

pub mod catalog;
pub mod day;
pub mod events;
pub mod items;
pub mod metrics;
pub mod normalize;
pub mod prices;
pub mod refresh;
pub mod views;

pub use day::{day, tz};
pub use normalize::{RawUsage, normalize};
pub use prices::{
    Price, PriceBook, PriceSource, Priced, Quote, base_model, default_prices, lookup, validate,
};
pub use refresh::{Refresher, rebuild};
pub use steve_store::Tokens;
