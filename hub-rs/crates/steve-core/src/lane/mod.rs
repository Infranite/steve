//! Merge lanes: one per target (an upstream repository and a branch), whoever asks to merge into it.
//! Every merge request into a target goes through its lane: alone when the lane is idle, in a batch when
//! others arrive while one is under way (plan: `steve-merge-window-20261104/PLAN.md`).
//!
//! - [`target`]: which target a working copy merges into, the same for every clone, remote spelling and
//!   worktree of the repository;
//! - [`precheck`]: what a request brings that the target, or another request, doesn't have already;
//! - [`batch`]: the window of a lane (straight through when idle, batched when concurrent), as a pure
//!   state machine.
//!
//! Both are local git: fetch first ([`precheck::fetch`]), off the core's lock.

pub mod batch;
pub mod precheck;
pub mod profile;
pub mod target;
