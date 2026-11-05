//! Knowledge base: sources, index, retrieval and citations
//!
//! Empty skeleton (slice I0); slices add their `mod` lines here.

// K0: tokenizing, chunking, extraction, evaluation.
pub mod chunk;
pub mod eval;
pub mod extract_text;
pub mod seg;
#[cfg(test)]
mod testdata;

// K1: index and retrieval.
pub mod api;
pub mod index;
pub mod notes;
pub mod openviking;
pub mod port;
pub mod repo;
pub mod retrieve;
pub mod service;
pub mod sources;
pub mod state;
pub mod sync;

// K2: folder and file sources.
pub mod guard;
pub mod scan;
// K3: item and chat sources, candidate memories.
pub mod candidates;
pub mod extract;

pub use api::routes;
pub use guard::TokenGuard;
pub use retrieve::{Hit, Purpose, Query, anchor_mems, record_use, retrieve};
pub use sources::files::FilesLoader;
pub use sources::folder::{FolderLoader, FolderPolicy, spawn_scans};
pub use state::{Guard, KbEvent, Knowledge, NoGuard};

// K4: prompts, citations, MCP tools, uses.
pub mod cite;
pub mod context;
pub mod tools;
pub mod uses;

pub use context::{KnowledgeContext, chat_context, session_context};

// K5: conversational configuration.
pub mod drafts;
