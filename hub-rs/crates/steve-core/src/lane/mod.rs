//! Merge lanes: one per target (an upstream repository and a branch), whoever asks to merge into it.
//! Every merge request into a target goes through its lane: alone when the lane is idle, in a batch when
//! others arrive while one is under way.
//!
//! - [`target`]: which target a working copy merges into, the same for every clone, remote spelling and
//!   worktree of the repository;
//! - [`precheck`]: what a request brings that the target, or another request, doesn't have already;
//! - [`batch`]: the window of a lane (straight through when idle, batched when concurrent), as a pure
//!   state machine.
//!
//! `target` and `precheck` are local git: fetch first ([`precheck::fetch`]), off the core's lock; `batch` touches
//! nothing outside itself.

pub mod batch;
pub mod drive;
pub mod exec;
pub mod precheck;
pub mod profile;
pub mod status;
pub mod store;
pub mod target;
