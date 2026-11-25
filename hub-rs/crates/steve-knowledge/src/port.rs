//! The knowledge store port: the Hub decides what may be read ([`Scope`]), a store only finds
//! ([`KnowledgeStore::search`]). Ranking by anchors, de-duplication, numbering and the citation
//! ledger stay in [`crate::retrieve`]; `never` and secrets never reach a store (they are kept out
//! when indexing). [`SqliteStore`] is the FTS index in the Hub's database; another store (the
//! OpenViking plugin) implements the same trait out of process.
//!
//! The call is synchronous on purpose: retrieval runs inside the callers' database reads (prompt
//! context, compose), and a store out of process answers on loopback within a short deadline. A
//! store that is not available is an error for the caller, never a fall-back to another store (two
//! stores would drift apart).

use steve_store::base::{Level, Viewer, Visibility};
pub use steve_store::knowledge::Candidate;
use steve_store::knowledge::{ChunkFilter, Chunks, Documents};
use steve_store::{Members, R, Result, StoreError};

use crate::retrieve::{CANDIDATES, Purpose};
use crate::seg::{fts_query, fts_query_relaxed, is_single_cjk};

/// Personal reads follow [`Viewer::sees`], with no creator or team/space restriction.
/// Purpose/audience, source and disclosure-level constraints still apply.
/// `never` is never in it. Built only by [`Scope::for_viewer`] (and, for a conversation, by its
/// card), so callers never write their own conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    /// Workspace and caller identity shared by both filtering paths.
    pub viewer: Viewer,
    /// Only `workspace` objects (service audiences, or shared-workspace main-thread chat).
    pub workspace_only: bool,
    /// Only these source kinds (`notes`, `folder`, `mems` …).
    pub kinds: Option<Vec<String>>,
    /// Documents of paused sources (`source_id`s) are left out …
    pub paused_docs: Vec<String>,
    /// … as are memories and bots' memories when their source is paused.
    pub paused_mems: bool,
    pub paused_bot_mems: bool,
}

impl Scope {
    /// The scope of `viewer` for `purpose`, optionally only `kinds`.
    pub fn for_viewer(
        c: &mut R<'_>,
        viewer: &Viewer,
        purpose: Purpose,
        kinds: Option<&[&str]>,
    ) -> Result<Self> {
        let workspace_only = viewer.is_service()
            || (purpose == Purpose::Chat && Members::new(c).count(&viewer.ws)? > 1);
        let mut paused_docs = Vec::new();
        let (mut paused_mems, mut paused_bot_mems) = (false, false);
        for (id, kind) in Documents(c).paused(&viewer.ws)? {
            match kind.as_str() {
                "mems" => paused_mems = true,
                "bot_mems" => paused_bot_mems = true,
                _ => paused_docs.push(id),
            }
        }
        Ok(Self {
            viewer: viewer.clone(),
            workspace_only,
            kinds: kinds.map(|k| k.iter().map(|s| s.to_string()).collect()),
            paused_docs,
            paused_mems,
            paused_bot_mems,
        })
    }

    /// Whether a candidate a store returned is inside (the Hub's second filter; documents of paused
    /// sources are the store's to leave out, a candidate does not carry its source).
    pub fn admits(&self, c: &Candidate) -> bool {
        if c.level == Level::Never {
            return false;
        }
        let visible = self.viewer.sees(&c.visibility, &c.created_by);
        // Service and shared-conversation audiences are independent of member read access.
        let audience_ok = !self.workspace_only || c.visibility == Visibility::Workspace;
        let kind_ok = self
            .kinds
            .as_ref()
            .is_none_or(|k| k.contains(&c.source_kind));
        let paused = (self.paused_mems && c.obj_type == "mem")
            || (self.paused_bot_mems && c.obj_type == "bot_mem");
        visible && audience_ok && kind_ok && !paused
    }

    fn filter(&self) -> ChunkFilter<'_> {
        ChunkFilter {
            ws: &self.viewer.ws,
            workspace_only: self.workspace_only || self.viewer.is_service(),
            kinds: self.kinds.as_deref(),
            paused_docs: &self.paused_docs,
            paused_mems: self.paused_mems,
            paused_bot_mems: self.paused_bot_mems,
        }
    }
}

/// Which query. Keyword matching first (precise, what the retrieval eval tunes); meaning next,
/// from a store that has it (it adds what other words find); the relaxed keywords fill in last when
/// still few documents were found. Scores only compare within one pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Strict,
    Semantic,
    Relaxed,
}

/// A place knowledge is stored and found.
pub trait KnowledgeStore: Send + Sync {
    /// `sqlite`, `openviking`.
    fn id(&self) -> &'static str;

    /// At most `limit` candidates for `text` inside `scope` (nothing outside it). `c` is the
    /// Hub's connection: a store in the Hub's database reads through it, one elsewhere ignores it.
    /// A store without a pass (no meaning, no relaxed keywords) answers it with nothing.
    fn search(
        &self,
        c: &mut R<'_>,
        scope: &Scope,
        text: &str,
        pass: Pass,
        limit: usize,
    ) -> Result<Vec<Candidate>>;
}

/// The store could not answer (down, timed out): the caller says knowledge is unavailable.
pub fn unavailable(store: &str, why: impl std::fmt::Display) -> StoreError {
    StoreError::Hook(format!("知识库暂不可用（{store}：{why}）"))
}

/// The FTS index in the Hub's database (`kb_chunks`, `kb_fts`): bm25 with the title weighted 2;
/// a one-character CJK query matches titles and memories by `instr` instead.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteStore;

impl SqliteStore {
    pub fn chunk_in_scope(
        c: &mut R<'_>,
        scope: &Scope,
        kind: &str,
        id: &str,
        seq: i64,
    ) -> Result<Option<Candidate>> {
        Chunks(c).chunk_in_scope(&scope.filter(), kind, id, seq)
    }
}

impl KnowledgeStore for SqliteStore {
    fn id(&self) -> &'static str {
        "sqlite"
    }

    fn search(
        &self,
        c: &mut R<'_>,
        scope: &Scope,
        text: &str,
        pass: Pass,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if pass == Pass::Semantic {
            return Ok(Vec::new());
        }
        let filter = scope.filter();
        let limit = limit.min(CANDIDATES);
        if is_single_cjk(text) {
            return if pass == Pass::Relaxed {
                Ok(Vec::new())
            } else {
                Chunks(c).containing(&filter, text, limit)
            };
        }
        let expr = match pass {
            Pass::Strict => fts_query(text),
            Pass::Semantic => None,
            // only when it differs from the strict one
            Pass::Relaxed => {
                fts_query(text).and_then(|s| fts_query_relaxed(text).filter(|r| *r != s))
            }
        };
        let Some(expr) = expr else {
            return Ok(Vec::new());
        };
        Chunks(c).fulltext(&filter, &expr, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(user: &str, workspace_only: bool) -> Scope {
        Scope {
            viewer: Viewer::bare("default", user),
            workspace_only,
            kinds: None,
            paused_docs: Vec::new(),
            paused_mems: false,
            paused_bot_mems: false,
        }
    }

    fn cand(vis: Visibility, by: &str, level: Level) -> Candidate {
        Candidate {
            obj_type: "chunk".into(),
            obj_id: "d1".into(),
            seq: 0,
            heading: String::new(),
            text: "x".into(),
            source_kind: "notes".into(),
            label: String::new(),
            level,
            visibility: vis,
            created_by: by.into(),
            updated: 0,
            base: 1.0,
            title: None,
            doc_ref: None,
        }
    }

    /// The Hub's second filter is `Viewer::sees` plus `never` and the purpose's rule.
    #[test]
    fn admits_what_the_viewer_sees_and_never_never() {
        let viewer = Viewer::bare("default", "usr_me");
        let s = scope("usr_me", false);
        for vis in [
            Visibility::Workspace,
            Visibility::Private,
            Visibility::Team("t1".into()),
            Visibility::Team("t2".into()),
            Visibility::Space("s1".into()),
        ] {
            for by in ["usr_me", "usr_other"] {
                let c = cand(vis.clone(), by, Level::Public);
                assert_eq!(s.admits(&c), viewer.sees(&vis, by), "{vis} by {by}");
                assert!(
                    !s.admits(&cand(vis.clone(), by, Level::Never)),
                    "never: {vis} by {by}"
                );
            }
        }
        // a shared main thread's chat: workspace objects only, also one's own private ones out
        let w = scope("usr_me", true);
        assert!(w.admits(&cand(Visibility::Workspace, "usr_other", Level::Public)));
        assert!(!w.admits(&cand(Visibility::Private, "usr_me", Level::Public)));
        assert!(!w.admits(&cand(
            Visibility::Team("t1".into()),
            "usr_me",
            Level::Public
        )));
        for user in ["", "svc:default"] {
            // Even without an extra audience flag, the shared Viewer rejects its own private data.
            let service = scope(user, false);
            assert!(!service.admits(&cand(Visibility::Private, user, Level::Reasoning)));
            assert!(service.admits(&cand(Visibility::Workspace, user, Level::Reasoning)));
        }
    }
    #[test]
    fn source_and_disclosure_constraints_are_independent_of_member_visibility() {
        let mut s = scope("usr_me", false);
        let mut candidate = cand(Visibility::Private, "usr_other", Level::Reasoning);
        assert!(s.admits(&candidate));
        s.kinds = Some(vec!["folder".into()]);
        assert!(!s.admits(&candidate));
        candidate.source_kind = "folder".into();
        assert!(s.admits(&candidate));
        candidate.obj_type = "mem".into();
        s.paused_mems = true;
        assert!(!s.admits(&candidate));
        candidate.obj_type = "bot_mem".into();
        assert!(s.admits(&candidate));
        s.paused_bot_mems = true;
        assert!(!s.admits(&candidate));
        candidate.obj_type = "chunk".into();
        assert!(s.admits(&candidate));
        candidate.level = Level::Never;
        assert!(!s.admits(&candidate));
    }
}
