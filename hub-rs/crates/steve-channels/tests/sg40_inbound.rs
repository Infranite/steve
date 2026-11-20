//! SG40: public admission, transaction and routing contracts before extraction.
mod sg40;
use serde_json::json;
use sg40::H;
use steve_channels::fake::channel::FakeChannel;
use steve_channels::{Next, SpaceKind, inbound, rules, spaces};
use steve_modules::testkit::{ME, WS};
use steve_store::base::Viewer;

#[tokio::test]
async fn sg40_validation_and_blocking_stop_before_lookup_or_identity_writes() {
    let h = H::new();
    for (space, id) in [("", "x"), ("group", ""), ("group", "fp:reserved")] {
        assert!(
            h.ch.on_inbound(WS, &h.conn, h.msg(space, id))
                .await
                .is_err()
        );
    }
    assert_eq!(*h.fake.info_calls.lock().unwrap(), 0);
    let first = h.inbound(h.msg("group", "first")).await;
    h.attention(&first.space_id, spaces::Attention::Blocked);
    let before = (
        h.n("SELECT count(*) FROM accounts"),
        *h.fake.info_calls.lock().unwrap(),
        *h.fake.member_calls.lock().unwrap(),
        h.page(&first.space_id),
    );
    let mut m = h.msg("group", "blocked");
    m.sender.handle = FakeChannel::handle("unew");
    let blocked = h.inbound(m).await;
    assert!(blocked.blocked && blocked.duplicate && blocked.message_id.is_empty());
    assert!(blocked.event.is_none() && blocked.facts.is_none());
    assert_eq!(blocked.next, Next::Record(inbound::LABEL_BLOCKED));
    assert_eq!(
        (
            h.n("SELECT count(*) FROM accounts"),
            *h.fake.info_calls.lock().unwrap(),
            *h.fake.member_calls.lock().unwrap(),
            h.page(&first.space_id)
        ),
        before
    );
}

#[tokio::test]
async fn sg40_metadata_fallback_dm_names_and_member_refresh_keep_transaction_boundaries() {
    let h = H::new();
    *h.fake.fail_info.lock().unwrap() = true;
    let mut m = h.msg("unknown", "fallback");
    m.space_kind = Some(SpaceKind::Dm);
    m.space_name = Some(String::new());
    m.sender.display_name = Some("Payload name".into());
    let r = h.inbound(m).await;
    let space = h
        .env
        .store()
        .reader(|c| spaces::get(c, &r.space_id))
        .unwrap()
        .unwrap();
    assert_eq!(space.name, "Payload name");
    assert_eq!(space.member_count, 0);
    assert!(r.sender_party_id.is_some() && r.event.is_some());
    *h.fake.fail_info.lock().unwrap() = false;
    h.fake.space(
        "unknown",
        "remote DM",
        SpaceKind::Dm,
        false,
        &[("ualice", "Alice")],
    );
    let before = *h.fake.info_calls.lock().unwrap();
    h.inbound(h.msg("unknown", "again")).await;
    assert_eq!(
        *h.fake.info_calls.lock().unwrap(),
        before,
        "known DMs are not refreshed"
    );
    *h.fake.fail_info.lock().unwrap() = true;
    let r = h.inbound(h.msg("unlisted-group", "group-first")).await;
    *h.fake.fail_info.lock().unwrap() = false;
    h.fake.space(
        "unlisted-group",
        "Remote group",
        SpaceKind::Group,
        true,
        &[("ualice", "Alice"), ("ubob", "Bob")],
    );
    h.inbound(h.msg("unlisted-group", "group-next")).await;
    let space = h
        .env
        .store()
        .reader(|c| spaces::get(c, &r.space_id))
        .unwrap()
        .unwrap();
    assert_eq!(space.member_count, 2);
    assert!(space.has_external);
}

#[tokio::test]
async fn sg40_redelivery_completes_threads_once_and_preserves_the_first_event_and_text() {
    let h = H::new();
    let mut m = h.msg("dm", "delivery");
    m.source_id = Some("source-delivery".into());
    let r = h.inbound(m.clone()).await;
    let before = h.row(&r.message_id);
    m.text = "redelivery must not replace text".into();
    m.thread_key = Some("thread".into());
    m.reply_to = Some("parent".into());
    let repeat = h.inbound(m.clone()).await;
    assert!(repeat.duplicate && repeat.event.is_none());
    assert_eq!(repeat.message_id, r.message_id);
    let row = h.row(&r.message_id);
    assert_eq!(row.text, before.text);
    assert_eq!(row.event_id, before.event_id);
    assert_eq!(row.thread_key.as_deref(), Some("thread"));
    assert_eq!(row.reply_to.as_deref(), Some("parent"));
    m.external_id = "upgraded-id".into();
    let source_copy = h.inbound(m).await;
    assert!(source_copy.duplicate);
    assert_eq!(source_copy.message_id, r.message_id);
    assert_eq!(h.page(&r.space_id).len(), 1);
}

#[tokio::test]
async fn sg40_history_opens_no_group_card_until_live_and_loop_guard_precedence_is_stable() {
    let h = H::new();
    let mut old = h.msg("group", "old");
    old.created -= 3_600_000;
    let r = h.inbound(old).await;
    assert_eq!(r.history, Some(inbound::History::Late));
    assert_eq!(r.next, Next::Record(inbound::LABEL_HISTORY_LATE));
    assert!(r.decision_id.is_none());
    assert_eq!(h.row(&r.message_id).history.as_deref(), Some("late"));
    let live = h.inbound(h.msg("group", "live")).await;
    assert!(live.decision_id.is_some());
    assert_eq!(live.next, Next::Handle);
    h.attention(&live.space_id, spaces::Attention::Quiet);
    let mut own = h.msg("group", "bot-own");
    own.sender.handle = FakeChannel::handle(FakeChannel::BOT);
    assert_eq!(h.inbound(own).await.next, Next::Own, "own wins over quiet");
    let mut bot = h.msg("group", "another-bot");
    bot.sender.is_bot = true;
    assert_eq!(
        h.inbound(bot.clone()).await.next,
        Next::Record(inbound::LABEL_QUIET)
    );
    h.attention(&live.space_id, spaces::Attention::Realtime);
    bot.external_id = "bot-live".into();
    assert_eq!(h.inbound(bot).await.next, Next::Record(inbound::LABEL_BOT));
}

#[tokio::test]
async fn sg40_response_rules_and_personal_off_record_policy_keep_early_return_order() {
    let h = H::new();
    h.env
        .store()
        .transaction(|tx| {
            spaces::set_account_rules(
                tx,
                &Viewer::bare(WS, ME),
                &h.conn,
                &json!([{"when":{"kind":"dm"},"act":"reply"}]),
                h.ch.now(),
            )
        })
        .unwrap();
    let r =
        h.ch.on_message(WS, &h.conn, h.msg("dm", "rule-reply"))
            .await
            .unwrap();
    let response = r.response.unwrap();
    assert_eq!(response.rule, Some(0));
    // Without a composing responder the chosen reply becomes an approval draft.
    assert_eq!(response.act, rules::Act::Draft);
    for (policy, label) in [
        (spaces::ReplyAsYou::Off, inbound::LABEL_REPLY_OFF),
        (spaces::ReplyAsYou::Record, inbound::LABEL_REPLY_RECORD),
    ] {
        let seed =
            h.ch.on_inbound(WS, &h.own, h.msg("dm-own", &format!("seed-{label}")))
                .await
                .unwrap();
        h.env
            .store()
            .transaction(|tx| {
                spaces::write_reply_as_you(tx, WS, &seed.space_id, policy, h.ch.now())
            })
            .unwrap();
        let r =
            h.ch.on_message(WS, &h.own, h.msg("dm-own", &format!("policy-{label}")))
                .await
                .unwrap();
        assert_eq!(r.response.unwrap().label, label);
        let mut own = h.msg("dm-own", &format!("mine-{label}"));
        own.mine = true;
        let r = h.ch.on_message(WS, &h.own, own).await.unwrap();
        assert_eq!(r.next, Next::Own);
        assert_eq!(r.response.unwrap().label, inbound::LABEL_OWN);
    }
    assert!(h.fake.sent.lock().unwrap().is_empty());
}
