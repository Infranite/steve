//! SG35: compact pure-rendering and SQLite/port contracts on the original implementation.
#[path = "sg35/mod.rs"]
mod f;
use f::{H, History, Spy};
use std::sync::{Arc, atomic::Ordering};
use steve_channels::deliver::{Auth, Purpose};
use steve_channels::{
    Replies,
    channel::ChannelAdapter,
    compose::{self, BotInfo, ComposeOpts, Layers},
    ext::{ChannelDecl, ChannelSide, MessageIds, Receive},
    fake::channel::FakeChannel,
    history::Source,
    im_context::{self, AccountKind, Disclosure, Mark, Named, TriggerFacts},
    respond::Responder,
    rules::{Act, ItemMode},
    spaces,
};
use steve_modules::testkit::{ME, WS};
use steve_store::base::Viewer;

fn message_snapshot(h: &H, space: &str) -> serde_json::Value {
    serde_json::to_value(
        h.env
            .store()
            .reader(|c| steve_channels::messages::page(c, space, ME, None, 100))
            .unwrap(),
    )
    .unwrap()
}

fn side() -> ChannelSide {
    let a = Arc::new(FakeChannel::new());
    ChannelSide::new(
        ChannelDecl::new(FakeChannel::ID)
            .handle_system(FakeChannel::SYSTEM)
            .features(a.features()),
        a.clone(),
    )
    .adapter(a)
}

#[test]
fn sg35_channel_declaration_validation_preserves_boundaries_and_error_precedence() {
    assert_eq!(side().check(), Ok(()));
    for (precision, ok) in [(0, false), (1, true), (60_000, true), (60_001, false)] {
        let mut s = side();
        s.decl.created_precision_ms = precision;
        assert_eq!(s.check().is_ok(), ok);
    }
    let mut s = side();
    s.decl.adapter.clear();
    assert_eq!(
        s.check().unwrap_err(),
        "a channel side without an adapter id"
    );
    let mut s = side();
    s.decl.adapter = "other".into();
    assert!(
        s.check()
            .unwrap_err()
            .starts_with("provider fake_chan declared for adapter other")
    );
    let mut s = side();
    s.decl.handle_system.clear();
    assert!(s.check().unwrap_err().contains("no handle system"));
    let mut s = side();
    s.decl.mode = spaces::Mode::Own;
    assert!(
        s.check()
            .unwrap_err()
            .contains("without an act-as-user view")
    );
    let mut s = side();
    s.decl.receive = Receive::Poll;
    s.decl.receive_cap = Some("absent-cap".into());
    assert!(
        s.check()
            .unwrap_err()
            .contains("receive capability absent-cap")
    );
    let mut s = side();
    s.decl.message_ids = MessageIds::None;
    assert!(
        s.check()
            .unwrap_err()
            .contains("does not preserve source ids")
    );
    let mut history = History::default();
    history.id = "different".into();
    assert!(
        side()
            .history(Arc::new(history))
            .check()
            .unwrap_err()
            .contains("history view different")
    );
}

#[test]
fn sg35_prompt_keeps_order_authority_data_escaping_and_own_instruction_rules() {
    let mut l = Layers {
        im_context: " <identity/> ".into(),
        bot: Some(BotInfo {
            id: "b".into(),
            name: "B".into(),
            role: "role".into(),
            persona: "tone".into(),
            rules: "rule".into(),
            prompt: String::new(),
            model: None,
        }),
        persona: "persona".into(),
        account_duty: "account duty".into(),
        space_duty: "space duty".into(),
        anchors: "anchors".into(),
        occasion: "occasion".into(),
        owner_block: "owner".into(),
        message: "message".into(),
        sender: "sender".into(),
        pending: vec!["pending".into()],
        research: Some("data\n\"quoted\"".into()),
        revise: Some("revise".into()),
        now: "fixed time".into(),
        ..Default::default()
    };
    let p = compose::prompt(&l);
    let heads = [
        compose::H_BOT,
        compose::H_PERSONA,
        compose::H_ACCOUNT,
        compose::H_SPACE,
        compose::H_ANCHORS,
        compose::H_KNOWLEDGE,
        compose::H_HISTORY,
        compose::H_MESSAGE,
        compose::H_PENDING,
    ];
    let at = heads.map(|s| p.find(s).unwrap());
    assert!(at.windows(2).all(|x| x[0] < x[1]));
    assert!(p.starts_with(compose::SAFETY_RULES));
    assert!(p.contains(&serde_json::to_string(l.research.as_ref().unwrap()).unwrap()));
    assert!(p.ends_with(compose::OUTPUT_BOT));
    l.own = true;
    let p = compose::prompt(&l);
    assert!(p.contains(compose::H_DECIDE));
    assert!(p.ends_with(compose::OUTPUT));
    l.instruction = Some("explicit task".into());
    let p = compose::prompt(&l);
    assert!(!p.contains(compose::H_DECIDE));
    assert!(p.contains(compose::H_TASK));
    l.bot.as_mut().unwrap().prompt = "custom bot instructions".into();
    let p = compose::prompt(&l);
    assert!(p.contains("custom bot instructions"));
    assert!(!p.contains("你代表 Bot「B」说话"));
}

#[tokio::test]
async fn sg35_gather_preserves_identity_anchors_conversation_and_persona_validation() {
    let h = H::new();
    h.plan("room", "old", "previous message").await;
    let plan = h.plan("room", "main", "current message").await;
    let kb = steve_knowledge::Knowledge::bare();
    let layers = h
        .env
        .store()
        .reader(|c| compose::gather(c, kb.store(), &plan, &ComposeOpts::default(), h.ch.now()))
        .unwrap();
    assert_eq!(layers.message, "current message");
    assert!(!layers.anchors.is_empty());
    assert!(!layers.history.is_empty());
    assert_eq!(layers.model.as_deref(), Some("model-sg35"));
    assert!(layers.bot.is_some());
    assert!(!layers.own);
    let prompt = compose::prompt(&layers);
    assert!(prompt.contains("previous message") && prompt.contains("current message"));
    let missing = ComposeOpts {
        persona_id: Some("missing-persona".into()),
        ..Default::default()
    };
    assert_eq!(
        h.env
            .store()
            .reader(|c| compose::gather(c, kb.store(), &plan, &missing, h.ch.now()))
            .unwrap_err()
            .status(),
        404
    );
}

#[tokio::test]
async fn sg35_im_identity_build_and_xml_render_keep_audience_bounds_and_data_rules() {
    let h = H::new();
    let plan = h.plan("room", "message", "hello").await;
    let mut x = h
        .env
        .store()
        .reader(|c| im_context::build(c, &plan))
        .unwrap();
    assert_eq!(x.account, AccountKind::Bot);
    assert_eq!(x.message, plan.message_id);
    assert_eq!(x.chat_id, plan.space_id);
    assert!(x.typed_by_human);
    x.is_owner = false;
    x.chat_name = im_context::CLOSE.into();
    x.sender_name = "<sender>".into();
    x.members = 0;
    x.external = (0..im_context::POLICY.external_named + 2)
        .map(|i| Named {
            name: format!("name{i}"),
            org: Some("<org>".into()),
        })
        .collect();
    x.has_external = true;
    let rendered = x.render();
    assert_eq!(rendered.matches(im_context::CLOSE).count(), 1);
    assert!(rendered.contains("&lt;sender&gt;"));
    assert!(rendered.contains("另有 2 位"));
    assert!(rendered.contains(im_context::RULE_NOT_OWNER));
    assert!(rendered.contains("members=\"未知\""));
    x.account = AccountKind::Own;
    x.owner = "Alice".into();
    x.is_owner = true;
    x.disclosure = Disclosure::Private;
    x.mark = Mark::Off;
    x.trigger = Some(TriggerFacts {
        word: "/ask".into(),
        instruction: "<instruction>".into(),
        source_message: "m".into(),
    });
    let rendered = x.render();
    assert!(rendered.contains("以Alice本人账号发出"));
    assert!(rendered.contains("&lt;instruction&gt;"));
    assert!(!rendered.contains(im_context::RULE_NOT_OWNER));
    assert!(rendered.contains(im_context::RULE_DATA));
}

#[tokio::test]
async fn sg35_history_remote_is_sorted_deduped_clamped_and_never_stored_or_identified() {
    let h = H::new();
    let plan = h.plan("room", "stored", "stored message").await;
    let at = h.ch.now();
    let from = at - 10;
    let to = at + 10;
    let make = |id: &str, t: i64, sender: &str| {
        let mut m = FakeChannel::message("room", id, sender, id);
        m.created = t;
        m
    };
    let mut bot = make("bot", at + 1, FakeChannel::BOT);
    bot.sender.is_bot = true;
    *h.history.messages.lock().unwrap() = vec![
        make("later", at + 8, "remote"),
        make("a", at, "remote"),
        make("a", at + 2, "remote"),
        bot,
        make("", at + 3, "remote"),
        make("", at + 4, "remote"),
        make("too old", from - 1, "remote"),
    ];
    let before = (
        message_snapshot(&h, &plan.space_id),
        h.n("SELECT count(*) FROM accounts", &[]),
    );
    let page =
        h.ch.read_history(&Viewer::bare(WS, ME), &plan.space_id, from, to, 2)
            .await
            .unwrap();
    assert_eq!(page.source, Source::Remote);
    assert!(page.more);
    assert_eq!(page.lines.len(), 2);
    assert_eq!(page.lines[0].text.as_deref(), Some("a"));
    assert!(page.lines[1].out);
    assert_eq!(
        h.history.requests.lock().unwrap()[0],
        ("room".into(), from, to, 3)
    );
    assert_eq!(
        before,
        (
            message_snapshot(&h, &plan.space_id),
            h.n("SELECT count(*) FROM accounts", &[])
        )
    );
}

#[tokio::test]
async fn sg35_history_falls_back_and_refuses_bad_ranges_or_never_before_remote_access() {
    let h = H::new();
    let p = h.plan("room", "message", "stored body").await;
    let at = h.ch.now();
    h.history.fail.store(true, Ordering::SeqCst);
    let got =
        h.ch.read_history(&Viewer::bare(WS, ME), &p.space_id, at - 1, at + 1, 0)
            .await
            .unwrap();
    assert_eq!(got.source, Source::Stored);
    assert_eq!(got.lines.len(), 1);
    assert_eq!(got.lines[0].text.as_deref(), Some("stored body"));
    let calls = h.history.requests.lock().unwrap().len();
    assert!(
        h.ch.read_history(&Viewer::bare(WS, ME), &p.space_id, at + 1, at, 1)
            .await
            .is_err()
    );
    assert!(
        h.ch.read_history(
            &Viewer::bare(WS, ME),
            &p.space_id,
            0,
            steve_channels::history::MAX_RANGE_MS + 1,
            1
        )
        .await
        .is_err()
    );
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE space_settings SET disclosure='never' WHERE space_id=?",
                [&p.space_id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        h.ch.read_history(&Viewer::bare(WS, ME), &p.space_id, at - 1, at + 1, 1)
            .await
            .is_err()
    );
    assert_eq!(h.history.requests.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn sg35_name_lookup_records_deduped_permissions_and_connection_failures_without_network() {
    for failed in [false, true] {
        let h = H::new();
        h.plan("room", "message", "hello").await;
        let scope = steve_channels::adapters::feishu::profiles::CONTACT_SCOPE;
        h.profiles.answer.lock().unwrap().missing_scopes = vec![
            (scope.into(), "directory".into()),
            (scope.into(), "again".into()),
        ];
        h.profiles.answer.lock().unwrap().granted_scopes = vec!["sg35-granted".into()];
        h.profiles.fail.store(failed, Ordering::SeqCst);
        h.env
            .store()
            .transaction(|tx| {
                steve_modules::accounts::connections::repo::note_permissions(
                    tx,
                    &h.conn,
                    &[("sg35-granted", "previous refusal")],
                    &[],
                    h.ch.now(),
                )
            })
            .unwrap();
        h.ch.kick_names(WS, true);
        let report = h.ch.resolve_names(WS).await.unwrap();
        let calls = h.profiles.calls.load(Ordering::SeqCst);
        assert!(calls > 0);
        assert_eq!(report.lookups, calls);
        if failed {
            assert_eq!(report.failed, calls);
            assert_eq!(report.failed_conns, [h.conn.clone()]);
        } else {
            assert_eq!(report.failed, 0);
            assert_eq!(
                report
                    .missing
                    .iter()
                    .filter(|(id, s)| id == &h.conn && s == scope)
                    .count(),
                1
            );
            let config: String = h
                .env
                .store()
                .reader(|c| {
                    Ok::<_, steve_store::StoreError>(c.query_row(
                        "SELECT config FROM connections WHERE id=?",
                        [&h.conn],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert!(config.contains(scope));
            let config: serde_json::Value = serde_json::from_str(&config).unwrap();
            assert_eq!(
                config["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["scope"] == "sg35-granted")
                    .unwrap()["status"],
                "granted"
            );
        }
    }
}

#[tokio::test]
async fn sg35_seen_merges_mentions_clips_snippets_and_checks_quiet_before_truncating_spaces() {
    let h = H::new();
    let mut plans = Vec::new();
    for i in 0..6 {
        plans.push(
            h.plan(&format!("room{i}"), &format!("m{i}"), &"字".repeat(100))
                .await,
        );
    }
    let party = plans[0].sender_party_id.clone().unwrap();
    h.fake.space(
        "mention",
        "Mention",
        steve_channels::SpaceKind::Group,
        false,
        &[],
    );
    h.env.clock.advance(1_000);
    let mentioned_at = h.ch.now();
    let mut m = FakeChannel::message("mention", "mention-message", "uother", "hello guest");
    m.created = mentioned_at;
    m.mentions.push(steve_channels::Mention {
        handle: Some(FakeChannel::handle("uguest")),
        name: "Guest".into(),
        is_connection: false,
    });
    let mention = h.ch.on_message(WS, &h.conn, m).await.unwrap();
    h.env
        .store()
        .transaction(|tx| {
            for p in &plans[1..] {
                spaces::write_attention(tx, WS, &p.space_id, spaces::Attention::Quiet, h.ch.now())?;
            }
            spaces::write_attention(
                tx,
                WS,
                &mention.space_id,
                spaces::Attention::Quiet,
                h.ch.now(),
            )
        })
        .unwrap();
    let got = h
        .env
        .store()
        .reader(|c| {
            steve_channels::seen::seen(
                c,
                &Viewer::bare(WS, ME),
                &[party.clone(), "not-seen".into()],
            )
        })
        .unwrap();
    assert_eq!(got.len(), 1);
    let s = &got[0].1;
    assert_eq!(s.spaces.len(), 5);
    assert_eq!(s.spaces[0].id, mention.space_id);
    assert_eq!(s.last_at, Some(mentioned_at));
    assert!(!s.quiet);
    assert_eq!(s.messages, 6);
    assert_eq!(
        s.last.as_ref().unwrap().text,
        format!("{}…", "字".repeat(80))
    );
    h.env
        .store()
        .transaction(|tx| {
            spaces::write_attention(
                tx,
                WS,
                &plans[0].space_id,
                spaces::Attention::Quiet,
                h.ch.now(),
            )
        })
        .unwrap();
    let got = h
        .env
        .store()
        .reader(|c| steve_channels::seen::seen(c, &Viewer::bare(WS, ME), &[party]))
        .unwrap();
    assert!(got[0].1.quiet);
}

#[tokio::test]
async fn sg35_reply_producer_keeps_approval_authorization_and_failed_generation_fallbacks() {
    let h = H::new();
    let mut plan = h.plan("room", "message", "hello").await;
    let spy = Arc::new(Spy::default());
    let replies = Replies::new().outbound(spy.clone());
    for (act, approve) in [(Act::Reply, false), (Act::Draft, true)] {
        plan.act = act;
        h.env.gateway.reply(
            r#"{"outcome":"reply","text":"scripted reply","needs_approval":false,"why":"answer"}"#,
        );
        let acted = replies.act(&h.ch, &plan).await.unwrap().unwrap();
        assert_eq!(acted.out_message_id.as_deref(), Some("sg35-out"));
        let calls = spy.calls.lock().unwrap();
        let req = calls.last().unwrap();
        assert_eq!(req.text, "scripted reply");
        assert_eq!(req.approve, approve);
        assert_eq!(req.purpose, Purpose::Reply);
        assert_eq!(matches!(req.auth, Auth::Rule { .. }), act == Act::Reply);
    }
    let before = spy.calls.lock().unwrap().len();
    h.env.gateway.fail("model refused");
    assert!(replies.act(&h.ch, &plan).await.unwrap().is_none());
    assert_eq!(spy.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn sg35_item_producer_keeps_request_link_transaction_and_acknowledgement_refusal_semantics() {
    let h = H::new();
    let mut p = h.plan("room", "message", "  Build   this feature  ").await;
    let spy = Arc::new(Spy::default());
    let replies = Replies::new().outbound(spy.clone());
    for mode in [ItemMode::Start, ItemMode::Propose] {
        p.act = Act::CreateItem(mode);
        spy.fail.store(mode == ItemMode::Propose, Ordering::SeqCst);
        let acted = replies.act(&h.ch, &p).await.unwrap().unwrap();
        let item = acted.item_id.unwrap();
        let created = h.core.created();
        let req = created.last().unwrap();
        assert_eq!(req.title, "Build this feature");
        assert_eq!(req.propose, mode == ItemMode::Propose);
        assert_eq!(req.bot, "steve");
        assert!(req.goal.contains("Build   this feature"));
        let source = h
            .env
            .store()
            .reader(|c| steve_channels::messages::get(c, &p.message_id))
            .unwrap()
            .unwrap();
        assert_eq!(source.work_item_id.as_deref(), Some(item.as_str()));
        assert_eq!(h.n("SELECT count(*) FROM links WHERE subject_type='item' AND subject_id=? AND predicate='from'",&[&item]),1);
    }
    let before = spy.calls.lock().unwrap().len();
    spy.fail.store(false, Ordering::SeqCst);
    let acted = replies.to_item(&h.ch, &p).await.unwrap().unwrap();
    assert!(acted.label.contains("没有发送回复"));
    assert!(acted.item_id.is_some());
    assert_eq!(spy.calls.lock().unwrap().len(), before);
}
