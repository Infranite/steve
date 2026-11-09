//! The merge queue (E1): merging and releasing apart. Per target (an upstream repository and a
//! branch) a queue merges continuously, batching itself when busy ([`queue`]); a release train
//! collects what merged and releases it at once, by time, by count or when you say so ([`train`]).
//! Both are pure state machines over the target's rows; [`policy`] holds every number they go by.

pub mod policy;
pub mod queue;
pub mod store;
pub mod train;
