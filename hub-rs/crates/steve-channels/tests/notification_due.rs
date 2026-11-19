//! The real notification source and migrated store, without external sends.
use std::sync::Arc;
use steve_channels::notifications::NotificationDue;
use steve_channels::outbound::OutLink;
use steve_channels::{Channels, Outbox};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
use steve_runtime::due::contract::{self, Fixture};

const NOW: i64 = 1_900_000_000_000;
struct Notifications {
    source: NotificationDue,
    env: TestEnv,
    unknown_topic: bool,
}
impl Notifications {
    fn new() -> Self {
        let setup = FlowSetup::new(Arc::new(FakeCore::default()));
        let env = TestEnv::new(|reg| {
            setup.register(reg);
            connections::register(reg);
        });
        env.clock.set(NOW);
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(&env.dir.path().join("keys")).unwrap()),
        );
        vault.init(WS, ME).unwrap();
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(vec!["http://test.invalid".into()], true, |_| None),
        )
        .build();
        let flow = setup
            .connections(conns.clone())
            .build(env.modules.clone(), Arc::new(LinearExecutor));
        let channels = Channels::builder(env.modules.clone(), conns, Arc::new(flow)).build();
        env.store().with_conn(|c| {
            c.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES (?1,?2,1,'now','UTC',0,0,0)", [WS,ME])?;
            Ok(())
        }).unwrap();
        Self {
            source: NotificationDue {
                channels,
                outbox: Arc::new(Outbox::new(OutLink::new())),
            },
            env,
            unknown_topic: false,
        }
    }
    fn ids(&self, state: &str) -> Vec<String> {
        self.env
            .store()
            .with_conn(|c| {
                let mut st = c.prepare("SELECT id FROM personal_notifications WHERE state=?")?;
                Ok(st
                    .query_map([state], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .unwrap()
    }
}
impl Fixture for Notifications {
    type Source = NotificationDue;
    fn source(&self) -> &NotificationDue {
        &self.source
    }
    fn batch(&self) -> usize {
        50
    }
    fn add(&self, at: i64, poison: bool) -> String {
        let id = steve_store::uid("notification");
        // A poison record has a permanently broken route (enabled policy, no self party).
        // Other records are processed by terminal cancellation (no policy), not a network send.
        let user = if poison { ME } else { "usr_other" };
        let topic = if poison && self.unknown_topic {
            "future.topic"
        } else {
            "todo.due"
        };
        self.env.store().with_conn(|c| {
            // Simulate a row from a newer producer/import; the migrated schema rejects unknown topics.
            c.pragma_update(None, "ignore_check_constraints", self.unknown_topic)?;
            let inserted = c.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,next_attempt_at,created,topic)
              VALUES (?1,?2,?3,?1,?1,'now','body','waiting',?4,?5,'todo.due')", rusqlite::params![id,WS,user,at,NOW]);
            if inserted.is_ok() {
                c.execute("UPDATE personal_notifications SET topic=? WHERE id=?", [&topic, &id.as_str()])?;
            }
            c.pragma_update(None, "ignore_check_constraints", false)?;
            inserted?;
            Ok(())
        }).unwrap();
        id
    }
    fn done(&self) -> Vec<String> {
        self.ids("cancelled")
    }
    // The domain has no attempt-count dead-letter state. The existing body-retention
    // tombstone is its terminal state, after 30 days; never add a test-only migration.
    fn dead(&self) -> Vec<String> {
        self.ids("purged")
    }
}
#[tokio::test]
async fn notification_source_obeys_the_shared_contract_with_retention_tombstones() {
    contract::check(Notifications::new, NOW, 32).await;
}

#[tokio::test]
async fn the_owned_name_job_is_singleton_and_can_restart_after_cancellation() {
    let f = Notifications::new();
    let task = f.source.channels.spawn_names().unwrap();
    assert_eq!(task.name(), "channels.names");
    assert!(f.source.channels.spawn_names().is_none());
    task.stop();
    task.await;
    let restarted = f
        .source
        .channels
        .spawn_names()
        .expect("cancelled job released its running flag");
    restarted.stop();
    restarted.await;
}

#[tokio::test]
async fn unknown_topics_advance_and_obey_the_shared_contract() {
    use steve_runtime::due::DueSource;
    let new = || {
        let mut f = Notifications::new();
        f.unknown_topic = true;
        f
    };
    let f = new();
    let poison = f.add(NOW, true);
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + 10_000));
    assert!(!f.done().contains(&poison));
    contract::check(new, NOW, 32).await;
}
