//! A user's own environment variables on a Node (`node_env`, path model §4.5.3): kept on the Hub,
//! set only in the harnesses of runs whose principal is that user (or, for `svc:<ws>`, that
//! workspace's service principal), on that Node. They take the place of inheriting the Node
//! owner's environment for what a run needs of its own: a registry token, `GOPRIVATE`, a proxy.
//!
//! Only the user edits their own rows; a workspace's owner edits its service principal's. Nobody
//! else reads them, the Node's owner included (who can still see a running process's environment
//! on their machine: that is why a Node's owner is in every run's audience). A value kept in the
//! vault (`vault_ref`) is not supported yet: such a row is refused.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use steve_store::StoreError;

use super::principal::{self, Principal};

/// The longest value kept (a token or a short list; not a file).
pub const MAX_VALUE: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub key: String,
    pub value: Option<String>,
    pub vault_ref: Option<String>,
    pub note: String,
    pub updated_by: String,
    pub updated: i64,
}

/// The variables `session`'s harness gets on `node`: its principal's rows with a value.
pub fn for_session(
    c: &Connection,
    node: &str,
    session: &str,
) -> Result<Vec<(String, String)>, StoreError> {
    let Some((_, p)) = principal::of_session(c, session)? else {
        return Ok(vec![]);
    };
    let mut q = c.prepare(
        "SELECT key, value FROM node_env WHERE node_id=? AND principal=? AND value IS NOT NULL ORDER BY key",
    )?;
    let rows = q
        .query_map(rusqlite::params![node, p.key()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(k, _)| steve_node_api::env::user_env_refusal(k).is_none())
        .collect())
}

/// Why `me`, working in `ws`, may not edit `principal`'s variables on `node` (`None`: they may).
/// Their own; their workspace's service principal when they own the workspace. On the Hub's local
/// Node, or on a Node they paired.
pub fn refusal(
    c: &Connection,
    me: &str,
    ws: &str,
    node: &str,
    principal: &str,
) -> Result<Option<String>, StoreError> {
    match Principal::parse(principal) {
        Principal::User(u) if u == me => {}
        Principal::Svc(w) if w == ws => {
            let owner: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=? AND role='owner')",
                [ws, me],
                |r| r.get(0),
            )?;
            if !owner {
                return Ok(Some(
                    "only the workspace's owner sets its service principal's".into(),
                ));
            }
        }
        _ => return Ok(Some("only your own".into())),
    }
    if node == "local" {
        return Ok(None);
    }
    let owner: Option<String> = c
        .query_row(
            "SELECT owner FROM nodes WHERE node_id=? AND state IN ('active','paused')",
            [node],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match owner {
        Some(o) if o == me => None,
        Some(_) => Some("only on a Node you paired, until shared Nodes are sandboxed".into()),
        None => Some("no such Node".into()),
    })
}

pub fn list(c: &Connection, node: &str, principal: &str) -> Result<Vec<Row>, StoreError> {
    let mut q = c.prepare(
        "SELECT key, value, vault_ref, note, updated_by, updated FROM node_env
          WHERE node_id=? AND principal=? ORDER BY key",
    )?;
    let rows = q
        .query_map(rusqlite::params![node, principal], |r| {
            Ok(Row {
                key: r.get(0)?,
                value: r.get(1)?,
                vault_ref: r.get(2)?,
                note: r.get(3)?,
                updated_by: r.get(4)?,
                updated: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Why `key` = `value` may not be saved (`None`: it may).
pub fn value_refusal(key: &str, value: &str) -> Option<String> {
    if let Some(why) = steve_node_api::env::user_env_refusal(key) {
        return Some(format!("{key}: {why}"));
    }
    if value.len() > MAX_VALUE || value.contains('\0') {
        return Some(format!("{key}: the value is too long or has a NUL"));
    }
    None
}

pub fn put(
    c: &Connection,
    node: &str,
    principal: &str,
    key: &str,
    value: &str,
    note: &str,
    by: &str,
) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO node_env(node_id,principal,key,value,vault_ref,note,updated_by,updated)
         VALUES (?,?,?,?,NULL,?,?,?)
         ON CONFLICT(node_id,principal,key) DO UPDATE SET value=excluded.value, vault_ref=NULL,
           note=excluded.note, updated_by=excluded.updated_by, updated=excluded.updated",
        rusqlite::params![node, principal, key, value, note, by, steve_store::now()],
    )?;
    Ok(())
}

pub fn delete(c: &Connection, node: &str, principal: &str, key: &str) -> Result<bool, StoreError> {
    Ok(c.execute(
        "DELETE FROM node_env WHERE node_id=? AND principal=? AND key=?",
        rusqlite::params![node, principal, key],
    )? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspace_members(workspace_id TEXT, user_id TEXT, role TEXT, created INTEGER);
             CREATE TABLE items(id TEXT, workspace_id TEXT, requested_by TEXT);
             CREATE TABLE sessions(id TEXT, workspace_id TEXT, item_id TEXT);
             CREATE TABLE runtime_sessions(session_id TEXT, principal TEXT, audience TEXT);
             CREATE TABLE nodes(node_id TEXT, owner TEXT, state TEXT);
             CREATE TABLE node_env(node_id TEXT, principal TEXT, key TEXT, value TEXT, vault_ref TEXT,
               note TEXT NOT NULL DEFAULT '', updated_by TEXT, updated INTEGER, PRIMARY KEY(node_id,principal,key));
             INSERT INTO workspace_members VALUES ('team','u1','owner',1),('team','u2','member',2);
             INSERT INTO items VALUES ('i1','team','u2'),('i2','team',NULL);
             INSERT INTO sessions VALUES ('s1','team','i1'),('s2','team','i2');
             INSERT INTO runtime_sessions(session_id) VALUES ('s1'),('s2');
             INSERT INTO nodes VALUES ('n_u1','u1','active'),('n_u2','u2','active'),('n_old','u2','revoked');",
        )
        .unwrap();
        c
    }

    #[test]
    fn a_run_gets_its_principals_variables_on_its_node_only() {
        let c = db();
        put(&c, "local", "u2", "NPM_TOKEN", "t2", "", "u2").unwrap();
        put(&c, "local", "u1", "NPM_TOKEN", "t1", "", "u1").unwrap();
        put(&c, "n_u2", "u2", "GOPRIVATE", "x", "", "u2").unwrap();
        put(&c, "local", "svc:team", "CI", "1", "", "u1").unwrap();
        // a row someone wrote past the checks is still not handed over
        put(&c, "local", "u2", "HOME", "/h", "", "u2").unwrap();
        assert_eq!(
            for_session(&c, "local", "s1").unwrap(),
            [("NPM_TOKEN".to_string(), "t2".to_string())]
        );
        assert_eq!(
            for_session(&c, "n_u2", "s1").unwrap(),
            [("GOPRIVATE".to_string(), "x".to_string())]
        );
        assert_eq!(
            for_session(&c, "local", "s2").unwrap(),
            [("CI".to_string(), "1".to_string())],
            "nobody asked: the service principal's"
        );
        put(&c, "local", "u2", "NPM_TOKEN", "t2b", "new", "u2").unwrap();
        assert_eq!(
            list(&c, "local", "u2").unwrap()[1].value.as_deref(),
            Some("t2b")
        );
        assert!(delete(&c, "local", "u2", "NPM_TOKEN").unwrap());
        assert!(!delete(&c, "local", "u2", "NPM_TOKEN").unwrap());
    }

    #[test]
    fn only_ones_own_and_the_owner_for_the_workspace() {
        let c = db();
        let why = |me: &str, node: &str, p: &str| refusal(&c, me, "team", node, p).unwrap();
        assert_eq!(why("u2", "local", "u2"), None);
        assert_eq!(why("u2", "n_u2", "u2"), None);
        assert!(why("u2", "n_u1", "u2").is_some(), "someone else's Node");
        assert!(why("u2", "n_old", "u2").is_some(), "a revoked Node");
        assert!(
            why("u2", "local", "u1").is_some(),
            "someone else's variables"
        );
        assert!(why("u2", "local", "svc:team").is_some(), "not the owner");
        assert_eq!(why("u1", "local", "svc:team"), None);
        assert!(why("u1", "local", "svc:other").is_some());
        assert!(value_refusal("STEVE_X", "1").is_some());
        assert!(value_refusal("X", &"a".repeat(MAX_VALUE + 1)).is_some());
        assert_eq!(value_refusal("NPM_TOKEN", "abc"), None);
    }
}
