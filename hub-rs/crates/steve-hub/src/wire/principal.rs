//! Whose access a run uses (its principal) and who reads what it makes (its audience).
//!
//! A principal is a person, by user id, or a workspace's service principal `svc:<workspace>` for
//! work nobody in particular asked for (Steve, a flow, a schedule). An item's runs use the access of
//! the member who asked for it (`items.requested_by`) while they are a member, else the service
//! principal: never a guess such as "the only member", which stops being true when someone joins.
//! A run's output is read by that one person when the workspace is theirs alone, else by the
//! workspace. Both are recorded on `runtime_sessions` when a session is bound to a Node.
//!
//! Recorded, not yet decided on: whether a remote Node takes some work still follows the live rule
//! of [`super::node_binding::remote_refusal`] (H1), since the Hub does not record who sent each
//! message into a session, so another member could steer a session done with its owner's access.

use rusqlite::{Connection, OptionalExtension};
use steve_store::StoreError;

const SVC: &str = "svc:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    User(String),
    /// The service principal of this workspace.
    Svc(String),
}

impl Principal {
    /// As stored: the user id, or `svc:<workspace>`.
    pub fn key(&self) -> String {
        match self {
            Principal::User(u) => u.clone(),
            Principal::Svc(ws) => format!("{SVC}{ws}"),
        }
    }

    pub fn parse(s: &str) -> Principal {
        match s.strip_prefix(SVC) {
            Some(ws) => Principal::Svc(ws.to_string()),
            None => Principal::User(s.to_string()),
        }
    }

    pub fn user(&self) -> Option<&str> {
        match self {
            Principal::User(u) => Some(u),
            Principal::Svc(_) => None,
        }
    }
}

fn members(c: &Connection, ws: &str) -> Result<Vec<String>, StoreError> {
    let mut q =
        c.prepare("SELECT user_id FROM workspace_members WHERE workspace_id=? ORDER BY user_id")?;
    let v = q
        .query_map([ws], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(v)
}

/// `requested` while a member of `ws`, else `ws`'s service principal.
pub fn resolve(c: &Connection, ws: &str, requested: Option<&str>) -> Result<Principal, StoreError> {
    let members = members(c, ws)?;
    Ok(match requested.filter(|u| members.iter().any(|m| m == u)) {
        Some(u) => Principal::User(u.to_string()),
        None => Principal::Svc(ws.to_string()),
    })
}

/// Work of `ws` nobody in particular asked for: a job, a project's or a knowledge source's files.
pub fn of_workspace(c: &Connection, ws: &str) -> Result<Principal, StoreError> {
    resolve(c, ws, None)
}

/// The principal of `item`'s runs, with its workspace. `None`: no such item.
pub fn of_item(c: &Connection, item: &str) -> Result<Option<(String, Principal)>, StoreError> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT workspace_id, requested_by FROM items WHERE id=?",
            [item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((ws, by)) => Ok(Some((ws.clone(), resolve(c, &ws, by.as_deref())?))),
        None => Ok(None),
    }
}

/// The principal of `session`'s runs, with its workspace: as recorded when it was bound to a Node,
/// else as its item (or, without one, its workspace) gives it now. `None`: no such session.
pub fn of_session(
    c: &Connection,
    session: &str,
) -> Result<Option<(String, Principal)>, StoreError> {
    let row: Option<(String, Option<String>, Option<String>)> = c
        .query_row(
            "SELECT s.workspace_id, s.item_id, r.principal FROM sessions s
              LEFT JOIN runtime_sessions r ON r.session_id=s.id WHERE s.id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((ws, item, recorded)) = row else {
        return Ok(None);
    };
    if let Some(p) = recorded {
        return Ok(Some((ws, Principal::parse(&p))));
    }
    if let Some(item) = item
        && let Some((_, p)) = of_item(c, &item)?
    {
        return Ok(Some((ws, p)));
    }
    let p = of_workspace(c, &ws)?;
    Ok(Some((ws, p)))
}

/// Who reads what `p`'s run in `ws` makes: `user:<p>` when `ws` is theirs alone, else
/// `workspace:<ws>`; a Node's owner (`node_owner`) is always among them, since they see everything
/// on their machine.
pub fn audience(
    c: &Connection,
    ws: &str,
    p: &Principal,
    node_owner: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    let members = members(c, ws)?;
    let mut a = match (p.user(), members.as_slice()) {
        (Some(u), [only]) if only == u => vec![format!("user:{u}")],
        _ => vec![format!("workspace:{ws}")],
    };
    if let Some(o) = node_owner {
        let covered = a[0] == format!("user:{o}")
            || (a[0].starts_with("workspace:") && members.iter().any(|m| m == o));
        if !covered {
            a.push(format!("user:{o}"));
        }
    }
    Ok(a)
}

/// Records `session`'s principal and audience as they are now; a Node's owner (`node_owner`: none
/// for the local Node, run by the Hub's own account) joins the audience.
pub fn record_in(
    c: &Connection,
    session: &str,
    node_owner: Option<&str>,
) -> Result<(), StoreError> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT workspace_id, item_id FROM sessions WHERE id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((ws, item)) = row else {
        return Ok(());
    };
    let requested: Option<String> = match item.as_deref() {
        Some(i) => c
            .query_row("SELECT requested_by FROM items WHERE id=?", [i], |r| {
                r.get(0)
            })
            .optional()?
            .flatten(),
        None => None,
    };
    let p = resolve(c, &ws, requested.as_deref())?;
    let a = audience(c, &ws, &p, node_owner)?;
    c.execute(
        "UPDATE runtime_sessions SET principal=?, audience=? WHERE session_id=?",
        rusqlite::params![
            p.key(),
            serde_json::to_string(&a).unwrap_or_default(),
            session
        ],
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
             CREATE TABLE items(id TEXT, workspace_id TEXT, requested_by TEXT);
             CREATE TABLE sessions(id TEXT, workspace_id TEXT, item_id TEXT);
             CREATE TABLE runtime_sessions(session_id TEXT, principal TEXT, audience TEXT);
             INSERT INTO workspace_members VALUES ('solo','u1','owner',1),('team','u1','owner',1),('team','u2','member',2);
             INSERT INTO items VALUES ('i1','solo',NULL),('i2','team','u2'),('i3','team',NULL),('i4','team','u9'),('i6','solo','u1');
             INSERT INTO sessions VALUES ('s1','solo','i1'),('s2','team','i2'),('s3','team','i3'),('s4','team','i4'),('s5','team',NULL),('s6','solo','i6');
             INSERT INTO runtime_sessions(session_id) VALUES ('s1'),('s2'),('s3'),('s4'),('s5'),('s6');",
        )
        .unwrap();
        c
    }

    #[test]
    fn the_asker_while_a_member_else_the_workspace() {
        let c = db();
        let p = |s: &str| of_session(&c, s).unwrap().unwrap().1;
        assert_eq!(
            p("s1"),
            Principal::Svc("solo".into()),
            "nobody asked: not a guess"
        );
        assert_eq!(p("s2"), Principal::User("u2".into()));
        assert_eq!(p("s3"), Principal::Svc("team".into()));
        assert_eq!(p("s4"), Principal::Svc("team".into()), "u9 is no member");
        assert_eq!(p("s5"), Principal::Svc("team".into()));
        assert_eq!(p("s6"), Principal::User("u1".into()));
        assert!(of_session(&c, "nope").unwrap().is_none());
        assert_eq!(Principal::parse("svc:team"), Principal::Svc("team".into()));
        assert_eq!(Principal::Svc("team".into()).key(), "svc:team");
    }

    #[test]
    fn what_is_recorded_stays_and_the_nodes_owner_reads_it_too() {
        let c = db();
        record_in(&c, "s1", None).unwrap();
        record_in(&c, "s2", Some("u3")).unwrap();
        record_in(&c, "s3", Some("u1")).unwrap();
        record_in(&c, "s6", None).unwrap();
        let row = |s: &str| -> (String, String) {
            c.query_row(
                "SELECT principal, audience FROM runtime_sessions WHERE session_id=?",
                [s],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            row("s1"),
            ("svc:solo".into(), r#"["workspace:solo"]"#.into())
        );
        assert_eq!(
            row("s2"),
            ("u2".into(), r#"["workspace:team","user:u3"]"#.into())
        );
        assert_eq!(
            row("s3"),
            ("svc:team".into(), r#"["workspace:team"]"#.into())
        );
        assert_eq!(row("s6"), ("u1".into(), r#"["user:u1"]"#.into()));
        // u2 leaves: the recorded principal stays
        c.execute("DELETE FROM workspace_members WHERE user_id='u2'", [])
            .unwrap();
        assert_eq!(
            of_session(&c, "s2").unwrap().unwrap().1,
            Principal::User("u2".into())
        );
    }
}
