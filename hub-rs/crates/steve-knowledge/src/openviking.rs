//! The OpenViking store (default knowledge store, user decision 2026-11-04): a separate OpenViking
//! server on loopback (AGPL, so out of process) holds every indexed chunk as a file
//! `viking://resources/steve/{ws}/o/{obj_type}/{obj_id}/{seq}.md`, with tags `vis=`, `by=`, `lvl=`,
//! `kind=`, and finds them by meaning ([`Pass::Semantic`]). The Hub's own index stays: it is what
//! the store mirrors (state, not deltas: [`OpenViking::sync_object`] rewrites an object from it, so
//! a repeated or late sync is harmless), the keyword passes keep searching it, and every chunk the
//! store finds is read back from it under the [`Scope`], so nothing the store returns can widen
//! what may be read. `never` chunks are not written.
//!
//! The server: trusted mode, one root key, account `steve`; vectors only (`vectors_only`, no model
//! call), local CPU embedding. See `ops/openviking/`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use steve_store::outbox::{self, Applied, ConsumeError, Consumer, Event, Since};
use steve_store::{Result, Store, StoreError, Tx};

use crate::port::{Candidate, KnowledgeStore, Pass, Scope, SqliteStore, unavailable};

/// The outbox topic of an index change: `{obj_type, obj_id}` of an object whose chunks changed.
pub const TOPIC: &str = "kb.object.changed";
pub const TOPIC_V: i64 = 1;

/// A search must answer within this (it runs inside the callers' reads).
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);
/// How many subject searches run at once.
const FIND_PARALLEL: usize = 8;
/// Writing one object's chunks (embedding on CPU, waited for).
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(120);
/// Below this cosine similarity a chunk is not about the query.
pub const MIN_SCORE: f64 = 0.5;

/// Where the server is and who Steve is to it.
#[derive(Debug, Clone)]
pub struct OpenVikingConfig {
    /// `http://127.0.0.1:<port>`
    pub url: String,
    /// The server's root key (trusted mode).
    pub key: String,
    /// The OpenViking account Steve's knowledge lives in.
    pub account: String,
}

/// The client: search ([`KnowledgeStore`]) and mirroring ([`OpenViking::sync_object`]).
pub struct OpenViking {
    cfg: OpenVikingConfig,
    search: ureq::Agent,
    write: ureq::Agent,
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// The directory of an object.
pub fn object_dir(ws: &str, obj_type: &str, obj_id: &str) -> String {
    format!("viking://resources/steve/{ws}/o/{obj_type}/{obj_id}")
}

fn objects_root(ws: &str) -> String {
    format!("viking://resources/steve/{ws}/o")
}

/// `(obj_type, obj_id, seq)` of a chunk file's URI.
fn parse_uri(uri: &str) -> Option<(String, String, i64)> {
    let rest = uri.strip_prefix("viking://resources/steve/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [_ws, "o", t, id, file] => {
            let seq = file.strip_suffix(".md")?.parse().ok()?;
            Some((t.to_string(), id.to_string(), seq))
        }
        _ => None,
    }
}

/// Tags are `k=v` and lower-cased by the server.
fn tag(k: &str, v: &str) -> String {
    format!("{k}={}", v.to_lowercase())
}

impl OpenViking {
    pub fn new(cfg: OpenVikingConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            search: agent(SEARCH_TIMEOUT),
            write: agent(WRITE_TIMEOUT),
        })
    }

    fn post(
        &self,
        agent: &ureq::Agent,
        path: &str,
        body: &Value,
    ) -> std::result::Result<Value, String> {
        let url = format!("{}{path}", self.cfg.url.trim_end_matches('/'));
        let mut res = agent
            .post(&url)
            .header("X-API-Key", &self.cfg.key)
            .header("X-OpenViking-Account", &self.cfg.account)
            .header("X-OpenViking-User", "steve")
            .send_json(body)
            .map_err(|e| e.to_string())?;
        res.body_mut()
            .read_json::<Value>()
            .map_err(|e| e.to_string())
    }

    fn delete(&self, uri: &str) -> std::result::Result<(), String> {
        let url = format!("{}/api/v1/fs", self.cfg.url.trim_end_matches('/'));
        self.write
            .delete(&url)
            .query("uri", uri)
            .query("recursive", "true")
            .query("wait", "true")
            .header("X-API-Key", &self.cfg.key)
            .header("X-OpenViking-Account", &self.cfg.account)
            .header("X-OpenViking-User", "steve")
            .call()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Whether the server is up and can embed (`GET /ready`).
    pub fn ready(&self) -> std::result::Result<(), String> {
        let url = format!("{}/ready", self.cfg.url.trim_end_matches('/'));
        let mut res = self.search.get(&url).call().map_err(|e| e.to_string())?;
        let v: Value = res.body_mut().read_json().map_err(|e| e.to_string())?;
        match v["status"].as_str() {
            Some("ready") => Ok(()),
            _ => Err(format!("not ready: {v}")),
        }
    }

    /// Makes the store hold exactly what the Hub's index holds for one object: removed when it has
    /// no chunks (or only `never` ones), else rewritten. `ws` is where it was (from the event).
    pub fn sync_object(
        &self,
        c: &Connection,
        ws: &str,
        obj_type: &str,
        obj_id: &str,
    ) -> std::result::Result<usize, String> {
        let rows: Vec<(i64, String, String, String, String, String, String)> = {
            let mut st = c
                .prepare_cached(
                    "SELECT seq, heading, text, visibility, level, created_by, source_kind FROM kb_chunks
                      WHERE workspace_id = ? AND obj_type = ? AND obj_id = ? AND level <> 'never' ORDER BY seq",
                )
                .map_err(|e| e.to_string())?;
            st.query_map(params![ws, obj_type, obj_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .and_then(|it| it.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|e| e.to_string())?
        };
        let dir = object_dir(ws, obj_type, obj_id);
        self.delete(&dir)?;
        if rows.is_empty() {
            return Ok(0);
        }
        self.post(&self.write, "/api/v1/fs/mkdir", &json!({ "uri": dir }))?;
        let ops: Vec<Value> = rows
            .iter()
            .map(|(seq, heading, text, vis, lvl, by, kind)| {
                let content = if heading.is_empty() {
                    text.clone()
                } else {
                    format!("{heading}\n\n{text}")
                };
                json!({
                    "uri": format!("{dir}/{seq}.md"),
                    "content": content,
                    "mode": "upsert",
                    "tags": [tag("vis", vis), tag("by", by), tag("lvl", lvl), tag("kind", kind)],
                })
            })
            .collect();
        let n = ops.len();
        self.post(
            &self.write,
            "/api/v1/content/batch-write",
            &json!({
                "root_uri": dir,
                "operations": ops,
                "processing_mode": "vectors_only",
                "wait": true,
            }),
        )?;
        Ok(n)
    }

    /// `uri → score` of the chunk files found for `text` among those tagged `tags`.
    fn find(
        &self,
        ws: &str,
        text: &str,
        tags: &[String],
        limit: usize,
    ) -> std::result::Result<Vec<(String, f64)>, String> {
        let v = self.post(
            &self.search,
            "/api/v1/search/find",
            &json!({
                "query": text,
                "target_uri": objects_root(ws),
                "limit": limit,
                "level": 2,
                "score_threshold": MIN_SCORE,
                "tags": tags,
            }),
        )?;
        Ok(v["result"]["resources"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| Some((r["uri"].as_str()?.to_string(), r["score"].as_f64()?)))
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl KnowledgeStore for OpenViking {
    fn id(&self) -> &'static str {
        "openviking"
    }

    /// Meaning only (the keyword passes are the Hub index's, [`Hybrid`]). One search per readable
    /// subject (workspace, one's own, each scope), then each chunk read back from the Hub's index
    /// under the scope.
    fn search(
        &self,
        c: &Connection,
        scope: &Scope,
        text: &str,
        pass: Pass,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if pass != Pass::Semantic || text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut subjects = vec![vec![tag("vis", "workspace")]];
        if !scope.workspace_only {
            subjects.push(vec![tag("by", &scope.user)]);
            subjects.extend(scope.scopes.iter().map(|s| vec![tag("vis", s)]));
        }
        // The finds run side by side (a few at a time), so a slow server costs one timeout per
        // round rather than one per subject.
        let mut results = Vec::with_capacity(subjects.len());
        for round in subjects.chunks(FIND_PARALLEL) {
            std::thread::scope(|t| {
                let calls: Vec<_> = round
                    .iter()
                    .map(|tags| t.spawn(|| self.find(&scope.ws, text, tags, limit)))
                    .collect();
                for h in calls {
                    results.push(
                        h.join()
                            .unwrap_or_else(|_| Err("search thread panicked".into())),
                    );
                }
            });
        }
        let mut best: HashMap<(String, String, i64), f64> = HashMap::new();
        for found in results {
            let found = found.map_err(|e| unavailable(self.id(), e))?;
            for (uri, score) in found {
                if let Some(k) = parse_uri(&uri) {
                    let s = best.entry(k).or_insert(score);
                    *s = s.max(score);
                }
            }
        }
        let mut out = Vec::new();
        for ((t, id, seq), score) in best {
            if let Some(mut cand) = SqliteStore::chunk_in_scope(c, scope, &t, &id, seq)? {
                cand.base = score;
                out.push(cand);
            }
        }
        out.sort_by(|a, b| b.base.total_cmp(&a.base));
        out.truncate(limit);
        Ok(out)
    }
}

/// The default store with OpenViking: keyword passes from the Hub's index, meaning from
/// OpenViking.
pub struct Hybrid {
    pub keywords: SqliteStore,
    pub meaning: Arc<OpenViking>,
}

impl KnowledgeStore for Hybrid {
    fn id(&self) -> &'static str {
        "openviking"
    }

    fn search(
        &self,
        c: &Connection,
        scope: &Scope,
        text: &str,
        pass: Pass,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        match pass {
            Pass::Semantic => self.meaning.search(c, scope, text, pass, limit),
            _ => self.keywords.search(c, scope, text, pass, limit),
        }
    }
}

/// Records that `obj_type`/`obj_id` of `ws` changed in the Hub's index (in its transaction).
pub fn changed(tx: &Tx, ws: &str, obj_type: &str, obj_id: &str) -> Result<()> {
    outbox::emit(
        tx,
        ws,
        TOPIC,
        TOPIC_V,
        &json!({ "obj_type": obj_type, "obj_id": obj_id }),
    )?;
    Ok(())
}

/// Every object in the index, recorded as changed: the first mirroring of an index that existed
/// before the store did. Returns how many.
pub fn backfill(store: &Store) -> Result<usize> {
    store.transaction(|tx| {
        let objs: Vec<(String, String, String)> = {
            let mut st = tx.prepare(
                "SELECT DISTINCT workspace_id, obj_type, obj_id FROM kb_chunks WHERE level <> 'never'",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (ws, t, id) in &objs {
            changed(tx, ws, t, id)?;
        }
        Ok(objs.len())
    })
}

/// `kb.openviking`: mirrors each changed object (at least once; a repeat rewrites the same state).
pub struct Mirror {
    pub ov: Arc<OpenViking>,
    pub store: Store,
}

impl Mirror {
    pub const ID: &'static str = "kb.openviking";
}

impl Consumer for Mirror {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        TOPIC
    }

    fn since(&self) -> Since {
        Since::Start
    }

    fn effect(&self, ev: &Event) -> std::result::Result<(), ConsumeError> {
        if ev.version != TOPIC_V {
            return Err(ConsumeError::Reject(format!(
                "{} version {}",
                ev.topic, ev.version
            )));
        }
        let (Some(t), Some(id)) = (
            ev.payload["obj_type"].as_str(),
            ev.payload["obj_id"].as_str(),
        ) else {
            return Err(ConsumeError::Reject(format!(
                "{}: {}",
                ev.topic, ev.payload
            )));
        };
        self.store
            .reader(|c| {
                self.ov
                    .sync_object(c, &ev.workspace_id, t, id)
                    .map_err(StoreError::Hook)
            })
            .map(|_| ())
            .map_err(|e| ConsumeError::Retry(e.to_string()))
    }

    fn apply(&self, _tx: &Tx, ev: &Event) -> std::result::Result<Applied, ConsumeError> {
        Ok(Applied::noted(format!(
            "{}:{}",
            ev.payload["obj_type"].as_str().unwrap_or(""),
            ev.payload["obj_id"].as_str().unwrap_or("")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_uri_reads_back() {
        let u = format!("{}/3.md", object_dir("default", "chunk", "kd_1"));
        assert_eq!(parse_uri(&u), Some(("chunk".into(), "kd_1".into(), 3)));
        assert_eq!(
            parse_uri("viking://resources/steve/default/o/chunk/kd_1/.abstract.md"),
            None
        );
        assert_eq!(parse_uri("viking://resources/other/x.md"), None);
    }
}
