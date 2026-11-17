//! A trigger word end to end on the real channel path, as the Hub wires it: the channel module
//! hands your `/ai` to steve-core's conversation of the chat ([`CoreThread`]), steve-core asks the
//! channel module for the answer ([`ChanConvHost`]), the composer calls the model gateway
//! ([`SharedGateway`] over [`Fallback`]), and the model reaches for a tool the answer may not use
//! ([`CoreError::NeedsTools`]). No string is faked in between: what the gateway says is what the
//! hand-off reads.

use super::chan::{ChanConvHost, CoreThread};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_channels::compose::H_SAFETY;
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::{Channels, Replies, SpaceKind};
use steve_core::gateway::{OnText, OneCall, Tried};
use steve_core::{
    AgentRuntime, Answered, Core, CoreConfig, CoreError, Fallback, Flushed, Prompted, Released,
    Running, Stop,
};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{Modules, Registry};

/// The models of the gateway: `tool` reaches for a tool on the chat answer, `next` would answer it.
/// Every call is recorded as (model, what it was).
#[derive(Default)]
struct Models {
    calls: Mutex<Vec<(String, &'static str)>>,
}

impl Models {
    /// The models the chat answers and drafts were tried on, in order.
    fn answers(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, what)| *what == "answer")
            .map(|(m, _)| m.clone())
            .collect()
    }
}

/// [`Models`] as the gateway's one model.
struct One(Arc<Models>);

const ROUTE: &str = r#"{"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "看磁盘", "goal": "看看这台机器的磁盘用量", "bot": "steve"}"#;
const REPLY: &str = r#"{"text": "磁盘还剩很多。", "needs_approval": false, "why": "猜的"}"#;

#[async_trait::async_trait]
impl steve_core::OneModel for One {
    async fn once(
        &self,
        call: &OneCall<'_>,
        model: &str,
        _effort: Option<&str>,
        _on_text: Option<OnText<'_>>,
    ) -> Tried {
        let p = call.prompt;
        let (what, reply) = if p.starts_with("You are the router") {
            ("route", Ok(ROUTE.to_string()))
        } else if p.starts_with(H_SAFETY) {
            match model {
                "tool" => ("answer", Err(CoreError::NeedsTools)),
                _ => ("answer", Ok(REPLY.to_string())),
            }
        } else if p.starts_with(CHECK_HEAD) {
            (
                "check",
                Ok(r#"{"verdict": "pass", "spans": []}"#.to_string()),
            )
        } else {
            ("other", Ok("{}".to_string()))
        };
        self.0.calls.lock().unwrap().push((model.to_string(), what));
        Tried {
            reply: reply.map(|t| (t, None)),
            usage: vec![],
            timed_out: false,
        }
    }
}

/// Runs no session: an item that starts is ended at once, said on it (no harness in the test).
struct NoSessions;

impl AgentRuntime for NoSessions {
    fn refusal(&self, _session_id: &str) -> Option<String> {
        Some("这个测试不运行会话".into())
    }
    fn prompt(
        &self,
        _session_id: &str,
        _text: &str,
        _via: &str,
        _files: &[steve_core::repo::FileRef],
    ) -> steve_core::Result<Prompted> {
        Ok(Prompted::Sent)
    }
    fn running(&self, _session_id: &str) -> Option<Running> {
        None
    }
    fn stop(&self, _turn: &Running) -> steve_core::Result<Stop> {
        Ok(Stop::NotRunning)
    }
    fn is_busy(&self, _session_id: &str) -> bool {
        false
    }
    fn release(&self, _session_id: &str) -> steve_core::Result<Released> {
        Ok(Released::Gone)
    }
    fn answer_permission(
        &self,
        _decision: &str,
        _option: Option<&str>,
    ) -> steve_core::Result<Answered> {
        Ok(Answered::Gone)
    }
    fn hold(&self, _on: bool) {}
    fn is_holding(&self) -> bool {
        false
    }
    fn running_turns(&self) -> usize {
        0
    }
    fn flush(&self, _session_id: &str, _lead: Option<(&str, &str)>) -> Flushed {
        Flushed::Empty
    }
}

fn count(store: &steve_store::Store, sql: &str) -> i64 {
    store
        .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_word_whose_answer_needs_tools_makes_exactly_one_item() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = CoreConfig::for_home(dir.path());
    for d in [&cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let store =
        steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| {
            c.execute_batch(&format!(
                "INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('{ME}', '{WS}', 'dev', 'Dev', 'owner', 1);
                 INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES ('{WS}', '{ME}', 'owner', 1);"
            ))?;
            Ok(())
        })
        .unwrap();

    // the gateway as wired: the fallback over two models, shared with the modules
    let models = Arc::new(Models::default());
    let gateway: Arc<dyn steve_core::ModelGateway> = Arc::new(Fallback {
        one: One(models.clone()),
        fast: vec!["tool".into(), "next".into()],
        smart: vec!["tool".into(), "next".into()],
        models: None,
    });
    let core = Core::new(
        steve_core::repo::Db::new(store.clone()),
        Arc::new(NoSessions),
        gateway.clone(),
        cfg,
    );
    core.seed().unwrap();

    let flow_setup = FlowSetup::new(Arc::new(FakeCore::default()));
    let disclosure = Disclosure::with_models(Vec::new());
    let link = OutLink::new();
    let mut registry = Registry::new();
    flow_setup.register(&mut registry);
    connections::register(&mut registry);
    registry.outbound_check(Box::new(disclosure.clone()));
    registry.decision(SUBJECT_MESSAGE, link.handler());
    let shared: Arc<dyn steve_modules::Gateway> =
        Arc::new(steve_core::SharedGateway(gateway.clone()));
    let modules = Modules::new(store.clone(), shared.clone(), registry);
    disclosure.bind(store.clone(), shared);
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        store.clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let outbox = Arc::new(Outbox::new(link));
    let replies = Arc::new(Replies::new().outbound(outbox.clone()));
    let flow = flow_setup
        .connections(conns.clone())
        .build(modules.clone(), Arc::new(LinearExecutor));
    let channels = Channels::builder(modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies)
        .flow(flow)
        .reactor(outbox.clone())
        .thread(Arc::new(CoreThread(core.clone())))
        .build();
    assert!(core.set_conv_host(Arc::new(ChanConvHost(channels.downgrade()))));

    // your own account, `/ai` on in direct messages
    let (me, _) = FakeChannel::seed_connection(&store, &caller(WS, ME), Some("upx")).unwrap();
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    channels
        .on_message(
            WS,
            &me,
            FakeChannel::message("D_LI", "d0", "ulilei", "在吗"),
        )
        .await
        .unwrap();
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
                rusqlite::params![
                    json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
                    me
                ],
            )?;
            Ok(())
        })
        .unwrap();
    // a reply drafted for someone else's message is no hand-off: its model reached for a tool,
    // the next model wrote it
    for _ in 0..500 {
        if models.answers().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(models.answers(), ["tool", "next"]);
    models.calls.lock().unwrap().clear();
    let items = || count(&store, "SELECT count(*) FROM items");
    let before = items();

    channels
        .on_message(
            WS,
            &me,
            FakeChannel::message("D_LI", "d1", "upx", "/ai 看下我现在磁盘情况"),
        )
        .await
        .unwrap();
    for _ in 0..500 {
        if items() > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // what else would follow (a second hand-off, a late answer) has had its time
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(items(), before + 1, "exactly one item");
    let answers = models.answers();
    assert_eq!(
        answers,
        ["tool"],
        "the chat answer is not tried on the next model: it is handed on"
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM route_log WHERE outcome = 'needs_tools' AND item_id IS NOT NULL"
        ),
        1
    );
    // the conversation of the chat says so, with the item
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM messages WHERE space LIKE 'chan:%' AND author = 'steve' AND item_id IS NOT NULL"
        ),
        1
    );
}
