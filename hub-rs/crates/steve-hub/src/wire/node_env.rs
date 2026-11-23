//! A user's own environment variables on a Node (`node_env`, path model §4.5.3): kept on the Hub,
//! set only in the harnesses of runs whose principal is that user (or, for `svc:<ws>`, that
//! workspace's service principal), on that Node. They take the place of inheriting the Node
//! owner's environment for what a run needs of its own: a registry token, `GOPRIVATE`, a proxy.
//!
//! Only the user edits their own rows; a workspace's owner edits its service principal's. No other
//! user reads them through Steve; they are kept in plain text in the Hub's database and the
//! Node's journal. A value kept in the vault (`vault_ref`) is not supported yet.
//!
//! Until a run uses its principal's own harness credentials, the Hub records who sent each message
//! into a session, and runs on one Node cannot see each other's processes, they reach a run only
//! when that cannot hand them to anyone else: its principal is the person who owns the Node (their
//! own machine, their own logins), the only member of the run's workspace (nobody else can steer
//! the session and ask it to print them), and nobody else's runs share the Node (a process's
//! environment is readable to every process of the same OS user: on the Hub's local Node, a Hub
//! with one user only; a remote Node runs only its owner's own work, H1). Anything else is kept
//! and waits.

use steve_store::{Members, Read, Store, StoreError, node_directory, node_environment as env};

use super::principal::{self, Principal};

/// The longest value kept (a token or a short list; not a file).
pub const MAX_VALUE: usize = 16 * 1024;

/// Who owns this Node for the purpose of forwarding a principal's environment.
pub fn node_owner(r: &mut impl Read, node: &str) -> Result<Option<String>, StoreError> {
    if node == "local" {
        env::local_owner(r)
    } else {
        node_directory::owner_if_connectable(r, node)
    }
}

/// The variables a session's harness gets when its principal is the Node's sole workspace member.
pub fn for_session(
    store: &Store,
    node: &str,
    session: &str,
) -> Result<Vec<(String, String)>, StoreError> {
    store.read(|r| {
        let Some((ws, p)) = store.read(|c| principal::of_session(c, session))? else {
            return Ok(vec![]);
        };
        let Some(me) = p.user() else {
            return Ok(vec![]);
        };
        if node_owner(r, node)?.as_deref() != Some(me)
            || env::has_other_member(r, &ws, me)?
            || (node == "local" && env::has_multiple_users(r)?)
        {
            return Ok(vec![]);
        }
        Ok(env::values(r, node, me)?
            .into_iter()
            .filter(|(key, _)| steve_node_api::env::user_env_refusal(key).is_none())
            .collect())
    })
}

/// Why `me`, working in `ws`, may not edit `principal`'s variables on `node` (`None`: they may).
/// Their own; their workspace's service principal when they own the workspace. On the Hub's local
/// Node, or on a Node they paired.
pub fn refusal(
    c: &mut impl Read,
    me: &str,
    ws: &str,
    node: &str,
    principal: &str,
) -> Result<Option<String>, StoreError> {
    match Principal::parse(principal) {
        Principal::User(u) if u == me => {}
        Principal::Svc(w) if w == ws => {
            if Members::new(c).role(ws, me)?.as_deref() != Some("owner") {
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
    let owner = node_owner(c, node)?;
    Ok(match owner {
        Some(o) if o == me => None,
        Some(_) => Some("only on a Node you paired, until shared Nodes are sandboxed".into()),
        None => Some("no such Node".into()),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        steve_store::testkit::hub::node_env_fixture(&store).unwrap();
        (dir, store)
    }

    fn put(
        s: &Store,
        node: &str,
        principal: &str,
        key: &str,
        value: &str,
        note: &str,
        by: &str,
    ) -> Result<(), StoreError> {
        s.write(|w| {
            env::put(
                w,
                env::Change {
                    node,
                    principal,
                    key,
                    value,
                    note,
                    actor: by,
                },
                1,
            )
        })
    }

    #[test]
    fn only_the_nodes_owner_alone_in_the_workspace_gets_them() {
        let (_dir, c) = db();
        put(&c, "local", "u1", "NPM_TOKEN", "t1", "", "u1").unwrap();
        put(&c, "local", "u2", "NPM_TOKEN", "t2", "", "u2").unwrap();
        put(&c, "n_u2", "u2", "GOPRIVATE", "x", "", "u2").unwrap();
        put(&c, "local", "svc:team", "CI", "1", "", "u1").unwrap();
        // a row someone wrote past the checks is still not handed over
        put(&c, "local", "u1", "HOME", "/h", "", "u1").unwrap();
        let env = |node: &str, s: &str| for_session(&c, node, s).unwrap();
        let one = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        // the Hub has another user: their runs on its local Node could read the owner's environment
        assert_eq!(env("local", "s3"), vec![]);
        // a member on their own paired Node, alone in the workspace
        assert_eq!(env("n_u2", "s4"), one("GOPRIVATE", "x"));
        // not their Node: they would steer its owner's logins
        assert_eq!(env("local", "s4"), vec![]);
        // a shared workspace: another member could ask the session to print them
        assert_eq!(env("local", "s1"), vec![]);
        assert_eq!(env("local", "s5"), vec![], "even the owner's, there");
        assert_eq!(
            env("local", "s2"),
            vec![],
            "the service principal's: anyone's run"
        );
        // a Hub of one user: its owner's, on the local Node
        steve_store::testkit::hub::remove_other_node_env_user(&c).unwrap();
        assert_eq!(env("local", "s3"), one("NPM_TOKEN", "t1"));
        put(&c, "local", "u1", "NPM_TOKEN", "t1b", "new", "u1").unwrap();
        assert_eq!(env("local", "s3"), one("NPM_TOKEN", "t1b"));
        assert!(
            c.write(|w| env::delete(w, "local", "u1", "NPM_TOKEN"))
                .unwrap()
        );
        assert!(
            !c.write(|w| env::delete(w, "local", "u1", "NPM_TOKEN"))
                .unwrap()
        );
        assert_eq!(env("local", "s3"), vec![]);
    }

    #[test]
    fn only_ones_own_and_the_owner_for_the_workspace() {
        let (_dir, c) = db();
        let why =
            |me: &str, node: &str, p: &str| c.read(|r| refusal(r, me, "team", node, p)).unwrap();
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
