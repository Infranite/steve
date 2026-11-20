//! SG22: membership contracts fixed before extracting the event handlers.
#[path = "support/digest_env.rs"]
mod digest_env;

use digest_env::TestEnv;
use std::sync::Arc;
use steve_channels::channel::{HandleRef, MemberInfo};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::{Channels, SpaceEvent, SpaceKind, space_decisions, spaces};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
use steve_types::{BusEvent, ChannelsEvent, DomainEvent};

struct H {
    env: TestEnv,
    _keys: tempfile::TempDir,
    fake: Arc<FakeChannel>,
    ch: Channels,
    conn: String,
}

fn setup() -> H {
    setup_with_adapter(true)
}

fn setup_with_adapter(registered: bool) -> H {
    let flow = FlowSetup::new(Arc::new(FakeCore::default()));
    let env = TestEnv::new(|r| {
        flow.register(r);
        connections::register(r);
        space_decisions::register(r);
    });
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec![], true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let sink = flow
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let mut builder = Channels::builder(env.modules.clone(), conns, Arc::new(sink));
    if registered {
        builder = builder.adapter(fake.clone());
    }
    let ch = builder.build();
    let (conn, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).unwrap();
    env.take_events();
    H {
        env,
        _keys: keys,
        fake,
        ch,
        conn,
    }
}

impl H {
    async fn event(&self, ev: SpaceEvent) -> steve_channels::membership::SpaceChange {
        self.ch
            .on_space_event(WS, &self.conn, ev)
            .await
            .unwrap()
            .unwrap()
    }
    fn space(&self, id: &str) -> spaces::SpaceRow {
        self.env
            .store()
            .reader(|c| spaces::get(c, id))
            .unwrap()
            .unwrap()
    }
    fn n(&self, sql: &str, args: &[&str]) -> i64 {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    sql,
                    rusqlite::params_from_iter(args),
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
    fn space_events(&self, id: &str) -> usize {
        self.env.take_events().iter().filter(|e| matches!(e,
            BusEvent::Domain(DomainEvent::Channels(ChannelsEvent::Space { id: x, ws })) if x.as_str() == id && ws.as_str() == WS)).count()
    }
}
fn joined(ext: &str) -> SpaceEvent {
    SpaceEvent::Joined {
        space_ext_id: ext.into(),
        name: Some("event name".into()),
        has_external: Some(true),
    }
}
fn member(handle: &str, external: bool) -> MemberInfo {
    MemberInfo {
        handle: FakeChannel::handle(handle),
        aliases: vec![],
        name: handle.into(),
        role: "member".into(),
        external,
    }
}

#[tokio::test]
async fn sg22_unknown_events_are_silent_and_invalid_ids_fail_before_lookup() {
    let h = setup();
    for event in [
        SpaceEvent::Left {
            space_ext_id: "unknown".into(),
            archived: true,
        },
        SpaceEvent::MembersAdded {
            space_ext_id: "unknown".into(),
            members: vec![member("unew", true)],
            has_external: Some(true),
        },
        SpaceEvent::MembersRemoved {
            space_ext_id: "unknown".into(),
            members: vec![FakeChannel::handle("unew")],
            has_external: None,
        },
        SpaceEvent::Updated {
            space_ext_id: "unknown".into(),
            name: Some("rename".into()),
            has_external: None,
        },
    ] {
        assert!(
            h.ch.on_space_event(WS, &h.conn, event)
                .await
                .unwrap()
                .is_none()
        );
    }
    for ext in [String::new(), "x".repeat(501)] {
        let err =
            h.ch.on_space_event(WS, "missing-connection", joined(&ext))
                .await
                .unwrap_err();
        assert!(err.to_string().contains("space_ext_id"), "{err}");
    }
    assert_eq!(*h.fake.info_calls.lock().unwrap(), 0);
    assert_eq!(*h.fake.member_calls.lock().unwrap(), 0);
    assert_eq!(h.n("SELECT count(*) FROM spaces", &[]), 0);
    assert!(h.env.take_events().is_empty());
}

#[tokio::test]
async fn sg22_join_uses_provider_or_event_fallback_and_reuses_the_open_decision() {
    for (registered, fresh) in [(true, true), (true, false), (false, false)] {
        let h = setup_with_adapter(registered);
        h.fake.space(
            "room",
            "provider name",
            SpaceKind::Group,
            false,
            &[
                (FakeChannel::BOT, "bot"),
                ("ualice", "Alice"),
                ("ubob", "Bob"),
            ],
        );
        *h.fake.fail_info.lock().unwrap() = !fresh;
        let first = h.event(joined("room")).await;
        assert!(first.created);
        assert_eq!(first.members_changed, if fresh { 2 } else { 0 });
        assert_eq!(first.has_external, !fresh);
        let space = h.space(&first.space_id);
        assert_eq!(
            space.name,
            if fresh { "provider name" } else { "event name" }
        );
        assert_eq!(space.member_count, if fresh { 3 } else { 0 });
        assert_eq!(
            h.n(
                "SELECT count(*) FROM space_members WHERE space_id=?",
                &[&space.id]
            ),
            if fresh { 3 } else { 1 }
        );
        assert_eq!(h.space_events(&space.id), 2);
        let again = h.event(joined("room")).await;
        assert!(!again.created);
        assert_eq!(again.members_changed, 0);
        assert!(again.decision_id.is_none());
        let decision = first.decision_id.unwrap();
        assert_eq!(
            h.n(
                "SELECT count(*) FROM decisions WHERE id=? AND status='open'",
                &[&decision]
            ),
            1
        );
        assert_eq!(
            *h.fake.info_calls.lock().unwrap(),
            if registered { 2 } else { 0 }
        );
        assert_eq!(
            *h.fake.member_calls.lock().unwrap(),
            if registered { 2 } else { 0 }
        );
    }
}

#[tokio::test]
async fn sg22_leave_and_archive_expire_once_and_rejoin_reopens_setup() {
    for (archived, status, reason) in [(false, "left", "left"), (true, "archived", "disbanded")] {
        let h = setup();
        let first = h.event(joined("room")).await;
        h.env.take_events();
        let left = h
            .event(SpaceEvent::Left {
                space_ext_id: "room".into(),
                archived,
            })
            .await;
        assert_eq!(left.status, status);
        assert_eq!(h.space_events(&left.space_id), 1);
        let decision = first.decision_id.unwrap();
        assert_eq!(h.n("SELECT count(*) FROM decisions WHERE id=? AND status='expired' AND json_extract(result,'$.reason')=?", &[&decision, reason]), 1);
        let repeated = h
            .event(SpaceEvent::Left {
                space_ext_id: "room".into(),
                archived,
            })
            .await;
        assert_eq!(repeated, left);
        assert_eq!(h.space_events(&left.space_id), 0);
        let rejoined = h.event(joined("room")).await;
        assert!(!rejoined.created);
        assert_eq!(rejoined.status, "active");
        let new_decision = rejoined.decision_id.unwrap();
        assert_ne!(decision, new_decision);
        assert_eq!(
            h.n(
                "SELECT count(*) FROM decisions WHERE id=? AND status='open'",
                &[&new_decision]
            ),
            1
        );
    }
}

#[tokio::test]
async fn sg22_member_deltas_refresh_counts_and_preserve_external_fallback_rules() {
    for fresh in [false, true] {
        let h = setup();
        h.fake.space(
            "room",
            "initial",
            SpaceKind::Group,
            false,
            &[("ualice", "Alice")],
        );
        let space = h.event(joined("room")).await.space_id;
        h.env.take_events();
        h.fake.space(
            "room",
            "fresh",
            SpaceKind::Group,
            false,
            &[
                ("ualice", "Alice"),
                ("ux1", "1"),
                ("ux2", "2"),
                ("ux3", "3"),
                ("ux4", "4"),
            ],
        );
        *h.fake.fail_info.lock().unwrap() = !fresh;
        let added = h
            .event(SpaceEvent::MembersAdded {
                space_ext_id: "room".into(),
                members: vec![
                    member(FakeChannel::BOT, false),
                    member("ualice", false),
                    member("ucarol", true),
                ],
                has_external: Some(false),
            })
            .await;
        assert_eq!(added.members_changed, 1);
        assert!(
            added.has_external,
            "an external newcomer overrides a stale internal provider flag"
        );
        assert_eq!(h.space(&space).member_count, if fresh { 5 } else { 2 });
        assert_eq!(
            h.n(
                "SELECT count(*) FROM space_members WHERE space_id=?",
                &[&space]
            ),
            3
        );
        assert_eq!(h.space_events(&space), 1);
        h.fake.space(
            "room",
            "read after leave",
            SpaceKind::Group,
            true,
            &[("ualice", "Alice"), ("ux1", "1"), ("ux2", "2")],
        );
        let removed = h
            .event(SpaceEvent::MembersRemoved {
                space_ext_id: "room".into(),
                members: vec![
                    HandleRef {
                        extra: vec!["ucarol".into()],
                        ..FakeChannel::handle("not-primary")
                    },
                    FakeChannel::handle(FakeChannel::BOT),
                    FakeChannel::handle("unknown"),
                ],
                has_external: Some(false),
            })
            .await;
        assert_eq!(removed.members_changed, 1);
        assert_eq!(removed.has_external, fresh);
        assert_eq!(h.space(&space).member_count, if fresh { 3 } else { 1 });
        assert_eq!(
            h.n(
                "SELECT count(*) FROM space_members WHERE space_id=?",
                &[&space]
            ),
            2
        );
        assert_eq!(h.space_events(&space), 1);
        let no_delta = h
            .event(SpaceEvent::MembersRemoved {
                space_ext_id: "room".into(),
                members: vec![FakeChannel::handle("ucarol")],
                has_external: None,
            })
            .await;
        assert_eq!(no_delta.members_changed, 0);
        assert_eq!(no_delta.has_external, fresh);
        assert_eq!(h.space(&space).member_count, if fresh { 3 } else { 1 });
        assert_eq!(
            *h.fake.member_calls.lock().unwrap(),
            1,
            "delta events never refresh the member list"
        );
    }
}

#[tokio::test]
async fn sg22_updated_name_wins_and_empty_or_missing_flags_keep_the_existing_values() {
    for (fresh, name, flag, expected_name, external) in [
        (
            true,
            Some(" event rename "),
            Some(false),
            "event rename",
            true,
        ),
        (true, Some("  "), None, "provider rename", true),
        (false, None, None, "initial", false),
        (false, Some(" fallback "), Some(true), "fallback", true),
    ] {
        let h = setup();
        h.fake.space(
            "room",
            "initial",
            SpaceKind::Group,
            false,
            &[("ualice", "Alice")],
        );
        let space = h.event(joined("room")).await.space_id;
        h.env.take_events();
        h.fake.space(
            "room",
            "provider rename",
            SpaceKind::Group,
            true,
            &[("ualice", "Alice"), ("ubob", "Bob")],
        );
        *h.fake.fail_info.lock().unwrap() = !fresh;
        let updated = h
            .event(SpaceEvent::Updated {
                space_ext_id: "room".into(),
                name: name.map(str::to_owned),
                has_external: flag,
            })
            .await;
        let row = h.space(&space);
        assert_eq!(row.name, expected_name);
        assert_eq!(row.member_count, if fresh { 2 } else { 1 });
        assert_eq!(updated.has_external, external);
        assert_eq!(h.space_events(&space), 1);
    }
}

#[tokio::test]
async fn sg22_member_update_failure_rolls_back_identity_membership_and_events() {
    let h = setup();
    let first = h.event(joined("room")).await;
    let before = h.n("SELECT count(*) FROM accounts", &[]);
    h.env.store().with_conn(|c| {
        c.execute_batch("CREATE TEMP TRIGGER sg22_abort BEFORE UPDATE ON spaces BEGIN SELECT RAISE(ABORT,'sg22 rollback'); END;")?;
        Ok(())
    }).unwrap();
    h.env.take_events();
    let error =
        h.ch.on_space_event(
            WS,
            &h.conn,
            SpaceEvent::MembersAdded {
                space_ext_id: "room".into(),
                members: vec![member("unew", true)],
                has_external: Some(true),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sg22 rollback"), "{error}");
    assert_eq!(h.n("SELECT count(*) FROM accounts", &[]), before);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM space_members WHERE space_id=?",
            &[&first.space_id]
        ),
        1
    );
    assert_eq!(h.space(&first.space_id).member_count, 0);
    assert!(h.env.take_events().is_empty());
}
