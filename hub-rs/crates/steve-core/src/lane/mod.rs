//! Merge lanes: one per target (an upstream repository and a branch), whoever asks to merge into it.
//! Every merge request into a target goes through its lane: alone when the lane is idle, in a batch when
//! others arrive while one is under way.
//!
//! - [`target`]: which target a working copy merges into, the same for every clone, remote spelling and
//!   worktree of the repository;
//! - [`batch`]: the window of a lane (straight through when idle, batched when concurrent), as a pure
//!   state machine.
//!
//! `target` is local git: fetch first ([`crate::upstream::fetch`]), off the core's lock; `batch` touches
//! nothing outside itself. Whether a change is on its target already is [`crate::upstream::presence`].

pub mod batch;
pub mod drive;
pub mod exec;
pub mod profile;
pub mod status;
pub mod store;
pub mod target;
