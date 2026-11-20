//! Direct polling contracts; real temporary Vault, existing FakeFeishu, no background polling races.
use super::*;
use crate::adapters::feishu::auth::{FeishuApp, FeishuUser};
use crate::fake::feishu::{FakeFeishu, World};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use steve_modules::accounts::connections::{self, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
#[path = "../support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;

type AtRead = Box<dyn FnOnce() + Send>;
struct PollClock {
    base: Arc<steve_modules::FakeClock>,
    remaining: AtomicUsize,
    jump: AtomicI64,
    at_read: Mutex<Option<AtRead>>,
}
impl PollClock {
    fn arm(&self, reads: usize, jump: i64, action: Option<AtRead>) {
        self.jump.store(jump, Ordering::SeqCst);
        *self.at_read.lock().unwrap() = action;
        self.remaining.store(reads, Ordering::SeqCst);
    }
}
impl steve_runtime::Clock for PollClock {
    fn now(&self) -> i64 {
        let n = self
            .remaining
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n > 0).then(|| n - 1)
            })
            .unwrap_or(0);
        if n == 1 {
            let action = self.at_read.lock().unwrap().take();
            if let Some(action) = action {
                action();
            }
            self.base.advance(self.jump.load(Ordering::SeqCst));
        }
        steve_runtime::Clock::now(&*self.base)
    }
    fn sleep(
        &self,
        d: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.base.sleep(d)
    }
}
struct Fixture {
    env: TestEnv,
    regs: Registrations,
    fake: FakeFeishu,
    clock: Arc<PollClock>,
    _keys: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let fake = FakeFeishu::start_with(World::sample()).await.unwrap();
        let mut env = TestEnv::new(connections::register);
        env.clock.set(1_000_000);
        let clock = Arc::new(PollClock {
            base: env.clock.clone(),
            remaining: AtomicUsize::new(0),
            jump: AtomicI64::new(0),
            at_read: Mutex::new(None),
        });
        let mut registry = steve_modules::Registry::new();
        connections::register(&mut registry);
        env.modules = Modules::with_clock(
            env.store().clone(),
            env.gateway.clone(),
            registry,
            clock.clone(),
        );
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        vault.init(WS, ME).unwrap();
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(vec!["http://sg21.test:18990".into()], true, |_| None),
        )
        .provider(Arc::new(FeishuApp::new()))
        .provider(Arc::new(FeishuUser::with_base(fake.base_url())))
        .build();
        let regs = Registrations::new(conns, Bases::fake(fake.base_url()))
            .with_tick(Duration::from_millis(1));
        Self {
            env,
            regs,
            fake,
            clock,
            _keys: keys,
        }
    }
    async fn pending(&self, interval: i64, left_ms: i64) -> String {
        let who = caller(WS, ME);
        let prepared = self
            .regs
            .conns
            .prepare_credential(
                &who,
                TICKET_TITLE,
                vec![(TICKET_FIELD.into(), Zeroizing::new("sg21-ticket".into()))],
            )
            .await
            .unwrap();
        let id = uid("reg");
        let now = self.regs.now();
        self.regs.tx(|tx|{
            let item=self.regs.conns.insert_prepared(tx,&prepared)?;
            tx.execute("INSERT INTO chan_app_registrations(id,workspace_id,initiator,brand_requested,brand,device_item,user_code,verification_uri_complete,interval_s,expires_at,state,created,updated)
                VALUES(?,?,?,'feishu','feishu',?,'SG21','https://example.invalid/confirm',?,?,?, ?,?)",
                params![id,WS,ME,item,interval,now+left_ms,state::PENDING,now,now])?;Ok(())
        }).unwrap();
        id
    }
    fn row(&self, id: &str) -> Row {
        self.regs.read(|c| load(c, id)).unwrap().unwrap()
    }
    fn live_items(&self) -> i64 {
        self.regs.read(|c| {
            let decryptable_deleted: i64 = c.query_row(
                "SELECT count(*) FROM vault_items WHERE status='deleted' AND dek_wrapped IS NOT NULL",
                [], |r| r.get(0),
            )?;
            assert_eq!(decryptable_deleted, 0, "deleted credentials must have no wrapped data key");
            c.query_row("SELECT count(*) FROM vault_items WHERE status='active'", [], |r| r.get(0))
        }).unwrap()
    }
    fn assert_redacted(&self, id: &str) {
        let out = self.row(id).out();
        let text = out.to_string();
        assert!(!text.contains("sg21-ticket"));
        assert!(out.get("device_code").is_none());
        assert!(out.get("client_secret").is_none());
    }
}

#[tokio::test]
async fn sg21_poll_terminal_rows_and_missing_rows_do_nothing() {
    for state in [
        state::SAVED,
        state::VERIFYING,
        state::SUCCEEDED,
        state::DENIED,
        state::EXPIRED,
        state::FAILED,
        state::CANCELLED,
    ] {
        let h = Fixture::new().await;
        let id = h.pending(1, 60_000).await;
        h.regs
            .tx(|tx| {
                tx.execute(
                    "UPDATE chan_app_registrations SET state=? WHERE id=?",
                    params![state, id],
                )?;
                Ok(())
            })
            .unwrap();
        let before = h.row(&id).out();
        let count = h.live_items();
        h.regs.poll_until_done(&id, false).await.unwrap();
        h.regs.poll_until_done("missing", true).await.unwrap();
        assert_eq!(h.row(&id).out(), before);
        assert_eq!(h.live_items(), count);
        assert!(h.fake.reg_calls().is_empty());
    }
}
#[tokio::test]
async fn sg21_poll_expiry_distinguishes_restart_and_checks_again_after_ticket_read() {
    for resumed in [false, true] {
        for after_ticket in [false, true] {
            let h = Fixture::new().await;
            let id = h.pending(1, if after_ticket { 1_000 } else { 0 }).await;
            if after_ticket {
                h.clock.arm(2, 1_001, None);
            }
            h.regs.poll_until_done(&id, resumed).await.unwrap();
            let r = h.row(&id);
            assert_eq!(r.state, state::EXPIRED);
            assert_eq!(
                r.error_kind.as_deref(),
                Some(if resumed {
                    Kind::CredentialsLost.code()
                } else {
                    Kind::Expired.code()
                })
            );
            assert_eq!(r.app_may_exist, resumed);
            assert!(r.device_item.is_none());
            assert_eq!(h.live_items(), 0);
            assert!(h.fake.reg_calls().is_empty());
            h.assert_redacted(&id);
        }
    }
}
#[tokio::test]
async fn sg21_poll_missing_and_transiently_unreadable_tickets_have_different_outcomes() {
    for transient in [false, true] {
        let h = Fixture::new().await;
        let id = h.pending(1, 1_000).await;
        if transient {
            h.regs.tx(|tx|{tx.execute_batch("CREATE TRIGGER sg21_refuse_ticket_use BEFORE INSERT ON secret_uses BEGIN SELECT RAISE(FAIL,'sg21 temporary ticket audit failure'); END;")?;Ok(())}).unwrap();
            assert!(h.regs.ticket(&h.row(&id)).await.is_err());
            // The next loop's deadline read ends the retry; treating this as a gone ticket would fail.
            h.clock.arm(2, 1_001, None);
        } else {
            h.regs.tx(|tx| h.regs.drop_ticket(tx, &id)).unwrap();
        }
        h.regs.poll_until_done(&id, false).await.unwrap();
        let r = h.row(&id);
        assert_eq!(
            r.state,
            if transient {
                state::EXPIRED
            } else {
                state::FAILED
            }
        );
        assert_eq!(
            r.error_kind.as_deref(),
            Some(if transient {
                Kind::Expired.code()
            } else {
                Kind::Provider.code()
            })
        );
        assert!(r.device_item.is_none());
        assert_eq!(h.live_items(), 0);
        assert!(h.fake.reg_calls().is_empty());
    }
}
#[tokio::test]
async fn sg21_poll_classification_backoff_and_switch_sequence_table() {
    for case in [
        "pending",
        "slow",
        "cap",
        "denied",
        "expired",
        "expired-resumed",
        "provider-failed",
        "switch",
        "contradiction",
    ] {
        let mut h = Fixture::new().await;
        let interval = if case == "cap" { 59 } else { 1 };
        let id = h.pending(interval, 60_000).await;
        let deadline = h.row(&id).expires_at;
        let denied = json!({"error":"access_denied"});
        let answers = match case {
            "pending" => vec![json!({"error":"authorization_pending"}), denied],
            "slow" | "cap" => vec![json!({"error":"slow_down"}), denied],
            "expired" | "expired-resumed" => vec![json!({"error":"expired_token"})],
            "provider-failed" => {
                vec![json!({"error":"not_supported","error_description":"must never be returned"})]
            }
            "switch" => vec![json!({"user_info":{"tenant_brand":"lark"}}), denied],
            "contradiction" => vec![
                json!({"user_info":{"tenant_brand":"lark"}}),
                h.fake.reg_issued("feishu"),
            ],
            _ => vec![denied],
        };
        if matches!(case, "switch" | "contradiction") {
            // A 60-second tick makes an accidental wait after switching fail the bounded harness.
            h.regs.tick = Duration::from_secs(60);
        }
        h.fake.reg_polls(answers);
        tokio::time::timeout(
            Duration::from_secs(3),
            h.regs.poll_until_done(&id, case == "expired-resumed"),
        )
        .await
        .unwrap()
        .unwrap();
        let r = h.row(&id);
        let expected = match case {
            "expired" | "expired-resumed" => state::EXPIRED,
            "provider-failed" | "contradiction" => state::FAILED,
            _ => state::DENIED,
        };
        assert_eq!(r.state, expected, "{case}");
        assert_eq!(
            r.interval_s,
            match case {
                "slow" => 6,
                "cap" => 60,
                _ => interval,
            },
            "{case}"
        );
        assert_eq!(
            r.expires_at, deadline,
            "switch must never extend the deadline"
        );
        assert_eq!(
            r.brand,
            if matches!(case, "switch" | "contradiction") {
                "lark"
            } else {
                "feishu"
            }
        );
        assert_eq!(r.switched, matches!(case, "switch" | "contradiction"));
        assert_eq!(
            r.app_may_exist,
            matches!(case, "expired-resumed" | "contradiction")
        );
        assert!(r.device_item.is_none());
        assert_eq!(h.live_items(), 0);
        let calls = h.fake.reg_calls();
        assert_eq!(
            calls.iter().map(|(b, _)| b.as_str()).collect::<Vec<_>>(),
            if matches!(case, "switch" | "contradiction") {
                vec!["feishu", "lark"]
            } else if matches!(case, "pending" | "slow" | "cap") {
                vec!["feishu", "feishu"]
            } else {
                vec!["feishu"]
            }
        );
        assert!(
            h.fake
                .reg_forms()
                .iter()
                .all(|f| f["action"] == "poll" && f["device_code"] == "sg21-ticket")
        );
        assert!(!r.out().to_string().contains("must never be returned"));
        h.assert_redacted(&id);
    }
}
#[tokio::test]
async fn sg21_poll_request_failure_increments_interval_with_a_ceiling_before_deadline_exit() {
    for interval in [1, 60] {
        let mut h = Fixture::new().await;
        let id = h.pending(interval, 1_000).await;
        Arc::make_mut(&mut h.regs.bases).feishu_accounts = "invalid URL".into();
        h.clock.arm(3, 1_001, None);
        h.regs.poll_until_done(&id, false).await.unwrap();
        let r = h.row(&id);
        assert_eq!(r.interval_s, (interval + 1).min(60));
        assert_eq!(r.state, state::EXPIRED);
        assert!(h.fake.reg_calls().is_empty());
        assert_eq!(h.live_items(), 0);
    }
}
#[tokio::test]
async fn sg21_poll_keeps_issued_credentials_after_lateness_or_member_departure_but_not_cancel() {
    for case in ["saved", "late", "cancelled", "member-left"] {
        let h = Fixture::new().await;
        let id = h.pending(1, 1_000).await;
        // Exercise poll/save only. The existing RAII running mark suppresses its separate verifier;
        // the unchanged integration suite covers the real save→verify→bind chain.
        let _verify = h.regs.mark(format!("verify:{id}")).unwrap();
        let action: Option<AtRead> = match case {
            "cancelled" => {
                let regs = h.regs.clone();
                let id = id.clone();
                Some(Box::new(move || {
                    regs.cancel(&caller(WS, ME), &id).unwrap();
                }))
            }
            "member-left" => {
                let store = h.env.store().clone();
                Some(Box::new(move || {
                    store
                        .transaction(|tx| -> crate::ChannelResult<()> {
                            tx.execute(
                                "DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?",
                                params![WS, ME],
                            )?;
                            Ok(())
                        })
                        .unwrap();
                }))
            }
            _ => None,
        };
        h.clock
            .arm(3, if case == "late" { 1_001 } else { 0 }, action);
        h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
        h.regs.poll_until_done(&id, false).await.unwrap();
        let r = h.row(&id);
        assert!(r.device_item.is_none());
        if case == "cancelled" {
            assert_eq!(r.state, state::CANCELLED);
            assert!(r.vault_item.is_none());
            assert!(r.app_may_exist);
            assert_eq!(h.live_items(), 0);
        } else {
            assert_eq!(r.state, state::SAVED);
            assert!(r.vault_item.is_some());
            assert_eq!(h.live_items(), 1);
            assert_eq!(
                r.error_kind.as_deref(),
                (case == "member-left").then_some(Kind::MemberGone.code())
            );
            assert_eq!(r.client_id.as_deref(), Some(h.fake.app().app_id.as_str()));
        }
        if case == "late" {
            assert!(r.expires_at < h.regs.now());
        }
        let connections = h
            .regs
            .read(|c| {
                c.query_row("SELECT count(*) FROM connections", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .unwrap();
        assert_eq!(connections, 0);
        h.assert_redacted(&id);
    }
}
#[tokio::test]
async fn sg21_poll_store_failure_is_returned_after_provider_response_and_keeps_the_ticket() {
    let h = Fixture::new().await;
    let id = h.pending(1, 60_000).await;
    let item = h.row(&id).device_item;
    h.regs.tx(|tx|{tx.execute_batch("CREATE TRIGGER sg21_refuse_end BEFORE UPDATE ON chan_app_registrations BEGIN SELECT RAISE(FAIL,'sg21 registration storage refusal'); END;")?;Ok(())}).unwrap();
    h.fake.reg_polls(vec![json!({"error":"access_denied"})]);
    let err = h.regs.poll_until_done(&id, false).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("sg21 registration storage refusal")
    );
    let r = h.row(&id);
    assert_eq!(r.state, state::PENDING);
    assert_eq!(r.device_item, item);
    assert_eq!(h.live_items(), 1);
    assert_eq!(h.fake.reg_calls().len(), 1);
}
