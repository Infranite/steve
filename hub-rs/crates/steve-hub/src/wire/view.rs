//! What a run would be given under the path model (S2, in shadow): one `view()` for every kind of
//! run, made from its principal ([`super::principal`]) and its work. For an item's session: the
//! session's and the item's directories read-write (a directory project's root included), and the
//! knowledge folders of its workspace that its principal may see read-only. Nothing else of the
//! machine; Steve's own state, credentials and caches come with later steps.
//!
//! It decides nothing yet. When a session is bound it is recorded with how it differs from what the
//! local Node lets every run reach today (every workspace's projects, sessions and folders): the
//! difference is what the deny-by-default sandbox will take away, and the counts say how much.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::Digest as _;
use steve_store::StoreError;

use super::principal;

/// How many paths of a difference are kept as a sample (the count is of all of them).
const SAMPLE: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub principal: String,
    pub audience: Vec<String>,
    /// Sorted, deduplicated, absolute.
    pub rw: Vec<String>,
    pub ro: Vec<String>,
}

impl View {
    /// A digest of exactly this view (its JSON; the lists are sorted).
    pub fn digest(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(sha2::Sha256::digest(json))
    }
}

fn clean(mut v: Vec<String>) -> Vec<String> {
    v.retain(|p| Path::new(p).is_absolute());
    for p in &mut v {
        while p.len() > 1 && p.ends_with('/') {
            p.pop();
        }
    }
    v.sort();
    v.dedup();
    v
}

/// The view of `session` (an item's or a plain session's); `None`: no such session.
pub fn of_session(c: &Connection, session: &str) -> Result<Option<View>, StoreError> {
    let row: Option<(String, Option<String>, String)> = c
        .query_row(
            "SELECT workspace_id, item_id, cwd FROM sessions WHERE id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((ws, item, cwd)) = row else {
        return Ok(None);
    };
    let Some((_, p)) = principal::of_session(c, session)? else {
        return Ok(None);
    };
    let mut rw = vec![cwd];
    if let Some(item) = &item {
        let i: Option<(Option<String>, Option<String>, Option<i64>)> = c
            .query_row(
                "SELECT i.cwd, p.path, p.worktree FROM items i LEFT JOIN projects p ON p.id=i.project_id
                  WHERE i.id=?",
                [item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((icwd, ppath, worktree)) = i {
            rw.extend(icwd);
            // a directory project: the run works in the project itself
            if worktree == Some(0) {
                rw.extend(ppath);
            }
        }
    }
    let mut q = c.prepare(
        "SELECT json_extract(config,'$.path'), visibility, created_by FROM kb_sources
          WHERE workspace_id=? AND kind='folder' AND json_type(config,'$.path')='text' AND paused=0",
    )?;
    let me = p.user().map(str::to_string);
    let ro: Vec<String> = q
        .query_map([&ws], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(_, vis, by)| vis != "private" || me.as_deref() == Some(by.as_str()))
        .map(|(path, _, _)| path)
        .collect();
    let audience = principal::audience(c, &ws, &p, None)?;
    let rw = clean(rw);
    let mut ro = clean(ro);
    ro.retain(|r| !rw.contains(r));
    Ok(Some(View {
        principal: p.key(),
        audience,
        rw,
        ro,
    }))
}

/// `path` lies in one of `dirs` (or is one).
fn within(path: &str, dirs: &[String]) -> bool {
    dirs.iter()
        .any(|d| Path::new(path).starts_with(Path::new(d)))
}

/// How `view` differs from `today` (every directory the Node lets a run reach now): today's outside
/// the view, and the view's outside today's. Each sorted; the second only counts paths, not
/// whether they exist.
pub fn diff(view: &View, today: &[String]) -> (Vec<String>, Vec<String>) {
    let mine: Vec<String> = view.rw.iter().chain(&view.ro).cloned().collect();
    let today = clean(today.to_vec());
    let extra = today
        .iter()
        .filter(|t| !within(t, &mine))
        .cloned()
        .collect();
    let missing = mine
        .iter()
        .filter(|m| !within(m, &today))
        .cloned()
        .collect();
    (extra, missing)
}

/// Records `session`'s view on `node`, with its difference from `today` (`None`: today's roots are
/// not known here, a remote Node's are its own).
pub fn record_in(
    c: &Connection,
    session: &str,
    node: &str,
    today: Option<&[String]>,
) -> Result<(), StoreError> {
    let Some(v) = of_session(c, session)? else {
        return Ok(());
    };
    let digest = v.digest();
    let sample = |p: &[String]| serde_json::to_string(&p[..p.len().min(SAMPLE)]).ok();
    let (extra_n, extra, missing_n, missing) = match today {
        Some(t) => {
            let (e, m) = diff(&v, t);
            (
                Some(e.len() as i64),
                sample(&e),
                Some(m.len() as i64),
                sample(&m),
            )
        }
        None => (None, None, None, None),
    };
    // the kind of run: only sessions are bound to a Node so far (jobs and commands come later)
    let kind = "session";
    c.execute(
        "INSERT INTO view_shadow(session_id,node_id,kind,at,digest,view,extra_count,extra,missing_count,missing)
         VALUES (?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(session_id) DO UPDATE SET node_id=excluded.node_id, kind=excluded.kind, at=excluded.at,
           digest=excluded.digest, view=excluded.view, extra_count=excluded.extra_count, extra=excluded.extra,
           missing_count=excluded.missing_count, missing=excluded.missing",
        rusqlite::params![
            session,
            node,
            kind,
            steve_store::now(),
            digest,
            serde_json::to_string(&v).unwrap_or_default(),
            extra_n,
            extra,
            missing_n,
            missing
        ],
    )?;
    c.execute(
        "UPDATE runtime_sessions SET view_digest=? WHERE session_id=?",
        rusqlite::params![digest, session],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspace_members(workspace_id TEXT, user_id TEXT, role TEXT, created INTEGER);
             CREATE TABLE items(id TEXT, workspace_id TEXT, requested_by TEXT, cwd TEXT, project_id TEXT);
             CREATE TABLE projects(id TEXT, path TEXT, worktree INTEGER);
             CREATE TABLE sessions(id TEXT, workspace_id TEXT, item_id TEXT, cwd TEXT);
             CREATE TABLE runtime_sessions(session_id TEXT, principal TEXT, audience TEXT, view_digest TEXT);
             CREATE TABLE kb_sources(id TEXT, workspace_id TEXT, kind TEXT, config TEXT, visibility TEXT, created_by TEXT, paused INTEGER);
             CREATE TABLE view_shadow(session_id TEXT PRIMARY KEY, node_id TEXT, kind TEXT, at INTEGER, digest TEXT, view TEXT,
               extra_count INTEGER, extra TEXT, missing_count INTEGER, missing TEXT);
             INSERT INTO workspace_members VALUES ('team','u1','owner',1),('team','u2','member',2),('other','u3','owner',1);
             INSERT INTO projects VALUES ('pg','/w/git-proj',1),('pd','/w/dir-proj',0),('po','/w/other-proj',1);
             INSERT INTO items VALUES ('i1','team','u2','/h/worktrees/ST-1','pg'),('i2','team',NULL,'/w/dir-proj','pd');
             INSERT INTO sessions VALUES ('s1','team','i1','/h/worktrees/ST-1'),('s2','team','i2','/w/dir-proj/');
             INSERT INTO runtime_sessions(session_id) VALUES ('s1'),('s2');
             INSERT INTO kb_sources VALUES
               ('k1','team','folder','{\"path\":\"/docs/shared\"}','workspace','u1',0),
               ('k2','team','folder','{\"path\":\"/docs/u1-private\"}','private','u1',0),
               ('k3','team','folder','{\"path\":\"/docs/u2-private\"}','private','u2',0),
               ('k4','other','folder','{\"path\":\"/docs/elsewhere\"}','workspace','u3',0);",
        )
        .unwrap();
        c
    }

    #[test]
    fn an_items_session_sees_its_work_and_what_its_principal_may_read() {
        let c = db();
        let v = of_session(&c, "s1").unwrap().unwrap();
        assert_eq!(v.principal, "u2");
        assert_eq!(v.audience, ["workspace:team"]);
        // a git project's item works in its own worktree, not the project
        assert_eq!(v.rw, ["/h/worktrees/ST-1"]);
        // the workspace's shared folder and u2's own private one; not u1's, not another workspace's
        assert_eq!(v.ro, ["/docs/shared", "/docs/u2-private"]);
        let v = of_session(&c, "s2").unwrap().unwrap();
        assert_eq!(v.principal, "svc:team");
        assert_eq!(v.rw, ["/w/dir-proj"], "a directory project's root, once");
        assert_eq!(v.ro, ["/docs/shared"], "nobody's private folders");
        assert_ne!(v.digest(), of_session(&c, "s1").unwrap().unwrap().digest());
    }

    #[test]
    fn the_shadow_log_names_what_the_view_would_take_away() {
        let c = db();
        let today: Vec<String> = [
            "/w/git-proj",
            "/w/other-proj",
            "/h/worktrees/ST-1",
            "/h/worktrees",
            "/docs/shared",
            "/docs/elsewhere",
        ]
        .map(String::from)
        .to_vec();
        record_in(&c, "s1", "local", Some(&today)).unwrap();
        let (n, extra, mn, missing, digest): (i64, String, i64, String, String) = c
            .query_row(
                "SELECT extra_count, extra, missing_count, missing, digest FROM view_shadow WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            extra,
            r#"["/docs/elsewhere","/h/worktrees","/w/git-proj","/w/other-proj"]"#
        );
        // u2's private folder is in the view but not a root today
        assert_eq!((mn, missing.as_str()), (1, r#"["/docs/u2-private"]"#));
        let recorded: String = c
            .query_row(
                "SELECT view_digest FROM runtime_sessions WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, digest);
        // a remote Node's roots are not known here: the view alone
        record_in(&c, "s2", "n_remote", None).unwrap();
        let n: Option<i64> = c
            .query_row(
                "SELECT extra_count FROM view_shadow WHERE session_id='s2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, None);
    }
}
