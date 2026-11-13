//! A Bot's skills on their way to its sessions' harnesses (`steve_modules::bot_skills`).
//!
//! When a session is opened or reopened on a Node ([`super::node_runtime`]), its Bot's enabled
//! skills go into [`OpenSession::skills`] as they are then. The Node writes them where the
//! session's harness adapter declares it reads skills, in the session's own harness directory;
//! an adapter that declares none gets none. The same path serves every harness and every Node.
//!
//! [`OpenSession::skills`]: steve_node_api::OpenSession::skills

use rusqlite::Connection;
use steve_node_api::Skill;
use steve_store::bot_skills as repo;

/// The skills `session`'s harness is given: its Bot's enabled ones, as they are now.
pub fn for_session(c: &Connection, session: &str) -> steve_store::Result<Vec<Skill>> {
    Ok(repo::enabled_for_session(c, session)?
        .into_iter()
        .map(|s| Skill {
            name: s.name,
            description: s.description,
            content: s.content,
        })
        .collect())
}
