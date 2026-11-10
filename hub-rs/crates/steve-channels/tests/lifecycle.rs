//! The connection runtime (`steve_channels::lifecycle`) with a scripted hook: the events tapped
//! before the connections are listed, one supervisor per connection, a restart that waits for the
//! old run, the old run's sink refusing, what a stopped run returns dropped, failures and panics
//! retried with a backoff and reported to health, a finished run left alone, connections paired
//! with a bot re-evaluated on the bot's events, and shutdown aborting a run that does not stop;
//! the read views bound to one connection (`ConnQueries`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_channels::channel::{MemberInfo, SendTarget, SpaceInfo};
use steve_channels::ext::Queries;
use steve_channels::ext::{
    ChannelDecl, ConnRun, ConnSink, ConnState, ConnectionHook, Mode, Pairing, Receive,
};
use steve_channels::lifecycle::{ConnectionRuntime, Timing};
use steve_channels::{
    ChannelAdapter, ChannelFeatures, Channels, HandleRef, InboundMessage, OutContent,
};
use steve_flow::testkit::FakeCore;
use steve_flow::{AdapterError, FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{
    self, AccountInfo, AuthKind, ConnCx, ConnectionProvider, Connections, ConnectionsConfig,
    ProviderError,
};
use steve_modules::testkit::{ME, TestEnv, WS};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tokio::sync::mpsc;

const T: Duration = Duration::from_secs(10);

/// A provider with nothing behind it; `test_self` pairs with a `test_bot`, required.
struct Provider {
    id: &'static str,
    pairing: Option<Pairing>,
}

#[async_trait]
impl ConnectionProvider for Provider {
    fn id(&self) -> &str {
        self.id
    }
    fn system(&self) -> &str {
        "test"
    }
    fn name(&self) -> &str {
        self.id
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn pairing(&self) -> Option<Pairing> {
        self.pairing.clone()
    }
    async fn verify(&self, _cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        Err(ProviderError::Unsupported)
    }
}

/// `test_bot` as a channel: it receives every message of its spaces when `config.all`.
#[async_trait]
impl ChannelAdapter for Provider {
    fn features(&self) -> ChannelFeatures {
        ChannelFeatures::default()
    }
    fn receives_all(&self, config: &Value) -> bool {
        config["all"] == true
    }
    async fn space_info(&self, _cx: &ConnCx, _ext: &str) -> Result<SpaceInfo, AdapterError> {
        Err(ProviderError::Unsupported.into())
    }
    async fn members(&self, _cx: &ConnCx, _ext: &str) -> Result<Vec<MemberInfo>, AdapterError> {
        Err(ProviderError::Unsupported.into())
    }
    async fn send(
        &self,
        _cx: &ConnCx,
        _to: &SendTarget,
        _content: &OutContent,
    ) -> Result<String, AdapterError> {
        Err(ProviderError::Unsupported.into())
    }
    async fn recall(&self, _cx: &ConnCx, _ext: &str) -> Result<(), AdapterError> {
        Err(ProviderError::Unsupported.into())
    }
}

/// What a run of the probe does, per connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// Until stopped, then 100 ms to wind down.
    UntilStop,
    /// Returns `Ok` at once.
    Finish,
    Fail,
    FailAuth,
    Panic,
    /// Never looks at `stop`.
    IgnoreStop,
    /// Until stopped, then `Err(Auth)` (a credential destroyed under it).
    FailWhenStopped,
    /// `Err(Auth)` when the test says so (`Probe::fail_now`), before it is stopped; or until stopped.
    FailOnSignal,
}

#[derive(Default)]
struct Probe {
    how: Mutex<HashMap<String, How>>,
    log: Mutex<Vec<String>>,
    sinks: Mutex<Vec<(String, ConnSink)>>,
    states: Mutex<Vec<ConnState>>,
    fail_now: tokio::sync::Notify,
}

impl Probe {
    fn set(&self, conn: &str, how: How) {
        self.how.lock().unwrap().insert(conn.into(), how);
    }
    fn push(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    fn count(&self, entry: &str) -> usize {
        self.log().iter().filter(|e| *e == entry).count()
    }
}

/// Logs `<conn>:dropped` when a run is dropped before it ended (aborted).
struct Dropped<'a>(&'a Probe, String, bool);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        if !self.2 {
            self.0.push(format!("{}:dropped", self.1));
        }
    }
}

#[async_trait]
impl ConnectionHook for Probe {
    async fn run(&self, run: ConnRun) -> Result<(), AdapterError> {
        let id = run.state.id.clone();
        self.push(format!("{id}:start"));
        self.sinks
            .lock()
            .unwrap()
            .push((id.clone(), run.inbound.clone()));
        self.states.lock().unwrap().push(run.state.clone());
        let how = self
            .how
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or(How::UntilStop);
        let mut guard = Dropped(self, id.clone(), false);
        let r = match how {
            How::UntilStop => {
                run.stop.cancelled().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            }
            How::Finish => Ok(()),
            How::Fail => Err(ProviderError::Transient("down".into()).into()),
            How::FailAuth => Err(ProviderError::Auth("expired".into()).into()),
            How::Panic => panic!("probe panics"),
            How::IgnoreStop => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            }
            How::FailWhenStopped => {
                run.stop.cancelled().await;
                Err(ProviderError::Auth("credential gone".into()).into())
            }
            How::FailOnSignal => tokio::select! {
                _ = self.fail_now.notified() => Err(ProviderError::Auth("credential gone".into()).into()),
                _ = run.stop.cancelled() => Ok(()),
            },
        };
        guard.2 = true;
        self.push(format!("{id}:end"));
        r
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    probe: Arc<Probe>,
    rt: ConnectionRuntime,
    tx: mpsc::Sender<Value>,
    rx: Option<mpsc::Receiver<Value>>,
}

fn setup(timing: Timing) -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let bot = Arc::new(Provider {
        id: "test_bot",
        pairing: None,
    });
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None),
    )
    .provider(bot.clone())
    .provider(Arc::new(Provider {
        id: "test_self",
        pairing: Some(Pairing {
            with: vec!["test_bot".into()],
            required: true,
        }),
    }))
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(bot)
        .build();
    let probe = Arc::new(Probe::default());
    let rt = ConnectionRuntime::builder(ch.clone())
        .hook(
            "test_bot",
            Some(ChannelDecl::new("test_bot")),
            probe.clone(),
        )
        .hook(
            "test_self",
            Some(
                ChannelDecl::new("test_self")
                    .mode(Mode::Own)
                    .receive(Receive::Poll, Some("msg.receive")),
            ),
            probe.clone(),
        )
        .timing(timing)
        .build();
    let (tx, rx) = mpsc::channel(256);
    H {
        env,
        _keys: keys,
        ch,
        probe,
        rt,
        tx,
        rx: Some(rx),
    }
}

fn fast() -> Timing {
    Timing {
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        stop_grace: Duration::from_secs(2),
        shutdown_grace: Duration::from_millis(200),
    }
}

impl H {
    fn exec(&self, sql: &str) {
        self.env
            .store()
            .with_conn(|c| {
                c.execute_batch(&format!(
                    "PRAGMA foreign_keys = OFF; {sql}; PRAGMA foreign_keys = ON;"
                ))?;
                Ok(())
            })
            .expect(sql);
    }

    fn one(&self, sql: &str) -> String {
        self.env
            .store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get::<_, String>(0))?))
            .expect(sql)
    }

    /// A connection row, `ok` with `msg.receive`.
    fn add(&self, id: &str, adapter: &str, config: Value) {
        self.exec(&format!(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               VALUES ('acc_{id}', '{WS}', 'test', 'other', 'pty_{id}', 'bot', 'h_{id}', 'h_{id}', 'connect', 1, 1);
             INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, credential_item, config, created_by, created, updated)
               VALUES ('{id}', '{WS}', 'acc_{id}', '{adapter}', 'credential', '[\"msg.receive\"]', 'vi_{id}_1', '{config}', 'usr_dev', 1, 1)"
        ));
    }

    fn event(&self, id: &str) {
        let _ = self
            .tx
            .try_send(json!({"type": "connection", "id": id, "ws": WS}));
    }

    fn start(&mut self) {
        let rx = self.rx.take().expect("started once");
        drop(self.rt.start(rx));
    }

    async fn until(&self, what: &str, f: impl Fn(&H) -> bool) {
        let t0 = std::time::Instant::now();
        while !f(self) {
            assert!(
                t0.elapsed() < T,
                "timed out: {what}; log {:?}",
                self.probe.log()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_listed_and_changed_meanwhile_both_start() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    // made after the tap and before the listing: its event waits in the tap
    h.add("con_b", "test_bot", json!({}));
    h.event("con_b");
    // another adapter's connection: no hook, nothing runs
    h.add("con_x", "imap", json!({}));
    h.event("con_x");
    h.start();
    h.until("both run", |h| h.rt.running() == ["con_a", "con_b"])
        .await;
    // the repeated event changes nothing
    h.event("con_b");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.probe.count("con_b:start"), 1);
    // each got its channel settings in the declared mode first
    assert_eq!(
        h.one("SELECT mode FROM channel_settings WHERE connection_id = 'con_a'"),
        "bot"
    );
    // paused: stopped; resumed: started again
    h.exec("UPDATE connections SET status = 'paused' WHERE id = 'con_a'");
    h.event("con_a");
    h.until("stopped", |h| h.rt.running() == ["con_b"]).await;
    assert_eq!(h.probe.count("con_a:end"), 1);
    h.exec("UPDATE connections SET status = 'ok' WHERE id = 'con_a'");
    h.event("con_a");
    h.until("started again", |h| h.probe.count("con_a:start") == 2)
        .await;
    // deleted: stopped for good
    h.exec("DELETE FROM connections WHERE id = 'con_b'");
    h.event("con_b");
    h.until("gone", |h| h.rt.running() == ["con_a"]).await;
    h.rt.shutdown().await;
    assert!(h.rt.running().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_waits_for_the_old_run_whose_sink_then_refuses() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.start();
    h.until("runs", |h| h.probe.count("con_a:start") == 1).await;
    // a new credential: the old run is stopped and awaited, then the new one starts
    h.exec("UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'");
    h.event("con_a");
    h.until("restarted", |h| h.probe.count("con_a:start") == 2)
        .await;
    assert_eq!(
        h.probe.log(),
        ["con_a:start", "con_a:end", "con_a:start"],
        "the new run starts after the old one ended"
    );
    let sinks = h.probe.sinks.lock().unwrap().clone();
    let (old, new) = (&sinks[0].1, &sinks[1].1);
    assert!(old.is_stopped() && !new.is_stopped());
    let msg = InboundMessage::text("oc_1", "om_1", HandleRef::new("test", "u_1"), "hi", 1);
    let e = old.message(msg.clone()).await.expect_err("refused");
    assert!(
        matches!(e, steve_channels::ChannelError::Conflict(_)),
        "{e:?}"
    );
    // the sink is bound to its connection: another one's id is refused as well
    use steve_channels::MessageSink;
    assert!(new.inbound(WS, "con_other", msg).await.is_err());
    // a config change alone does not restart (the default fingerprint)
    h.exec("UPDATE connections SET config = '{\"x\":1}' WHERE id = 'con_a'");
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.probe.count("con_a:start"), 2);
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_stopped_run_returns_is_not_reported() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.probe.set("con_a", How::FailWhenStopped);
    h.start();
    h.until("runs", |h| h.probe.count("con_a:start") == 1).await;
    h.exec("UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'");
    h.event("con_a");
    h.until("restarted", |h| h.probe.count("con_a:start") == 2)
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.one("SELECT status FROM connections WHERE id = 'con_a'"),
        "ok"
    );
    h.rt.shutdown().await;
    assert_eq!(
        h.one("SELECT status FROM connections WHERE id = 'con_a'"),
        "ok"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_failing_on_a_credential_replaced_before_its_event_is_not_reported() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.probe.set("con_a", How::FailOnSignal);
    h.start();
    h.until("runs", |h| h.probe.count("con_a:start") == 1).await;
    // the credential replaced (the old item destroyed): the run fails on it before the event
    // has reached the runtime
    h.exec("UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'");
    h.probe.fail_now.notify_one();
    // not reported, and the run follows the new credential without waiting for the event
    h.until("started with the new credential", |h| {
        h.probe.count("con_a:start") == 2
    })
    .await;
    assert_eq!(
        h.probe.states.lock().unwrap()[1].credential.as_deref(),
        Some("vi_con_a_2")
    );
    // the event arrives late: nothing more to do
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.probe.count("con_a:start"), 2);
    assert_eq!(
        h.one("SELECT status FROM connections WHERE id = 'con_a'"),
        "ok"
    );
    assert_eq!(
        h.one("SELECT coalesce(json_extract(health, '$.last_error'), '') FROM connections WHERE id = 'con_a'"),
        ""
    );
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_and_panics_are_retried_and_reported() {
    let mut h = setup(fast());
    h.add("con_f", "test_bot", json!({}));
    h.add("con_p", "test_bot", json!({}));
    h.add("con_r", "test_bot", json!({}));
    h.probe.set("con_f", How::Fail);
    h.probe.set("con_p", How::Panic);
    h.probe.set("con_r", How::FailAuth);
    h.start();
    h.until("failed runs retried", |h| {
        h.probe.count("con_f:start") >= 3 && h.probe.count("con_p:start") >= 3
    })
    .await;
    // reported: failures count toward `error`, a panic is one too
    h.until("failures counted", |h| {
        h.one("SELECT status FROM connections WHERE id = 'con_f'") == "error"
            && h.one("SELECT status FROM connections WHERE id = 'con_p'") == "error"
    })
    .await;
    assert!(
        h.one("SELECT json_extract(health, '$.last_error') FROM connections WHERE id = 'con_f'")
            .contains("down")
    );
    // an auth failure: reauth, and no longer wanted once its event says so
    h.until("reauth", |h| {
        h.one("SELECT status FROM connections WHERE id = 'con_r'") == "reauth"
    })
    .await;
    h.event("con_r");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = h.probe.count("con_r:start");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(h.probe.count("con_r:start"), n, "not retried in reauth");
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_starts_again_only_when_wanted_again() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.probe.set("con_a", How::Finish);
    h.start();
    h.until("ran", |h| h.probe.count("con_a:end") == 1).await;
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.probe.count("con_a:start"), 1);
    h.exec("UPDATE connections SET status = 'paused' WHERE id = 'con_a'");
    h.event("con_a");
    h.exec("UPDATE connections SET status = 'ok' WHERE id = 'con_a'");
    // the two events may be read after both writes: then nothing changed, so also a new
    // credential (another fingerprint) starts it again
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(100)).await;
    if h.probe.count("con_a:start") == 1 {
        h.exec("UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'");
        h.event("con_a");
    }
    h.until("ran again", |h| h.probe.count("con_a:start") == 2)
        .await;
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_stop_holds_up_no_other_connection() {
    let mut h = setup(fast());
    h.add("con_slow", "test_bot", json!({}));
    h.probe.set("con_slow", How::IgnoreStop);
    h.start();
    h.until("runs", |h| h.probe.count("con_slow:start") == 1)
        .await;
    // its restart waits the whole grace (2 s) …
    h.exec("UPDATE connections SET credential_item = 'vi_2' WHERE id = 'con_slow'");
    h.event("con_slow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    // … while another connection starts at once
    let t0 = std::time::Instant::now();
    h.add("con_b", "test_bot", json!({}));
    h.event("con_b");
    h.until("the other runs", |h| h.probe.count("con_b:start") == 1)
        .await;
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(h.probe.count("con_slow:start"), 1);
    // then the slow one is aborted and started again
    h.until("aborted and restarted", |h| {
        h.probe.count("con_slow:dropped") == 1 && h.probe.count("con_slow:start") == 2
    })
    .await;
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_paired_with_a_bot_follow_its_events() {
    let mut h = setup(fast());
    h.add("con_bot", "test_bot", json!({}));
    h.add("con_me", "test_self", json!({"app_connection": "con_bot"}));
    // without a working pair it does not run (required)
    h.add("con_lone", "test_self", json!({}));
    h.start();
    h.until("both run", |h| h.rt.running() == ["con_bot", "con_me"])
        .await;
    let me = h
        .probe
        .states
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.id == "con_me")
        .cloned()
        .expect("state");
    assert!(me.live);
    assert_eq!(me.paired.as_ref().map(|p| p.id.as_str()), Some("con_bot"));
    assert_eq!(
        h.one("SELECT mode FROM channel_settings WHERE connection_id = 'con_me'"),
        "self"
    );
    // the bot paused: only its event comes, and the paired one stops with it
    h.exec("UPDATE connections SET status = 'paused' WHERE id = 'con_bot'");
    h.event("con_bot");
    h.until("both stopped", |h| h.rt.running().is_empty()).await;
    h.exec("UPDATE connections SET status = 'ok' WHERE id = 'con_bot'");
    h.event("con_bot");
    h.until("both run again", |h| {
        h.rt.running() == ["con_bot", "con_me"]
    })
    .await;
    // without `msg.receive`: not live
    h.exec("UPDATE connections SET caps = '[]' WHERE id = 'con_me'");
    h.event("con_me");
    h.until("receiving off", |h| h.rt.running() == ["con_bot"])
        .await;
    h.rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_aborts_a_run_that_does_not_stop() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.add("con_b", "test_bot", json!({}));
    h.probe.set("con_a", How::IgnoreStop);
    h.start();
    h.until("both run", |h| h.rt.running().len() == 2).await;
    let t0 = std::time::Instant::now();
    h.rt.shutdown().await;
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert!(h.rt.running().is_empty());
    assert_eq!(h.probe.count("con_a:dropped"), 1);
    assert_eq!(h.probe.count("con_b:end"), 1);
    // nothing starts after shutdown
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.probe.count("con_a:start"), 1);
    let _ = &h.ch;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_during_a_restart_waits_only_the_shutdown_grace() {
    let mut h = setup(fast());
    h.add("con_a", "test_bot", json!({}));
    h.probe.set("con_a", How::IgnoreStop);
    h.start();
    h.until("runs", |h| h.probe.count("con_a:start") == 1).await;
    // a restart: its supervisor waits up to the stop grace (2 s) for the old run …
    h.exec("UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'");
    h.event("con_a");
    tokio::time::sleep(Duration::from_millis(150)).await;
    // … but a shutdown meanwhile bounds it by the shutdown grace (200 ms)
    let t0 = std::time::Instant::now();
    h.rt.shutdown().await;
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert!(h.rt.running().is_empty());
    assert_eq!(h.probe.count("con_a:dropped"), 1);
    assert_eq!(h.probe.count("con_a:start"), 1, "the new run never started");
}

// ---------------------------------------------------------------------------------------------
// The read views.

#[tokio::test(flavor = "multi_thread")]
async fn the_read_views_are_bound_to_one_connection() {
    let h = setup(fast());
    h.add("con_me", "test_self", json!({}));
    h.add("con_bot", "test_bot", json!({"all": true}));
    h.add("con_bot2", "test_bot", json!({}));
    let views = Queries::new(h.ch.clone());
    let cx = |id: &str| h.ch.connections().cx(WS, id).expect("cx");
    let me = views.for_cx(&cx("con_me"));
    let other = views.for_cx(&cx("con_bot2"));
    h.exec(&format!(
        "INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated) VALUES
           ('sp_all', '{WS}', 'test', 'con_bot', 'group', 'oc_all', '全收', 'active', 1, 1),
           ('sp_at', '{WS}', 'test', 'con_bot2', 'group', 'oc_at', '只收提到', 'active', 1, 1),
           ('sp_mine', '{WS}', 'test', 'con_me', 'group', 'oc_mine', '我的', 'active', 1, 1);
         INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES
           ('sp_all', 'con_bot', '{WS}', 1), ('sp_at', 'con_bot2', '{WS}', 1), ('sp_mine', 'con_me', '{WS}', 1);
         INSERT INTO chan_messages (id, workspace_id, space_id, external_id, thread_key, direction, sender_account_id, text, status, created)
           VALUES ('cm_1', '{WS}', 'sp_all', 'om_1', 'om_1', 'in', 'acc_con_bot2', '你好', 'received', 1700000000123)"
    ));
    // covered only by a healthy connection that receives everything, never by itself
    assert!(me.covered("oc_all").expect("covered"));
    assert!(!me.covered("oc_at").expect("covered"));
    assert!(!me.covered("oc_mine").expect("covered"));
    h.exec("UPDATE connections SET status = 'error' WHERE id = 'con_bot'");
    assert!(!me.covered("oc_all").expect("covered"));

    // stored messages, by their provider id
    let msg = |ext: &str| InboundMessage::text("oc_all", ext, HandleRef::new("test", "u"), "", 1);
    assert!(me.known(&msg("om_1")).expect("known"));
    assert!(!me.known(&msg("om_2")).expect("known"));
    assert!(!me.known(&msg("")).expect("known"));
    let r = me
        .message_ref("oc_all", "om_1")
        .expect("ref")
        .expect("found");
    assert_eq!(r.id(), Some("om_1"));
    assert_eq!((r.created, r.created_exact), (1700000000123, true));
    assert_eq!(r.text.as_deref(), Some("你好"));
    assert_eq!(r.thread_key.as_deref(), Some("om_1"));
    assert_eq!(r.sender, Some(HandleRef::new("test", "h_con_bot2")));
    assert!(me.message_ref("oc_all", "om_2").expect("ref").is_none());
    assert!(
        me.message_ref_by_source("oc_all", "ev_1")
            .expect("ref")
            .is_none()
    );

    // cursors: each connection its own; a built-in poller's cursor is not taken over
    assert_eq!(me.cursor("feed").expect("cursor"), None);
    me.save_cursor("feed", "0042").expect("saved");
    me.save_cursor("feed", "0043:x").expect("saved");
    assert_eq!(
        me.cursor("feed").expect("cursor").as_deref(),
        Some("0043:x")
    );
    assert_eq!(other.cursor("feed").expect("cursor"), None);
    h.exec(&format!(
        "INSERT INTO chan_cursors (connection_id, workspace_id, container, kind, chat_id, cursor, created, updated)
           VALUES ('con_me', '{WS}', 'oc_poll', 'chat', 'oc_poll', 5, 1, 1)"
    ));
    assert!(me.save_cursor("oc_poll", "1").is_err());
    assert_eq!(me.cursor("oc_poll").expect("cursor"), None);
    // gone with the connection
    h.exec("DELETE FROM chan_cursors WHERE connection_id = 'con_me'; DELETE FROM connections WHERE id = 'con_me'");
    assert!(me.save_cursor("feed", "1").is_err());

    // a person's handle in another system: one valid everywhere, or under this connection
    h.exec(&format!(
        "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
           ('acc_f', '{WS}', 'feishu', 'feishu_user', 'pty_ann', 'personal', 'on_ann', 'on_ann', 'chat', 1, 1),
           ('acc_m', '{WS}', 'mail', 'email', 'pty_ann', 'work_email', 'ann@example.com', 'ann@example.com', 'chat', 2, 2);
         INSERT INTO account_handles (workspace_id, family, handle, account_id, connection_id, created) VALUES
           ('{WS}', 'feishu_user', 'on_ann', 'acc_f', NULL, 1),
           ('{WS}', 'feishu_user', 'ou_ann_bot2', 'acc_f', 'con_bot2', 2),
           ('{WS}', 'feishu_user', 'ou_ann_other', 'acc_f', 'con_x', 3),
           ('{WS}', 'email', 'ann@example.com', 'acc_m', NULL, 2)"
    ));
    let ann = HandleRef::new("feishu", "on_ann");
    assert_eq!(
        other.handle_for(&ann, "mail").expect("handle"),
        Some(HandleRef::new("mail", "ann@example.com"))
    );
    let by_mail = HandleRef::new("mail", "Ann@Example.com");
    assert_eq!(
        other.handle_for(&by_mail, "feishu").expect("handle"),
        Some(HandleRef::new("feishu", "ou_ann_bot2")),
        "the handle under this connection first"
    );
    assert_eq!(
        other
            .handle_for(&HandleRef::new("feishu", "on_nobody"), "mail")
            .expect("handle"),
        None
    );
}
