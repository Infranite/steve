//! The real notification source and migrated store, without external sends.
use std::sync::Arc;
use steve_channels::notifications::NotificationDue;
use steve_channels::outbound::OutLink;
use steve_channels::{Channels, Outbox};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_store::{
    notification_repo::{self, Policy},
    testkit::channels::notifications as fixture,
};
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use steve_modules::testkit::{ME, WS};
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
        env.store()
            .write(|w| {
                notification_repo::set_policy(
                    w,
                    WS,
                    ME,
                    &Policy {
                        enabled: true,
                        mode: "now".into(),
                        party: None,
                        space: None,
                        timezone: "UTC".into(),
                        quiet_start: None,
                        quiet_end: None,
                        starred_in_quiet: false,
                    },
                    0,
                )
            })
            .unwrap();
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
        fixture::ids_in_state(self.env.store(), state).unwrap()
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
        // A poison record has a permanently broken route (enabled policy, no self party).
        // Other records are processed by terminal cancellation (no policy), not a network send.
        let user = if poison { ME } else { "usr_other" };
        let topic = if poison && self.unknown_topic {
            "future.topic"
        } else {
            "todo.due"
        };
        fixture::seed_waiting(self.env.store(), WS, user, topic, at, NOW).unwrap()
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

#[tokio::test]
async fn committed_notification_intake_wakes_the_idle_loop_without_advancing_time() {
    let f = Notifications::new();
    let task = tokio::spawn(steve_channels::notifications::run(
        f.source.channels.clone(),
        f.source.outbox.clone(),
    ));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let id = f.add(NOW, false);
    for _ in 0..1000 {
        if f.done().contains(&id) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(f.done().contains(&id));
    task.abort();
    let _ = task.await;
}

#[path = "support/due_wakes.rs"]
mod due_wakes;
#[tokio::test]
async fn notification_commits_do_not_self_wake_into_a_busy_loop() {
    let f = Notifications::new();
    f.add(NOW, true);
    let ch = f.source.channels.clone();
    due_wakes::assert_no_self_wake_loop(
        f.source,
        &ch,
        steve_channels::notifications::NOTIFICATION_WAKE_TABLES,
    )
    .await;
}
