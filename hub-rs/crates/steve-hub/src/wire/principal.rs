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

use steve_store::{Read, Store, StoreError, W, node_sessions::authority};

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

/// `requested` while a member of `ws`, else `ws`'s service principal.
pub fn resolve(
    c: &mut impl Read,
    ws: &str,
    requested: Option<&str>,
) -> Result<Principal, StoreError> {
    let members = authority::members(c, ws)?;
    Ok(match requested.filter(|u| members.iter().any(|m| m == u)) {
        Some(u) => Principal::User(u.to_string()),
        None => Principal::Svc(ws.to_string()),
    })
}

/// Work of `ws` nobody in particular asked for: a job, a project's or a knowledge source's files.
pub fn of_workspace(c: &mut impl Read, ws: &str) -> Result<Principal, StoreError> {
    resolve(c, ws, None)
}

/// The principal of `item`'s runs, with its workspace. `None`: no such item.
pub fn of_item(c: &mut impl Read, item: &str) -> Result<Option<(String, Principal)>, StoreError> {
    let Some(item) = authority::item(c, item)? else {
        return Ok(None);
    };
    let principal = resolve(c, &item.workspace, item.requester.as_deref())?;
    Ok(Some((item.workspace, principal)))
}

/// The principal of `session`'s runs, with its workspace: as recorded when it was bound to a Node,
/// else as its item (or, without one, its workspace) gives it now. `None`: no such session.
pub fn of_session(
    c: &mut impl Read,
    session: &str,
) -> Result<Option<(String, Principal)>, StoreError> {
    let Some(authority::Session {
        workspace: ws,
        item,
        principal: recorded,
        ..
    }) = authority::session(c, session)?
    else {
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
    c: &mut impl Read,
    ws: &str,
    p: &Principal,
    node_owner: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    let members = authority::members(c, ws)?;
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
pub fn record_in(w: &mut W<'_>, session: &str, node_owner: Option<&str>) -> Result<(), StoreError> {
    let Some(s) = authority::session(w, session)? else {
        return Ok(());
    };
    let requested = match &s.item {
        Some(id) => authority::item(w, id)?.and_then(|i| i.requester),
        None => None,
    };
    let principal = resolve(w, &s.workspace, requested.as_deref())?;
    let audience = audience(w, &s.workspace, &principal, node_owner)?;
    authority::set_principal(w, session, &principal.key(), &audience)
}

/// A name for a path segment: what the Node accepts, and a short digest of the original so that
/// two names cleaned to the same text stay apart.
fn segment(s: &str) -> String {
    use sha2::Digest as _;
    let clean: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    let d = hex::encode(sha2::Sha256::digest(s.as_bytes()));
    format!("{clean}-{}", &d[..8])
}

/// `session`'s principal's own directory for `harness` on `node` (path model §4.5.1): one per
/// (workspace, principal, bot, adapter), recorded as `runtime_sessions.state_dir`. The principal
/// who owns the Node starts it from their own settings and login there.
pub fn harness_state_in(
    store: &Store,
    w: &mut W<'_>,
    session: &str,
    node: &str,
    harness: &str,
) -> Result<Option<steve_node_api::HarnessState>, StoreError> {
    let Some((ws, p)) = of_session(w, session)? else {
        return Ok(None);
    };
    let bot = authority::session(w, session)?.and_then(|s| s.bot);
    let dir = [
        segment(&ws),
        segment(&p.key()),
        segment(bot.as_deref().unwrap_or("none")),
        segment(harness),
    ]
    .join("/");
    let owner = super::node_env::node_owner(w, node)?;
    let import_owner = p.user().is_some() && owner.as_deref() == p.user();
    authority::set_state_dir(w, session, &dir)?;
    let owner_context =
        import_owner && store.reader(|c| steve_store::nodes::owner_context(c, node))?;
    Ok(Some(steve_node_api::HarnessState {
        dir,
        import_owner,
        owner_context,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use steve_store::testkit::hub::authority as fixture;

    fn db() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        fixture::principals(&store).unwrap();
        (dir, store)
    }

    #[test]
    fn the_asker_while_a_member_else_the_workspace() {
        let (_dir, store) = db();
        let p = |s: &str| store.read(|r| of_session(r, s)).unwrap().unwrap().1;
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
        assert!(store.read(|r| of_session(r, "nope")).unwrap().is_none());
        assert_eq!(Principal::parse("svc:team"), Principal::Svc("team".into()));
        assert_eq!(Principal::Svc("team".into()).key(), "svc:team");
    }

    #[test]
    fn what_is_recorded_stays_and_the_nodes_owner_reads_it_too() {
        let (_dir, store) = db();
        for (session, owner) in [
            ("s1", None),
            ("s2", Some("u3")),
            ("s3", Some("u1")),
            ("s6", None),
        ] {
            store.write(|w| record_in(w, session, owner)).unwrap();
        }
        let row = |s: &str| -> (String, String) {
            let s = store.read(|r| authority::session(r, s)).unwrap().unwrap();
            (
                s.principal.unwrap(),
                serde_json::to_string(&s.audience.unwrap()).unwrap(),
            )
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
        fixture::remove_member(&store, "team", "u2").unwrap();
        assert_eq!(
            store.read(|r| of_session(r, "s2")).unwrap().unwrap().1,
            Principal::User("u2".into())
        );
    }

    #[test]
    fn workflow_child_binding_preserves_the_requester_but_resolves_current_membership() {
        use steve_store::{NewBot, NewItem, NewSession, NoPublisher, Store, UnitSpec, WS};

        for (requested, leaves, expected) in [
            (
                Some("requester"),
                false,
                Principal::User("requester".into()),
            ),
            (None, false, Principal::Svc(WS.into())),
            (Some("requester"), true, Principal::Svc(WS.into())),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
            store.ensure_workspace().unwrap();
            store
                .write(|w| {
                    steve_store::UserRows(w).insert(
                        "requester",
                        WS,
                        "requester",
                        "requester",
                        "member",
                        1,
                    )?;
                    w.members().add(WS, "requester", "member", 1)
                })
                .unwrap();
            for id in ["writer", "reviewer"] {
                store
                    .bots()
                    .seed(
                        id,
                        false,
                        &NewBot {
                            name: id.into(),
                            harness: "fixture".into(),
                            ..Default::default()
                        },
                        WS,
                    )
                    .unwrap();
            }
            let parent = store
                .items()
                .create(
                    WS,
                    &NewItem {
                        tags: vec![
                            store
                                .tags()
                                .kinds(WS)
                                .unwrap()
                                .into_iter()
                                .find(|tag| tag.name == "feat")
                                .unwrap()
                                .id,
                        ],
                        title: "Collaborative work".into(),
                        bot_id: "writer".into(),
                        requested_by: requested.map(str::to_string),
                        ..Default::default()
                    },
                )
                .unwrap()
                .unwrap();
            let units = [
                ("work", "writer", "work", vec![]),
                ("review", "reviewer", "review", vec!["work".into()]),
                ("integrate", "writer", "integrate", vec!["review".into()]),
            ]
            .into_iter()
            .map(|(id, bot, kind, depends_on)| UnitSpec {
                id: id.into(),
                title: id.into(),
                goal: id.into(),
                bot_id: bot.into(),
                kind: kind.into(),
                depends_on,
                resources: vec![],
                required: true,
            })
            .collect::<Vec<_>>();
            store
                .workflows()
                .replace(&parent.id, 0, "Split the requested work", &units, 1, 1)
                .unwrap();
            let child = store.workflows().claim(&parent.id, "work").unwrap();
            let session = store
                .sessions()
                .create(&NewSession {
                    workspace_id: WS.into(),
                    item_id: child.item_id.clone(),
                    bot_id: Some("writer".into()),
                    harness: "fixture".into(),
                    cwd: dir.path().to_string_lossy().into(),
                    ..Default::default()
                })
                .unwrap();
            if leaves {
                fixture::remove_member(&store, WS, "requester").unwrap();
            }
            store
                .write(|w| {
                    fixture::bind_session(w, &session.id, "node")?;
                    record_in(w, &session.id, Some("node-owner"))
                })
                .unwrap();
            store
                .read::<_, StoreError>(|c| {
                    let item = authority::item(c, child.item_id.as_deref().unwrap())?.unwrap();
                    assert_eq!(item.requester.as_deref(), requested);
                    let bound = authority::session(c, &session.id)?.unwrap();
                    assert_eq!(bound.principal.as_deref(), Some(expected.key().as_str()));
                    assert_eq!(
                        bound.audience.unwrap(),
                        vec![
                            expected
                                .user()
                                .map(|u| format!("user:{u}"))
                                .unwrap_or_else(|| format!("workspace:{WS}")),
                            "user:node-owner".into(),
                        ]
                    );
                    assert_eq!(of_session(c, &session.id)?.unwrap().1, expected);
                    Ok(())
                })
                .unwrap();
        }
    }
}
