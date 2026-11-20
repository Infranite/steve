use crate::channel::{MessageRef, ReactionKind};
use crate::conversations::{Thread, Turn};
use crate::fake::channel::FakeChannel;
use crate::reactions::{Added, Reactor};
use crate::respond::{Acted, Plan, Responder};
use crate::rules::Act;
use crate::{ChannelResult, Channels, SpaceKind, messages, spaces};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::contacts::repo::{self, NewParty};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};

#[path = "../support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;

#[derive(Default)]
pub struct Script {
    pub calls: Mutex<Vec<String>>,
    pub acted: Mutex<Option<Acted>>,
    pub act_error: Mutex<Option<String>>,
    pub ack_error: Mutex<bool>,
    pub add_error: Mutex<Option<String>>,
    pub remove_error: Mutex<Option<String>>,
    pub hang: Mutex<bool>,
    pub remove_hang: Mutex<bool>,
    pub sink: Mutex<Option<&'static str>>,
    pub deliveries: Mutex<Vec<crate::deliver::OutRequest>>,
    pub pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    pub thread_error: Mutex<Option<String>>,
    pub turns: Mutex<Vec<Turn>>,
}
#[async_trait]
impl Responder for Script {
    async fn act(&self, ch: &Channels, p: &Plan) -> ChannelResult<Option<Acted>> {
        let state = ch
            .store()
            .reader(|c| -> ChannelResult<_> { Ok(crate::reactions::get(c, &p.message_id)?) })?
            .map(|r| r.state)
            .unwrap_or_default();
        self.calls.lock().unwrap().push(format!("act:{state}"));
        if let Some(e) = self.act_error.lock().unwrap().clone() {
            return Err(steve_modules::ApiError::Internal(e));
        }
        Ok(self.acted.lock().unwrap().clone())
    }
    fn outbound(&self) -> Option<&dyn crate::deliver::Outbound> {
        self.sink.lock().unwrap().is_some().then_some(self)
    }
    async fn acknowledge(&self, ch: &Channels, p: &Plan, text: &str) -> ChannelResult<bool> {
        let count: i64 = ch.store().reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT count(*) FROM decisions WHERE subject_id=?",
                [&p.message_id],
                |r| r.get(0),
            )?)
        })?;
        self.calls
            .lock()
            .unwrap()
            .push(format!("handoff:{count}:{text}"));
        if *self.ack_error.lock().unwrap() {
            return Err(steve_modules::ApiError::Internal(
                "scripted ack refusal".into(),
            ));
        }
        Ok(true)
    }
}
#[async_trait]
impl Reactor for Script {
    async fn add(
        &self,
        _: &Channels,
        _: &str,
        _: &str,
        msg: &MessageRef,
        kind: ReactionKind,
    ) -> Result<Added, String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("add:{kind:?}:{}", msg.id().unwrap()));
        let pause = self.pause.lock().unwrap().take();
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
        let hang = *self.hang.lock().unwrap();
        if hang {
            std::future::pending::<()>().await;
        }
        if let Some(e) = self.add_error.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(Added {
            reaction_id: format!("reaction-{kind:?}"),
            emoji: format!("emoji-{kind:?}"),
            as_user: true,
        })
    }
    async fn remove(
        &self,
        _: &Channels,
        _: &str,
        _: &str,
        msg: &MessageRef,
        id: &str,
        as_user: bool,
    ) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("remove:{id}:{as_user}:{}", msg.id().unwrap()));
        let hang = *self.remove_hang.lock().unwrap();
        if hang {
            std::future::pending::<()>().await;
        }
        match self.remove_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
#[async_trait]
impl crate::deliver::Outbound for Script {
    async fn deliver(
        &self,
        ch: &Channels,
        req: &crate::deliver::OutRequest,
    ) -> ChannelResult<crate::deliver::Delivered> {
        self.deliveries.lock().unwrap().push(req.clone());
        let mode = *self.sink.lock().unwrap();
        match mode {
            Some("refuse") => Err(steve_modules::ApiError::Internal(
                "scripted outbound refusal".into(),
            )),
            Some("mark-refuse") => {
                ch.store().transaction(|tx| -> ChannelResult<()> {
                    tx.execute_batch("CREATE TRIGGER sg19_refuse_mark BEFORE UPDATE ON messages BEGIN SELECT RAISE(FAIL,'sg19 mark refusal'); END;")?;
                    Ok(())
                })?;
                Ok(crate::deliver::Delivered::Skipped)
            }
            _ => Ok(crate::deliver::Delivered::Skipped),
        }
    }
}
impl Thread for Script {
    fn say(&self, _: &str, turn: Turn) -> Result<String, String> {
        self.calls.lock().unwrap().push("thread".into());
        self.turns.lock().unwrap().push(turn);
        match self.thread_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok("scripted-turn".into()),
        }
    }
}

pub struct Fixture {
    pub env: TestEnv,
    pub ch: Channels,
    pub script: Arc<Script>,
    pub conn: String,
    pub account: String,
    pub guest: String,
    pub guest_account: String,
    pub space: String,
    pub mode: spaces::Mode,
    _keys: tempfile::TempDir,
}
impl Fixture {
    pub fn new(own: bool, thread: bool) -> Self {
        let setup = FlowSetup::new(Arc::new(FakeCore::default()));
        let env = TestEnv::new(|r| {
            setup.register(r);
            connections::register(r);
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
            ConnectionsConfig::new(Vec::new(), true, |_| None),
        )
        .provider(fake.clone())
        .build();
        let flow = setup
            .connections(conns.clone())
            .build(env.modules.clone(), Arc::new(LinearExecutor));
        let script = Arc::new(Script::default());
        let mut builder = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
            .adapter(fake)
            .responder(script.clone())
            .reactor(script.clone());
        if thread {
            builder = builder.thread(script.clone());
        }
        let ch = builder.build();
        let (conn, account) =
            FakeChannel::seed_connection(env.store(), &caller(WS, ME), own.then_some("sg19-owner"))
                .unwrap();
        let guest = env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(repo::insert(tx, WS, &NewParty::new("person", "Guest", ME))?)
            })
            .unwrap()
            .id;
        let guest_account = env
            .store()
            .transaction(|tx| {
                steve_modules::accounts::repo::insert(
                    tx,
                    WS,
                    &steve_modules::accounts::repo::NewAccount {
                        system_id: FakeChannel::SYSTEM.into(),
                        family: "slack_user".into(),
                        party_id: guest.clone(),
                        kind: "personal".into(),
                        handle: "sg19-guest".into(),
                        handle_display: "sg19-guest".into(),
                        display_name: "Guest".into(),
                        verified: true,
                        confidence: 1.0,
                        source: "connect".into(),
                        source_label: String::new(),
                    },
                )
            })
            .unwrap()
            .id;
        let space = env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(spaces::locate_or_create(
                    tx,
                    &spaces::NewSpace {
                        ws: WS,
                        system_id: FakeChannel::SYSTEM,
                        connection_id: &conn,
                        kind: SpaceKind::Dm,
                        external_id: "sg19-chat",
                        name: "SG19",
                        member_count: 2,
                        has_external: false,
                        now: ch.now(),
                        platform: None,
                    },
                )?)
            })
            .unwrap()
            .0
            .id;
        Self {
            env,
            ch,
            script,
            conn,
            account,
            guest,
            guest_account,
            space,
            mode: if own {
                spaces::Mode::Own
            } else {
                spaces::Mode::Bot
            },
            _keys: keys,
        }
    }
    pub fn plan(&self, act: Act, text: &str) -> Plan {
        self.env.clock.advance(1_000);
        let id = self.inbound(text, None, Some(&self.guest));
        Plan {
            ws: WS.into(),
            connection_id: self.conn.clone(),
            mode: self.mode,
            space_id: self.space.clone(),
            space_name: "SG19".into(),
            message_id: id.clone(),
            external_id: self.provider_id(&id),
            thread_key: None,
            text: text.into(),
            sender_account_id: Some(self.guest_account.clone()),
            sender_party_id: Some(self.guest.clone()),
            sender_name: "Guest".into(),
            act,
            rule: Some(2),
            muted: false,
            trigger: None,
        }
    }
    pub fn inbound(&self, text: &str, account: Option<&str>, party: Option<&str>) -> String {
        let ext = steve_store::uid("external");
        self.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(messages::insert_inbound(
                    tx,
                    &messages::NewInbound {
                        ws: WS,
                        space_id: &self.space,
                        external_id: &ext,
                        thread_key: None,
                        reply_to: None,
                        sender_account_id: account.or(Some(&self.guest_account)),
                        sender_party_id: party,
                        text,
                        attachments: &json!([]),
                        mentions: &json!([]),
                        created: self.ch.now(),
                        content_key: None,
                        fallback_from: None,
                        history: None,
                        seen_by: 1,
                    },
                )?)
            })
            .unwrap()
            .unwrap()
    }
    pub fn grant(&self, yes: bool) {
        self.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(tx.execute(
                    "UPDATE connections SET caps=? WHERE id=?",
                    rusqlite::params![
                        if yes {
                            r#"["msg.receive","msg.send","msg.send_as_user"]"#
                        } else {
                            r#"["msg.receive"]"#
                        },
                        self.conn
                    ],
                )?)
            })
            .unwrap();
    }
    pub fn provider_id(&self, id: &str) -> String {
        self.env
            .store()
            .reader(|c| messages::message_ref(c, id, &self.conn))
            .unwrap()
            .unwrap()
            .id()
            .unwrap()
            .to_string()
    }
    pub fn row(&self, id: &str) -> Option<crate::reactions::ReactionRow> {
        self.env
            .store()
            .reader(|c| -> ChannelResult<_> { Ok(crate::reactions::get(c, id)?) })
            .unwrap()
    }
    pub fn calls(&self) -> Vec<String> {
        self.script.calls.lock().unwrap().clone()
    }
    pub fn clear_calls(&self) {
        self.script.calls.lock().unwrap().clear();
    }
    pub fn seed_reaction(&self, id: &str, state: &str, rid: Option<&str>, error: Option<&str>) {
        self.env.store().transaction(|tx| -> ChannelResult<_> {
            Ok(tx.execute(
                "INSERT INTO chan_reactions(message_id,workspace_id,state,reaction_id,emoji,as_user,error,updated)
                 VALUES(?,?,?,?,?,?,?,17)",
                rusqlite::params![id, WS, state, rid, rid.map(|_| "old-emoji"), rid.is_some(), error],
            )?)
        }).unwrap();
    }
    pub fn attach_event(&self, id: &str) -> String {
        let event = steve_store::uid("event");
        self.env.store().transaction(|tx| -> ChannelResult<()> {
            tx.execute(
                "INSERT INTO ext_events(id,workspace_id,source,type,dedupe_key,occurred,received,summary,outcome)
                 VALUES(?,?,'im','im.message',?,1,1,'SG19','recorded')",
                rusqlite::params![event, WS, event],
            )?;
            messages::set_event(tx, id, &event)?;
            Ok(())
        }).unwrap();
        event
    }
    pub fn event(&self, id: &str) -> (String, String, String, Option<String>, Option<String>) {
        self.env.store().reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT outcome,outcome_label,reason,decision_id,work_item_id FROM ext_events WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?)
        }).unwrap()
    }
}
