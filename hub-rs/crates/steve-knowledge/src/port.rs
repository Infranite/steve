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

use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use steve_store::base::{Level, Viewer, Visibility, bind};
use steve_store::{Result, StoreError};

use crate::repo;
use crate::retrieve::{CANDIDATES, Purpose};
use crate::seg::{fts_query, fts_query_relaxed, is_single_cjk};

/// What may be read: the same rule as [`Viewer::sees`] (everything `workspace`, everything `user`
/// created, and the `scopes` codes such as `team:<id>`), plus the purpose's and the sources' rules.
/// `never` is never in it. Built only by [`Scope::for_viewer`] (and, for a conversation, by its
/// card), so callers never write their own conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    pub ws: String,
    /// What this user created is readable whatever its visibility.
    pub user: String,
    /// `team:<id>` / `space:<id>` codes readable beside `workspace`.
    pub scopes: Vec<String>,
    /// Only `workspace` objects (the main thread's chat in a shared workspace, §3.3).
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
        c: &Connection,
        viewer: &Viewer,
        purpose: Purpose,
        kinds: Option<&[&str]>,
    ) -> Result<Self> {
        let workspace_only = purpose == Purpose::Chat && repo::member_count(c, &viewer.ws)? > 1;
        let mut paused_docs = Vec::new();
        let (mut paused_mems, mut paused_bot_mems) = (false, false);
        for (id, kind) in repo::paused(c, &viewer.ws)? {
            match kind.as_str() {
                "mems" => paused_mems = true,
                "bot_mems" => paused_bot_mems = true,
                _ => paused_docs.push(id),
            }
        }
        Ok(Self {
            ws: viewer.ws.clone(),
            user: viewer.user.clone(),
            scopes: viewer.scopes.clone(),
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
        let seen = match &c.visibility {
            Visibility::Workspace => true,
            _ if self.workspace_only => false,
            _ if c.created_by == self.user => true,
            Visibility::Private => false,
            v => self.scopes.contains(&v.code()),
        };
        let kind_ok = self
            .kinds
            .as_ref()
            .is_none_or(|k| k.contains(&c.source_kind));
        let paused = (self.paused_mems && c.obj_type == "mem")
            || (self.paused_bot_mems && c.obj_type == "bot_mem");
        seen && kind_ok && !paused
    }

    /// The `WHERE` tail on `kb_chunks c` for [`SqliteStore`].
    fn sql(&self) -> (String, Vec<Sql>) {
        let v = Viewer {
            ws: self.ws.clone(),
            user: self.user.clone(),
            scopes: self.scopes.clone(),
        };
        let vis = v.sql("c");
        let mut sql = format!("c.workspace_id = ? AND c.level <> 'never' AND {}", vis.sql);
        let mut p = vec![Sql::Text(self.ws.clone())];
        p.extend(vis.params);
        if self.workspace_only {
            sql.push_str(" AND c.visibility = 'workspace'");
        }
        if let Some(kinds) = &self.kinds {
            if kinds.is_empty() {
                sql.push_str(" AND 0");
            } else {
                sql.push_str(&format!(
                    " AND c.source_kind IN ({})",
                    vec!["?"; kinds.len()].join(", ")
                ));
                p.extend(kinds.iter().map(|k| Sql::Text(k.clone())));
            }
        }
        if self.paused_mems {
            sql.push_str(" AND c.obj_type <> 'mem'");
        }
        if self.paused_bot_mems {
            sql.push_str(" AND c.obj_type <> 'bot_mem'");
        }
        if !self.paused_docs.is_empty() {
            sql.push_str(&format!(
                " AND NOT (c.obj_type = 'chunk' AND c.obj_id IN (SELECT id FROM kb_docs WHERE source_id IN ({})))",
                vec!["?"; self.paused_docs.len()].join(", ")
            ));
            p.extend(self.paused_docs.iter().cloned().map(Sql::Text));
        }
        (sql, p)
    }
}

/// One candidate a store found, before the Hub ranks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub obj_type: String,
    pub obj_id: String,
    pub seq: i64,
    pub heading: String,
    pub text: String,
    pub source_kind: String,
    pub label: String,
    pub level: Level,
    pub visibility: Visibility,
    pub created_by: String,
    pub updated: i64,
    /// The store's score, larger = better; only compared within one [`Pass`].
    pub base: f64,
    pub title: Option<String>,
    pub doc_ref: Option<String>,
}

/// Which query: the strict one first; the relaxed one fills in when it found few documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Strict,
    Relaxed,
}

/// A place knowledge is stored and found.
pub trait KnowledgeStore: Send + Sync {
    /// `sqlite`, `openviking`.
    fn id(&self) -> &'static str;

    /// At most `limit` candidates for `text` inside `scope` (nothing outside it). `c` is the
    /// Hub's connection: a store in the Hub's database reads through it, one elsewhere ignores it.
    /// A store that has no relaxed query answers [`Pass::Relaxed`] with nothing.
    fn search(
        &self,
        c: &Connection,
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
/// a one-character CJK query matches titles and memories by `instr` instead (§3.2).
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteStore;

const COLS: &str = "c.obj_type, c.obj_id, c.seq, c.heading, c.text, c.source_kind, c.label, c.level, c.visibility, c.created_by, c.updated";

fn candidate(r: &rusqlite::Row, base: Option<f64>) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        obj_type: r.get(0)?,
        obj_id: r.get(1)?,
        seq: r.get(2)?,
        heading: r.get(3)?,
        text: r.get(4)?,
        source_kind: r.get(5)?,
        label: r.get(6)?,
        level: Level::from_db(&r.get::<_, String>(7)?),
        visibility: Visibility::from_db(&r.get::<_, String>(8)?),
        created_by: r.get(9)?,
        updated: r.get(10)?,
        title: r.get(11)?,
        doc_ref: r.get(12)?,
        // bm25 is negative, smaller = better
        base: match base {
            Some(b) => b,
            None => -r.get::<_, f64>(13)?,
        },
    })
}

impl KnowledgeStore for SqliteStore {
    fn id(&self) -> &'static str {
        "sqlite"
    }

    fn search(
        &self,
        c: &Connection,
        scope: &Scope,
        text: &str,
        pass: Pass,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        let tail = scope.sql();
        let limit = limit.min(CANDIDATES) as i64;
        if is_single_cjk(text) {
            if pass == Pass::Relaxed {
                return Ok(Vec::new());
            }
            let sql = format!(
                "SELECT {COLS}, d.title, d.ref FROM kb_chunks c
                   LEFT JOIN kb_docs d ON c.obj_type = 'chunk' AND d.id = c.obj_id
                  WHERE ((c.obj_type IN ('mem', 'bot_mem') AND instr(c.text, ?) > 0)
                      OR (c.obj_type = 'chunk' AND c.seq = 0 AND instr(d.title, ?) > 0))
                    AND {} ORDER BY c.updated DESC LIMIT {limit}",
                tail.0
            );
            let mut p = vec![Sql::Text(text.into()), Sql::Text(text.into())];
            p.extend(tail.1);
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(bind(p), |r| candidate(r, Some(1.0)))?;
            return Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        let expr = match pass {
            Pass::Strict => fts_query(text),
            // only when it differs from the strict one
            Pass::Relaxed => {
                fts_query(text).and_then(|s| fts_query_relaxed(text).filter(|r| *r != s))
            }
        };
        let Some(expr) = expr else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT {COLS}, d.title, d.ref, bm25(kb_fts, 1.0, 2.0) AS r
               FROM kb_fts JOIN kb_chunks c ON c.id = kb_fts.rowid
               LEFT JOIN kb_docs d ON c.obj_type = 'chunk' AND d.id = c.obj_id
              WHERE kb_fts MATCH ? AND {} ORDER BY r LIMIT {limit}",
            tail.0
        );
        let mut p = vec![Sql::Text(expr)];
        p.extend(tail.1);
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(bind(p), |r| candidate(r, None))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(user: &str, scopes: &[&str], workspace_only: bool) -> Scope {
        Scope {
            ws: "default".into(),
            user: user.into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
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
        let viewer = Viewer {
            ws: "default".into(),
            user: "usr_me".into(),
            scopes: vec!["team:t1".into()],
        };
        let s = scope("usr_me", &["team:t1"], false);
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
        let w = scope("usr_me", &["team:t1"], true);
        assert!(w.admits(&cand(Visibility::Workspace, "usr_other", Level::Public)));
        assert!(!w.admits(&cand(Visibility::Private, "usr_me", Level::Public)));
        assert!(!w.admits(&cand(
            Visibility::Team("t1".into()),
            "usr_me",
            Level::Public
        )));
    }
}
