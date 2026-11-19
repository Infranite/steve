//! Shared runtime facilities (W2b §2); no workspace dependencies.
pub mod clock;
pub use clock::{Clock, FakeClock, SystemClock};
#[cfg(feature = "due")]
pub mod due;
#[cfg(feature = "policy")]
pub mod policy;
#[cfg(feature = "schedule")]
pub mod schedule;
#[cfg(feature = "tz")]
pub mod tz;

#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "jobs")]
pub use jobs::{FirstRun, Task, every, spawn};
