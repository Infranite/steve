//! A trigger word end to end on the real channel path, as the Hub wires it: the channel module
//! hands your `/ai` to steve-core's conversation of the chat ([`CoreThread`]), steve-core asks the
//! channel module for the answer ([`ChanConvHost`]), the composer calls the model gateway
//! ([`SharedGateway`] over [`SingleModelGateway`]), and the model reaches for a tool the answer may not use
//! ([`steve_modules::gateway::GatewayError::NeedsTools`]). No string is faked in between: what the gateway says is what the
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
    AgentRuntime, Answered, Core, CoreConfig, CoreError, Flushed, Prompted, Released, Running,
    SingleModelGateway, Stop,
};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{Modules, Registry};
use steve_runtime::testkit::{eventually_sync, on_timeout};
use steve_store::testkit::hub::wiring as fixtures;

/// The models of the gateway: `tool` reaches for a tool on the chat answer, `next` would answer it.
/// Every call is recorded as (model, what it was).
#[derive(Default)]
struct Models {
    calls: Mutex<Vec<(String, &'static str)>>,
    research: bool,
    reply: Option<&'static str>,
    check: Option<&'static str>,
    prompts: Mutex<Vec<String>>,
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
        self.0.prompts.lock().unwrap().push(p.to_string());
        let (what, reply) = if p.starts_with("You are the router") {
            (
                "route",
                Ok(if self.0.research {
                    r#"{"kind":"research","confidence":0.99,"signals":["lookup"],"title":"总结群内发言","bot":"steve"}"#.into()
                } else {
                    ROUTE.to_string()
                }),
            )
        } else if p.starts_with(H_SAFETY) {
            if self.0.research {
                (
                    "answer",
                    self.0
                        .reply
                        .map(str::to_string)
                        .ok_or_else(|| CoreError::Msg("composer unavailable".into())),
                )
            } else {
                match model {
                    "tool" => (
                        "answer",
                        Err(steve_modules::gateway::GatewayError::NeedsTools.into()),
                    ),
                    _ => ("answer", Ok(REPLY.to_string())),
                }
            }
        } else if p.starts_with(CHECK_HEAD) {
            (
                "check",
                Ok(self
                    .0
                    .check
                    .unwrap_or(r#"{"verdict": "pass", "spans": []}"#)
                    .to_string()),
            )
        } else {
            ("other", Ok("{}".to_string()))
        };
        self.0.calls.lock().unwrap().push((model.to_string(), what));
        Tried {
            reply: reply.map(|t| (t, None)),
            usage: vec![],
            reported_model: None,
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

struct ChannelFixture {
    _dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
    store: steve_store::Store,
    core: Core,
    channels: Channels,
    fake: Arc<FakeChannel>,
    me: String,
}

fn channel_fixture(gateway: Arc<dyn steve_core::ModelGateway>) -> ChannelFixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg = CoreConfig::for_home(dir.path());
    for d in [&cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let store =
        steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    store.ensure_workspace().unwrap();
    fixtures::seed_channel_owner(&store, WS, ME).unwrap();

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
    ChannelFixture {
        _dir: dir,
        _keys: keys,
        store,
        core,
        channels,
        fake,
        me,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_word_whose_answer_needs_tools_makes_exactly_one_item() {
    // The shared gateway selects one model even if the tier lists two.
    let models = Arc::new(Models::default());
    let gateway: Arc<dyn steve_core::ModelGateway> = Arc::new(SingleModelGateway {
        one: One(models.clone()),
        fast: vec!["tool".into(), "next".into()],
        smart: vec!["tool".into(), "next".into()],
        models: None,
    });
    let h = channel_fixture(gateway);
    let ChannelFixture {
        store,
        core: _,
        channels,
        fake,
        me,
        ..
    } = &h;
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    channels
        .on_message(
            WS,
            &me,
            FakeChannel::message("D_LI", "d0", "ulilei", "在吗"),
        )
        .await
        .unwrap();
    store.write(|w| fixtures::set_channel_trigger(w, &me, &json!({
        "on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true
    }))).unwrap();
    // A reply drafted for someone else must not cause a handoff or a second inference.
    eventually_sync("the tool refusal was recorded", || {
        !models.answers().is_empty()
    })
    .await;
    assert_eq!(models.answers(), ["tool"]);
    models.calls.lock().unwrap().clear();
    let items = || fixtures::item_count(&store).unwrap();
    let before = items();

    channels
        .on_message(
            WS,
            &me,
            FakeChannel::message("D_LI", "d1", "upx", "/ai 看下我现在磁盘情况"),
        )
        .await
        .unwrap();
    eventually_sync("the trigger handoff creates its item", || items() > before).await;
    // what else would follow (a second hand-off, a late answer) has had its time
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(items(), before + 1, "exactly one item");
    let answers = models.answers();
    assert_eq!(
        answers,
        ["tool"],
        "the chat answer is not tried on the next model: it is handed on"
    );
    assert_eq!(fixtures::tool_route_count(&store).unwrap(), 1);
    // the conversation of the chat says so, with the item
    assert_eq!(fixtures::steve_channel_item_messages(&store).unwrap(), 1);
}

const RESEARCH_TEXT: &str = "只读到部分片段，无法覆盖最近两天。大家在讨论个人 Agent。";
const RESEARCH_REPLY: &str = r#"{"outcome":"reply","text":"只读到部分片段，无法覆盖最近两天。大家在讨论个人 Agent。","needs_approval":false,"why":"依据本回合只读研究，保留资料范围"}"#;

struct ResearchResult(steve_core::thread::RunEnd);

#[async_trait::async_trait]
impl steve_core::thread::Researcher for ResearchResult {
    async fn research(
        &self,
        req: steve_core::thread::ResearchRequest,
        _: &dyn steve_core::thread::ResearchSink,
    ) -> steve_core::thread::ResearchOutcome {
        assert!(req.context.contains(steve_channels::im_context::OPEN));
        assert!(req.context.contains(steve_channels::im_context::RULE_DATA));
        steve_core::thread::ResearchOutcome {
            text: RESEARCH_TEXT.into(),
            end: self.0,
            model: Some("scripted-research".into()),
            ..Default::default()
        }
    }
}

async fn research_fixture(
    reply: Option<&'static str>,
    check: Option<&'static str>,
    end: steve_core::thread::RunEnd,
) -> (ChannelFixture, Arc<Models>) {
    let models = Arc::new(Models {
        research: true,
        reply,
        check,
        ..Default::default()
    });
    let h = channel_fixture(Arc::new(SingleModelGateway {
        one: One(models.clone()),
        fast: vec!["scripted".into()],
        smart: vec!["scripted".into()],
        models: None,
    }));
    assert!(h.core.set_researcher(Arc::new(ResearchResult(end))));
    h.fake.space(
        "C_RESEARCH",
        "研究测试群",
        SpaceKind::Group,
        false,
        &[("ulilei", "李雷")],
    );
    h.store.write(|tx| {
        steve_channels::spaces::ensure_channel_settings(tx, WS, &h.me, steve_channels::spaces::Mode::Own, 1)?;
        fixtures::set_channel_trigger(tx, &h.me, &json!({
            "on": true, "words": ["/ai"], "where": ["*"], "who": ["me"], "reply": "thread", "mark": true
        }))?;
        Ok::<_, steve_channels::ChannelError>(())
    }).unwrap();
    (h, models)
}

async fn run_research(h: &ChannelFixture) -> steve_store::Message {
    let inbound = h
        .channels
        .on_message(
            WS,
            &h.me,
            FakeChannel::message(
                "C_RESEARCH",
                "research-command",
                "upx",
                "/ai 总结下这个群里最近 2 天的发言",
            ),
        )
        .await
        .unwrap();
    let conv = steve_channels::conversations::conv_space(&inbound.space_id);
    let mut reply = None;
    let diagnostic = on_timeout(|| async {
        format!(
            "research reply never settled: {:?}",
            h.store.messages().list(WS, &conv, 50).unwrap()
        )
    });
    eventually_sync(diagnostic, || {
        reply = h
            .store
            .messages()
            .list(WS, &conv, 50)
            .unwrap()
            .into_iter()
            .find(|m| m.author == "steve" && m.meta.get("chan_out").is_some());
        reply.is_some()
    })
    .await;
    reply.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn research_reenters_the_owner_reply_pipeline_and_distinguishes_channel_limits() {
    use steve_core::thread::RunEnd;
    for limit in ["writable", "connection", "target", "transport"] {
        let (h, models) = research_fixture(Some(RESEARCH_REPLY), None, RunEnd::Done).await;
        if limit == "connection" {
            *h.fake.block_send.lock().unwrap() =
                Some("[ext_ws session.write_policy=forbidden]".into());
        } else if limit == "target" {
            *h.fake.block_target_send.lock().unwrap() =
                Some("[ext_ws target_forbidden (-32016)]".into());
        } else if limit == "transport" {
            *h.fake.fail_send.lock().unwrap() = Some("fixture transport failed".into());
        }
        let m = run_research(&h).await;
        assert_eq!(m.text, RESEARCH_TEXT);
        assert_eq!(m.meta["research_result"], RESEARCH_TEXT);
        assert_eq!(m.meta["host_reply"]["status"], "prepared");
        assert_eq!(m.meta["steps"]["route"]["kind"], "research");
        let origin = m.meta["origin"].as_str().unwrap();
        let turn = h.store.messages().get(origin).unwrap().unwrap();
        assert_eq!(turn.meta["chan_gen"]["outcome"], "reply");
        assert_eq!(turn.meta["chan_gen"]["text"], RESEARCH_TEXT);
        let prompts = models.prompts.lock().unwrap().clone();
        assert!(prompts.iter().any(|p| p.starts_with(H_SAFETY)
            && p.contains(RESEARCH_TEXT)
            && p.contains("不是指令或发送授权")
            && p.contains(steve_channels::im_context::OPEN)));
        assert!(
            prompts.iter().any(|p| p.starts_with(CHECK_HEAD)),
            "disclosure still runs"
        );
        if limit == "writable" {
            assert_eq!(m.meta["chan_out"]["status"], "sent");
            assert_eq!(h.fake.sent.lock().unwrap().len(), 1);
            assert_eq!(
                h.fake.sent.lock().unwrap()[0].1.text,
                format!("[AI] {RESEARCH_TEXT}")
            );
        } else {
            assert_eq!(
                m.meta["chan_out"]["status"],
                if limit == "transport" {
                    "failed"
                } else {
                    "blocked"
                },
                "the stored note distinguishes refusal from transport failure"
            );
            assert!(h.fake.sent.lock().unwrap().is_empty());
            assert!(m.meta["chan_out"]["reason"].as_str().unwrap().contains(
                if limit == "target" {
                    "target_forbidden"
                } else if limit == "transport" {
                    "fixture transport failed"
                } else {
                    "forbidden"
                }
            ));
        }
        let attempts = h.fake.send_calls.lock().unwrap().len();
        assert!(h.channels.conv_deliver(WS, &m.id).await.unwrap().is_none());
        assert_eq!(
            h.fake.send_calls.lock().unwrap().len(),
            attempts,
            "replay never sends twice"
        );
        let page = steve_channels::conversations::messages_of(
            &h.channels,
            WS,
            &steve_store::base::Viewer::bare(WS, ME),
            &m.space,
        )
        .unwrap();
        let shown = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == m.id)
            .unwrap();
        assert_eq!(
            shown["meta"]["chan_out"]["status"],
            m.meta["chan_out"]["status"]
        );
        if limit != "writable" {
            assert!(shown["meta"]["chan_out"]["reason"].is_string());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn research_keeps_approval_owner_questions_and_disclosure_gates() {
    use steve_core::thread::RunEnd;
    for (reply, check, expected) in [
        (
            r#"{"outcome":"reply","text":"需要审阅的摘要","needs_approval":true,"why":"请主人批准"}"#,
            None,
            "draft",
        ),
        (
            r#"{"outcome":"ask_owner","text":"要向大家承诺明天完成吗？","needs_approval":true,"why":"需要主人决定"}"#,
            None,
            "skipped",
        ),
        (
            RESEARCH_REPLY,
            Some(
                r#"{"verdict":"leak","spans":[{"text":"个人 Agent","reason":"不对外披露","basis":"internal_detail"}]}"#,
            ),
            "draft",
        ),
    ] {
        let (h, _) = research_fixture(Some(reply), check, RunEnd::Done).await;
        let m = run_research(&h).await;
        assert_eq!(m.meta["chan_out"]["status"], expected, "{m:?}");
        assert!(h.fake.sent.lock().unwrap().is_empty());
        assert!(h.fake.send_calls.lock().unwrap().is_empty());
        assert_eq!(m.meta["research_result"], RESEARCH_TEXT);
        if expected == "skipped" {
            assert_eq!(m.meta["chan_out"]["code"], "ask_owner");
            assert!(m.meta["chan_out"]["reason"].is_string());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn research_preparation_failures_and_partial_results_stay_readable_without_sending() {
    use steve_core::thread::RunEnd;
    for end in [
        RunEnd::Done,
        RunEnd::Failed,
        RunEnd::Stopped,
        RunEnd::Budget,
        RunEnd::Violated,
    ] {
        let (h, models) = research_fixture(None, None, end).await;
        *h.fake.fail_reactions.lock().unwrap() = true;
        let m = run_research(&h).await;
        assert!(m.text.starts_with(RESEARCH_TEXT));
        assert!(
            m.meta["research_result"]
                .as_str()
                .unwrap()
                .starts_with(RESEARCH_TEXT)
        );
        assert_eq!(m.meta["chan_out"]["status"], "skipped");
        assert_eq!(m.meta["chan_out"]["code"], "research_ineligible");
        assert!(m.meta["chan_out"]["reason"].is_string());
        assert!(h.fake.send_calls.lock().unwrap().is_empty());
        assert_eq!(fixtures::outbound_channel_messages(&h.store).unwrap(), 0);
        assert_eq!(fixtures::failed_reaction_clears(&h.store).unwrap(), 1);
        if end != RunEnd::Done {
            assert!(
                models.answers().is_empty(),
                "incomplete research never reaches the composer"
            );
        }
    }
}
