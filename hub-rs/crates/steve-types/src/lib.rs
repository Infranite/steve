//! Steve's shared contract types: types only, no production IO or internal crate dependencies.
//! Use concrete payloads or opaque `RawJson`, never general-purpose JSON trees or object maps.
//!
//! TypeScript generation is test-only. Add every new public data type to the explicit list in
//! `gen.rs`. Default `cargo test -p steve-types` compares all bindings and the complete directory.
//! To rewrite, run from `hub-rs`: `STEVE_TS_BLESS=1 cargo test -p steve-types gen`.
//! Generated files live in `web/src/gen`, are committed, and must never be edited or merged by hand.
//! The generator prepends the Steve provenance header to ts-rs's own generated header, and uses
//! TypeScript `number` for JSON integer fields (not ts-rs's default `bigint`).

pub mod automation;
pub mod autonomy;
pub mod channels;
pub mod config;
pub mod devices;
pub mod error;
pub mod event;
pub mod fact;
pub mod id;
pub mod integrations;
pub mod knowledge;
pub mod people;
pub mod raw;
pub mod stats;
pub mod stream;
pub mod vault;
pub mod work;

pub use error::{Code, Error, HttpErrorBody, NOW_MAX_ATTEMPTS, Param, Params, Reason, Retry};
pub use event::{DomainEvent, Envelope, LiveEvent, Scope, Scoped};
pub use fact::DurableFact;
pub use id::*;
pub use raw::RawJson;
pub use stream::StreamControl;

#[cfg(test)]
mod r#gen;

// Keep the guard's forbidden-pattern fixtures outside the source tree it scans.
#[cfg(test)]
#[path = "../tests/support/opaque_json.rs"]
mod opaque_json;
